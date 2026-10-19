//! `brief init` (and `brief` on first run): onboarding in the terminal. What brief is, the agent
//! that keeps you accountable (and its model), your objectives in a conversation with that agent,
//! the agents and tools brief reads, and a first brief with your feedback on it. Only the briefs
//! themselves live on the web.

use crate::agent::{self, Agent, Tools};
use crate::interview::{self, Message, Objective, Transcript};
use crate::store::{Feedback, Store};
use crate::{anki, browsing, calendar, connect, generate, granola, todoist};
use anyhow::Result;
use ratatui::crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

const ACCENT: &str = "\x1b[38;5;208m";
const DIM: &str = "\x1b[38;5;244m";
const FAINT: &str = "\x1b[38;5;240m";
const GOOD: &str = "\x1b[38;5;114m";
const BAD: &str = "\x1b[38;5;203m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const BANNER: [(&str, u8); 6] = [
    ("██████╗ ██████╗ ██╗███████╗███████╗", 202),
    ("██╔══██╗██╔══██╗██║██╔════╝██╔════╝", 202),
    ("██████╔╝██████╔╝██║█████╗  █████╗  ", 208),
    ("██╔══██╗██╔══██╗██║██╔══╝  ██╔══╝  ", 208),
    ("██████╔╝██║  ██║██║███████╗██║     ", 214),
    ("╚═════╝ ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ", 214),
];
const SUGGEST_EMAIL: &str = "hi@manoletre.com";
const COMPOSIO_INSTALL: &str = "curl -fsSL https://composio.dev/install | sh";
const STEPS: usize = 6;

/// The whole onboarding. Blocking: call it from `spawn_blocking`.
pub fn run(store: Store) -> Result<()> {
    exit_on_ctrl_c();
    intro();
    if !pick_agent(&store, 1)? {
        return Ok(());
    }
    set_objectives(&store, Some(2))?;
    connect_agents(&store, 3)?;
    connect_tools(&store, 4)?;
    background(&store, 5)?;
    first_brief(&store, 6)?;
    let mut cfg = store.config();
    cfg.onboarded = true;
    store.save_config(&cfg)?;
    outro(&store);
    Ok(())
}

/// Just the objectives conversation (the app's Objectives → i).
pub fn interview_only(store: Store) -> Result<()> {
    exit_on_ctrl_c();
    let (agent, _) = agent::configured(&store);
    if agent.find().is_none() {
        say(&format!("{BAD}{} isn't installed.{RESET} Run `brief init` to pick the agent brief works with.", agent.name()));
        return Ok(());
    }
    set_objectives(&store, None)
}

// ── steps ────────────────────────────────────────────────────

fn intro() {
    println!();
    for (row, color) in BANNER {
        println!("  \x1b[38;5;{color}m{row}{RESET}");
    }
    println!();
    say(&format!("{BOLD}brief is a shared memory that keeps you accountable.{RESET}"));
    println!();
    para("The AI agents you work with every day (Claude Code, Codex, Cline, ChatGPT…) write down what you work on. Every morning brief reads that, with your calendar, to-dos and meetings, holds it up against your goals and tells you, like a candid friend, what to do more of, less of, or stop.");
    println!();
    para(&format!("{DIM}Setup takes about 15 minutes: pick the agent that keeps you accountable, set your goals with it, connect your agents and tools, then read your first brief.{RESET}"));
    println!();
    if select("Ready?", &[Choice::new("Let's start", "")], 0).is_none() {
        std::process::exit(0);
    }
}

