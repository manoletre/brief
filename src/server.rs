//! `brief serve`: MCP over streamable HTTP at /mcp, the briefs website at /, and a
//! scheduler that writes the daily brief once per day after `brief_hour`.

use crate::{anki, browsing, generate, html, mcp, store::{Feedback, Store}};
use anyhow::Result;
use axum::{
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Local, NaiveDate, Timelike};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct App {
    store: Store,
    generating: AtomicBool,
    last_error: Mutex<Option<String>>,
    last_attempt: Mutex<Option<DateTime<Local>>>,
    sessions: Mutex<HashMap<String, mcp::Session>>,
    /// The Funnel-facing listener is bound.
    public_up: AtomicBool,
    anki_busy: AtomicBool,
}

type S = State<Arc<App>>;

pub async fn serve(store: Store) -> Result<()> {
    let cfg = store.config();
    let app = Arc::new(App {
        store,
        generating: AtomicBool::new(false),
        last_error: Mutex::new(None),
        last_attempt: Mutex::new(None),
        sessions: Mutex::new(HashMap::new()),
        public_up: AtomicBool::new(false),
        anki_busy: AtomicBool::new(false),
    });

    // A new build of brief may ship a new Chrome extension: write it so Chrome reloads it.
    browsing::refresh(&app.store);
    tokio::spawn(scheduler(app.clone()));
    if cfg.tunnel {
        tokio::spawn(serve_public(app.clone(), cfg.public_port()));
    }

    let router = Router::new()
        .route("/", get(index))
        .route("/today", get(today))
        .route("/brief/:date", get(brief))
        .route("/api/browsing", post(add_browsing))
        .route("/api/feedback", post(add_feedback))
        .route("/api/generate", post(start_generate))
        .route("/api/regenerate/:date", post(start_regenerate))
        .route("/media/:date/:version/:file", get(media))
        .route("/api/status", get(status))
        .route("/icon.svg", get(|| async { ([("content-type", "image/svg+xml")], mcp::ICON_SVG) }))
        .route("/icon.png", get(|| async { ([("content-type", "image/png")], mcp::ICON_PNG) }))
        .merge(mcp_routes())
        .layer(middleware::from_fn(local_guard))
        .with_state(app);

    let addr = format!("127.0.0.1:{}", cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("brief: serving on http://{addr}  (MCP endpoint: http://{addr}/mcp)");
    axum::serve(listener, router).with_graceful_shutdown(async { tokio::signal::ctrl_c().await.ok(); }).await?;
    Ok(())
}

fn mcp_routes() -> Router<Arc<App>> {
    Router::new().route("/mcp", post(mcp_post).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(mcp_delete))
}

/// The main port is for this Mac only: requests must be addressed to localhost, and browser
/// requests from other sites are refused (blocks cross-site posts and DNS rebinding).
async fn local_guard(req: Request, next: Next) -> Response {
    let h = req.headers();
    let header = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
    if !header("host").is_some_and(is_local) {
        return (StatusCode::FORBIDDEN, "brief only answers on 127.0.0.1").into_response();
    }
    // brief's Chrome extension posts visits from its own origin; that route checks its key.
    let extension = req.uri().path() == "/api/browsing" && header("origin").is_some_and(|o| o.starts_with("chrome-extension://"));
    if header("origin").is_some_and(|o| !is_local(o)) && !extension {
        return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
    }
    next.run(req).await
}

/// `localhost:7878`, `http://127.0.0.1:7878`, `[::1]` and the like.
fn is_local(v: &str) -> bool {
    let host = v.split_once("://").map_or(v, |(_, h)| h);
    let name = match host.rsplit_once(':') {
        Some((n, p)) if p.chars().all(|c| c.is_ascii_digit()) => n,
        _ => host,
    };
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

/// What Tailscale Funnel forwards to: only /mcp, and only with the access key. It has its own
/// port so nothing arriving from the internet can reach the briefs website.
async fn serve_public(app: Arc<App>, port: u16) {
    let router = mcp_routes().layer(middleware::from_fn_with_state(app.clone(), key_guard)).with_state(app.clone());
    let addr = format!("127.0.0.1:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => return eprintln!("brief: couldn't open the public port {addr}: {e}"),
    };
    eprintln!("brief: public MCP endpoint on http://{addr}/mcp (for Tailscale Funnel)");
    app.public_up.store(true, Ordering::SeqCst);
    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("brief: public port stopped: {e}");
    }
    app.public_up.store(false, Ordering::SeqCst);
}

async fn key_guard(State(app): S, req: Request, next: Next) -> Response {
    let Some(key) = app.store.config().access_key else {
        return (StatusCode::FORBIDDEN, "remote access disabled").into_response();
    };
    let bearer = req.headers().get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    let query_key = req.uri().query().unwrap_or("").split('&').find_map(|kv| kv.strip_prefix("key="));
    if ![bearer, query_key].into_iter().flatten().any(|k| same(k, &key)) {
        return (StatusCode::UNAUTHORIZED, "missing or wrong key").into_response();
    }
    next.run(req).await
}

/// Constant-time comparison, so the key can't be guessed one character at a time.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn scheduler(app: Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    for minute in 0u64.. {
        tick.tick().await;
        anki_upkeep(&app, minute);
        if minute % (24 * 60) == 0 {
            let _ = browsing::prune(&app.store);
        }
        let now = Local::now();
        if now.hour() < app.store.config().brief_hour || app.store.brief(now.date_naive()).is_some() {
            continue;
        }
        // After a failure, retry at most once an hour.
        let recent = app.last_attempt.lock().unwrap().is_some_and(|t| now - t < chrono::Duration::hours(1));
        if !recent {
            spawn_generate(&app);
        }
    }
}

