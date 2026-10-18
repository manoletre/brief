//! Builds the daily brief by running the local Claude Code CLI (`claude -p`),
//! so it uses your existing Claude subscription. No API key involved.

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
    let prompt = build_prompt(store);
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

fn find_claude() -> Option<std::path::PathBuf> {
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

fn build_prompt(store: &Store) -> String {
    let now = Local::now();
    let entries = store.entries();
    let last_brief = store.brief_dates().first().and_then(|d| store.brief(*d));
    let since = last_brief
        .as_ref()
        .and_then(|b| b.generated_at)
        .unwrap_or(now - Duration::days(7));
    let history_cutoff = now - Duration::days(45);

    let mut p = String::new();
    let _ = writeln!(p, "You are writing the user's personal daily brief for {}.", now.format("%A, %B %-d, %Y"));
    p.push_str(
        "The user logs what they work on from several AI chatbots into a journal. Your job is to read their \
objectives and the journal, then write a brief they can read in 10-20 minutes that makes them better at \
their projects. Be a sharp, candid mentor, not a summariser. Prioritise:\n\
- feedback and direction on what they're actually doing, tied to their objectives\n\
- connections between projects, or between their work and ideas they may not know\n\
- blind spots: risks, alternatives, prior art, or considerations they are not taking into account\n\
- 3-6 concrete things to read or watch (papers, talks, videos, posts, repos). Use web search to find \
real, current resources and verify each URL exists; never invent links\n\
- a few concrete next steps for today\n\n\
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

    p.push_str(
        r#"
# Output format

Reply with ONLY a JSON object, no prose before or after, matching:

{
  "headline": "one line capturing the most important thing today",
  "summary": "2-3 short paragraphs of markdown: where the user stands and what matters most",
  "reading_minutes": 15,
  "sections": [
    {
      "title": "section title, e.g. Feedback, Connections, Blind spots, Read & watch, Today",
      "items": [
        {
          "title": "short title",
          "body": "markdown, 1-3 paragraphs, specific and actionable",
          "kind": "feedback | connection | blindspot | resource | next_step | question",
          "project": "project name or empty",
          "links": [{"title": "...", "url": "https://...", "type": "paper | video | article | repo | other"}]
        }
      ]
    }
  ]
}
"#,
    );
    p
}
