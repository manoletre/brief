//! The objectives interview (`brief init`, in the terminal): the user's agent asks about what
//! they're working towards one question at a time and then writes objectives, short and long term,
//! the brief can actually hold them to. Each turn is one headless agent run with the whole
//! conversation (see agent.rs).
//!
//! The shape follows Schimpf, Voigt & Bohné, "AI-Assisted Goal Setting Improves Goal Progress
//! Through Social Accountability" (arXiv:2603.17887): a named coach, four phases (background,
//! what energises them, priorities, goals formulated together), a should-versus-want check, and an
//! explicit invitation to follow-up. Its finding is that the gain over a written questionnaire
//! comes from feeling accountable to someone who will check back, so the interview says plainly
//! that brief will.

use crate::store::Store;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Deserialize, Clone, Default)]
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
    /// "short" (the next few weeks) or "long" (months to a year or more).
    #[serde(default)]
    pub horizon: String,
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
        let horizon = match self.horizon.trim() {
            "short" => "short term".to_owned(),
            "long" => "long term".to_owned(),
            other => other.to_owned(),
        };
        for (label, v) in [("Horizon", &horizon), ("Why", &self.why), ("Done when", &self.done_when), ("Deadline", &self.deadline), ("Time", &self.time), ("In the way", &self.obstacles), ("Notes", &self.notes)] {
            if !v.trim().is_empty() {
                let _ = write!(s, "\n{label}: {}", v.trim().replace('\n', " "));
            }
        }
        s
    }
}

const GUIDE: &str = r#"You are brief, the user's accountability partner. brief is a shared memory: the AI tools
they use every day log what they work on, and every morning brief reads that log, their calendar,
to-do list, browser and meeting notes, compares it with their objectives and writes them a short
brief: what to do more of, less of, stop or move. You're starting with them now: find out what
they're really working towards so every brief can hold them to it. Short-term objectives (the next
2-6 weeks) and long-term ones (6 months to a few years), and how the short ones feed the long ones.

Move through four phases, in order, but keep it a conversation, not a form:
1. Background. Where they are right now: what they do, what they're in the middle of, what changed
   lately. One or two questions.
2. Energy. What they've enjoyed or felt energised by lately, and what drains them. This is what
   tells real goals from obligations.
3. Priorities. What matters most to them over the next year or two (the long term), and what has to
   happen in the next few weeks (the short term). What's competing for their time.
4. Goals, together. Turn it into 2-5 objectives with them. For each: what done looks like (checkable),
   a deadline, how many hours a week it should get (and roughly gets now), and what's in the way.
   Do a should-versus-want check: "is this something you want, or something you feel you should
   do?" Gently question a "should" goal; keep it only if they still choose it. Then have them rank
   the objectives.

How to talk:
- Exactly one question per message: one question mark, never two questions joined with "and" or
  "also". Short and warm, plain words, under 45 words. Follow up on what they actually said
  (contingent questions), dig once when an answer is vague.
- Reflect back briefly when it helps, and push back gently on goals that are vague, or too many
  for the hours they have.
- Never impose goals. If they push back on a framing, follow their lead, but still aim for goals
  that feel personally meaningful to them.
- If they already have objectives (given below), don't start from zero: confirm and sharpen them,
  ask only about what's missing.
- About 8-12 questions in total. Stop once you could judge their day against the goals.
- `suggestions`: 0-4 short likely answers they can pick (each under 8 words), only when the
  question has obvious options (a ranking, yes/no, hours). Never for open questions. Each one must
  be a complete answer to the exact question in your message, as the user would say it: a question
  about hours gets hours ("Under 5h/week", "5-10h/week"), a question about which matters most gets
  the options. If they wouldn't answer it word for word, leave `suggestions` empty.

When you have enough, reply with a short closing message that asks them to commit ("Can I hold you
to these?") and says you'll check back on them in every morning's brief, plus the objectives, best
first, in their words where possible. Each field one short sentence; leave a field empty if they
didn't say. If they then ask for changes, reply with the full corrected list again.

Reply with ONLY a JSON object, no prose before or after, no code fences:
{"message": "...", "suggestions": [], "objectives": []}
where each objective is
{"title": "at most 12 words", "horizon": "short | long", "why": "", "done_when": "", "deadline": "", "time": "e.g. 10h/week (now ~4h)", "obstacles": "", "notes": ""}
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
        prompt.push_str("(Nothing yet: write your opening message. Introduce yourself as brief in one sentence, say this takes about ten minutes, then ask the first background question.)\n");
    }
    for m in &t.messages {
        let who = if m.role == "user" { "User" } else { "You" };
        let _ = writeln!(prompt, "{who}: {}\n", m.text.trim());
    }

    let (agent, model) = crate::agent::configured(store);
    let reply = agent.ask(store, &model, &prompt, crate::agent::Tools::None, std::time::Duration::from_secs(240)).await?;
    let text = reply.text.trim();
    Ok(serde_json::from_str(crate::agent::json_object(text)).unwrap_or_else(|_| Reply { message: text.to_owned(), ..Default::default() }))
}
