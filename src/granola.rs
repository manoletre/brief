//! The user's Granola meeting notes: who they talked to, what was decided and what they promised,
//! next to what they log, plan and aim for.
//!
//! Granola has no public REST API for personal accounts, but it runs an official MCP server
//! (mcp.granola.ai) whose sign-in works like Todoist's: one ⏎ and a click on Allow (see
//! oauth.rs). brief talks MCP to it directly and only calls its read-only tools:
//! `list_meetings` and `get_meetings`.
//!
//!   granola.json   client id and tokens (readable only by you)

use crate::oauth::{self, Provider};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, Local, NaiveDate};
use serde_json::{json, Value};
use std::fmt::Write as _;

const MCP_URL: &str = "https://mcp.granola.ai/mcp";
const PROTOCOL: &str = "2025-06-18";
/// Meetings whose full notes go into the brief, and how much of each.
const MAX_MEETINGS: usize = 12;
const MAX_NOTES_CHARS: usize = 6_000;
const MAX_TOTAL_CHARS: usize = 45_000;

pub const GRANOLA: Provider = Provider {
    name: "Granola",
    file: "granola.json",
    register: "https://mcp-auth.granola.ai/oauth2/register",
    authorize: "https://mcp-auth.granola.ai/oauth2/authorize",
    token: "https://mcp-auth.granola.ai/oauth2/token",
    revoke: None,
    scope: "openid profile email offline_access",
    resource: Some(MCP_URL),
};

pub fn auth(store: &Store) -> Option<oauth::Auth> {
    GRANOLA.auth(store)
}

/// Opens Granola's consent page and waits until the user allows (or denies) brief.
pub fn connect(store: &Store) -> Result<String> {
    let (mut a, tok) = GRANOLA.connect(store)?;
    let claims = oauth::id_token_claims(&tok);
    a.account = oauth::account_name(claims["name"].as_str().unwrap_or(""), claims["email"].as_str().unwrap_or(""));
    if a.account.is_empty() {
        a.account = Mcp::open(store).and_then(|mut m| m.call("get_account_info", json!({}))).map(|t| first_email(&t)).unwrap_or_default();
    }
    GRANOLA.save(store, &a)?;
    Ok(match a.account.as_str() {
        "" => "Connected to Granola. Your meeting notes will be part of tomorrow's brief.".into(),
        who => format!("Connected to Granola as {who}. Your meeting notes will be part of tomorrow's brief."),
    })
}

pub fn disconnect(store: &Store) -> Result<String> {
    GRANOLA.disconnect(store)
}

// ── A minimal MCP client (streamable HTTP) ──

struct Mcp<'a> {
    store: &'a Store,
    token: String,
    session: Option<String>,
    next_id: i64,
}

impl<'a> Mcp<'a> {
    fn open(store: &'a Store) -> Result<Self> {
        let mut m = Mcp { store, token: GRANOLA.access_token(store, false)?, session: None, next_id: 1 };
        m.rpc("initialize", json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": { "name": "brief", "version": env!("CARGO_PKG_VERSION") },
        }))?;
        m.post(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(m)
    }

    /// The text a tool returns.
    fn call(&mut self, tool: &str, args: Value) -> Result<String> {
        let result = self.rpc("tools/call", json!({ "name": tool, "arguments": args }))?;
        let text: Vec<&str> = result["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect();
        let text = text.join("\n");
        if result["isError"].as_bool() == Some(true) {
            bail!("Granola's {tool} failed: {}", text.chars().take(300).collect::<String>());
        }
        Ok(text)
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let reply = self.post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let msg = reply.into_iter().find(|m| m["id"] == id).with_context(|| format!("Granola didn't answer {method}"))?;
        if let Some(err) = msg.get("error") {
            bail!("Granola: {}", err["message"].as_str().unwrap_or("error"));
        }
        Ok(msg["result"].clone())
    }

    /// One JSON-RPC message; returns the messages that came back (plain JSON or an SSE stream).
    fn post(&mut self, body: &Value) -> Result<Vec<Value>> {
        let mut retried = false;
        loop {
            let mut headers = vec!["Accept: application/json, text/event-stream".to_owned(), format!("MCP-Protocol-Version: {PROTOCOL}")];
            if let Some(s) = &self.session {
                headers.push(format!("Mcp-Session-Id: {s}"));
            }
            let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
            let r = oauth::request("POST", MCP_URL, Some(&self.token), &headers, &[], Some(body), true)?;
            if r.code == 401 && !retried {
                self.token = GRANOLA.access_token(self.store, true)?;
                retried = true;
                continue;
            }
            if !(200..300).contains(&r.code) {
                bail!("Granola answered {}: {}", r.code, r.body.chars().take(300).collect::<String>().trim());
            }
            if let Some(s) = r.header("mcp-session-id") {
                self.session = Some(s.to_owned());
            }
            return Ok(messages(&r.body));
        }
    }
}

