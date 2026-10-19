//! Builds the daily brief by running the user's agent CLI headless (Claude Code by default, or
//! Codex, Cline, OpenCode; see agent.rs), so it uses their existing subscription. No API key involved.

use crate::{anki, browsing, calendar, granola, search, todoist};
use crate::store::{Brief, Research, Store};
use anyhow::Result;
use chrono::{Duration, Local, NaiveDate};
use std::fmt::Write as _;

const TIMEOUT_MINUTES: u64 = 20;

pub async fn generate(store: &Store) -> Result<Brief> {
    write(store, Local::now().date_naive()).await
}

/// Write the brief for `date` again with today's method, as it would have been written that day
/// (only the journal and feedback up to then; calendar, browsing, to-dos and meetings are read
/// live, so only for today). The old version is kept and shown next to it.
pub async fn regenerate(store: &Store, date: NaiveDate) -> Result<Brief> {
    write(store, date).await
}

async fn write(store: &Store, date: NaiveDate) -> Result<Brief> {
    let cfg = store.config();
    let live = date >= Local::now().date_naive();
    let previous = store.brief(date);
    let as_of = if live {
        Local::now()
    } else {
        previous.as_ref().and_then(|b| b.generated_at).unwrap_or_else(|| end_of(date))
    };
    let since = last_brief_time(store, date, as_of);
    let blocking = |f: fn(&Store, chrono::DateTime<Local>) -> String| {
        let store = store.clone();
        async move {
            if !live {
                return String::new();
            }
            tokio::task::spawn_blocking(move || f(&store, since)).await.unwrap_or_default()
        }
    };
    let (anki, todo, meetings, cal) = tokio::join!(
        blocking(|s, t| anki::brief_context(s, t)),
        blocking(|s, t| todoist::brief_context(s, t)),
        blocking(|s, t| granola::brief_context(s, t)),
        blocking(|s, t| calendar::brief_context(s, t)),
    );
    let web = if live { browsing::brief_context(store, since) } else { String::new() };
    let (agent, model) = crate::agent::configured(store);
    let provider = if agent.runs_commands() { search::active(store) } else { search::Provider::Claude };
    let ctx = Ctx { date, as_of, since, live, provider, previous: previous.as_ref() };
    let prompt = build_prompt(store, &ctx, &Sources { anki: &anki, todo: &todo, meetings: &meetings, calendar: &cal, browsing: &web });
    let tools = crate::agent::Tools::Research { composio: cfg.composio && live, search: provider != search::Provider::Claude };
    let reply = agent.ask(store, &model, &prompt, tools, std::time::Duration::from_secs(TIMEOUT_MINUTES * 60)).await?;
    let text = reply.text.as_str();

    let mut brief = parse_brief(text);
    brief.date = Some(date);
    brief.generated_at = Some(if live { Local::now() } else { as_of });
    brief.cost_usd = reply.cost_usd;
    let queries = serde_json::from_str::<serde_json::Value>(crate::agent::json_object(text))
        .ok()
        .and_then(|v| serde_json::from_value::<Vec<String>>(v["research_queries"].clone()).ok())
        .unwrap_or_default();
    brief.research = Some(Research { provider: provider.id().into(), queries });
    for (si, section) in brief.sections.iter_mut().enumerate() {
        for (ii, item) in section.items.iter_mut().enumerate() {
            item.id = format!("s{si}i{ii}");
        }
    }
    // Keep the old one before the new one takes its place.
    brief.version = store.archive_brief(date)?.map_or(1, |n| n + 1);
    // Each version has its own videos, so older versions keep theirs.
    let _ = std::fs::remove_dir_all(store.media_dir(date).join(format!("v{}", brief.version)));
    store.save_brief(&brief)?;
    crate::video::render_all(store, &mut brief).await;
    store.save_brief(&brief)?;
    Ok(brief)
}

fn end_of(date: NaiveDate) -> chrono::DateTime<Local> {
    date.and_hms_opt(23, 59, 0).and_then(|t| t.and_local_timezone(Local).single()).unwrap_or_else(Local::now)
}

