//! The objectives interview (/onboarding): instead of a blank list, Claude asks about what you're
//! working towards one question at a time (why it matters, what done looks like, by when, how
//! much time it gets, what's in the way, how the goals rank) and then writes objectives the brief
//! can actually prioritise against. Each turn is one `claude -p` call with the whole conversation.

use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::Write as _;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Deserialize)]
pub struct Transcript {
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Message {
    /// "assistant" or "user".
    pub role: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Reply {
    pub message: String,
    /// Set once the interview has enough: the proposed objectives, best first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objectives: Vec<Objective>,
    /// Up to 4 short answers the user can tap instead of typing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Objective {
    pub title: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub done_when: String,
    #[serde(default)]
    pub deadline: String,
    #[serde(default)]
    pub time: String,
    #[serde(default)]
    pub obstacles: String,
    #[serde(default)]
    pub notes: String,
}

impl Objective {
    /// As stored in objectives.md: the goal, then one indented line per detail.
    pub fn to_item(&self, rank: usize) -> String {
        let mut s = format!("{}\nPriority: {rank}", self.title.trim());
        for (label, v) in [("Why", &self.why), ("Done when", &self.done_when), ("Deadline", &self.deadline), ("Time", &self.time), ("In the way", &self.obstacles), ("Notes", &self.notes)] {
            if !v.trim().is_empty() {
                let _ = write!(s, "\n{label}: {}", v.trim().replace('\n', " "));
            }
        }
        s
    }
}

const GUIDE: &str = r#"You are onboarding the user to brief, a personal assistant that writes them a daily brief. Every
morning the brief compares where their time actually went (calendar, browser, what they logged from
chatbots, to-do list) against their objectives, and helps them reprioritise. So the objectives must
be specific enough to judge a day against. Interview them to get there.

How to interview:
- One question per message. Short and warm, plain words, no lists of questions. Under 45 words.
- Start broad: everything they're working towards right now (work, job search, learning, side
  projects, health, people). Then go objective by objective.
- For each objective find out: why it matters to them (the real reason, dig once if the answer is
  vague); what "done" or success looks like, ideally something you could check; a deadline or
  time frame; how many hours a week they want it to get, and roughly how much it really gets now;
  what's in the way.
- Then ask them to rank the objectives against each other, and whether anything they spend time
  on serves none of them (and should get less).
- Reflect back briefly when it helps ("So the Liquid application is the real goal and the chess
  project is a way to show post-training skill?"), and push back gently on goals that are vague
  or too many for the hours they have.
- If they already have objectives (given below), don't start from zero: confirm and sharpen them,
  ask only about what's missing.
- About 8-14 questions in total. Don't drag it out; stop once you could judge their day.
- `suggestions`: 0-4 short likely answers they can tap (each under 8 words), only when a question
  has obvious options (e.g. a ranking, yes/no, hours). Never for open questions.

When you have enough, reply with a short closing message and the objectives, best first, in
their words where possible. Each field one short sentence; leave a field empty if they didn't say.
If they then ask for changes, reply with the full corrected list again.

Reply with ONLY a JSON object, no prose before or after:
{"message": "...", "suggestions": [], "objectives": []}
where each objective is
{"title": "at most 12 words", "why": "", "done_when": "", "deadline": "", "time": "e.g. 10h/week (now ~4h)", "obstacles": "", "notes": ""}
and "objectives" stays empty until you're proposing them."#;

pub async fn turn(store: &Store, t: &Transcript) -> Result<Reply> {
    let mut prompt = String::from(GUIDE);
    prompt.push_str("\n\n# Their current objectives\n\n");
    let existing = store.objectives();
    prompt.push_str(if existing.trim().is_empty() { "(none yet)" } else { existing.trim() });
    let recent: Vec<_> = store.entries().into_iter().rev().take(25).collect();
    if !recent.is_empty() {
        prompt.push_str("\n\n# What they've logged lately (for context; don't recite it)\n\n");
        for e in recent.iter().rev() {
            let _ = writeln!(prompt, "- {} [{}] {}", e.ts.format("%b %-d"), e.project, e.summary);
        }
    }
    prompt.push_str("\n\n# The conversation so far\n\n");
    if t.messages.is_empty() {
        prompt.push_str("(Nothing yet: write your opening message. Say in one sentence what this is for, then ask the first question.)\n");
    }
    for m in &t.messages {
        let who = if m.role == "user" { "User" } else { "You" };
        let _ = writeln!(prompt, "{who}: {}\n", m.text.trim());
    }

    let claude = crate::generate::find_claude().context("could not find the `claude` CLI; install Claude Code or put it on PATH")?;
    let mut child = Command::new(claude)
        .args(["-p", "--output-format", "json", "--model", "sonnet", "--tools", ""])
        .current_dir(store.root.join("work"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to start claude")?;
    child.stdin.take().unwrap().write_all(prompt.as_bytes()).await?;
    let out = tokio::time::timeout(std::time::Duration::from_secs(180), child.wait_with_output()).await.context("claude timed out")??;
    if !out.status.success() {
        bail!("claude exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    }
    let envelope: Value = serde_json::from_slice(&out.stdout).context("unexpected output from claude")?;
    let text = envelope.get("result").and_then(Value::as_str).unwrap_or_default();
    let json = match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if b > a => &text[a..=b],
        _ => "",
    };
    Ok(serde_json::from_str(json).unwrap_or_else(|_| Reply { message: text.trim().to_owned(), ..Default::default() }))
}