/// JSON-RPC messages in a response body: one JSON value, or the `data:` lines of an SSE stream.
fn messages(body: &str) -> Vec<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
        return match v {
            Value::Array(a) => a,
            v => vec![v],
        };
    }
    let mut out = Vec::new();
    let mut data = String::new();
    for line in body.lines().chain([""]) {
        if let Some(d) = line.strip_prefix("data:") {
            data.push_str(d.trim_start());
        } else if line.trim().is_empty() && !data.is_empty() {
            out.extend(serde_json::from_str::<Value>(&data).ok());
            data.clear();
        }
    }
    out
}

// ── For the daily brief ──

/// The Granola part of the brief prompt: full notes for meetings since the last brief, and
/// titles of the rest of the month. Empty when Granola isn't connected.
pub fn brief_context(store: &Store, since: DateTime<Local>) -> String {
    if auth(store).is_none() {
        return String::new();
    }
    match context(store, since) {
        Ok(text) => text,
        Err(e) => format!("(Granola is connected but couldn't be read today: {e:#}. Mention this in one short line.)\n"),
    }
}

fn context(store: &Store, since: DateTime<Local>) -> Result<String> {
    let mut m = Mcp::open(store)?;
    let month = m.call("list_meetings", json!({ "time_range": "last_30_days" }))?;

    // Meetings since the last brief: this week's, plus last week's if the last brief was before Monday.
    let monday = Local::now().date_naive() - chrono::Duration::days(Local::now().weekday().num_days_from_monday() as i64);
    let mut recent = meetings(&m.call("list_meetings", json!({ "time_range": "this_week" }))?);
    if since.date_naive() < monday {
        recent.extend(meetings(&m.call("list_meetings", json!({ "time_range": "last_week" }))?));
    }
    recent.retain(|(_, date)| date.map_or(true, |d| d >= since.date_naive()));
    recent.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
    recent.dedup_by(|a, b| a.0 == b.0);
    recent.truncate(MAX_MEETINGS);

    let mut p = String::new();
    if !recent.is_empty() {
        let _ = writeln!(p, "Notes from {} meeting(s) since the last brief:\n", recent.len());
        let ids: Vec<&str> = recent.iter().map(|(id, _)| id.as_str()).collect();
        let mut total = 0;
        for chunk in ids.chunks(10) {
            let notes = m.call("get_meetings", json!({ "meeting_ids": chunk }))?;
            for part in split_per_meeting(&notes, chunk) {
                if total > MAX_TOTAL_CHARS {
                    break;
                }
                let part = cut(part.trim(), MAX_NOTES_CHARS);
                total += part.len();
                p.push_str(&part);
                p.push_str("\n\n");
            }
        }
    } else {
        p.push_str("No meetings since the last brief.\n\n");
    }
    p.push_str("All meetings in the last 30 days (for context):\n\n");
    p.push_str(&cut(month.trim(), 12_000));
    p.push('\n');
    Ok(p)
}

/// Meeting ids in a `list_meetings` answer, each with its date. Dates come either before every
/// id or after it, so look at which side the first one is on.
fn meetings(text: &str) -> Vec<(String, Option<NaiveDate>)> {
    let ids = uuids(text);
    let before = ids.first().is_some_and(|(_, at)| first_date(&text[..*at]).is_some());
    ids.iter()
        .enumerate()
        .map(|(i, (id, at))| {
            let date = if before {
                let from = if i > 0 { ids[i - 1].1 + 36 } else { 0 };
                last_date(&text[from..*at])
            } else {
                first_date(&text[at + 36..ids.get(i + 1).map_or(text.len(), |n| n.1)])
            };
            (id.clone(), date)
        })
        .collect()
}