/// Take the JSON object out of the model's reply; fall back to showing the raw text.
fn parse_brief(text: &str) -> Brief {
    serde_json::from_str::<Brief>(crate::agent::json_object(text)).unwrap_or_else(|_| Brief {
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

/// When the brief before `date` was written (or a week before `as_of`): "new" means after this.
fn last_brief_time(store: &Store, date: NaiveDate, as_of: chrono::DateTime<Local>) -> chrono::DateTime<Local> {
    store
        .brief_dates()
        .into_iter()
        .filter(|d| *d < date)
        .find_map(|d| store.brief(d).and_then(|b| b.generated_at))
        .filter(|t| *t < as_of)
        .unwrap_or(as_of - Duration::days(7))
}

/// Which brief is being written.
struct Ctx<'a> {
    date: NaiveDate,
    /// What counts as "now": only journal entries and feedback up to then are read.
    as_of: chrono::DateTime<Local>,
    since: chrono::DateTime<Local>,
    /// Today's brief, so calendar, browsing, to-dos and meetings are read too.
    live: bool,
    provider: search::Provider,
    /// The version being rewritten, if there is one.
    previous: Option<&'a Brief>,
}

/// What the connected apps say, each already formatted for the prompt ("" when not connected).
struct Sources<'a> {
    anki: &'a str,
    todo: &'a str,
    meetings: &'a str,
    calendar: &'a str,
    browsing: &'a str,
}

fn build_prompt(store: &Store, ctx: &Ctx, src: &Sources) -> String {
    let Sources { anki, todo, meetings, .. } = *src;
    let entries = store.entries();
    let since = ctx.since;
    let history_cutoff = ctx.as_of - Duration::days(45);

    let mut p = String::new();
    let _ = writeln!(p, "You are writing the user's personal daily brief for {}.", ctx.date.format("%A, %B %-d, %Y"));
    p.push_str(
        "The brief has two jobs. First, hold the user to what they say they want: compare what they're TRYING to \
accomplish (their objectives, below) with what they're actually BUILDING and doing (their journal, calendar, \
browsing, to-dos, meetings), and tell them plainly where the two have come apart. Second, make them better at the \
work: teach them the one or two things they most need to understand right now, and find them hidden gems (papers, \
repos, write-ups, people) they would not have found on their own.\n\n\
# Voice\n\n\
You are a demanding mentor who respects them too much to flatter them. Be blunt and specific. If the thing they \
spent the weekend building doesn't move an objective, say so in the first item, with the numbers. If a plan is weak, \
say why and what a strong one looks like. No softening (\"you might consider\", \"it could be worth\"), no \
cheerleading, no generic advice that would fit anyone. Credit real progress in one line when it's earned, then move \
on. Harsh about the work, never about the person.\n\n\
# What to look for\n\n\
- Trying vs building. For each active project in the journal, what objective is it for, and does what they built \
this week actually get them closer? Look for: polishing tools instead of using them, building infrastructure before \
having one result, a side project eating the hours of the main goal, research that never turns into a decision, \
a deadline nothing prepares for. Say what the objective needs next instead.\n\
- Where the time went: hours per objective (and \"no objective\") since the last brief, with real numbers.\n\
- Feedback on the actual work: decisions, approach, quality. Point at the decision and say if it was right.\n\
- What they need to learn: a concept they are fumbling (a struggle in the journal, the same question twice, a \
decision made on a hunch, an interview topic coming up they haven't practised). Teach it in the brief.\n\
- Hidden gems: research their open problems on the web (see Research below) and bring back what moves them forward.\n\
- Connections between projects, blind spots and risks.\n\
- 2-4 concrete next steps for today, placed in their free time when the calendar is known.\n\n\
# Research\n\n\
Before writing, research. Take the 3-6 most important open problems from the journal (a bug they are stuck on, a \
design choice they are weighing, a technique they are trying, a company or interview they are preparing for) and \
search for each, several queries per problem. You are looking for hidden gems, not the obvious:\n\
- Recent papers (last 18 months) with results on exactly their problem, ideally with code; a small repo that \
already solved it; a practitioner's write-up with real numbers or an ablation; a talk; a person worth following; \
the docs page or issue that answers their question.\n\
- NOT famous classics, generic tutorials, listicles, vendor marketing or anything they already read (see Browsing).\n\
- Read every source you recommend before recommending it, and take the specific thing they'd get from it: a number, \
a trick, a design, a warning. Never invent links or claims; if the search found nothing good, say less.\n",
    );
    p.push_str(&search::prompt_section_for(store, ctx.provider));
    p.push_str(
        "List every search query you ran in `research_queries`.\n\n\
# How to write\n\n\
- The user reads one item at a time and rates each, so every item must stand on its own.\n\
- Every item has a `takeaway`: the single thing to remember or do, one sentence of at most 20 words. Someone \
who reads only the takeaways gets the whole brief.\n\
- Every item has a `why`: the evidence it's based on (\"Your calendar shows 11h of meetings since Monday and none \
on DRC_explorer\", \"You logged three attempts at the reward function\", \"From your objective: Liquid application \
by Nov 15\"). One or two sentences. Never vague.\n\
- Every item has an `objective`: the objective it serves, quoted short as the user wrote it, or \"none\".\n\
- At most 10 items in the whole brief. Each item has 2-4 steps, each one sentence of at most 18 words. Fewer, \
sharper points beat coverage.\n\
- The first item is a `gap` item (\"Trying vs building\"): a `compare` visual with left = what they're building or \
doing, right = what the objective actually needs. Add more gap items only for other big gaps.\n\
- 1-2 `learn` items: one concept, taught. The video does the explaining (see Videos); `about` is 1-2 sentences \
on why it matters for their work. Steps are what to do with it; links point to the best source you read.\n\
- 2-3 `resource` items, the hidden gems: exactly one link each, an `about` of 2-3 short sentences (what it found, \
what the user takes from it) and a `why` that names the problem from their journal. If explaining the method \
needs more than that, give it a video instead of a longer paragraph. Steps say what to look for or try.\n\
- Write like a terse note from a colleague: state the fact, then what to do. Short sentences, plain words. \
Spell out acronyms; a technical word that's truly needed goes into `terms` with a one-line plain meaning.\n\
- Nothing that sounds written by an AI. No clever or punchy titles, wordplay, or two-part contrasts \
(\"You built studios; interviews waited\", \"X, not Y\", \"less A, more B\"). No semicolons or em dashes for effect, \
no rhetorical questions, no lists of three for rhythm, no \"here's the thing\", \"the real question\", \"quietly\", \
\"the elephant\", no buzzwords (\"leverage\", \"surface\", \"unblock\", \"bandwidth\", \"north star\", \"double down\"). \
A title says the point plainly: \"No interview prep logged; four interviews this week\" is wrong (two clauses \
glued), \"Four interviews this week and no prep logged\" is right.\n\
- Visual. Give most items a `visual`: a small diagram that shows the point at a glance, labels 1-5 words. The \
time item should be a `bars` visual of hours per objective since the last brief; say in its `why` what the hours \
are based on.\n\
- Concrete. Name the actual file, person, page, meeting, date or number. Never invent numbers.\n\n\
# Videos\n\n\
Give 1-3 items a `video`: the `learn` items, and each hidden gem whose method is easier seen than read. A video is \
a few short silent clips (manim, like 3Blue1Brown), one per beat: the reader looks at a clip, reads its caption \
under it, and presses Next. Each clip is one simple diagram that gives the idea of its beat at a glance; keep it \
minimal, the point is to give them the idea, not a show. Only use a video where SEEING helps: a mechanism, a \
pipeline, a before/after, a comparison with numbers. Not for advice or to-dos. The content comes from the source \
you read: its actual method and numbers, not general knowledge. A storyboard:\n\
  {\"title\": \"at most 7 words\", \"insight\": \"the one sentence they must leave with\", \"source\": \"title, url\",\n\
   \"beats\": [{\"say\": \"caption on screen, at most 18 words\", \"show\": \"what to draw: shapes, labels, numbers, \
what moves or changes, what is highlighted\"}]}\n\
2-4 beats, in teaching order (the problem, the idea, how it works, the evidence). Captions are plain sentences, \
at most 18 words. Each `show` describes one diagram with at most ~6 things on it. Be precise in `show` (\"three boxes left to right: Query, Retriever, LFM-2.5; arrow; the \
retriever box splits into BM25 and dense; bars: recall 0.61 vs 0.78\").\n\n\
Learn from the user's feedback on past briefs below: do more of what they rated up, less of what they rated down, \
and act on their comments. Don't repeat items from recent briefs unless there is something new.\n\n",
    );
    if !ctx.live {
        let _ = writeln!(
            p,
            "Note: you are writing this brief as of {} (not today). Write it as if it were that morning. Use only the \
evidence below; calendar, browsing, to-dos and meetings aren't available for past days.\n",
            ctx.as_of.format("%A %B %-d, %H:%M")
        );
    }
    if let Some(old) = ctx.previous {
        p.push_str("# The version you're replacing\n\nThis brief was written before with an older method. The user wants a better one. Here's what it said; keep what they liked, drop what they disliked, act on their comments, and fix what it got wrong or left shallow:\n\n");
        let _ = writeln!(p, "Headline: {}", old.headline);
        for s in &old.sections {
            for i in &s.items {
                let _ = writeln!(p, "- [{}] {}: {}", i.kind, i.title, if i.takeaway.is_empty() { &i.body } else { &i.takeaway });
            }
        }
        p.push('\n');
    }

    p.push_str("# Objectives\n\n");
    let objectives = store.objectives();
    p.push_str(if objectives.trim().is_empty() { "(The user has not written objectives yet.)" } else { objectives.trim() });

    p.push_str("\n\n# New in the journal since the last brief\n\n");
    let fresh: Vec<_> = entries.iter().filter(|e| e.ts > since && e.ts <= ctx.as_of).collect();
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
    // Feedback up to then, and everything said about this very brief (that's what a rewrite is for).
    let recent_fb: Vec<_> = feedback.iter().filter(|f| f.brief_date == ctx.date || (f.ts > history_cutoff && f.ts <= ctx.as_of)).collect();
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
    for date in store.brief_dates().into_iter().filter(|d| *d < ctx.date).take(5) {
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

    if store.config().composio && ctx.live {
        p.push_str(
            "\n# Composio\n\nThe user connected more of their apps (mail, chat, docs, issue trackers…) through Composio. \
You can read them with the `composio` command: `composio search \"<what you need>\"` finds the right tool, \
`composio execute <TOOL_SLUG> --get-schema` shows its inputs, and `composio execute <TOOL_SLUG> -d '{...}'` runs \
it. Only ever run tools that READ (list, get, search, fetch); never send, post, create, update or delete \
anything. Look at what happened since the last brief in the apps that matter for their objectives (unanswered \
threads, mentions, issues assigned to them, docs they edited) and use it like the other evidence above; say in \
`why` which app it came from. Skip this if the command fails.\n\n",
        );
    }

    p.push_str(
        r#"
# Output format

Reply with ONLY a JSON object, no prose before or after, matching:

{
  "headline": "one line, at most 14 words: the most important thing today",
  "summary": "at most 2 short sentences of markdown",
  "reading_minutes": 6,
  "think_first": {"question": "optional: one question to think about before reading", "answer": "1-2 sentences"},
  "terms": [{"term": "RAG", "meaning": "the model looks things up in your documents before answering"}],
  "research_queries": ["every query you searched"],
  "sections": [
    {
      "title": "Trying vs building | Where your time went | Reprioritise | Learn | Feedback | Connections | Watch out | Hidden gems | Today",
      "items": [
        {
          "title": "at most 8 words",
          "kind": "gap | time | reprioritise | learn | feedback | connection | blindspot | resource | next_step | question",
          "project": "project name or empty",
          "objective": "the objective it serves, short, or none",
          "takeaway": "the one thing to remember or do, at most 20 words",
          "why": "why it's in today's brief: the evidence, 1-2 sentences",
          "about": "learn and resource items: what it is and what they get from it",
          "steps": ["2-4 sentences, each at most 18 words"],
          "visual": null,
          "video": null,
          "links": [{"title": "...", "url": "https://...", "type": "paper | video | article | repo | other"}]
        }
      ]
    }
  ]
}

`visual` is null or one of these (keep each small: 2-5 entries):
  {"type": "flow", "nodes": ["Write promise", "Build eval", "Then fix retriever"], "highlight": 1}
      steps in order, an arrow between each; highlight (optional) marks the one that matters most
  {"type": "compare", "left": {"title": "Building", "points": ["...", "..."]}, "right": {"title": "Objective needs", "points": ["...", "..."]}}
      two things side by side; the right one is the recommended one
  {"type": "bars", "unit": "hours", "bars": [{"label": "chessLLM", "value": 6}, {"label": "brief", "value": 2}]}
      compare amounts; only real numbers from the evidence, never made up
  {"type": "timeline", "events": [{"when": "Tue 10:30", "what": "Coding round"}, {"when": "Tue 12:00", "what": "ML round"}]}
      dates and deadlines in order
  {"type": "stat", "value": "48h", "label": "until your first interview"}
      one number that matters

`video` is null or a storyboard as described under Videos. An item with a video can still have a visual.

Use only the sections you need, in this order. Next steps in "Today" usually need no visual; each step there is
one action, and becomes a checkbox.
"#,
    );
    p
}