/// Returns false if the user wants to stop here.
fn pick_agent(store: &Store, step: usize) -> Result<bool> {
    header(step, "Your accountability agent");
    para("brief works through a coding agent you already use, on your own account. It runs your goals conversation now and writes your brief every morning.");
    loop {
        let found = wait_blocking("Looking for agents on this Mac", || agent::ALL.map(|a| (a.find().is_some(), a.account())));
        let current = agent::configured(store).0;
        let mut choices: Vec<Choice> = agent::ALL
            .iter()
            .zip(&found)
            .map(|(a, (installed, account))| {
                let state = match (installed, account) {
                    (false, _) => State::Off("not installed".into()),
                    (true, None) => State::Warn("not signed in".into()),
                    (true, Some(who)) if who.is_empty() => State::On("signed in".into()),
                    (true, Some(who)) => State::On(format!("signed in · {who}")),
                };
                Choice::new(a.name(), a.blurb()).state(state)
            })
            .collect();
        choices.push(Choice::new("Suggest one", &format!("email us the agent you'd like ({SUGGEST_EMAIL})")));
        let default = agent::ALL
            .iter()
            .position(|a| *a == current)
            .filter(|&i| found[i].1.is_some())
            .or_else(|| found.iter().position(|(_, acc)| acc.is_some()))
            .unwrap_or(0);
        let Some(i) = select("Which agent should keep you accountable?", &choices, default) else { return Ok(false) };
        if i == agent::ALL.len() {
            suggest_agent();
            continue;
        }
        let a = agent::ALL[i];
        let (installed, account) = &found[i];
        if !installed {
            say(&format!("{} isn't installed yet. It installs with {ACCENT}{}{RESET}", a.name(), a.install_cmd()));
            if select("Install it now?", &[Choice::new("Install", "runs the command above here"), Choice::new("Pick another agent", "")], 0) == Some(0) {
                shell(a.install_cmd());
            }
            continue;
        }
        if account.is_none() {
            say(&format!("{} needs you to sign in first.", a.name()));
            if select("Sign in now?", &[Choice::new("Sign in", &format!("runs `{} {}`", a.id(), a.login_args().join(" "))), Choice::new("Pick another agent", "")], 0) == Some(0) {
                run_interactive(&a.find().unwrap(), a.login_args());
                if wait_blocking("Checking", move || a.account()).is_none() {
                    say(&format!("{BAD}Still not signed in to {}.{RESET} Try again, or pick another agent.", a.name()));
                }
            }
            continue;
        }

        let cfg = store.config();
        let models = a.models();
        let mut mchoices: Vec<Choice> = models.iter().map(|m| Choice::new(m.label, m.hint)).collect();
        mchoices[0] = Choice::new(&format!("{} (recommended)", models[0].label), models[0].hint);
        let default = if cfg.agent == a.id() { models.iter().position(|m| m.id == cfg.model).unwrap_or(0) } else { 0 };
        let Some(m) = select(&format!("Which model should {} use?", a.name()), &mchoices, default) else { continue };
        let mut cfg = store.config();
        cfg.agent = a.id().into();
        cfg.model = models[m].id.into();
        store.save_config(&cfg)?;

        let (s, model) = (store.clone(), cfg.model.clone());
        let hello = wait(&format!("Saying hello to {}", a.name()), async move {
            a.ask(&s, &model, "Reply with exactly the word: ready", Tools::None, Duration::from_secs(180)).await
        });
        match hello {
            Ok(_) => {
                done(&format!("{} with {} will keep you accountable.", a.name(), a.model_label(&cfg.model)));
                return Ok(true);
            }
            Err(e) => {
                say(&format!("{BAD}{} didn't answer:{RESET} {}", a.name(), one_line(&format!("{e:#}"))));
                say(&format!("{DIM}Try another model, check `{}` works on its own, or pick another agent.{RESET}", a.id()));
            }
        }
    }
}

fn suggest_agent() {
    let subject = "An agent for brief";
    let body = "Hi! I'd like brief to work with this agent:\n\n(name and link)\n\nWhy I use it:\n\n";
    connect::open(&format!("mailto:{SUGGEST_EMAIL}?subject={}&body={}", crate::html::url_encode(subject), crate::html::url_encode(body)));
    say(&format!("Opened an email to {SUGGEST_EMAIL} in your mail app. Thanks! Pick one of the others to start with meanwhile."));
}

