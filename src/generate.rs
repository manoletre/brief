//! Builds the daily brief by running the local Claude Code CLI (`claude -p`),
//! so it uses your existing Claude subscription. No API key involved.

use crate::{anki, browsing, calendar, granola, todoist};
use crate::store::{Brief, Store};
use anyhow::{bail, Context, Result};
use chrono::{Duration, Local};
use serde_json::Value;
use std::fmt::Write as _;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const TIMEOUT_MINUTES: u64 = 20;

pub async fn generate(store: &Store) -> Result<Brief> {
    let cfg = store.config();
    let since = last_brief_time(store);
    let anki = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || anki::brief_context(&store, since))
    };
    let todo = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || todoist::brief_context(&store, since))
    };
    let meetings = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || granola::brief_context(&store, since))
    };
    let cal = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || calendar::brief_context(&store, since))
    };
    let (anki, todo, meetings, cal) = (anki.await.unwrap_or_default(), todo.await.unwrap_or_default(), meetings.await.unwrap_or_default(), cal.await.unwrap_or_default());
    let web = browsing::brief_context(store, since);
    let prompt = build_prompt(store, &Sources { anki: &anki, todo: &todo, meetings: &meetings, calendar: &cal, browsing: &web });
    let claude = find_claude().context("could not find the `claude` CLI; install Claude Code or put it on PATH")?;

    let mut child = Command::new(claude)
        .args(["-p", "--output-format", "json", "--model", &cfg.model])
        .args(["--allowedTools", "WebSearch,WebFetch"])
        .args(["--disallowedTools", "Bash,Edit,Write,NotebookEdit"])
        // Empty working dir so no project CLAUDE.md leaks into the brief.
        .current_dir(store.root.join("work"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to start claude")?;
    child.stdin.take().unwrap().write_all(prompt.as_bytes()).await?;

    let out = tokio::time::timeout(std::time::Duration::from_secs(TIMEOUT_MINUTES * 60), child.wait_with_output())
        .await
        .context("claude timed out")??;
    if !out.status.success() {
        bail!("claude exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    }

    let envelope: Value = serde_json::from_slice(&out.stdout).context("unexpected output from claude")?;
    if envelope.get("is_error").and_then(Value::as_bool) == Some(true) {
        bail!("claude reported an error: {}", envelope.get("result").unwrap_or(&Value::Null));
    }
    let text = envelope.get("result").and_then(Value::as_str).unwrap_or_default();

    let mut brief = parse_brief(text);
    brief.date = Some(Local::now().date_naive());
    brief.generated_at = Some(Local::now());
    brief.cost_usd = envelope.get("total_cost_usd").and_then(Value::as_f64);
    for (si, section) in brief.sections.iter_mut().enumerate() {
        for (ii, item) in section.items.iter_mut().enumerate() {
            item.id = format!("s{si}i{ii}");
        }
    }
    store.save_brief(&brief)?;
    Ok(brief)
}

/// Take the JSON object out of the model's reply; fall back to showing the raw text.
fn parse_brief(text: &str) -> Brief {
    let json = match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if b > a => &text[a..=b],
        _ => "",
    };
    serde_json::from_str::<Brief>(json).unwrap_or_else(|_| Brief {
        headline: "Daily brief (unstructured)".into(),
        summary: text.to_owned(),
        ..Default::default()
    })
}

pub fn find_claude() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let mut candidates: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("claude")).collect())
        .unwrap_or_default();
    // launchd starts us with a bare PATH, so also check the usual install spots.
    candidates.extend([
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
        "/opt/homebrew/bin/claude".into(),
        "/usr/local/bin/claude".into(),
    ]);
    candidates.into_iter().find(|p| p.is_file())
}

/// When the last brief was written (or a week ago): "new" means after this.
fn last_brief_time(store: &Store) -> chrono::DateTime<Local> {
    store
        .brief_dates()
        .first()
        .and_then(|d| store.brief(*d))
        .and_then(|b| b.generated_at)
        .unwrap_or(Local::now() - Duration::days(7))
}

/// What the connected apps say, each already formatted for the prompt ("" when not connected).
struct Sources<'a> {
    anki: &'a str,
    todo: &'a str,
    meetings: &'a str,
    calendar: &'a str,
    browsing: &'a str,
}