/// While Anki is open (never opening it ourselves): every 10 minutes add cards that were waiting
/// for it, and every half hour write down buried cards before Anki unburies them the next day.
fn anki_upkeep(app: &Arc<App>, minute: u64) {
    let record = minute % 30 == 0;
    let flush = minute % 10 == 0 && anki::has_pending(&app.store);
    if !(flush || record) || app.anki_busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        if anki::available(false) {
            if record {
                if let Err(e) = anki::record_signals(&app.store) {
                    eprintln!("brief: couldn't read buried Anki cards: {e:#}");
                }
            }
            if flush {
                match anki::flush_pending(&app.store) {
                    Ok(n) if n > 0 => eprintln!("brief: added {n} waiting batch(es) of Anki cards"),
                    Ok(_) => {}
                    Err(e) => eprintln!("brief: couldn't add waiting Anki cards: {e:#}"),
                }
            }
        }
        app.anki_busy.store(false, Ordering::SeqCst);
    });
}

fn spawn_generate(app: &Arc<App>) {
    spawn_write(app, None);
}

/// Write today's brief, or rewrite the one of `date`, in the background.
fn spawn_write(app: &Arc<App>, date: Option<NaiveDate>) {
    if app.generating.swap(true, Ordering::SeqCst) {
        return;
    }
    if date.is_none() {
        *app.last_attempt.lock().unwrap() = Some(Local::now());
    }
    let app = app.clone();
    tokio::spawn(async move {
        eprintln!("brief: {}…", date.map_or("generating daily brief".to_owned(), |d| format!("rewriting the brief of {d}")));
        let result = match date {
            Some(d) => generate::regenerate(&app.store, d).await,
            None => generate::generate(&app.store).await,
        };
        match &result {
            Ok(b) => eprintln!("brief: done — {}", b.headline),
            Err(e) => eprintln!("brief: generation failed: {e:#}"),
        }
        *app.last_error.lock().unwrap() = result.err().map(|e| format!("{e:#}"));
        app.generating.store(false, Ordering::SeqCst);
    });
}

async fn index(State(app): S) -> Html<String> {
    let err = app.last_error.lock().unwrap().clone();
    Html(html::index(&app.store, app.generating.load(Ordering::SeqCst), err.as_deref()))
}

/// The newest brief (today's once it's written), or the list if there's none yet.
async fn today(State(app): S) -> Redirect {
    match app.store.brief_dates().first() {
        Some(d) => Redirect::to(&format!("/brief/{d}")),
        None => Redirect::to("/"),
    }
}

#[derive(Deserialize)]
struct BriefQuery {
    v: Option<u32>,
}

async fn brief(State(app): S, Path(date): Path<String>, axum::extract::Query(q): axum::extract::Query<BriefQuery>) -> Response {
    let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let dates = app.store.brief_dates();
    // Newest first: the one before `date` in the list is the next (newer) brief.
    let i = dates.iter().position(|d| *d == date);
    let newer = i.and_then(|i| i.checked_sub(1)).map(|i| dates[i]);
    let older = i.and_then(|i| dates.get(i + 1).copied());
    let current = app.store.brief(date);
    let shown = match q.v {
        Some(n) if current.as_ref().is_some_and(|b| b.version.max(1) != n) => app.store.brief_version(date, n),
        _ => current.clone(),
    };
    let Some(b) = shown else { return StatusCode::NOT_FOUND.into_response() };
    let mut versions = app.store.brief_versions(date);
    if let Some(c) = &current {
        versions.retain(|n| *n != c.version.max(1));
        versions.push(c.version.max(1));
    }
    let nav = html::Versions { all: versions, current: current.map_or(1, |c| c.version.max(1)), rewriting: app.generating.load(Ordering::SeqCst) };
    Html(html::brief(&b, &app.store.feedback(), older, newer, &nav)).into_response()
}

async fn start_regenerate(State(app): S, Path(date): Path<String>) -> StatusCode {
    let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else { return StatusCode::NOT_FOUND };
    if app.store.brief(date).is_none() {
        return StatusCode::NOT_FOUND;
    }
    spawn_write(&app, Some(date));
    StatusCode::ACCEPTED
}