/// The goals conversation, driven by the configured agent. `step` is None outside onboarding.
fn set_objectives(store: &Store, step: Option<usize>) -> Result<()> {
    let (a, _) = agent::configured(store);
    match step {
        Some(n) => header(n, "Your objectives"),
        None => println!(),
    }
    let existing = store.objective_list();
    if !existing.is_empty() {
        say(&format!("You have {} objective{} already:", existing.len(), if existing.len() == 1 { "" } else { "s" }));
        for (i, o) in existing.iter().enumerate() {
            say(&format!("  {FAINT}{}.{RESET} {}", i + 1, o.lines().next().unwrap_or_default()));
        }
        let choices = [Choice::new(&format!("Sharpen them with {}", a.name()), "a short conversation"), Choice::new("Keep them", "")];
        if select("What now?", &choices, if step.is_some() { 1 } else { 0 }) != Some(0) {
            return Ok(());
        }
    } else {
        para(&format!("{} will ask you a few questions, one at a time: where you are, what gives you energy, what matters most, and then you'll set short- and long-term goals together. About ten minutes.", a.name()));
        say(&format!("{DIM}Answer in your own words and press ⏎. Type a number to pick a suggestion, /skip to stop and write them yourself later.{RESET}"));
    }

    let mut t = Transcript::default();
    loop {
        let (s, transcript) = (store.clone(), t.clone());
        let reply = match wait(&format!("{} is thinking", a.name()), async move { interview::turn(&s, &transcript).await }) {
            Ok(r) => r,
            Err(e) => {
                say(&format!("{BAD}{} didn't answer:{RESET} {}", a.name(), one_line(&format!("{e:#}"))));
                match select("What now?", &[Choice::new("Try again", ""), Choice::new("Skip for now", "set them later with `brief` → Objectives")], 0) {
                    Some(0) => continue,
                    _ => return Ok(()),
                }
            }
        };
        agent_says(&reply.message);
        t.messages.push(Message { role: "assistant".into(), text: reply.message.clone() });

        if !reply.objectives.is_empty() {
            show_objectives(&reply.objectives);
            let choices = [
                Choice::new("Yes, hold me to these", "saves them; every brief is written against them"),
                Choice::new("Change something", "tell it what to change"),
                Choice::new("Skip for now", "nothing is saved"),
            ];
            match select("Save these objectives?", &choices, 0) {
                Some(0) => {
                    let items: Vec<String> = reply.objectives.iter().filter(|o| !o.title.trim().is_empty()).enumerate().map(|(i, o)| o.to_item(i + 1)).collect();
                    store.save_objective_list(&items)?;
                    done(&format!("Saved {} objectives. Edit them any time with `brief` → Objectives.", items.len()));
                    return Ok(());
                }
                Some(1) => {}
                _ => return Ok(()),
            }
        } else if !reply.suggestions.is_empty() {
            let list: Vec<String> = reply.suggestions.iter().enumerate().map(|(i, s)| format!("{ACCENT}{}{RESET} {s}", i + 1)).collect();
            say(&format!("       {}", list.join(&format!("{FAINT}  ·  {RESET}"))));
        }

        let answer = loop {
            let Some(line) = input() else { return Ok(()) };
            let line = line.trim().to_owned();
            if line.is_empty() {
                continue;
            }
            if line == "/skip" {
                say(&format!("{DIM}Skipped. Set them later with `brief` → Objectives (press i to talk it through again).{RESET}"));
                return Ok(());
            }
            match line.parse::<usize>() {
                Ok(n) if (1..=reply.suggestions.len()).contains(&n) => break reply.suggestions[n - 1].clone(),
                _ => break line,
            }
        };
        t.messages.push(Message { role: "user".into(), text: answer });
    }
}

