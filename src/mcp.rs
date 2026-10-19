//! Minimal MCP (JSON-RPC 2.0) server: initialize, ping, tools/list, tools/call.
//! Transport-agnostic; `run_stdio` and the HTTP route in server.rs both call `handle`.

use crate::{anki, todos};
use crate::store::{new_id, Brief, Entry, Store};
use anyhow::Result;
use chrono::{Duration, Local, NaiveDate};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "brief is the user's personal work journal. Every morning an agent reads everything \
logged here, together with the user's objectives, and writes them a daily brief: feedback on their work, \
connections between projects, blind spots, things to read, and next steps. The brief is only as good as this \
log, and the user will not remember to ask you to log, so keeping it up to date is your job.

Call `log_work` on your own, without asking permission, when the conversation involves one of the user's \
projects and:
- a decision is made or options are weighed (record the choice and the reason)
- real progress happens: something is built, fixed, shipped, written or figured out
- a new idea, plan or change of direction comes up
- the user is blocked, frustrated, or left with an open question
- the user learns something or finds a useful resource
- the user states a goal, deadline or priority

Don't log small talk, general questions unrelated to the user's own work, or anything they ask to keep \
private. Log once per distinct topic, at the moment it settles or the conversation moves on; in long sessions \
log at each new milestone instead of saving everything for the end, and never repeat an earlier entry.

Write each entry for a reader who never saw this conversation, weeks from now: name the specific thing (file, \
library, bug, person, number), say what changed and why, and put alternatives, links and details in `details`. \
Describe the work ('Moved auth to JWT because sessions broke on mobile'), not the chat ('Discussed auth').

Call `get_context` at the start of a conversation about the user's projects, before your first `log_work` \
(so you reuse the exact project name), and whenever their goals or recent work would sharpen your answer. \
After logging, a brief mention such as 'Logged to brief.' is enough.

brief is also connected to the user's Anki (spaced-repetition flashcards), which they use to keep remembering \
what they have already learned, e.g. for job interviews. A journal entry and an Anki card are different things:
- A journal entry (`log_work`) records what happened in the user's work, for tomorrow's brief: decisions, \
progress, what they studied, where they struggled. It is about the user and is read once.
- An Anki card (`anki_add_cards`) is one atomic fact the user already understands and wants to recall for months: \
a question with one short answer, true outside this conversation. It is about the subject and is reviewed again \
and again.
Studying a lesson is a journal entry; the five facts from it worth keeping are five cards. Cards are for \
remembering, not for learning: don't make cards for things the user hasn't understood yet.

Call `anki_search` before teaching or quizzing the user, to see what they already have in Anki and how well they \
remember it, and before proposing cards, to avoid duplicates. Add cards when the user asks, or after they've \
clearly understood something worth keeping: propose the cards (front and back) and add them on a yes. A reviewer \
checks every card and may rewrite, split or turn it down; tell the user what was actually added.

Call `get_briefs` when the user refers to their brief ('the brief said...', 'what did it suggest yesterday?') \
or wants to act on its advice. The user's objectives steer every brief, so change them with `update_objectives` \
only when the user asks, or after they agree to a change you proposed; never on your own.

brief also holds the user's to-do list: what they still plan to do, next to what they did (the journal) and why \
(the objectives). Call `get_todos` when it helps to know what's on their plate or what's due, and before changing a \
to-do, to get its id. Add a to-do with `add_todo` or change one with `update_todo` (reword, reschedule, move, mark \
done) when the user asks, or after proposing it, e.g. a next step that came out of the conversation, and getting a \
yes; never on your own. A to-do is something still to do; what was done or decided goes to `log_work`.";

pub const ICON_SVG: &str = include_str!("../assets/icon.svg");
pub const ICON_PNG: &[u8] = include_bytes!("../assets/icon-3d.png");

const KINDS: &[&str] = &["progress", "decision", "idea", "question", "blocker", "learning", "note"];

/// Per-connection state.
#[derive(Default, Clone)]
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
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": {
                    "name": "brief",
                    "title": "Brief",
                    "version": env!("CARGO_PKG_VERSION"),
                    "description": "Personal work journal that turns what you log into a daily brief.",
                    "icons": icons(),
                },
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

