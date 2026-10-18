//! `brief serve`: MCP over streamable HTTP at /mcp, the briefs website at /, and a
//! scheduler that writes the daily brief once per day after `brief_hour`.

use crate::{generate, html, mcp, store::{Feedback, Store}};
use anyhow::Result;
use axum::{
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Json, Router,
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
    });

    tokio::spawn(scheduler(app.clone()));

    let router = Router::new()
        .route("/", get(index))
        .route("/brief/:date", get(brief))
        .route("/log", get(log))
        .route("/feedback", get(feedback_page))
        .route("/objectives", get(objectives).post(save_objectives))
        .route("/api/feedback", post(add_feedback))
        .route("/api/generate", post(start_generate))
        .route("/mcp", post(mcp_post).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(mcp_delete))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app);

    let addr = format!("127.0.0.1:{}", cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    eprintln!("brief: serving on http://{addr}  (MCP endpoint: http://{addr}/mcp)");
    axum::serve(listener, router).with_graceful_shutdown(async { tokio::signal::ctrl_c().await.ok(); }).await?;
    Ok(())
}

/// Requests that arrive through a tunnel (they carry proxy headers) may only reach /mcp,
/// and only with the access key. Browser requests from other origins are refused.
async fn guard(State(app): S, req: Request, next: Next) -> Response {
    let h = req.headers();
    let remote = ["x-forwarded-for", "cf-connecting-ip", "forwarded", "x-real-ip"].iter().any(|k| h.contains_key(*k));
    if let Some(origin) = h.get("origin").and_then(|v| v.to_str().ok()) {
        let host = origin.split("://").nth(1).unwrap_or("").split(':').next().unwrap_or("");
        if !remote && !matches!(host, "localhost" | "127.0.0.1") {
            return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
        }
    }
    if remote {
        if req.uri().path() != "/mcp" {
            return StatusCode::NOT_FOUND.into_response();
        }
        let Some(key) = app.store.config().access_key else {
            return (StatusCode::FORBIDDEN, "remote access disabled; run `brief key` to enable").into_response();
        };
        let bearer = h.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
        let query_key = req.uri().query().unwrap_or("").split('&').find_map(|kv| kv.strip_prefix("key="));
        if bearer != Some(key.as_str()) && query_key != Some(key.as_str()) {
            return (StatusCode::UNAUTHORIZED, "missing or wrong key").into_response();
        }
    }
    next.run(req).await
}

async fn scheduler(app: Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        tick.tick().await;
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

fn spawn_generate(app: &Arc<App>) {
    if app.generating.swap(true, Ordering::SeqCst) {
        return;
    }
    *app.last_attempt.lock().unwrap() = Some(Local::now());
    let app = app.clone();
    tokio::spawn(async move {
        eprintln!("brief: generating daily brief…");
        let result = generate::generate(&app.store).await;
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

async fn brief(State(app): S, Path(date): Path<String>) -> Response {
    let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match app.store.brief(date) {
        Some(b) => Html(html::brief(&b, &app.store.feedback())).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct LogQuery {
    project: Option<String>,
}

async fn log(State(app): S, Query(q): Query<LogQuery>) -> Html<String> {
    Html(html::log(&app.store.entries(), q.project.as_deref()))
}

async fn feedback_page(State(app): S) -> Html<String> {
    // Keep only the latest feedback per item.
    let mut latest: Vec<Feedback> = Vec::new();
    for f in app.store.feedback() {
        latest.retain(|x| !(x.brief_date == f.brief_date && x.item_id == f.item_id));
        latest.push(f);
    }
    Html(html::feedback_page(&latest))
}

async fn objectives(State(app): S) -> Html<String> {
    Html(html::objectives(&app.store.objectives()))
}

#[derive(Deserialize)]
struct ObjectivesForm {
    text: String,
}

async fn save_objectives(State(app): S, Form(f): Form<ObjectivesForm>) -> Response {
    match app.store.save_objectives(&f.text.replace("\r\n", "\n")) {
        Ok(()) => Redirect::to("/objectives").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
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

    let replies: Vec<Value> = {
        let mut sessions = app.sessions.lock().unwrap();
        let session = sessions.entry(sid.clone()).or_default();
        messages.iter().filter_map(|m| mcp::handle(&app.store, session, m)).collect()
    };

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