fn show_objectives(objs: &[Objective]) {
    println!();
    let w = width().saturating_sub(7);
    for (i, o) in objs.iter().enumerate() {
        let horizon = match o.horizon.trim() {
            "short" => " · short term",
            "long" => " · long term",
            _ => "",
        };
        say(&format!("  {ACCENT}{}.{RESET} {BOLD}{}{RESET}{FAINT}{horizon}{RESET}", i + 1, o.title.trim()));
        for (label, v) in [("why", &o.why), ("done when", &o.done_when), ("by", &o.deadline), ("time", &o.time), ("in the way", &o.obstacles)] {
            if !v.trim().is_empty() {
                for (n, line) in wrap(&format!("{label}: {}", v.trim()), w).iter().enumerate() {
                    say(&format!("     {DIM}{}{line}{RESET}", if n == 0 { "" } else { "  " }));
                }
            }
        }
    }
}

/// Hook brief's MCP server up to the agents the user works in, so they log their work.
fn connect_agents(store: &Store, step: usize) -> Result<()> {
    let _ = store;
    header(step, "Connect your agents");
    para("Now let the agents you use every day write to brief, so it remembers what you work on. Each one gets brief's tools and logs your decisions, progress and blockers on its own.");
    say(&format!("{DIM}ChatGPT, claude.ai and Grok run in the cloud: connect them later from `brief` → Connect.{RESET}"));
    loop {
        let rows = wait_blocking("Checking your agents", || {
            vec![
                (connect::claude_code_installed(), connect::claude_code_connected()),
                (connect::claude_desktop_installed(), connect::claude_desktop_extension() || connect::claude_desktop_config_entry()),
                (Agent::Codex.find().is_some(), connect::codex_connected()),
                (Agent::Cline.find().is_some(), connect::cline_connected()),
                (Agent::OpenCode.find().is_some(), connect::opencode_connected()),
            ]
        });
        let names = ["Claude Code", "Claude Desktop", "Codex", "Cline", "OpenCode"];
        let any = rows.iter().any(|(_, on)| *on);
        let mut choices: Vec<Choice> = names
            .iter()
            .zip(&rows)
            .map(|(n, (installed, on))| {
                let state = if *on { State::On("connected".into()) } else if *installed { State::Warn("not connected".into()) } else { State::Off("not installed".into()) };
                Choice::new(n, "").state(state)
            })
            .collect();
        choices.push(if any { Choice::new("Continue", "") } else { Choice::new("Do this later", "`brief` → Connect") });
        let first_open = rows.iter().position(|(installed, on)| *installed && !*on).unwrap_or(names.len());
        let Some(i) = select("Connect brief to:", &choices, first_open) else { return Ok(()) };
        if i == names.len() {
            return Ok(());
        }
        let (installed, on) = rows[i];
        if on {
            say(&format!("{} is already connected.", names[i]));
            continue;
        }
        if !installed {
            say(&format!("{} isn't installed on this Mac.", names[i]));
            continue;
        }
        let s = store.clone();
        let result = wait_blocking(&format!("Connecting {}", names[i]), move || match i {
            0 => connect::set_claude_code(true),
            1 => connect::install_claude_desktop(&s),
            2 => connect::set_codex(true),
            3 => connect::set_cline(true),
            _ => connect::set_opencode(true),
        });
        report(result);
    }
}