fn build_prompt(store: &Store, src: &Sources) -> String {
    let Sources { anki, todo, meetings, .. } = *src;
    let now = Local::now();
    let entries = store.entries();
    let since = last_brief_time(store);
    let history_cutoff = now - Duration::days(45);

    let mut p = String::new();
    let _ = writeln!(p, "You are writing the user's personal daily brief for {}.", now.format("%A, %B %-d, %Y"));
    p.push_str(
        "The brief exists to help the user REPRIORITISE. You see their objectives (what they want, ranked, with \
why, deadlines and time budgets when they gave them) and evidence of what they actually do: what they logged from \
their AI chatbots, their calendar, the pages they spent time on in their browser, their to-do list and meeting notes. \
Compare the two honestly, like a sharp, candid mentor, and tell them what to change: what to do more of, less of, \
stop, start or move, starting today. Not a summary of their week.\n\n\
Look for:\n\
- Drift: time (calendar, browsing, journal) going to things that serve no objective, or to a low-ranked one, \
while a higher-ranked objective or a near deadline gets little. Use real numbers from the data.\n\
- Objectives with no time, no tasks and no calendar slot behind them.\n\
- Today's calendar and free blocks versus the to-do list: what fits, what doesn't, what to move.\n\
- Feedback on the actual work: decisions, approach, quality, tied to an objective.\n\
- Connections between projects, or to ideas they may not know; blind spots and risks.\n\
- 1-3 things to read or watch that serve the top objectives right now. Use web search to find real, current \
resources and check each URL exists; never invent links. Prefer ones that build on what they've been reading.\n\
- 2-4 concrete next steps for today, placed in their free time when the calendar is known.\n\n\
# How to write\n\n\
- The user reads one item at a time and rates each, so every item must stand on its own.\n\
- Every item has a `takeaway`: the single thing to remember or do, one sentence of at most 20 words. Someone \
who reads only the takeaways gets the whole brief.\n\
- Every item has a `why`: why it's in today's brief, pointing at the evidence (\"Your calendar shows 11h of \
meetings since Monday and none on DRC_explorer\", \"You read three posts on GRPO yesterday\", \"From your \
objective: Liquid application by Nov 15\"). One or two sentences. Never vague (\"this could be useful\").\n\
- Every item has an `objective`: the objective it serves, quoted short as the user wrote it, or \"none\" if \
it's about something that serves no objective.\n\
- At most 9 items in the whole brief. Each item has 2-3 steps, and every step is one sentence of at most 16 \
words. Fewer, sharper points beat full coverage. Cut whatever doesn't help them reprioritise or do better work.\n\
- Things to read or watch: exactly one link per item, so each can be rated on its own, and an `about`: a \
paragraph (3-5 sentences, plain words) on what it covers and what they'd get out of it. `why` says why it fits \
them now. Steps can be what to look for while reading.\n\
- Plain words. Write like you're talking to a smart friend from outside the field. Short sentences, everyday \
words, no buzzwords or management-speak (no \"leverage\", \"surface\", \"unblock\", \"bandwidth\", \
\"north star\", \"double down\"). Spell out acronyms. If a technical word is truly needed, use it and add it \
to `terms` with a one-line plain meaning.\n\
- Visual. Give most items a `visual`: a small diagram that shows the point at a glance. Labels are 1-5 words. \
Steps are revealed one at a time next to the visual, so order them like a short explanation. The first item \
should usually be a `bars` visual of hours per objective (and \"no objective\") since the last brief, from \
calendar + browsing + journal; say in its `why` what the hours are based on.\n\
- Concrete. Name the actual file, person, page, meeting, date or number instead of talking in general. Never \
invent numbers.\n\n\
Learn from the user's feedback on past briefs below: do more of what they rated up, less of what they \
rated down, and act on their comments. Don't repeat items from recent briefs unless there is something new.\n\n",
    );

    p.push_str("# Objectives\n\n");
    let objectives = store.objectives();
    p.push_str(if objectives.trim().is_empty() { "(The user has not written objectives yet.)" } else { objectives.trim() });

    p.push_str("\n\n# New in the journal since the last brief\n\n");
    let fresh: Vec<_> = entries.iter().filter(|e| e.ts > since).collect();
    if fresh.is_empty() {
        p.push_str("(Nothing new. Focus on the objectives and on the history below.)\n");
    }
    for e in &fresh {
        let _ = writeln!(p, "- {} | project: {} | {} | via {}\n  {}", e.ts.format("%Y-%m-%d %H:%M"), e.project, e.kind, e.source, e.summary);
        if !e.details.is_empty() {
            let _ = writeln!(p, "  details: {}", e.details.replace('\n', "\n  "));
        }
        if !e.tags.is_empty() {
            let _ = writeln!(p, "  tags: {}", e.tags.join(", "));
        }
    }

    p.push_str("\n# Earlier journal entries (last 45 days, for context)\n\n");
    for e in entries.iter().filter(|e| e.ts <= since && e.ts > history_cutoff).rev().take(150) {
        let _ = writeln!(p, "- {} [{}] {}: {}", e.ts.format("%Y-%m-%d"), e.project, e.kind, e.summary);
    }

    p.push_str("\n# The user's feedback on past briefs\n\n");
    let feedback = store.feedback();
    let recent_fb: Vec<_> = feedback.iter().filter(|f| f.ts > history_cutoff).collect();
    if recent_fb.is_empty() {
        p.push_str("(none yet)\n");
    }
    for f in recent_fb {
        let target = if f.item_title.is_empty() { "the whole brief".to_owned() } else { format!("\"{}\"", f.item_title) };
        let rating = match f.rating.as_str() {
            "up" => "liked",
            "down" => "disliked",
            _ => "commented on",
        };
        let _ = write!(p, "- {} {rating} {target}", f.brief_date);
        if !f.comment.is_empty() {
            let _ = write!(p, ": \"{}\"", f.comment);
        }
        p.push('\n');
    }

    p.push_str("\n# Recent briefs (avoid repeating)\n\n");
    for date in store.brief_dates().into_iter().take(5) {
        if let Some(b) = store.brief(date) {
            let titles: Vec<_> = b.sections.iter().flat_map(|s| s.items.iter().map(|i| i.title.as_str())).collect();
            let _ = writeln!(p, "- {date}: {} | {}", b.headline, titles.join("; "));
        }
    }

    if !anki.is_empty() {
        p.push_str(
            "\n# Anki\n\nThe user keeps what they've already learned (for their interviews) in Anki flashcards. Their \
Anki today:\n\n",
        );
        p.push_str(anki);
        p.push_str(
            "\nIn \"Today\", include one Anki session item whenever cards are due: say how many, roughly how long \
(about 8 seconds a card) and which deck first, ideally timed before the interview or lesson it supports. If some \
cards keep being forgotten, say which idea to relearn instead of drilling it. If the journal shows the user \
understood something worth keeping that has no card yet, suggest asking their chatbot to make cards for it. Cards \
are for remembering, not for learning something new, so don't suggest cards for things they haven't learned yet.\n",
        );
    }

    if !todo.is_empty() {
        p.push_str(
            "\n# Todoist\n\nThe user's to-do list: what they still plan to do. The journal says what they did, the \
objectives say why; this says what's next. Use it to tie the three together:\n\
- Objectives with no tasks behind them, and projects or piles of tasks that serve no objective.\n\
- Work in the journal that no task planned, and tasks that keep slipping (many days late, or overdue in bulk).\n\
- Too much due today or before a deadline in the objectives or journal, and what could move.\n\
- Completed tasks that close something the journal was working towards.\n\
In \"Today\", prefer next steps that are already Todoist tasks: quote the task as written and say it's from \
Todoist. When you suggest something new that isn't there, say it's worth adding to Todoist. Never list the to-do \
list back to them; only bring up tasks that matter today.\n\n",
        );
        p.push_str(todo);
    }

    if !src.calendar.is_empty() {
        p.push_str(
            "\n# Calendar\n\nFrom the user's calendar app: where their time went since the last brief, today with its free \
blocks, and the next 7 days. Use it to:\n\
- Count hours that went to each objective (and to none) since the last brief.\n\
- Place today's next steps in real free blocks (\"in your 14:00-16:00 gap\"), and say plainly when today is too \
full for what the to-do list or objectives want.\n\
- Spot deadlines, interviews and events coming up that need preparation that isn't planned anywhere.\n\
- Spot recurring meetings or commitments that serve no objective.\n\
Never list the calendar back to them.\n\n",
        );
        p.push_str(src.calendar);
    }

    if !src.browsing.is_empty() {
        p.push_str(
            "\n# Browsing\n\nPages the user had in front of them since the last brief, with time actually spent \
(active tab, focused window, not idle) and, for pages they stayed on, the start of the text. It shows what they \
really spent attention on, including work and reading no chatbot logged. Use it to:\n\
- Count time per objective: map sites and pages to objectives (a GitHub repo, docs, papers, job sites); \
video, social and news usually serve none unless the content clearly does.\n\
- Notice what they're researching or reading, and connect it to their projects, or point out it's a detour.\n\
- Pick reading suggestions that build on what they read, without suggesting pages they already read.\n\
Be matter-of-fact about distraction, not preachy. Never list their history back to them or quote URLs \
unless the point is about that page.\n\n",
        );
        p.push_str(src.browsing);
    }

    if !meetings.is_empty() {
        p.push_str(
            "\n# Granola\n\nThe user's meeting notes (Granola): who they talked to and what was said. Read them \
against the objectives, journal and to-do list:\n\
- Promises and action items from meetings, theirs or owed to them, that no task or journal entry picks up. \
Name the person and the meeting.\n\
- Follow-ups due after interviews and calls (thank-you notes, sending something, a reply they're waiting on).\n\
- Decisions, advice or feedback from a meeting that changes what they should do on a project, or contradicts it.\n\
- People or ideas from one meeting that connect to another project.\n\
Never summarise meetings back; only bring up what changes what they should do today.\n\n",
        );
        p.push_str(meetings);
    }

    p.push_str(
        r#"
# Output format

Reply with ONLY a JSON object, no prose before or after, matching:

{
  "headline": "one line, at most 14 words: the most important thing today",
  "summary": "at most 2 short sentences of markdown",
  "reading_minutes": 5,
  "think_first": {"question": "optional: one question to think about before reading", "answer": "1-2 sentences"},
  "terms": [{"term": "RAG", "meaning": "the model looks things up in your documents before answering"}],
  "sections": [
    {
      "title": "Where your time went | Reprioritise | Feedback | Connections | Watch out | Read & watch | Today",
      "items": [
        {
          "title": "at most 8 words",
          "kind": "time | reprioritise | feedback | connection | blindspot | resource | next_step | question",
          "project": "project name or empty",
          "objective": "the objective it serves, short, or none",
          "takeaway": "the one thing to remember or do, at most 20 words",
          "why": "why it's in today's brief: the evidence, 1-2 sentences",
          "about": "resources only: 3-5 sentences on what it covers and what they'd get from it",
          "steps": ["2-3 sentences, each at most 16 words"],
          "visual": null,
          "links": [{"title": "...", "url": "https://...", "type": "paper | video | article | repo | other"}]
        }
      ]
    }
  ]
}

`visual` is null or one of these (keep each small: 2-5 entries):
  {"type": "flow", "nodes": ["Write promise", "Build eval", "Then fix retriever"], "highlight": 1}
      steps in order, an arrow between each; highlight (optional) marks the one that matters most
  {"type": "compare", "left": {"title": "Now", "points": ["...", "..."]}, "right": {"title": "Better", "points": ["...", "..."]}}
      two options side by side; the right one is the recommended one
  {"type": "bars", "unit": "hours", "bars": [{"label": "chessLLM", "value": 6}, {"label": "brief", "value": 2}]}
      compare amounts; only real numbers from the journal or objectives, never made up
  {"type": "timeline", "events": [{"when": "Tue 10:30", "what": "Coding round"}, {"when": "Tue 12:00", "what": "ML round"}]}
      dates and deadlines in order
  {"type": "stat", "value": "48h", "label": "until your first interview"}
      one number that matters

Use only the sections you need, in this order. "Where your time went" and "Reprioritise" come first when there's
calendar, browsing or journal data to go on. Next steps in "Today" usually need no visual; each step there is
one action, and becomes a checkbox.
"#,
    );
    p
}