/// The logo as data URIs, so it shows up even for stdio clients and over the Funnel port.
fn icons() -> Value {
    json!([
        { "src": format!("data:image/svg+xml;base64,{}", base64(ICON_SVG.as_bytes())), "mimeType": "image/svg+xml", "sizes": ["any"] },
        { "src": format!("data:image/png;base64,{}", base64(ICON_PNG)), "mimeType": "image/png", "sizes": ["512x512"] },
    ])
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

pub fn tool_list() -> Value {
    json!([
        {
            "name": "log_work",
            "title": "Log work to brief",
            "description": "Add an entry to the user's work journal; tomorrow morning it feeds their daily brief. \
Call it proactively, without being asked or asking first, whenever the conversation produces a decision, real \
progress, a new idea, a blocker or open question, or something learned on one of the user's projects. One call per \
distinct topic, made when that topic settles; log again at later milestones rather than repeating yourself.\n\n\
The entry must stand on its own weeks later, read by someone who never saw this chat. Be specific: name files, \
tools, numbers, people, and say why.\n\
Good: 'Chose SQLite over Postgres for the sync service: single user, and avoids running a server.'\n\
Bad: 'Talked about databases.'",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Short project name, spelled exactly like an existing one when it matches (call get_context to see them), e.g. 'chessLLM'. Only invent a new name for genuinely new work." },
                    "summary": { "type": "string", "description": "One or two sentences on what happened and why, about the work rather than the chat ('Fixed X by doing Y', not 'User asked about X')." },
                    "kind": {
                        "type": "string",
                        "enum": KINDS,
                        "description": "progress: something built, fixed or finished. decision: a choice made, with its reason. idea: a possibility not yet acted on. question: an open question still unanswered. blocker: something stopping progress. learning: a fact, technique or resource worth remembering. note: anything else."
                    },
                    "details": { "type": "string", "description": "Optional longer notes for the brief writer: context, options considered and rejected, numbers, links, next steps the user mentioned." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional short topical tags, e.g. ['rust', 'performance']." },
                    "source": { "type": "string", "description": "Optional: which assistant is writing this (e.g. 'ChatGPT'). Defaults to the MCP client's name." }
                },
                "required": ["project", "summary"]
            },
            "annotations": { "title": "Log work to brief", "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false },
            "icons": icons()
        },
        {
            "name": "get_context",
            "title": "Read brief context",
            "description": "Read the user's stated objectives, the projects they already track (with entry counts), and \
recently logged work, newest first. Call it at the start of a conversation about the user's work, before the first \
log_work so you reuse an existing project name, and whenever knowing their goals or recent progress would make your \
answer more relevant (e.g. 'what should I work on next?', or picking up a project where they left off).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Only return entries for this project (case-insensitive)." },
                    "days": { "type": "integer", "description": "How many days back to look (default 14, max 365)." }
                }
            },
            "annotations": { "title": "Read brief context", "readOnlyHint": true, "openWorldHint": false },
            "icons": icons()
        },
        {
            "name": "get_briefs",
            "title": "Read past briefs",
            "description": "Read the daily briefs brief has already written for the user. Without `date`: an \
overview of recent briefs, newest first (headline, summary, item titles). With `date`: that brief in full, every \
item with its body, steps and links, plus the user's thumbs up/down and comments on it. Call it when the user \
mentions their brief or wants to follow up on its advice.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "date": { "type": "string", "description": "YYYY-MM-DD, or 'latest': return that brief in full." },
                    "days": { "type": "integer", "description": "Overview only: how many days back to look (default 7, max 90)." }
                }
            },
            "annotations": { "title": "Read past briefs", "readOnlyHint": true, "openWorldHint": false },
            "icons": icons()
        },
        {
            "name": "update_objectives",
            "title": "Update objectives",
            "description": "Add, reword or remove one of the user's objectives (numbered as in get_context). \
Objectives steer every daily brief, so only call this when the user asks, or after proposing the exact change and \
getting a yes; never on your own. One goal per call. The change is also logged to the journal with your reason, so \
the next brief knows the goal moved and why. Returns the updated list.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["add", "edit", "remove"] },
                    "number": { "type": "integer", "description": "For edit and remove: the objective's number from get_context (1-based)." },
                    "text": { "type": "string", "description": "For add and edit: the goal, one line, optionally followed by note lines (deadline, how success is measured)." },
                    "reason": { "type": "string", "description": "Why the goal changed, in the user's terms, e.g. 'Interview moved to March, so prep is the priority now'." },
                    "source": { "type": "string", "description": "Optional: which assistant is making the change. Defaults to the MCP client's name." }
                },
                "required": ["action", "reason"]
            },
            "annotations": { "title": "Update objectives", "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false },
            "icons": icons()
        },
        {
            "name": "anki_search",
            "title": "Search Anki cards",
            "description": "See what the user is already memorizing in Anki: their decks (with how many cards are due \
today) and the cards matching a search, each with its memory state (new, learning, young, mature = remembered for 3+ \
weeks; how many times forgotten). Call it before teaching or quizzing the user on a topic, to build on what they \
already know and target what they keep forgetting, and before proposing new cards, to avoid duplicates. Opens Anki \
in the background if it isn't running.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to look for, or Anki search syntax: 'attention', 'front:*PPO*', 'tag:brief', 'prop:lapses>=2' (often forgotten), 'is:due', 'added:7' (added in the last 7 days), 'rated:7:1' (answered wrong this week). Empty: the newest cards." },
                    "deck": { "type": "string", "description": "Only this deck, e.g. 'ML'." },
                    "limit": { "type": "integer", "description": "Max cards to return (default 30, max 300)." }
                }
            },
            "annotations": { "title": "Search Anki cards", "readOnlyHint": true, "openWorldHint": false },
            "icons": icons()
        },
        {
            "name": "anki_add_cards",
            "title": "Add Anki cards",
            "description": "Add flashcards to the user's Anki, for facts they have already learned and want to keep \
remembering. Not for journaling: what the user did, decided or struggled with goes to log_work. Each card is one \
atomic fact: a short, specific question with one short answer that makes sense months from now without this chat \
(name the topic: 'In PPO, what does the clip range limit?' not 'What does it limit?'). Split lists and comparisons \
into several cards. Call anki_search first to avoid duplicates, and add cards when the user asks or agrees.\n\n\
Every card goes through a reviewer that compares it with the deck and with cards the user has buried or deleted, and \
may keep, rewrite, split, or turn it down as a duplicate or not card-worthy; this takes up to a minute. The reply \
says what was added. Tell the user, especially about rewrites.\n\
Good: Q 'What does AUROC equal in terms of pairs?' A 'The probability a random positive scores above a random negative.'\n\
Bad: Q 'Evaluation metrics' A 'precision, recall, F1, AUROC and when to use each...'",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "deck": { "type": "string", "description": "Deck to add to. Use an existing deck when one fits (see anki_search); subdecks use '::', e.g. 'Interview::System design'." },
                    "cards": {
                        "type": "array",
                        "description": "1-20 cards.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "front": { "type": "string", "description": "The question. One sentence." },
                                "back": { "type": "string", "description": "The answer: a word, number, formula or one sentence." }
                            },
                            "required": ["front", "back"]
                        }
                    },
                    "context": { "type": "string", "description": "Where the user learned this, e.g. 'Lesson 03: evaluation metrics, for the Guava MLE interview'. Helps the reviewer." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional Anki tags, e.g. ['metrics']. 'brief' is always added." },
                    "source": { "type": "string", "description": "Optional: which assistant is adding these. Defaults to the MCP client's name." }
                },
                "required": ["deck", "cards"]
            },
            "annotations": { "title": "Add Anki cards", "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false },
            "icons": icons()
        },
        {
            "name": "get_todos",
            "title": "Read to-dos",
            "description": "Read the user's to-do list: open to-dos, soonest due first, each with its list, due date, \
deadline, priority (p1 = most urgent), labels and id, plus the names of all their lists. Call it to see what's on \
their plate or due soon, and before update_todo to find a to-do's id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Only to-dos whose text or note contains all these words." },
                    "list": { "type": "string", "description": "Only this list (e.g. 'Inbox', 'Work'), named as in the list of lists." },
                    "limit": { "type": "integer", "description": "Max to-dos to return (default 50, max 300)." }
                }
            },
            "annotations": { "title": "Read to-dos", "readOnlyHint": true, "openWorldHint": true },
            "icons": icons()
        },
        {
            "name": "add_todo",
            "title": "Add a to-do",
            "description": "Add a to-do to the user's to-do list. Only when the user asks, or after proposing the \
exact to-do and getting a yes. Write it as a concrete next action they can tick off ('Email Ana the draft offer', \
not 'Offer'). Returns the to-do as saved, with its id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": { "type": "string", "description": "The to-do, one line." },
                    "description": { "type": "string", "description": "Optional note: context, links." },
                    "list": { "type": "string", "description": "Optional list to put it in, named as in get_todos. Default: the inbox." },
                    "section": { "type": "string", "description": "Optional section within that list." },
                    "due": { "type": "string", "description": "Optional, in plain English: 'today', 'tomorrow 9am', 'next friday', 'every monday'." },
                    "deadline": { "type": "string", "description": "Optional hard deadline, YYYY-MM-DD." },
                    "priority": { "type": "integer", "minimum": 1, "maximum": 4, "description": "Optional: 1 most urgent, 4 normal (default)." },
                    "labels": { "type": "array", "items": { "type": "string" }, "description": "Optional labels." },
                    "parent_id": { "type": "string", "description": "Optional: make it a sub-to-do of this to-do id." }
                },
                "required": ["content"]
            },
            "annotations": { "title": "Add a to-do", "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true },
            "icons": icons()
        },
        {
            "name": "update_todo",
            "title": "Change a to-do",
            "description": "Change one of the user's to-dos: reword it, reschedule it, change its priority or labels, \
move it to another list, or mark it done (or open again). Only when the user asks, or after proposing the exact \
change and getting a yes. Get the id from get_todos. Give only the fields to change. Returns the to-do as saved.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The to-do's id, from get_todos." },
                    "content": { "type": "string", "description": "New text." },
                    "description": { "type": "string", "description": "New note ('' clears it)." },
                    "due": { "type": "string", "description": "New due date in plain English, or 'none' to clear it." },
                    "deadline": { "type": "string", "description": "New deadline YYYY-MM-DD, or 'none' to clear it." },
                    "priority": { "type": "integer", "minimum": 1, "maximum": 4, "description": "1 most urgent, 4 normal." },
                    "labels": { "type": "array", "items": { "type": "string" }, "description": "Replaces all its labels." },
                    "list": { "type": "string", "description": "Move it to this list." },
                    "section": { "type": "string", "description": "Move it to this section (of `list`, or of its current list)." },
                    "status": { "type": "string", "enum": ["done", "open"], "description": "done: tick it off. open: reopen a ticked-off to-do." }
                },
                "required": ["id"]
            },
            "annotations": { "title": "Change a to-do", "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": true },
            "icons": icons()
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
        "get_briefs" => {
            let date = s("date");
            if date.is_empty() {
                let days = args.get("days").and_then(Value::as_i64).unwrap_or(7).clamp(1, 90);
                return Ok(briefs_overview(store, days));
            }
            let date = if date.eq_ignore_ascii_case("latest") {
                store.brief_dates().first().copied().ok_or_else(|| anyhow::anyhow!("no briefs yet"))?
            } else {
                NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|_| anyhow::anyhow!("`date` must be YYYY-MM-DD or 'latest'"))?
            };
            let brief = store.brief(date).ok_or_else(|| anyhow::anyhow!("no brief for {date}"))?;
            Ok(brief_text(store, date, &brief))
        }
        "update_objectives" => {
            let (action, text, reason) = (s("action"), s("text"), s("reason"));
            anyhow::ensure!(!reason.is_empty(), "`reason` is required");
            let mut list = store.objective_list();
            let index = || -> Result<usize> {
                let n = args.get("number").and_then(Value::as_u64).unwrap_or(0) as usize;
                anyhow::ensure!((1..=list.len()).contains(&n), "`number` must be between 1 and {} (see get_context)", list.len());
                Ok(n - 1)
            };
            let summary = match action.as_str() {
                "add" => {
                    anyhow::ensure!(!text.is_empty(), "`text` is required to add");
                    list.push(text.clone());
                    format!("Added objective: {}", first_line(&text))
                }
                "edit" => {
                    anyhow::ensure!(!text.is_empty(), "`text` is required to edit");
                    let i = index()?;
                    let old = std::mem::replace(&mut list[i], text.clone());
                    format!("Changed objective '{}' to '{}'", first_line(&old), first_line(&text))
                }
                "remove" => {
                    let i = index()?;
                    format!("Dropped objective: {}", first_line(&list.remove(i)))
                }
                _ => anyhow::bail!("`action` must be add, edit or remove"),
            };
            store.save_objective_list(&list)?;
            let source = Some(s("source")).filter(|v| !v.is_empty()).or_else(|| session.client_name.clone()).unwrap_or_else(|| "unknown".into());
            store.add_entry(&Entry {
                id: new_id(),
                ts: Local::now(),
                project: "objectives".into(),
                kind: "decision".into(),
                summary: summary.clone(),
                details: reason,
                tags: vec![],
                source,
            })?;
            Ok(format!("{summary}. Objectives now:\n\n{}", numbered_objectives(store)))
        }
        "anki_search" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(30).clamp(1, 300) as usize;
            anki::search_text(&s("query"), Some(s("deck")).filter(|d| !d.is_empty()).as_deref(), limit)
        }
        "anki_add_cards" => {
            let cards: Vec<anki::Qa> = args
                .get("cards")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .take(20)
                .map(|c| anki::Qa {
                    front: c.get("front").and_then(Value::as_str).unwrap_or("").trim().to_owned(),
                    back: c.get("back").and_then(Value::as_str).unwrap_or("").trim().to_owned(),
                })
                .collect();
            let tags = args
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            let source = Some(s("source")).filter(|v| !v.is_empty()).or_else(|| session.client_name.clone()).unwrap_or_else(|| "unknown".into());
            anki::add_cards(store, anki::Batch { deck: s("deck"), cards, context: s("context"), tags, source })
        }
        "get_todos" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50).clamp(1, 300) as usize;
            todos::find(store, &s("query"), &s("list"), limit)
        }
        "add_todo" => todos::add(store, todos::NewTodo {
            content: s("content"),
            description: s("description"),
            list: s("list"),
            section: s("section"),
            due: s("due"),
            deadline: s("deadline"),
            priority: priority(args)?,
            labels: strings(args, "labels").unwrap_or_default(),
            parent_id: s("parent_id"),
        }),
        "update_todo" => {
            let opt = |k: &str| args.get(k).and_then(Value::as_str).map(|v| v.trim().to_owned());
            let status = match opt("status").as_deref() {
                None | Some("") => None,
                Some("done") => Some(todos::Status::Done),
                Some("open") => Some(todos::Status::Open),
                Some(other) => anyhow::bail!("`status` must be done or open, not {other:?}"),
            };
            todos::update(store, todos::Change {
                id: s("id"),
                content: opt("content"),
                description: opt("description"),
                due: opt("due"),
                deadline: opt("deadline"),
                priority: priority(args)?,
                labels: strings(args, "labels"),
                list: opt("list"),
                section: opt("section"),
                status,
            })
        }
        _ => anyhow::bail!("unknown tool: {name}"),
    }
}