/// The apps the brief reads: calendar, to-dos, meeting notes, browsing, Anki, and Composio for the rest.
fn connect_tools(store: &Store, step: usize) -> Result<()> {
    header(step, "Connect your tools");
    para("The more brief sees of your day, the more honest it gets. Everything stays on this Mac; brief only reads.");
    loop {
        let s = store.clone();
        let st = wait_blocking("Checking your tools", move || {
            (
                calendar::connected(&s),
                todoist::auth(&s).is_some(),
                granola::auth(&s).is_some(),
                browsing::last_seen(&s).is_some(),
                browsing::installed(&s),
                anki::available(false),
                s.config().composio,
            )
        });
        let (cal, todo, gran, seen, ext, ank, comp) = st;
        let on = |b: bool, off: &str| if b { State::On("connected".into()) } else { State::Off(off.into()) };
        let choices = vec![
            Choice::new("Calendar", "where your time goes · every account in the Calendar app").state(on(cal, "not connected")),
            Choice::new("Todoist", "your to-do list").state(on(todo, "not connected")),
            Choice::new("Granola", "your meeting notes").state(on(gran, "not connected")),
            Choice::new("Browser extension", &format!("what you read in {}", browsing::browser_name())).state(if seen {
                State::On("sending".into())
            } else if ext {
                State::Warn("not loaded yet".into())
            } else {
                State::Off("not installed".into())
            }),
            Choice::new("Anki", "your flashcards").state(if ank { State::On("connected".into()) } else { State::Off("not running".into()) }),
            Choice::new("Composio", "1,000+ more apps: Gmail, Slack, Notion, GitHub, Linear…").state(on(comp, "not connected")),
            if cal || todo || gran || seen || comp { Choice::new("Continue", "") } else { Choice::new("Do this later", "`brief` → Connect") },
        ];
        let Some(i) = select("Connect:", &choices, 0) else { return Ok(()) };
        let s = store.clone();
        match i {
            0 if !cal => {
                say("Building the Calendar helper; then macOS asks whether brief may see your calendar: click Allow.");
                report(wait_blocking("Connecting your calendar", move || calendar::connect(&s)));
            }
            1 if !todo => {
                say("Todoist opens in your browser: click Agree there.");
                report(wait_blocking("Waiting for Todoist", move || todoist::connect(&s)));
            }
            2 if !gran => {
                say("Granola opens in your browser: sign in and allow brief there.");
                report(wait_blocking("Waiting for Granola", move || granola::connect(&s)));
            }
            3 if !seen => {
                say(&format!("{}'s extensions page opens and the folder path is copied: turn on Developer mode, Load unpacked, ⌘⇧G, paste, ⏎.", browsing::browser_name()));
                report(wait_blocking("Installing the extension", move || browsing::install(&s).map(|t| t.lines().next().unwrap_or_default().to_owned())));
            }
            4 if !ank => {
                if wait_blocking("Opening Anki", || anki::available(true)) {
                    done("Anki is connected.");
                } else {
                    para("brief talks to Anki through the AnkiConnect add-on. In Anki: Tools → Add-ons → Get Add-ons…, paste 2055492159, restart Anki, then pick Anki here again.");
                }
            }
            5 if !comp => composio(store)?,
            6 => return Ok(()),
            _ => say(&format!("{} is already connected. Disconnect it from `brief` → Connect.", choices[i].label)),
        }
    }
}

fn composio(store: &Store) -> Result<()> {
    para("Composio signs you in to your other apps once and lets brief's agent read them with its `composio` command.");
    if agent::find_bin("composio").is_none() {
        say(&format!("Its CLI installs with {ACCENT}{COMPOSIO_INSTALL}{RESET}"));
        if select("Install it now?", &[Choice::new("Install", "runs the command above here"), Choice::new("Back", "")], 0) != Some(0) {
            return Ok(());
        }
        shell(COMPOSIO_INSTALL);
    }
    let Some(bin) = agent::find_bin("composio") else {
        say(&format!("{BAD}Couldn't find the composio command.{RESET} Install it, then pick Composio again."));
        return Ok(());
    };
    say("Signing in to Composio (a browser page opens)…");
    if !run_interactive(&bin, &["login"]) {
        say(&format!("{BAD}Composio sign-in didn't finish.{RESET} Pick Composio to try again."));
        return Ok(());
    }
    say("Which apps should brief read? Type their names, separated by spaces, e.g. gmail slack notion github linear.");
    let apps = input().unwrap_or_default();
    for app in apps.split([' ', ',']).map(str::trim).filter(|a| !a.is_empty()) {
        say(&format!("Linking {app}…"));
        run_interactive(&bin, &["link", &app.to_lowercase()]);
    }
    let mut cfg = store.config();
    cfg.composio = true;
    store.save_config(&cfg)?;
    done("Composio is connected. Your brief's agent reads those apps (read-only) each morning; add more with `composio link <app>`.");
    Ok(())
}

