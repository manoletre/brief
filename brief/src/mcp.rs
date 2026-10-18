//! Minimal MCP (JSON-RPC 2.0) server: initialize, ping, tools/list, tools/call.
//! Transport-agnostic; `run_stdio` and the HTTP route in server.rs both call `handle`.

use crate::store::{new_id, Entry, Store};
use anyhow::Result;
use chrono::{Duration, Local};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "This server is the user's personal work journal. Whenever the conversation \
involves meaningful work on one of the user's projects (progress, a decision, an idea, a blocker, an open \
question, something they learned), call `log_work` with a concise, self-contained summary. Log without \
being asked, once per distinct topic; never log small talk. Use `get_context` to see the user's goals and \
recent work when it would help you answer.";

const KINDS: &[&str] = &["progress", "decision", "idea", "question", "blocker", "learning", "note"];

/// Per-connection state.
#[derive(Default)]
pub struct Session {
    pub client_name: Option<String>,
}

pub fn handle(store: &Store, session: &mut Session, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    // Notifications (no id) and stray responses never get a reply.
    let id = id?;
    if method.is_empty() {
        return None;
    }

    let result = match method {
        "initialize" => {
            session.client_name = params
                .pointer("/clientInfo/name")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = if PROTOCOL_VERSIONS.contains(&requested) { requested } else { PROTOCOL_VERSIONS[0] };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "brief", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(store, session, name, &args) {
                Ok(text) => Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false })),
                Err(e) => Ok(json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true })),
            }
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    };

    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

fn tool_list() -> Value {
    json!([
        {
            "name": "log_work",
            "description": "Record something the user is working on so it shows up in their daily brief. \
Call this proactively whenever the user makes progress, takes a decision, has an idea, hits a blocker, \
asks an open question, or learns something relevant to one of their projects. Write the summary so it \
makes sense on its own weeks later, without this conversation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Short project name, reused consistently (e.g. 'chessLLM'). Call get_context to see existing names." },
                    "summary": { "type": "string", "description": "One or two sentences: what happened / what the user is thinking." },
                    "kind": { "type": "string", "enum": KINDS },
                    "details": { "type": "string", "description": "Optional longer notes: context, options considered, links, open questions." },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "source": { "type": "string", "description": "Optional: which assistant is writing this (e.g. 'ChatGPT')." }
                },
                "required": ["project", "summary"]
            }
        },
        {
            "name": "get_context",
            "description": "Read the user's stated objectives, their known projects, and recently logged work. \
Use it to pick consistent project names and to ground advice in what the user is actually doing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Only return entries for this project." },
                    "days": { "type": "integer", "description": "How many days back to look (default 14)." }
                }
            }
        }
    ])
}

fn call_tool(store: &Store, session: &Session, name: &str, args: &Value) -> Result<String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_owned();
    match name {
        "log_work" => {
            let (project, summary) = (s("project"), s("summary"));
            anyhow::ensure!(!project.is_empty() && !summary.is_empty(), "`project` and `summary` are required");
            let kind = s("kind");
            let kind = if KINDS.contains(&kind.as_str()) { kind } else { "note".into() };
            let source = Some(s("source"))
                .filter(|v| !v.is_empty())
                .or_else(|| session.client_name.clone())
                .unwrap_or_else(|| "unknown".into());
            let tags = args
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            let entry = Entry {
                id: new_id(),
                ts: Local::now(),
                project: project.clone(),
                kind,
                summary,
                details: s("details"),
                tags,
                source,
            };
            store.add_entry(&entry)?;
            Ok(format!("Logged to '{project}' (id {}).", entry.id))
        }
        "get_context" => {
            let days = args.get("days").and_then(Value::as_i64).unwrap_or(14).clamp(1, 365);
            Ok(context_text(store, Some(s("project")).filter(|p| !p.is_empty()).as_deref(), days))
        }
        _ => anyhow::bail!("unknown tool: {name}"),
    }
}

pub fn context_text(store: &Store, project: Option<&str>, days: i64) -> String {
    let entries = store.entries();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &entries {
        *counts.entry(e.project.as_str()).or_default() += 1;
    }
    let cutoff = Local::now() - Duration::days(days);

    let mut out = String::from("## Objectives\n\n");
    let objectives = store.objectives();
    out.push_str(if objectives.trim().is_empty() { "(none set yet)" } else { objectives.trim() });
    out.push_str("\n\n## Known projects\n\n");
    for (p, n) in &counts {
        out.push_str(&format!("- {p} ({n} entries)\n"));
    }
    out.push_str(&format!("\n## Logged work, last {days} days\n\n"));
    let recent: Vec<_> = entries
        .iter()
        .filter(|e| e.ts >= cutoff && project.map_or(true, |p| e.project.eq_ignore_ascii_case(p)))
        .collect();
    if recent.is_empty() {
        out.push_str("(nothing yet)\n");
    }
    for e in recent.iter().rev().take(100) {
        out.push_str(&format!("- {} [{}] {} ({}): {}\n", e.ts.format("%Y-%m-%d"), e.project, e.kind, e.source, e.summary));
    }
    out
}

/// Newline-delimited JSON-RPC over stdin/stdout, for clients that launch the server themselves
/// (Claude Desktop, Claude Code).
pub async fn run_stdio(store: Store) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut session = Session::default();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(|m| handle(&store, &mut session, m)).collect();
                (!replies.is_empty()).then(|| Value::Array(replies))
            }
            Ok(msg) => handle(&store, &mut session, &msg),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } })),
        };
        if let Some(reply) = reply {
            stdout.write_all(format!("{reply}\n").as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