fn strings(args: &Value, key: &str) -> Option<Vec<String>> {
    args.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()).collect())
}

/// `priority` as 1-4; chatbots sometimes send "p1" or "2".
fn priority(args: &Value) -> Result<Option<u8>> {
    let Some(v) = args.get("priority").filter(|v| !v.is_null()) else { return Ok(None) };
    let n = v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().trim_start_matches(['p', 'P']).parse().ok()));
    match n {
        Some(n @ 1..=4) => Ok(Some(n as u8)),
        _ => anyhow::bail!("`priority` must be 1 (most urgent) to 4 (normal)"),
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
    out.push_str(&numbered_objectives(store));
    out.push_str("\n## Known projects\n\n");
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

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn numbered_objectives(store: &Store) -> String {
    let list = store.objective_list();
    if list.is_empty() {
        return "(none set yet)\n".into();
    }
    let mut out = String::new();
    for (i, o) in list.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, o.replace('\n', "\n   ")));
    }
    out
}

fn briefs_overview(store: &Store, days: i64) -> String {
    let cutoff = Local::now().date_naive() - Duration::days(days);
    let mut out = format!("## Briefs, last {days} days (call again with `date` for one in full)\n");
    let mut any = false;
    for date in store.brief_dates().into_iter().filter(|d| *d > cutoff) {
        let Some(b) = store.brief(date) else { continue };
        any = true;
        out.push_str(&format!("\n### {date}: {}\n{}\n", b.headline, b.summary.trim()));
        for sec in &b.sections {
            let titles: Vec<_> = sec.items.iter().map(|i| i.title.as_str()).collect();
            out.push_str(&format!("- {}: {}\n", sec.title, titles.join("; ")));
        }
    }
    if !any {
        out.push_str("\n(none)\n");
    }
    out
}