fn background(store: &Store, step: usize) -> Result<()> {
    header(step, "Every morning");
    let hour = store.config().brief_hour;
    if crate::plist_path().is_ok_and(|p| p.exists()) {
        done(&format!("brief runs in the background and writes a new brief every morning after {hour:02}:00."));
        return Ok(());
    }
    para(&format!("brief writes a new brief every morning after {hour:02}:00 (or when your Mac wakes up), and keeps your agents' log. For that it runs quietly in the background."));
    let choices = [Choice::new("Run in the background", "starts at login · recommended"), Choice::new("Not now", "`brief` → Run in background")];
    if select("Start it?", &choices, 0) == Some(0) {
        report(crate::install(store));
    }
    Ok(())
}

fn first_brief(store: &Store, step: usize) -> Result<()> {
    header(step, "Your first brief");
    let (a, _) = agent::configured(store);
    if store.objective_list().is_empty() {
        say(&format!("{DIM}No objectives yet, so this one will be general.{RESET}"));
    }
    para(&format!("{} is reading your objectives and everything you connected, and looking up a few things worth reading. This takes a few minutes.", a.name()));
    let brief = loop {
        let s = store.clone();
        match wait("Writing your first brief", async move { generate::generate(&s).await }) {
            Ok(b) => break b,
            Err(e) => {
                say(&format!("{BAD}Couldn't write it:{RESET} {}", one_line(&format!("{e:#}"))));
                match select("What now?", &[Choice::new("Try again", ""), Choice::new("Skip", "it'll come tomorrow morning")], 0) {
                    Some(0) => continue,
                    _ => return Ok(()),
                }
            }
        }
    };
    println!();
    say(&format!("  {BOLD}{}{RESET}", brief.headline));
    for line in wrap(&strip_md(&brief.summary), width().saturating_sub(4)) {
        say(&format!("  {DIM}{line}{RESET}"));
    }
    let items: usize = brief.sections.iter().map(|s| s.items.len()).sum();
    let date = brief.date.unwrap_or_else(|| chrono::Local::now().date_naive());
    let port = store.config().port;
    let url = format!("http://127.0.0.1:{port}/brief/{date}");
    let choices = [Choice::new("Open it", &format!("{items} cards in your browser")), Choice::new("Skip", &url)];
    if select("Read it?", &choices, 0) == Some(0) {
        ensure_server(store);
        connect::open(&url);
        say(&format!("{DIM}Opened {url}. Rate each card with 👍/👎 there; come back here when you're done.{RESET}"));
    }

    // Feedback on the brief as a whole, which every later brief reads.
    let choices = [Choice::new("Spot on", ""), Choice::new("Useful, but some misses", ""), Choice::new("Not useful yet", "")];
    let rating = select("How was your first brief?", &choices, 0);
    say("What should it do differently tomorrow? (⏎ to skip)");
    let comment = input().unwrap_or_default();
    let rating = match rating {
        Some(0) => "up",
        Some(2) => "down",
        _ => "",
    };
    if !rating.is_empty() || !comment.trim().is_empty() {
        let comment = match (rating, comment.trim()) {
            ("", c) => format!("First brief: useful, but some misses. {c}"),
            (_, c) => c.to_owned(),
        };
        store.add_feedback(&Feedback {
            ts: chrono::Local::now(),
            brief_date: date,
            item_id: String::new(),
            item_title: String::new(),
            rating: rating.into(),
            comment: comment.trim().to_owned(),
        })?;
        done("Thanks. Tomorrow's brief takes this into account.");
    }
    Ok(())
}

fn outro(store: &Store) {
    let cfg = store.config();
    println!();
    say(&format!("{GOOD}✔{RESET} {BOLD}You're all set.{RESET}"));
    println!();
    para("Now just use your agents normally and live your life normally. brief will help you reach your goals and grow.");
    println!();
    say(&format!("{DIM}Your next brief comes tomorrow after {:02}:00 at http://127.0.0.1:{}/{RESET}", cfg.brief_hour, cfg.port));
    say(&format!("{DIM}Run `brief` to see your objectives, journal and connections.{RESET}"));
    println!();
}

