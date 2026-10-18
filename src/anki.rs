//! The user's Anki, through the AnkiConnect add-on (code 2055492159) on 127.0.0.1:8765.
//!
//! Journal entries and Anki cards are different things. A journal entry (`log_work`) records what
//! happened in the user's work, for the brief writer: decisions, progress, what they studied and
//! where they struggled. An Anki card is one atomic fact the user already understands and wants to
//! keep remembering; Anki schedules its reviews. Studying a lesson is a journal entry; the five
//! facts from it worth keeping are five cards.
//!
//! Every card a chatbot proposes goes through `review`: a model checks it is atomic and clear
//! (rewriting or splitting it if not), compares it with the deck, and learns the user's taste from
//! the cards they buried, suspended or deleted.
//!
//!   anki_cards.jsonl     every proposal: the drafts, the verdicts, the cards actually added
//!   anki_feedback.jsonl  cards the user buried, suspended or deleted (a bury only lasts until
//!                        the next day in Anki, so brief writes it down when it sees one)
//!   anki_pending.jsonl   proposals waiting for Anki or the reviewer; `brief serve` retries them

use crate::store::{append_jsonl, new_id, read_jsonl, Store};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const ADDR: &str = "127.0.0.1:8765";
const REVIEW_TIMEOUT: Duration = Duration::from_secs(120);
/// An interval this long means the card is remembered (Anki calls these "mature").
const MATURE_DAYS: i64 = 21;

// ── AnkiConnect ──

fn connect() -> std::io::Result<TcpStream> {
    TcpStream::connect_timeout(&ADDR.parse::<SocketAddr>().unwrap(), Duration::from_millis(500))
}

/// Anki is open with AnkiConnect listening. With `launch`, opens Anki in the background first
/// if needed and waits for it.
pub fn available(launch: bool) -> bool {
    if connect().is_ok() {
        return true;
    }
    if !launch || std::process::Command::new("open").args(["-g", "-a", "Anki"]).output().map_or(true, |o| !o.status.success()) {
        return false;
    }
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(25) {
        std::thread::sleep(Duration::from_millis(500));
        if call("version", json!({})).is_ok() {
            return true;
        }
    }
    false
}