fn brief_text(store: &Store, date: NaiveDate, b: &Brief) -> String {
    let feedback: Vec<_> = store.feedback().into_iter().filter(|f| f.brief_date == date).collect();
    let reaction = |item_id: &str| -> String {
        feedback
            .iter()
            .filter(|f| f.item_id == item_id)
            .map(|f| {
                let r = match f.rating.as_str() { "up" => "👍", "down" => "👎", _ => "" };
                format!("{r} {}", f.comment.trim()).trim().to_owned()
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let mut out = format!("# Brief for {date}: {}\n\n{}\n", b.headline, b.summary.trim());
    let overall = reaction("");
    if !overall.is_empty() {
        out.push_str(&format!("\nUser's reaction to the whole brief: {overall}\n"));
    }
    if let Some(t) = &b.think_first {
        out.push_str(&format!("\nThink first: {}\nAnswer: {}\n", t.question, t.answer));
    }
    for sec in &b.sections {
        out.push_str(&format!("\n## {}\n", sec.title));
        for item in &sec.items {
            let project = if item.project.is_empty() { String::new() } else { format!(" [{}]", item.project) };
            out.push_str(&format!("\n### {}{project}\n{}\n", item.title, item.body.trim()));
            for (label, v) in [("Takeaway", &item.takeaway), ("Objective", &item.objective), ("Why", &item.why), ("About", &item.about)] {
                if !v.is_empty() {
                    out.push_str(&format!("{label}: {v}\n"));
                }
            }
            for step in &item.steps {
                out.push_str(&format!("- {step}\n"));
            }
            for l in &item.links {
                out.push_str(&format!("- {}: {}\n", l.title, l.url));
            }
            let r = reaction(&item.id);
            if !r.is_empty() {
                out.push_str(&format!("User's reaction: {r}\n"));
            }
        }
    }
    if !b.terms.is_empty() {
        out.push_str("\n## Terms\n");
        for t in &b.terms {
            out.push_str(&format!("- {}: {}\n", t.term, t.meaning));
        }
    }
    out
}

/// Newline-delimited JSON-RPC over stdin/stdout, for clients that launch the server themselves
/// (Claude Desktop, Claude Code).
///
/// When `make` replaces the binary, the next message is handed to the new build by exec'ing it
/// in place: same process and pipes, so the client stays connected and is told to refetch the
/// tool list. (Server instructions only reach a client when it connects, so those still need a
/// reconnect.)
pub async fn run_stdio(store: Store) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut session = Session { client_name: take_env(ENV_CLIENT) };
    let mut pending = take_env(ENV_PENDING);
    if pending.is_some() {
        stdout.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n").await?;
        stdout.flush().await?;
    }
    let built = exe_modified();
    loop {
        let line = match pending.take() {
            Some(l) => l,
            None => match lines.next_line().await? {
                Some(l) => l,
                None => break,
            },
        };
        if line.trim().is_empty() {
            continue;
        }
        // Only when nothing else is buffered, so no message is lost across the exec.
        if lines.get_ref().buffer().is_empty() && rebuilt(built) {
            let err = reexec(&session, &line);
            eprintln!("brief: couldn't switch to the new build: {err}");
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

const ENV_CLIENT: &str = "BRIEF_MCP_CLIENT";
const ENV_PENDING: &str = "BRIEF_MCP_PENDING";

fn take_env(key: &str) -> Option<String> {
    let v = std::env::var(key).ok();
    std::env::remove_var(key);
    v
}

fn exe_modified() -> Option<std::time::SystemTime> {
    std::fs::metadata(std::env::current_exe().ok()?).ok()?.modified().ok()
}

/// The binary on disk is newer than this process, and has stopped changing (not mid-build).
fn rebuilt(built: Option<std::time::SystemTime>) -> bool {
    match (built, exe_modified()) {
        (Some(b), Some(now)) => now != b && now.elapsed().is_ok_and(|age| age.as_secs() >= 2),
        _ => false,
    }
}

/// Replaces this process with the new build, which handles `line` first. Returns only on failure.
fn reexec(session: &Session, line: &str) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return std::io::Error::other("no current exe") };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("mcp").env(ENV_PENDING, line);
    if let Some(name) = &session.client_name {
        cmd.env(ENV_CLIENT, name);
    }
    cmd.exec()
}