/// UUIDs (8-4-4-4-12 hex) and where they start, in order, without repeats.
fn uuids(text: &str) -> Vec<(String, usize)> {
    let b = text.as_bytes();
    let shape = |i: usize| {
        b.len() >= i + 36
            && (0..36).all(|k| if [8, 13, 18, 23].contains(&k) { b[i + k] == b'-' } else { b[i + k].is_ascii_hexdigit() })
            && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
            && b.get(i + 36).map_or(true, |c| !c.is_ascii_alphanumeric())
    };
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut i = 0;
    while i + 36 <= b.len() {
        if shape(i) {
            let id = text[i..i + 36].to_lowercase();
            if !out.iter().any(|(x, _)| *x == id) {
                out.push((id, i));
            }
            i += 36;
        } else {
            i += 1;
        }
    }
    out
}

fn dates(s: &str) -> impl Iterator<Item = NaiveDate> + '_ {
    let b = s.as_bytes();
    (0..b.len().saturating_sub(9)).filter_map(move |i| {
        let ymd = b[i..i + 10].iter().enumerate().all(|(k, c)| if k == 4 || k == 7 { *c == b'-' } else { c.is_ascii_digit() });
        ymd.then(|| NaiveDate::parse_from_str(&s[i..i + 10], "%Y-%m-%d").ok()).flatten()
    })
}

fn first_date(s: &str) -> Option<NaiveDate> {
    dates(s).next()
}

fn last_date(s: &str) -> Option<NaiveDate> {
    dates(s).last()
}

/// Cut a `get_meetings` answer into one piece per meeting, at each id's first mention.
fn split_per_meeting<'t>(text: &'t str, ids: &[&str]) -> Vec<&'t str> {
    let lower = text.to_lowercase();
    let mut starts: Vec<usize> = ids.iter().filter_map(|id| lower.find(&id.to_lowercase())).collect();
    starts.sort_unstable();
    if starts.len() < 2 {
        return vec![text];
    }
    // Start each piece at the beginning of the line (or tag) that holds the id.
    let starts: Vec<usize> = starts.iter().map(|&s| text[..s].rfind(['\n', '<']).map_or(0, |n| n)).collect();
    let mut out = vec![&text[..starts[0]]];
    for (k, &s) in starts.iter().enumerate() {
        out.push(&text[s..starts.get(k + 1).copied().unwrap_or(text.len())]);
    }
    out.retain(|p| !p.trim().is_empty());
    out
}

fn first_email(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || "\"'<>,()[]{}".contains(c))
        .find(|w| w.contains('@') && w.contains('.'))
        .unwrap_or("")
        .to_owned()
}

fn cut(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_owned();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (cut)", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sse_and_plain_json() {
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        assert_eq!(messages(sse)[0]["id"], 1);
        assert_eq!(messages("{\"id\":2}")[0]["id"], 2);
    }

    #[test]
    fn finds_meetings_with_their_dates() {
        let text = r#"<meeting id="0f8fad5b-d9cb-469f-a165-70867728950e" title="Hari sync" date="2026-10-07">
<meeting id="7c9e6679-7425-40de-944b-e07fc1f90ae7" title="Anshul" date="2026-10-12T11:15:00Z">
- 7C9E6679-7425-40DE-944B-E07FC1F90AE7 again"#;
        let m = meetings(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], ("0f8fad5b-d9cb-469f-a165-70867728950e".into(), NaiveDate::from_ymd_opt(2026, 10, 7)));
        assert_eq!(m[1].1, NaiveDate::from_ymd_opt(2026, 10, 12));

        let before = "2026-10-07 Hari sync (0f8fad5b-d9cb-469f-a165-70867728950e)\n2026-10-12 Anshul (7c9e6679-7425-40de-944b-e07fc1f90ae7)";
        let m = meetings(before);
        assert_eq!((m[0].1, m[1].1), (NaiveDate::from_ymd_opt(2026, 10, 7), NaiveDate::from_ymd_opt(2026, 10, 12)));
    }

    #[test]
    fn splits_notes_per_meeting() {
        let ids = ["0f8fad5b-d9cb-469f-a165-70867728950e", "7c9e6679-7425-40de-944b-e07fc1f90ae7"];
        let text = format!("<meeting id=\"{}\">A notes</meeting>\n<meeting id=\"{}\">B notes</meeting>", ids[0], ids[1]);
        let parts = split_per_meeting(&text, &ids);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("A notes") && parts[1].contains("B notes"));
    }

    #[test]
    fn finds_an_email() {
        assert_eq!(first_email("Account: {\"email\": \"m@x.io\", \"workspace\": \"w\"}"), "m@x.io");
    }
}