pub fn call(action: &str, params: Value) -> Result<Value> {
    let body = json!({ "action": action, "version": 6, "params": params }).to_string();
    let mut s = connect().context("Anki isn't open (or the AnkiConnect add-on, code 2055492159, isn't installed)")?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    write!(
        s,
        "POST / HTTP/1.1\r\nHost: {ADDR}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;

    // Read the headers, then exactly Content-Length bytes (don't rely on the server closing).
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            bail!("AnkiConnect closed the connection");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
    let len = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok());
    match len {
        Some(len) => {
            while buf.len() < header_end + len {
                let n = s.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
        }
        None => {
            s.read_to_end(&mut buf)?;
        }
    }
    let v: Value = serde_json::from_slice(&buf[header_end..]).context("unexpected reply from AnkiConnect")?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        bail!("AnkiConnect: {e}");
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

// ── Reading ──

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Card {
    pub note_id: i64,
    pub card_id: i64,
    pub deck: String,
    pub front: String,
    pub back: String,
    /// new, learning, young, mature, suspended or buried.
    pub status: String,
    pub interval_days: i64,
    pub reps: i64,
    pub lapses: i64,
}

impl Card {
    fn from_info(v: &Value) -> Card {
        let mut fields: Vec<(i64, String)> = v["fields"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, f)| (f["order"].as_i64().unwrap_or(0), plain(f["value"].as_str().unwrap_or(""))))
            .collect();
        fields.sort_by_key(|f| f.0);
        let mut fields = fields.into_iter().map(|f| f.1);
        let interval = v["interval"].as_i64().unwrap_or(0);
        let status = match v["queue"].as_i64().unwrap_or(0) {
            -3 | -2 => "buried",
            -1 => "suspended",
            0 => "new",
            1 | 3 | 4 => "learning",
            _ if interval >= MATURE_DAYS => "mature",
            _ => "young",
        };
        Card {
            note_id: v["note"].as_i64().unwrap_or(0),
            card_id: v["cardId"].as_i64().unwrap_or(0),
            deck: v["deckName"].as_str().unwrap_or("").to_owned(),
            front: fields.next().unwrap_or_default(),
            back: fields.next().unwrap_or_default(),
            status: status.into(),
            interval_days: interval.max(0),
            reps: v["reps"].as_i64().unwrap_or(0),
            lapses: v["lapses"].as_i64().unwrap_or(0),
        }
    }

    pub fn line(&self) -> String {
        let memory = match self.status.as_str() {
            "young" | "mature" => format!("{} {}d", self.status, self.interval_days),
            s => s.to_owned(),
        };
        format!(
            "- [{} · {memory} · reviewed {}×, forgotten {}×] Q: {} → A: {}",
            self.deck,
            self.reps,
            self.lapses,
            one_line(&self.front),
            one_line(&self.back)
        )
    }
}

/// Cards matching an Anki search (https://docs.ankiweb.net/searching.html), newest first,
/// one per note.
pub fn find(query: &str, limit: usize) -> Result<Vec<Card>> {
    let mut ids: Vec<i64> = serde_json::from_value(call("findCards", json!({ "query": query }))?)?;
    ids.sort_unstable_by(|a, b| b.cmp(a));
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for chunk in ids.chunks(200) {
        let infos = call("cardsInfo", json!({ "cards": chunk }))?;
        for info in infos.as_array().into_iter().flatten() {
            let card = Card::from_info(info);
            if seen.insert(card.note_id) {
                out.push(card);
            }
            if out.len() >= limit {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

pub struct Deck {
    pub name: String,
    pub new: i64,
    pub learning: i64,
    pub due: i64,
    pub total: i64,
}

pub fn decks() -> Result<Vec<Deck>> {
    let names: Vec<String> = serde_json::from_value(call("deckNames", json!({}))?)?;
    // One deck at a time: Anki refuses the whole request if any deck has no counts (e.g. the
    // hidden, empty Default deck).
    let mut out: Vec<Deck> = names
        .iter()
        .filter_map(|n| call("getDeckStats", json!({ "decks": [n] })).ok())
        .flat_map(|stats| stats.as_object().cloned().unwrap_or_default().into_values())
        .map(|s| Deck {
            name: s["name"].as_str().unwrap_or("").to_owned(),
            new: s["new_count"].as_i64().unwrap_or(0),
            learning: s["learn_count"].as_i64().unwrap_or(0),
            due: s["review_count"].as_i64().unwrap_or(0),
            total: s["total_in_deck"].as_i64().unwrap_or(0),
        })
        .filter(|d| !(d.name == "Default" && d.total == 0))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// What the `anki_search` tool returns: the decks, then the matching cards.
pub fn search_text(query: &str, deck: Option<&str>, limit: usize) -> Result<String> {
    anyhow::ensure!(available(true), "Anki isn't reachable. Open Anki (with the AnkiConnect add-on installed) and try again.");
    let mut out = String::from("## Anki decks (due today = reviews waiting; new = not studied yet)\n\n");
    for d in decks()? {
        let _ = writeln!(out, "- {}: {} cards, {} due today, {} learning, {} new", d.name, d.total, d.due, d.learning, d.new);
    }
    let mut q = query.trim().to_owned();
    if let Some(deck) = deck {
        q = format!("\"deck:{}\" {q}", deck.replace('"', ""));
    }
    let q = if q.trim().is_empty() { "deck:*".to_owned() } else { q };
    let cards = find(&q, limit)?;
    let _ = writeln!(
        out,
        "\n## Cards matching `{q}` ({} shown, newest first)\n\nmature = remembered for {MATURE_DAYS}+ days; young = still being learned; \"forgotten n×\" = lapses.\n",
        cards.len()
    );
    if cards.is_empty() {
        out.push_str("(none)\n");
    }
    for c in &cards {
        out.push_str(&c.line());
        out.push('\n');
    }
    Ok(out)
}

// ── The user's feedback: buried, suspended and deleted cards ──

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Signal {
    pub ts: DateTime<Local>,
    pub note_id: i64,
    /// buried, suspended or deleted.
    pub signal: String,
    #[serde(default)]
    pub deck: String,
    pub front: String,
    #[serde(default)]
    pub back: String,
}

/// Writes down cards the user has buried or suspended, and cards brief added that they deleted,
/// so the reviewer can learn from them later. Returns how many are new.
pub fn record_signals(store: &Store) -> Result<usize> {
    let path = store.root.join("anki_feedback.jsonl");
    let known: HashSet<(i64, String)> = read_jsonl::<Signal>(&path).into_iter().map(|s| (s.note_id, s.signal)).collect();
    let mut new = Vec::new();
    for c in find("is:buried OR is:suspended", 500)? {
        if !known.contains(&(c.note_id, c.status.clone())) {
            new.push(Signal { ts: Local::now(), note_id: c.note_id, signal: c.status, deck: c.deck, front: c.front, back: c.back });
        }
    }
    let added: Vec<(i64, String, Qa)> = proposals(store)
        .into_iter()
        .flat_map(|p| p.added.into_iter().map(move |a| (a.note_id, p.deck.clone(), a.card)))
        .filter(|(id, ..)| *id != 0 && !known.contains(&(*id, "deleted".to_owned())))
        .collect();
    if !added.is_empty() {
        let ids: Vec<i64> = added.iter().map(|a| a.0).collect();
        let infos = call("notesInfo", json!({ "notes": ids }))?;
        for ((id, deck, card), info) in added.into_iter().zip(infos.as_array().cloned().unwrap_or_default()) {
            if info.get("noteId").is_none() {
                new.push(Signal { ts: Local::now(), note_id: id, signal: "deleted".into(), deck, front: card.front, back: card.back });
            }
        }
    }
    for s in &new {
        append_jsonl(&path, s)?;
    }
    Ok(new.len())
}

pub fn signals(store: &Store) -> Vec<Signal> {
    read_jsonl(&store.root.join("anki_feedback.jsonl"))
}

// ── Adding cards, through the reviewer ──

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Qa {
    pub front: String,
    pub back: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Batch {
    pub deck: String,
    pub cards: Vec<Qa>,
    /// Where the user learned this (lesson, paper, interview prep topic).
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub source: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Added {
    pub note_id: i64,
    #[serde(flatten)]
    pub card: Qa,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Proposal {
    pub id: String,
    pub ts: DateTime<Local>,
    pub source: String,
    pub deck: String,
    #[serde(default)]
    pub context: String,
    pub draft: Qa,
    /// keep, rewrite, split, duplicate or reject.
    pub verdict: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub added: Vec<Added>,
}

pub fn proposals(store: &Store) -> Vec<Proposal> {
    read_jsonl(&store.root.join("anki_cards.jsonl"))
}

#[derive(Deserialize, Debug)]
struct Verdict {
    #[serde(default)]
    draft: usize,
    verdict: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    cards: Vec<Qa>,
}

/// Reviews the drafts and adds what passes to Anki. If Anki or the reviewer is unavailable, the
/// batch waits in anki_pending.jsonl and `brief serve` finishes it later. Returns a report for
/// the chatbot.
pub fn add_cards(store: &Store, batch: Batch) -> Result<String> {
    anyhow::ensure!(!batch.deck.trim().is_empty(), "`deck` is required");
    anyhow::ensure!(!batch.cards.is_empty(), "`cards` is empty");
    anyhow::ensure!(
        batch.cards.iter().all(|c| !c.front.trim().is_empty() && !c.back.trim().is_empty()),
        "every card needs a `front` and a `back`"
    );
    if !available(true) {
        queue(store, &batch)?;
        return Ok(format!(
            "Anki isn't open, so the {} card(s) were saved; brief will review them and add them to '{}' next time Anki is running.",
            batch.cards.len(),
            batch.deck
        ));
    }
    match review_and_add(store, &batch) {
        Ok(report) => Ok(report),
        Err(e) => {
            queue(store, &batch)?;
            Ok(format!("Couldn't review the cards right now ({e:#}). Saved them; brief will review and add them later."))
        }
    }
}

fn review_and_add(store: &Store, batch: &Batch) -> Result<String> {
    let _ = record_signals(store);
    let existing = find(&format!("\"deck:{}\"", batch.deck.replace('"', "")), 400).unwrap_or_default();
    let kept = find(&format!("prop:ivl>={MATURE_DAYS} prop:lapses=0 -is:suspended -is:buried"), 12).unwrap_or_default();
    let verdicts = review(store, batch, &existing, &kept)?;

    let mut tags: Vec<String> = vec!["brief".into()];
    tags.extend(batch.tags.iter().map(|t| t.split_whitespace().collect::<Vec<_>>().join("_")).filter(|t| !t.is_empty()));
    call("createDeck", json!({ "deck": batch.deck }))?;

    let mut report = String::new();
    let mut total = 0;
    for (i, draft) in batch.cards.iter().enumerate() {
        let v = verdicts.iter().find(|v| v.draft == i + 1);
        let (verdict, reason, cards) = match v {
            Some(v) => (v.verdict.as_str(), v.reason.as_str(), v.cards.clone()),
            None => ("reject", "the reviewer didn't return a verdict for this card", vec![]),
        };
        let cards = if matches!(verdict, "duplicate" | "reject") { vec![] } else { cards };
        let mut added = Vec::new();
        let mut failed = Vec::new();
        for card in cards.into_iter().filter(|c| !c.front.trim().is_empty() && !c.back.trim().is_empty()) {
            let mut card_tags = tags.clone();
            if verdict != "keep" {
                card_tags.push(format!("brief::{verdict}"));
            }
            let note = json!({
                "deckName": batch.deck,
                "modelName": "Basic",
                "fields": { "Front": to_html(&card.front), "Back": to_html(&card.back) },
                "tags": card_tags,
                "options": { "allowDuplicate": false, "duplicateScope": "deck" },
            });
            match call("addNote", json!({ "note": note })) {
                Ok(id) => added.push(Added { note_id: id.as_i64().unwrap_or(0), card }),
                Err(e) => failed.push(format!("{}: {e}", one_line(&card.front))),
            }
        }
        total += added.len();

        let _ = writeln!(report, "{}. \"{}\" → {verdict}: {reason}", i + 1, one_line(&draft.front));
        for a in &added {
            let _ = writeln!(report, "   added  Q: {}  A: {}", one_line(&a.card.front), one_line(&a.card.back));
        }
        for f in &failed {
            let _ = writeln!(report, "   not added ({f})");
        }
        let p = Proposal {
            id: new_id(),
            ts: Local::now(),
            source: batch.source.clone(),
            deck: batch.deck.clone(),
            context: batch.context.clone(),
            draft: draft.clone(),
            verdict: verdict.into(),
            reason: reason.into(),
            added,
        };
        append_jsonl(&store.root.join("anki_cards.jsonl"), &p)?;
    }
    Ok(format!(
        "Added {total} card(s) to Anki deck '{}' (tagged `brief`). Reviewer verdicts:\n{report}\nTell the user what was added, especially any rewrites.",
        batch.deck
    ))
}

fn queue(store: &Store, batch: &Batch) -> Result<()> {
    append_jsonl(&pending_path(store), batch)
}

fn pending_path(store: &Store) -> PathBuf {
    store.root.join("anki_pending.jsonl")
}

pub fn has_pending(store: &Store) -> bool {
    fs::metadata(pending_path(store)).is_ok_and(|m| m.len() > 0)
}

/// Retries waiting batches. Only call when Anki is reachable; batches that fail again go back
/// in the queue.
pub fn flush_pending(store: &Store) -> Result<usize> {
    let path = pending_path(store);
    let batches: Vec<Batch> = read_jsonl(&path);
    fs::write(&path, "")?;
    let mut done = 0;
    for b in batches {
        match review_and_add(store, &b) {
            Ok(_) => done += 1,
            Err(e) => {
                eprintln!("brief: pending Anki cards still can't be added: {e:#}");
                queue(store, &b)?;
            }
        }
    }
    Ok(done)
}

// ── The reviewer ──

const REVIEW_RULES: &str = "You review flashcards before they go into the user's Anki deck. The user uses Anki \
only to keep remembering things they have ALREADY learned (in a lesson, a paper, a mock interview); right now they \
are preparing for job interviews in machine learning, system design and coding. A card must let them check, in a \
few seconds, months from now, whether they still remember one thing.

# What makes a good card

- Atomic: one fact, one question, one answer. An answer that is a list, a comparison or several facts becomes \
several cards (\"split\").
- Clear on its own: the question has exactly one right answer and needs no other context. Name the topic in the \
question (\"In PPO, ...\", \"In a URL shortener, ...\"); never \"this\", \"it\", \"the above\".
- Short: the answer is a word, a number, a formula or one sentence (aim for under 15 words). The question is one \
sentence.
- Recall, not recognition: no yes/no questions, and the question must not give away the answer.
- Why and how questions are fine when the answer is one idea. Comparisons work best as \"What does X do that Y \
doesn't?\". Formulas: ask for the formula, or for one part of it and what it means.
- Keep the user's meaning and words. Don't add facts the draft doesn't contain beyond a few clarifying words. If \
the draft looks factually wrong, reject it and say what is wrong.
- Not card material: the user's own projects, plans, decisions or events (\"I chose SQLite\"), opinions, links, \
and anything with no stable answer. Those belong in their journal. Reject them.
- A card that asks the same thing as a card already in the deck is a duplicate, even with different wording. \
If the draft adds something new to an existing card, keep only the new part.

# Verdicts, per draft

- keep: already good; return it unchanged.
- rewrite: one improved card.
- split: two or more atomic cards that together cover the draft.
- duplicate: the deck already has it; no cards.
- reject: not worth a card, or wrong; no cards.
";

fn review(store: &Store, batch: &Batch, existing: &[Card], kept: &[Card]) -> Result<Vec<Verdict>> {
    let mut p = String::from(REVIEW_RULES);

    p.push_str("\n# The user's taste\n\nCards they buried, suspended or deleted. Bury means \"I didn't want to see this one\", so treat these as cards they found bad, work out why (too long? vague? trivial? not something they care about?) and don't let the drafts repeat the mistake:\n\n");
    let sigs = signals(store);
    if sigs.is_empty() {
        p.push_str("(none yet)\n");
    }
    for s in sigs.iter().rev().take(40) {
        let _ = writeln!(p, "- {} [{}] Q: {} → A: {}", s.signal, s.deck, one_line(&s.front), one_line(&s.back));
    }
    p.push_str("\nCards they have kept and remembered for weeks (their idea of a card that works):\n\n");
    if kept.is_empty() {
        p.push_str("(none yet)\n");
    }
    for c in kept {
        let _ = writeln!(p, "- Q: {} → A: {}", one_line(&c.front), one_line(&c.back));
    }

    let _ = writeln!(p, "\n# Cards already in the deck \"{}\" ({})\n", batch.deck, existing.len());
    if existing.is_empty() {
        p.push_str("(the deck is empty or new)\n");
    }
    for c in existing {
        let _ = writeln!(p, "- Q: {} → A: {}", one_line(&c.front), short(&one_line(&c.back), 160));
    }

    p.push_str("\n# Drafts to review\n\n");
    if !batch.context.is_empty() {
        let _ = writeln!(p, "Where the user learned this: {}\n", batch.context);
    }
    for (i, c) in batch.cards.iter().enumerate() {
        let _ = writeln!(p, "{}. Front: {}\n   Back: {}", i + 1, c.front.trim(), c.back.trim().replace('\n', "\n         "));
    }
    p.push_str(
        r#"
# Output

Reply with ONLY a JSON object, no prose before or after:

{"results": [{"draft": 1, "verdict": "keep | rewrite | split | duplicate | reject", "reason": "one short sentence", "cards": [{"front": "...", "back": "..."}]}]}

One result per draft, numbered like the drafts. "cards" holds the final cards to add: the draft unchanged for keep, the improved card for rewrite, 2 or more for split, and [] for duplicate or reject. Plain text, no HTML or markdown; use a line break only inside a formula or code.
"#,
    );

    let text = run_claude(store, &p)?;
    #[derive(Deserialize)]
    struct Out {
        results: Vec<Verdict>,
    }
    let json = match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if b > a => &text[a..=b],
        _ => bail!("the reviewer didn't reply with JSON"),
    };
    let out: Out = serde_json::from_str(json).context("couldn't read the reviewer's reply")?;
    Ok(out.results)
}

/// `claude -p` with no tools, in brief's empty work dir. Blocking, with a timeout.
fn run_claude(store: &Store, prompt: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let claude = crate::generate::find_claude().context("could not find the `claude` CLI")?;
    let mut child = Command::new(claude)
        .args(["-p", "--output-format", "json", "--model", &store.config().anki_model, "--tools", ""])
        .current_dir(store.root.join("work"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start claude")?;
    child.stdin.take().unwrap().write_all(prompt.as_bytes())?;
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > REVIEW_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the card reviewer timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        bail!("claude exited with {status}: {}", err.trim());
    }
    let envelope: Value = serde_json::from_str(&out).context("unexpected output from claude")?;
    if envelope.get("is_error").and_then(Value::as_bool) == Some(true) {
        bail!("claude reported an error: {}", envelope.get("result").unwrap_or(&Value::Null));
    }
    Ok(envelope.get("result").and_then(Value::as_str).unwrap_or_default().to_owned())
}

// ── For the daily brief ──

/// The Anki part of the brief prompt. Empty if Anki can't be reached.
pub fn brief_context(store: &Store, since: DateTime<Local>) -> String {
    if !available(true) {
        return String::new();
    }
    let _ = record_signals(store);
    let Ok(decks) = decks() else { return String::new() };
    let mut p = String::from("Decks (due today = reviews waiting now; new = never studied):\n");
    let (mut due, mut new) = (0, 0);
    for d in &decks {
        let _ = writeln!(p, "- {}: {} cards, {} due today, {} learning, {} new", d.name, d.total, d.due, d.learning, d.new);
        due += d.due + d.learning;
        new += d.new;
    }
    let _ = writeln!(p, "Total: {due} reviews due, {new} new cards waiting.");

    let struggling = find("rated:7:1", 10).unwrap_or_default();
    if !struggling.is_empty() {
        p.push_str("\nCards they answered wrong in the last 7 days:\n");
        for c in &struggling {
            let _ = writeln!(p, "{}", c.line());
        }
    }
    let leeches = find("prop:lapses>=3 -is:suspended", 10).unwrap_or_default();
    if !leeches.is_empty() {
        p.push_str("\nCards they keep forgetting (3+ lapses; maybe rewrite or relearn the idea):\n");
        for c in &leeches {
            let _ = writeln!(p, "{}", c.line());
        }
    }
    let fresh: Vec<Proposal> = proposals(store).into_iter().filter(|p| p.ts > since).collect();
    if !fresh.is_empty() {
        let added: usize = fresh.iter().map(|p| p.added.len()).sum();
        let changed = fresh.iter().filter(|p| p.verdict != "keep").count();
        let _ = writeln!(p, "\nSince the last brief, chatbots proposed {} cards; {added} were added ({changed} rewritten, split or turned down by the reviewer).", fresh.len());
    }
    let sigs: Vec<Signal> = signals(store).into_iter().filter(|s| s.ts > since).collect();
    if !sigs.is_empty() {
        p.push_str("\nCards the user buried, suspended or deleted since the last brief:\n");
        for s in &sigs {
            let _ = writeln!(p, "- {} [{}] {}", s.signal, s.deck, one_line(&s.front));
        }
    }
    if has_pending(store) {
        p.push_str("\nSome proposed cards are still waiting to be reviewed and added.\n");
    }
    p
}

// ── Helpers ──

/// Anki field HTML to plain text.
pub fn plain(html: &str) -> String {
    let s = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("</div>", "\n")
        .replace("</li>", "\n")
        .replace("<img", "[image]<img");
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    out.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n").trim().to_owned()
}

fn to_html(text: &str) -> String {
    crate::html::esc(text.trim()).replace('\n', "<br>")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn short(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}