// ── terminal helpers ─────────────────────────────────────────

/// Raw mode for one prompt; restored on drop, also on panic.
struct Raw;

impl Raw {
    fn on() -> Raw {
        let _ = terminal::enable_raw_mode();
        out("\x1b[?25l");
        Raw
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        out("\x1b[?25h");
    }
}

fn out(s: &str) {
    let mut o = std::io::stdout();
    let _ = o.write_all(s.as_bytes());
    let _ = o.flush();
}

/// The in-process server installs a Ctrl-C handler, which would swallow Ctrl-C here.
fn exit_on_ctrl_c() {
    Handle::current().spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = terminal::disable_raw_mode();
            out("\x1b[?25h\n");
            std::process::exit(130);
        }
    });
}

fn width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(80).clamp(40, 92)
}

fn say(s: &str) {
    println!("  {s}");
}

/// A wrapped paragraph.
fn para(s: &str) {
    for line in wrap(s, width().saturating_sub(4)) {
        say(&line);
    }
}

fn done(s: &str) {
    say(&format!("{GOOD}✔{RESET} {s}"));
}

fn report(r: Result<String>) {
    match r {
        Ok(msg) => done(msg.trim()),
        Err(e) => say(&format!("{BAD}✘ {}{RESET}", one_line(&format!("{e:#}")))),
    }
}

fn header(step: usize, title: &str) {
    println!();
    let rule = "─".repeat(width().saturating_sub(12 + title.chars().count()));
    say(&format!("{ACCENT}{step}/{STEPS}{RESET}  {BOLD}{title}{RESET} {FAINT}{rule}{RESET}"));
    println!();
}

fn agent_says(text: &str) {
    println!();
    let lines = wrap(text.trim(), width().saturating_sub(11));
    for (i, line) in lines.iter().enumerate() {
        say(&format!("{}{line}", if i == 0 { format!("{ACCENT}{BOLD}brief{RESET}  ") } else { "       ".into() }));
    }
}

/// A line typed by the user; None on end of input.
fn input() -> Option<String> {
    out(&format!("\n  {ACCENT}›{RESET} "));
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!();
            None
        }
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_owned()),
    }
}

enum State {
    On(String),
    Warn(String),
    Off(String),
}

struct Choice {
    label: String,
    hint: String,
    state: Option<State>,
}

impl Choice {
    fn new(label: &str, hint: &str) -> Choice {
        Choice { label: label.into(), hint: hint.into(), state: None }
    }

    fn state(mut self, s: State) -> Choice {
        self.state = Some(s);
        self
    }
}

/// Arrow-key menu. Returns the chosen index, or None on Esc / end of input.
fn select(question: &str, choices: &[Choice], start: usize) -> Option<usize> {
    let mut sel = start.min(choices.len().saturating_sub(1));
    println!();
    say(&format!("{ACCENT}?{RESET} {BOLD}{question}{RESET}"));
    let label_w = choices.iter().map(|c| c.label.chars().count()).max().unwrap_or(0) + 2;
    let state_w = choices.iter().filter_map(|c| c.state.as_ref()).map(|s| state_text(s).chars().count() + 4).max().unwrap_or(0);
    let render = |sel: usize| -> String {
        let mut s = String::new();
        for (i, c) in choices.iter().enumerate() {
            let on = i == sel;
            let pointer = if on { format!("{ACCENT}›{RESET} ") } else { "  ".into() };
            let label = if on { format!("{ACCENT}{BOLD}{:<label_w$}{RESET}", c.label) } else { format!("{:<label_w$}", c.label) };
            let state = match &c.state {
                Some(st) => {
                    let (dot, color) = match st {
                        State::On(_) => ("●", GOOD),
                        State::Warn(_) => ("○", ACCENT),
                        State::Off(_) => ("○", DIM),
                    };
                    let text = state_text(st);
                    let pad = state_w.saturating_sub(text.chars().count() + 2);
                    format!("{color}{dot} {text}{RESET}{}", " ".repeat(pad))
                }
                None => " ".repeat(state_w),
            };
            let room = width().saturating_sub(6 + label_w + state_w);
            let hint = truncate(&c.hint, room);
            s.push_str(&format!("\r\x1b[2K    {pointer}{label}{state}{DIM}{hint}{RESET}\r\n"));
        }
        s
    };
    let up = format!("\x1b[{}A", choices.len());
    let _raw = Raw::on();
    out(&render(sel));
    loop {
        let Ok(Event::Key(k)) = event::read() else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                drop(_raw);
                println!();
                std::process::exit(130);
            }
            KeyCode::Char('d') if k.modifiers.contains(KeyModifiers::CONTROL) => return None,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => sel = (sel + choices.len() - 1) % choices.len(),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => sel = (sel + 1) % choices.len(),
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < choices.len() => sel = c as usize - '1' as usize,
            KeyCode::Enter | KeyCode::Char(' ') => {
                // Collapse the menu into the answer.
                out(&format!("{up}\x1b[J    {GOOD}✔{RESET} {}\r\n", choices[sel].label));
                return Some(sel);
            }
            KeyCode::Esc => {
                out(&format!("{up}\x1b[J"));
                return None;
            }
            _ => continue,
        }
        out(&up);
        out(&render(sel));
    }
}