/// A brief's video: /media/DATE/vN/ITEM.mp4, with byte ranges so the player can seek.
async fn media(State(app): S, Path((date, version, file)): Path<(String, String, String)>, headers: HeaderMap) -> Response {
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') && !s.starts_with('.');
    let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else { return StatusCode::NOT_FOUND.into_response() };
    if !ok(&version) || !ok(&file) || !file.ends_with(".mp4") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = app.store.media_dir(date).join(&version).join(&file);
    let Ok(Ok(data)) = tokio::task::spawn_blocking(move || std::fs::read(path)).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let len = data.len();
    let range = headers.get("range").and_then(|v| v.to_str().ok()).and_then(|r| r.strip_prefix("bytes=")).and_then(|r| {
        let (a, b) = r.split_once('-')?;
        let start: usize = a.parse().ok()?;
        let end: usize = if b.is_empty() { len.checked_sub(1)? } else { b.parse::<usize>().ok()?.min(len.checked_sub(1)?) };
        (start <= end).then_some((start, end))
    });
    let mut h = HeaderMap::new();
    h.insert("content-type", HeaderValue::from_static("video/mp4"));
    h.insert("accept-ranges", HeaderValue::from_static("bytes"));
    h.insert("cache-control", HeaderValue::from_static("max-age=3600"));
    match range {
        Some((a, b)) => {
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {a}-{b}/{len}")) {
                h.insert("content-range", v);
            }
            (StatusCode::PARTIAL_CONTENT, h, data[a..=b].to_vec()).into_response()
        }
        None => (h, data).into_response(),
    }
}

#[derive(Deserialize)]
struct BrowsingIn {
    #[serde(default)]
    visits: Vec<browsing::Visit>,
}

async fn add_browsing(State(app): S, headers: HeaderMap, Json(b): Json<BrowsingIn>) -> Response {
    let sent = headers.get("x-brief-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !browsing::key(&app.store).is_some_and(|k| same(sent, &k)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let store = app.store.clone();
    if tokio::task::spawn_blocking(move || browsing::ingest(&store, b.visits)).await.map_or(true, |r| r.is_err()) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Json(serde_json::json!({ "build": browsing::build(), "installed_build": browsing::installed_build(&app.store) })).into_response()
}

#[derive(Deserialize)]
struct FeedbackIn {
    brief_date: NaiveDate,
    #[serde(default)]
    item_id: String,
    #[serde(default)]
    item_title: String,
    #[serde(default)]
    rating: String,
    #[serde(default)]
    comment: String,
}

async fn add_feedback(State(app): S, Json(f): Json<FeedbackIn>) -> StatusCode {
    let fb = Feedback {
        ts: Local::now(),
        brief_date: f.brief_date,
        item_id: f.item_id,
        item_title: f.item_title,
        rating: f.rating,
        comment: f.comment.trim().to_owned(),
    };
    match app.store.add_feedback(&fb) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn status(State(app): S) -> Json<Value> {
    let cfg = app.store.config();
    let live = cfg.tunnel && app.public_up.load(Ordering::SeqCst);
    let mcp_url = match (&cfg.public_url, &cfg.access_key) {
        (Some(u), Some(k)) if live => Some(format!("{u}/mcp?key={k}")),
        _ => None,
    };
    Json(serde_json::json!({
        "generating": app.generating.load(Ordering::SeqCst),
        "last_error": *app.last_error.lock().unwrap(),
        "tunnel": cfg.tunnel,
        "tunnel_url": mcp_url,
    }))
}

async fn start_generate(State(app): S) -> StatusCode {
    spawn_generate(&app);
    StatusCode::ACCEPTED
}

async fn mcp_post(State(app): S, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let incoming_sid = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let messages = match &body {
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    };
    let is_init = messages.iter().any(|m| m.get("method").and_then(Value::as_str) == Some("initialize"));
    let sid = if is_init { crate::store::new_id() } else { incoming_sid.unwrap_or_default() };

    // Tools can take a while (the Anki card reviewer runs Claude), so handle the messages on a
    // blocking thread without holding the sessions lock.
    let mut session = app.sessions.lock().unwrap().entry(sid.clone()).or_default().clone();
    let (replies, session) = {
        let app = app.clone();
        tokio::task::spawn_blocking(move || {
            let replies: Vec<Value> = messages.iter().filter_map(|m| mcp::handle(&app.store, &mut session, m)).collect();
            (replies, session)
        })
        .await
        .unwrap_or_default()
    };
    app.sessions.lock().unwrap().insert(sid.clone(), session);

    if replies.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    let payload = if body.is_array() { Value::Array(replies) } else { replies.into_iter().next().unwrap() };
    let mut resp = Json(payload).into_response();
    if is_init {
        resp.headers_mut().insert("mcp-session-id", HeaderValue::from_str(&sid).unwrap());
    }
    resp
}

async fn mcp_delete(State(app): S, headers: HeaderMap) -> StatusCode {
    if let Some(sid) = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()) {
        app.sessions.lock().unwrap().remove(sid);
    }
    StatusCode::NO_CONTENT
}