fn state_text(s: &State) -> &str {
    match s {
        State::On(t) | State::Warn(t) | State::Off(t) => t,
    }
}

/// Run a future on the runtime while a spinner turns.
fn wait<T: Send + 'static>(msg: &str, fut: impl std::future::Future<Output = T> + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    Handle::current().spawn(async move {
        let _ = tx.send(fut.await);
    });
    spin(msg, rx)
}

/// Run blocking work on a thread while a spinner turns.
fn wait_blocking<T: Send + 'static>(msg: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    spin(msg, rx)
}

fn spin<T>(msg: &str, rx: std::sync::mpsc::Receiver<T>) -> T {
    let start = Instant::now();
    let mut tick = 0;
    out("\x1b[?25l");
    loop {
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(v) => {
                out("\r\x1b[2K\x1b[?25h");
                return v;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let secs = start.elapsed().as_secs();
                let time = if secs >= 3 { format!(" {FAINT}{secs}s{RESET}") } else { String::new() };
                out(&format!("\r\x1b[2K  {ACCENT}{}{RESET} {DIM}{msg}…{RESET}{time}", SPINNER[tick % SPINNER.len()]));
                tick += 1;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                out("\x1b[?25h");
                panic!("background task stopped");
            }
        }
    }
}

/// Hand the terminal to another program (an installer, a sign-in). True if it succeeded.
fn run_interactive(bin: &std::path::Path, args: &[&str]) -> bool {
    println!();
    let ok = std::process::Command::new(bin).args(args).status().is_ok_and(|s| s.success());
    println!();
    ok
}

fn shell(cmd: &str) -> bool {
    say(&format!("{FAINT}$ {cmd}{RESET}"));
    run_interactive(std::path::Path::new("/bin/sh"), &["-c", cmd])
}

/// The briefs website, served from this process if the background service isn't running.
fn ensure_server(store: &Store) {
    let port = store.config().port;
    if crate::server_up(port) {
        return;
    }
    Handle::current().spawn(crate::server::serve(store.clone()));
    for _ in 0..30 {
        if crate::server_up(port) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn one_line(s: &str) -> String {
    truncate(&s.split_whitespace().collect::<Vec<_>>().join(" "), 400)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn strip_md(s: &str) -> String {
    s.replace("**", "").replace('`', "").replace("__", "")
}

/// Word-wrap plain text to `w` columns, keeping its line breaks. ANSI codes count as zero width.
fn wrap(text: &str, w: usize) -> Vec<String> {
    let visible = |s: &str| {
        let mut n = 0;
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, c) if c.is_ascii_alphabetic() => esc = false,
                (true, _) => {}
                _ => n += 1,
            }
        }
        n
    };
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && visible(&line) + 1 + visible(word) > w {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
}
