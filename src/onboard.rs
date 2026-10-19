//! `brief init` (and `brief` on first run): onboarding as one full-screen terminal app, in the
//! same look as the `brief` app. What brief is, the agent that keeps you accountable (and its
//! model), your objectives in a conversation with that agent, the agents and tools brief reads,
//! how it researches, and a first brief with your feedback on it. Only the briefs themselves live
//! on the web.
//!
//! Installers and sign-ins (`npm install`, `cline auth`, `composio login`…) need the real
//! terminal, so the app steps aside while they run and comes back when they're done.

use crate::agent::{self, Agent, Tools};
use crate::editor::Editor;
use crate::interview::{self, Message, Reply, Transcript};
use crate::store::{Brief, Feedback, Store};
use crate::tui::{ACCENT, BAD, BANNER, BANNER_COLORS, DIM, FAINT, GOOD, LINE, SPINNER};
use crate::{anki, browsing, calendar, connect, generate, granola, search, todoist, video};
use anyhow::Result;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Flex, Layout, Position, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Padding, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const SUGGEST_EMAIL: &str = "hi@manoletre.com";
const COMPOSIO_INSTALL: &str = "curl -fsSL https://composio.dev/install | sh";
/// Where Composio's installer puts its command, which a fresh shell may not have on PATH yet.
const COMPOSIO_PATH: &str = "export PATH=\"$HOME/.local/bin:$PATH\";";
const STAGES: [&str; 7] = ["Agent", "Objectives", "Agents", "Tools", "Research", "Mornings", "First brief"];
const AGENT_ROWS: [(&str, &str); 5] = [
    ("Claude Code", "Adds brief to Claude Code for all your projects."),
    ("Claude Desktop", "Installs brief as a Claude Desktop extension: Claude Desktop opens an install dialog, click Install."),
    ("Codex", "Adds brief to Codex's MCP servers (codex mcp add)."),
    ("Cline", "Adds brief to Cline's MCP servers, for the CLI and the editor extensions."),
    ("OpenCode", "Adds brief to OpenCode's config as a local MCP server."),
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Step {
    Intro,
    Agent,
    Model,
    Interview,
    Agents,
    Tools,
    ComposioApps,
    Research,
    ResearchKey,
    Videos,
    Background,
    Brief,
    Feedback,
    Done,
}

impl Step {
    /// Which of the stages in the header this step belongs to (0 for the intro, past the end when done).
    fn stage(self) -> usize {
        match self {
            Step::Intro => 0,
            Step::Agent | Step::Model => 1,
            Step::Interview => 2,
            Step::Agents => 3,
            Step::Tools | Step::ComposioApps => 4,
            Step::Research | Step::ResearchKey | Step::Videos => 5,
            Step::Background => 6,
            Step::Brief | Step::Feedback => 7,
            Step::Done => 8,
        }
    }
}

#[derive(PartialEq)]
enum Mode {
    Full,
    /// Just the objectives conversation (the app's Objectives → i).
    InterviewOnly,
    /// Just search and videos (`brief search setup`).
    ResearchOnly,
    /// Just the agent and its model (the app's Setup → Your agent).
    AgentOnly,
}

#[derive(Default, Clone, Copy)]
struct ToolStatus {
    calendar: bool,
    todoist: bool,
    granola: bool,
    browser_seen: bool,
    browser_installed: bool,
    anki: bool,
    composio: bool,
}

impl ToolStatus {
    fn any(&self) -> bool {
        self.calendar || self.todoist || self.granola || self.browser_seen || self.composio
    }
}

/// What a background job hands back.
enum Done {
    Agents(Vec<(bool, Option<String>)>),
    Hello(Result<(), String>),
    Turn(Result<Reply, String>),
    AgentRows(Vec<(bool, bool)>),
    Tools(ToolStatus),
    Connected(Result<String, String>),
    SearchTest(Result<String, String>),
    Brief(Result<Brief, String>),
}

#[derive(Clone, Copy, PartialEq)]
enum Job {
    Scan,
    Hello,
    Turn,
    Connect,
    Brief,
}

/// Commands that need the real terminal, and what to do once they're done.
struct Outside {
    title: String,
    cmds: Vec<Vec<String>>,
    after: After,
}

#[derive(Clone, Copy)]
enum After {
    Agents,
    ComposioApps,
    ComposioDone,
    Videos,
    Parallel,
}

struct App {
    store: Store,
    rt: tokio::runtime::Handle,
    mode: Mode,
    step: Step,
    sel: usize,
    tick: usize,
    toast: Option<(String, bool, Instant)>,
    job: Option<(Job, String, Instant, mpsc::Receiver<Done>)>,
    outside: Option<Outside>,
    quit: bool,
    error: Option<String>,
    // agent
    found: Vec<(bool, Option<String>)>,
    picked: Agent,
    // interview
    t: Transcript,
    last: Option<Reply>,
    input: Editor,
    scroll: u16,
    follow: bool,
    asking_existing: bool,
    proposing: bool,
    turn_failed: bool,
    /// A demo transcript being played back (`/name` in the interview), and its last suggestions.
    replay: std::collections::VecDeque<Message>,
    replay_at: Instant,
    replay_suggestions: Vec<String>,
    replay_objectives: Vec<interview::Objective>,
    // connections
    agent_rows: Vec<(bool, bool)>,
    tools: ToolStatus,
    apps: Editor,
    // research
    provider: search::Provider,
    key: Editor,
    // first brief
    brief: Option<Brief>,
    rating: usize,
    comment: Editor,
}

/// The whole onboarding.
pub fn run(store: Store) -> Result<()> {
    App::new(store, Mode::Full).run()
}

/// Just the objectives conversation.
pub fn interview_only(store: Store) -> Result<()> {
    App::new(store, Mode::InterviewOnly).run()
}

/// Just the search and video setup (`brief search setup`).
pub fn research_only(store: Store) -> Result<()> {
    App::new(store, Mode::ResearchOnly).run()
}

/// Just picking the agent and model.
pub fn agent_only(store: Store) -> Result<()> {
    App::new(store, Mode::AgentOnly).run()
}

impl App {
    fn new(store: Store, mode: Mode) -> App {
        let picked = agent::configured(&store).0;
        App {
            store,
            rt: tokio::runtime::Handle::current(),
            mode,
            step: Step::Intro,
            sel: 0,
            tick: 0,
            toast: None,
            job: None,
            outside: None,
            quit: false,
            error: None,
            found: Vec::new(),
            picked,
            t: Transcript::default(),
            last: None,
            input: Editor::new(""),
            scroll: 0,
            follow: true,
            asking_existing: false,
            proposing: false,
            turn_failed: false,
            replay: Default::default(),
            replay_at: Instant::now(),
            replay_suggestions: Vec::new(),
            replay_objectives: Vec::new(),
            agent_rows: Vec::new(),
            tools: ToolStatus::default(),
            apps: Editor::new(""),
            provider: search::Provider::Parallel,
            key: Editor::new(""),
            brief: None,
            rating: 0,
            comment: Editor::new(""),
        }
    }

    fn run(mut self) -> Result<()> {
        match self.mode {
            Mode::Full => {}
            Mode::InterviewOnly => self.enter(Step::Interview),
            Mode::ResearchOnly => self.enter(Step::Research),
            Mode::AgentOnly => self.enter(Step::Agent),
        }
        let mut terminal = enter_screen();
        let result = self.event_loop(&mut terminal);
        leave_screen();
        result
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            self.update();
            if let Some(o) = self.outside.take() {
                self.run_outside(terminal, o);
                continue;
            }
            if self.quit {
                return Ok(());
            }
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(80))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        self.on_key(k);
                    }
                }
            }
        }
    }

    // ── state ────────────────────────────────────────────────

    fn update(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        self.play_demo();
        let Some((_, _, _, rx)) = &self.job else { return };
        let Ok(done) = rx.try_recv() else { return };
        self.job = None;
        self.handle(done);
    }

    fn say(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error, Instant::now()));
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn working_on(&self, kind: Job) -> bool {
        self.job.as_ref().is_some_and(|(k, ..)| *k == kind)
    }

    fn spawn_blocking(&mut self, kind: Job, label: &str, f: impl FnOnce() -> Done + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        self.job = Some((kind, label.into(), Instant::now(), rx));
    }

    fn spawn(&mut self, kind: Job, label: &str, fut: impl std::future::Future<Output = Done> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.rt.spawn(async move {
            let _ = tx.send(fut.await);
        });
        self.job = Some((kind, label.into(), Instant::now(), rx));
    }

    fn enter(&mut self, step: Step) {
        self.step = step;
        self.sel = 0;
        self.error = None;
        match step {
            Step::Agent => self.scan_agents(),
            Step::Interview => {
                self.t = Transcript::default();
                self.last = None;
                self.proposing = false;
                self.turn_failed = false;
                self.follow = true;
                self.input = Editor::new("");
                self.asking_existing = !self.store.objective_list().is_empty();
                self.sel = if self.asking_existing && self.mode == Mode::Full { 1 } else { 0 };
                if !self.asking_existing {
                    self.start_turn();
                }
            }
            Step::Agents => self.scan_agent_rows(),
            Step::Tools => self.scan_tools(),
            Step::ComposioApps => self.apps = Editor::new(""),
            Step::Research => {
                let active = search::active(&self.store);
                self.sel = if active == search::Provider::Claude && self.mode == Mode::Full { 0 } else { search::ALL.len() };
            }
            Step::ResearchKey => self.key = Editor::new(""),
            Step::Brief if self.brief.is_none() => self.start_brief(),
            Step::Brief => self.sel = 1,
            Step::Feedback => {
                self.rating = 0;
                self.comment = Editor::new("");
            }
            Step::Done => {
                let mut cfg = self.store.config();
                cfg.onboarded = true;
                if let Err(e) = self.store.save_config(&cfg) {
                    self.say(format!("Couldn't save: {e}"), true);
                }
            }
            Step::Intro | Step::Model | Step::Videos | Step::Background => {}
        }
    }

    fn scan_agents(&mut self) {
        self.spawn_blocking(Job::Scan, "Looking for agents on this Mac", || Done::Agents(agent::ALL.map(|a| (a.find().is_some(), a.account())).to_vec()));
    }

    fn scan_agent_rows(&mut self) {
        self.spawn_blocking(Job::Scan, "Checking your agents", || {
            Done::AgentRows(vec![
                (connect::claude_code_installed(), connect::claude_code_connected()),
                (connect::claude_desktop_installed(), connect::claude_desktop_extension() || connect::claude_desktop_config_entry()),
                (Agent::Codex.find().is_some(), connect::codex_connected()),
                (Agent::Cline.find().is_some(), connect::cline_connected()),
                (Agent::OpenCode.find().is_some(), connect::opencode_connected()),
            ])
        });
    }

    fn scan_tools(&mut self) {
        let s = self.store.clone();
        self.spawn_blocking(Job::Scan, "Checking your tools", move || {
            Done::Tools(ToolStatus {
                calendar: calendar::connected(&s),
                todoist: todoist::auth(&s).is_some(),
                granola: granola::auth(&s).is_some(),
                browser_seen: browsing::last_seen(&s).is_some(),
                browser_installed: browsing::installed(&s),
                anki: anki::available(false),
                composio: s.config().composio,
            })
        });
    }

    fn start_turn(&mut self) {
        let (s, t) = (self.store.clone(), self.t.clone());
        self.turn_failed = false;
        self.follow = true;
        let name = self.picked.name();
        self.spawn(Job::Turn, &format!("{name} is thinking"), async move { Done::Turn(interview::turn(&s, &t).await.map_err(|e| one_line(&format!("{e:#}")))) });
    }

    fn start_brief(&mut self) {
        let s = self.store.clone();
        self.spawn(Job::Brief, "Writing your first brief", async move { Done::Brief(generate::generate(&s).await.map_err(|e| one_line(&format!("{e:#}")))) });
    }

    fn test_search(&mut self, key: Option<String>) {
        let p = self.provider;
        if let Err(e) = search::choose(&self.store, p, key.as_deref()) {
            self.error = Some(format!("Couldn't save: {e}"));
            return;
        }
        let s = self.store.clone();
        self.spawn_blocking(Job::Connect, &format!("Searching with {}", p.name()), move || Done::SearchTest(search::test(&s).map_err(|e| one_line(&format!("{e:#}")))));
    }

    fn handle(&mut self, done: Done) {
        match done {
            Done::Agents(found) => {
                let current = agent::ALL.iter().position(|a| *a == self.picked);
                self.sel = current.filter(|&i| found[i].1.is_some()).or_else(|| found.iter().position(|(_, acc)| acc.is_some())).unwrap_or(0);
                self.found = found;
            }
            Done::Hello(Ok(())) => {
                let cfg = self.store.config();
                self.say(format!("{} with {} will keep you accountable.", self.picked.name(), self.picked.model_label(&cfg.model)), false);
                if self.mode == Mode::AgentOnly {
                    self.quit = true;
                } else {
                    self.enter(Step::Interview);
                }
            }
            Done::Hello(Err(e)) => self.error = Some(format!("{} didn't answer: {e}", self.picked.name())),
            Done::Turn(Ok(reply)) => {
                self.t.messages.push(Message { role: "assistant".into(), text: reply.message.clone() });
                self.proposing = !reply.objectives.is_empty();
                self.sel = 0;
                self.last = Some(reply);
            }
            Done::Turn(Err(e)) => {
                self.turn_failed = true;
                self.error = Some(e);
            }
            Done::AgentRows(rows) => {
                if self.step == Step::Agents && self.agent_rows.is_empty() {
                    self.sel = rows.iter().position(|(installed, on)| *installed && !*on).unwrap_or(rows.len());
                }
                self.agent_rows = rows;
            }
            Done::Tools(t) => self.tools = t,
            Done::Connected(r) => {
                match r {
                    Ok(msg) => self.say(msg.lines().next().unwrap_or_default().trim().to_owned(), false),
                    Err(e) => self.say(one_line(&e), true),
                }
                match self.step {
                    Step::Agents => self.scan_agent_rows(),
                    _ => self.scan_tools(),
                }
            }
            Done::SearchTest(Ok(msg)) => {
                self.say(msg, false);
                self.enter(Step::Research);
                self.sel = search::ALL.len();
            }
            Done::SearchTest(Err(e)) => {
                // Don't leave the brief on a provider that doesn't work.
                let mut st = search::settings(&self.store);
                st.keys.remove(self.provider.id());
                st.provider = search::Provider::Claude.id().into();
                let _ = search::save(&self.store, &st);
                self.error = Some(format!("{e}. Check the key and try again."));
            }
            Done::Brief(Ok(b)) => {
                self.brief = Some(b);
                self.sel = 0;
            }
            Done::Brief(Err(e)) => {
                self.error = Some(e);
                self.sel = 0;
            }
        }
    }

    // ── keys ─────────────────────────────────────────────────

    fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match self.step {
            Step::Intro => match k.code {
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right => self.enter(Step::Agent),
                KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
            Step::Agent => self.agent_key(k),
            Step::Model => self.model_key(k),
            Step::Interview => self.interview_key(k),
            Step::Agents => self.agents_key(k),
            Step::Tools => self.tools_key(k),
            Step::ComposioApps => match k.code {
                KeyCode::Esc => self.enter(Step::Tools),
                KeyCode::Enter => self.link_apps(),
                _ => edit(&mut self.apps, k),
            },
            Step::Research => self.research_key(k),
            Step::ResearchKey => self.research_key_key(k),
            Step::Videos => self.videos_key(k),
            Step::Background => self.background_key(k),
            Step::Brief => self.brief_key(k),
            Step::Feedback => self.feedback_key(k),
            Step::Done => match k.code {
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
        }
    }

    /// ↑↓ over `n` rows; returns true when the key was a move.
    fn move_sel(&mut self, k: &KeyEvent, n: usize) -> bool {
        match k.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.sel = (self.sel + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.sel = (self.sel + 1) % n,
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => self.sel = c as usize - '1' as usize,
            _ => return false,
        }
        self.error = None;
        true
    }

    fn agent_key(&mut self, k: KeyEvent) {
        if self.move_sel(&k, agent::ALL.len() + 1) {
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Left if self.mode == Mode::AgentOnly => self.quit = true,
            KeyCode::Esc | KeyCode::Left => self.enter(Step::Intro),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') if !self.busy() => {
                if self.sel == agent::ALL.len() {
                    suggest_agent();
                    self.say(format!("Opened an email to {SUGGEST_EMAIL}. Thanks! Pick one of these to start with meanwhile."), false);
                    return;
                }
                let a = agent::ALL[self.sel];
                let Some((installed, account)) = self.found.get(self.sel).cloned() else { return };
                if !installed {
                    self.outside = Some(Outside { title: format!("Installing {}", a.name()), cmds: vec![sh(a.install_cmd())], after: After::Agents });
                } else if account.is_none() {
                    let mut cmd = vec![a.find().unwrap().display().to_string()];
                    cmd.extend(a.login_args().iter().map(|s| s.to_string()));
                    self.outside = Some(Outside { title: format!("Signing in to {}", a.name()), cmds: vec![cmd], after: After::Agents });
                } else {
                    self.picked = a;
                    let cfg = self.store.config();
                    self.step = Step::Model;
                    self.error = None;
                    self.sel = if cfg.agent == a.id() { a.models().iter().position(|m| m.id == cfg.model).unwrap_or(0) } else { 0 };
                }
            }
            _ => {}
        }
    }

    fn model_key(&mut self, k: KeyEvent) {
        let models = self.picked.models();
        if !self.busy() && self.move_sel(&k, models.len()) {
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Left if !self.busy() => self.enter(Step::Agent),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') if !self.busy() => {
                let mut cfg = self.store.config();
                cfg.agent = self.picked.id().into();
                cfg.model = models[self.sel].id.into();
                if let Err(e) = self.store.save_config(&cfg) {
                    self.error = Some(format!("Couldn't save: {e}"));
                    return;
                }
                let (a, s, model) = (self.picked, self.store.clone(), cfg.model);
                self.spawn(Job::Hello, &format!("Saying hello to {}", a.name()), async move {
                    let r = a.ask(&s, &model, "Reply with exactly the word: ready", Tools::None, Duration::from_secs(180)).await;
                    Done::Hello(r.map(|_| ()).map_err(|e| one_line(&format!("{e:#}"))))
                });
            }
            _ => {}
        }
    }

    fn interview_key(&mut self, k: KeyEvent) {
        if self.asking_existing {
            if self.move_sel(&k, 2) {
                return;
            }
            match k.code {
                KeyCode::Enter | KeyCode::Char(' ') if self.sel == 0 => {
                    self.asking_existing = false;
                    self.start_turn();
                }
                KeyCode::Enter | KeyCode::Char(' ') => self.after_interview(),
                KeyCode::Esc => self.back_from_interview(),
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Up => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(1);
                return;
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
                return;
            }
            KeyCode::PageUp => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(10);
                return;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(10);
                return;
            }
            KeyCode::Esc => {
                if self.mode == Mode::Full {
                    self.say("Skipped. Set your objectives any time from `brief` → Objectives.", false);
                }
                self.after_interview();
                return;
            }
            _ => {}
        }
        if self.proposing {
            match k.code {
                KeyCode::Left | KeyCode::BackTab => self.sel = (self.sel + 2) % 3,
                KeyCode::Right | KeyCode::Tab => self.sel = (self.sel + 1) % 3,
                KeyCode::Enter | KeyCode::Char(' ') => match self.sel {
                    0 => self.save_objectives(),
                    1 => {
                        self.proposing = false;
                        self.follow = true;
                    }
                    _ => self.after_interview(),
                },
                _ => {}
            }
            return;
        }
        if self.busy() || !self.replay.is_empty() {
            return;
        }
        if k.code != KeyCode::Enter {
            edit(&mut self.input, k);
            return;
        }
        let text = self.input.text();
        if text.is_empty() {
            if self.turn_failed {
                self.error = None;
                self.start_turn();
            }
            return;
        }
        if let Some(name) = text.strip_prefix('/') {
            self.input = Editor::new("");
            self.load_demo(name.trim());
            return;
        }
        let suggestions = self.last.as_ref().map(|r| r.suggestions.clone()).unwrap_or_default();
        let answer = match text.parse::<usize>() {
            Ok(n) if (1..=suggestions.len()).contains(&n) => suggestions[n - 1].clone(),
            _ => text,
        };
        self.t.messages.push(Message { role: "user".into(), text: answer });
        self.input = Editor::new("");
        self.error = None;
        self.start_turn();
    }

    /// `/name` in the interview plays back a saved conversation from `demos/name.json` (in the
    /// source checkout or in ~/.brief), then the agent carries on from its last question. For
    /// demos: the files stay local (the checkout's demos/ is excluded from git).
    fn load_demo(&mut self, name: &str) {
        #[derive(serde::Deserialize)]
        struct Demo {
            messages: Vec<Message>,
            #[serde(default)]
            suggestions: Vec<String>,
            /// When set, the demo ends on these proposed objectives, ready to save.
            #[serde(default)]
            objectives: Vec<interview::Objective>,
        }
        let file = format!("{name}.json");
        let found = [std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("demos").join(&file), self.store.root.join("demos").join(&file)]
            .into_iter()
            .find_map(|p| std::fs::read_to_string(p).ok());
        let Some(demo) = found.and_then(|t| serde_json::from_str::<Demo>(&t).ok()) else {
            self.say(format!("No demo called /{name}."), true);
            return;
        };
        self.t = Transcript::default();
        self.last = None;
        self.proposing = false;
        self.turn_failed = false;
        self.error = None;
        self.follow = true;
        self.replay = demo.messages.into();
        self.replay_suggestions = demo.suggestions;
        self.replay_objectives = demo.objectives;
        self.replay_at = Instant::now();
    }

    /// One message of a demo at a time, quick enough to watch it scroll by.
    fn play_demo(&mut self) {
        if self.replay.is_empty() || self.replay_at.elapsed() < Duration::from_millis(250) {
            return;
        }
        let m = self.replay.pop_front().unwrap();
        self.replay_at = Instant::now();
        self.follow = true;
        if self.replay.is_empty() && m.role == "assistant" {
            let objectives = std::mem::take(&mut self.replay_objectives);
            self.proposing = !objectives.is_empty();
            self.sel = 0;
            self.last = Some(Reply { message: m.text.clone(), suggestions: std::mem::take(&mut self.replay_suggestions), objectives });
        }
        self.t.messages.push(m);
    }

    fn save_objectives(&mut self) {
        let Some(reply) = &self.last else { return };
        let items: Vec<String> = reply.objectives.iter().filter(|o| !o.title.trim().is_empty()).enumerate().map(|(i, o)| o.to_item(i + 1)).collect();
        match self.store.save_objective_list(&items) {
            Ok(()) => {
                self.say(format!("Saved {} objectives. Every brief is written against them.", items.len()), false);
                self.after_interview();
            }
            Err(e) => self.say(format!("Couldn't save: {e}"), true),
        }
    }

    fn after_interview(&mut self) {
        self.job = None;
        match self.mode {
            Mode::Full => self.enter(Step::Agents),
            _ => self.quit = true,
        }
    }

    fn back_from_interview(&mut self) {
        match self.mode {
            Mode::Full => self.enter(Step::Agent),
            _ => self.quit = true,
        }
    }

    fn agents_key(&mut self, k: KeyEvent) {
        let n = AGENT_ROWS.len() + 1;
        if self.move_sel(&k, n) {
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Left => self.enter(Step::Interview),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') if !self.busy() => {
                if self.sel == AGENT_ROWS.len() {
                    return self.enter(Step::Tools);
                }
                let Some(&(installed, on)) = self.agent_rows.get(self.sel) else { return };
                let name = AGENT_ROWS[self.sel].0;
                if on {
                    return self.say(format!("{name} is already connected."), false);
                }
                if !installed {
                    return self.say(format!("{name} isn't installed on this Mac."), true);
                }
                let (i, s) = (self.sel, self.store.clone());
                self.spawn_blocking(Job::Connect, &format!("Connecting {name}"), move || {
                    let r = match i {
                        0 => connect::set_claude_code(true),
                        1 => connect::install_claude_desktop(&s),
                        2 => connect::set_codex(true),
                        3 => connect::set_cline(true),
                        _ => connect::set_opencode(true),
                    };
                    Done::Connected(r.map_err(|e| format!("{e:#}")))
                });
            }
            _ => {}
        }
    }

    fn tools_key(&mut self, k: KeyEvent) {
        if self.move_sel(&k, 7) {
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Left => self.enter(Step::Agents),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') if !self.busy() => {
                let t = self.tools;
                let s = self.store.clone();
                let connected = |name: &str| format!("{name} is already connected. Disconnect it from `brief` → Connect.");
                match self.sel {
                    0 if t.calendar => self.say(connected("Calendar"), false),
                    0 => {
                        self.say("Building the Calendar helper, then macOS asks whether brief may see your calendar: click Allow.", false);
                        self.spawn_blocking(Job::Connect, "Connecting your calendar", move || Done::Connected(calendar::connect(&s).map_err(|e| format!("{e:#}"))));
                    }
                    1 if t.todoist => self.say(connected("Todoist"), false),
                    1 => {
                        self.say("Todoist opens in your browser: click Agree there.", false);
                        self.spawn_blocking(Job::Connect, "Waiting for Todoist", move || Done::Connected(todoist::connect(&s).map_err(|e| format!("{e:#}"))));
                    }
                    2 if t.granola => self.say(connected("Granola"), false),
                    2 => {
                        self.say("Granola opens in your browser: sign in and allow brief there.", false);
                        self.spawn_blocking(Job::Connect, "Waiting for Granola", move || Done::Connected(granola::connect(&s).map_err(|e| format!("{e:#}"))));
                    }
                    3 if t.browser_seen => self.say(connected("The browser extension"), false),
                    3 => {
                        self.say(format!("{}'s extensions page opens and the folder is copied: Developer mode, Load unpacked, ⌘⇧G, paste, ⏎.", browsing::browser_name()), false);
                        self.spawn_blocking(Job::Connect, "Installing the extension", move || Done::Connected(browsing::install(&s).map_err(|e| format!("{e:#}"))));
                    }
                    4 if t.anki => self.say(connected("Anki"), false),
                    4 => self.spawn_blocking(Job::Connect, "Opening Anki", || {
                        Done::Connected(if anki::available(true) {
                            Ok("Anki is connected.".into())
                        } else {
                            Err("Anki needs the AnkiConnect add-on: in Anki, Tools → Add-ons → Get Add-ons…, paste 2055492159, restart Anki.".into())
                        })
                    }),
                    5 if t.composio => self.say(connected("Composio"), false),
                    5 => {
                        let mut cmds = Vec::new();
                        if agent::find_bin("composio").is_none() {
                            cmds.push(sh(COMPOSIO_INSTALL));
                        }
                        cmds.push(sh(&format!("{COMPOSIO_PATH} composio login")));
                        self.outside = Some(Outside { title: "Signing in to Composio (a browser page opens)".into(), cmds, after: After::ComposioApps });
                    }
                    _ => self.enter(Step::Research),
                }
            }
            _ => {}
        }
    }

    fn link_apps(&mut self) {
        let names = self.apps.text();
        let apps: Vec<String> = names.split([' ', ',']).map(shell_word).filter(|a| !a.is_empty()).collect();
        if apps.is_empty() {
            return self.finish_composio();
        }
        let cmds = apps.iter().map(|a| sh(&format!("{COMPOSIO_PATH} composio link {a}"))).collect();
        self.outside = Some(Outside { title: format!("Linking {} in Composio", apps.join(", ")), cmds, after: After::ComposioDone });
    }

    fn finish_composio(&mut self) {
        let mut cfg = self.store.config();
        cfg.composio = true;
        match self.store.save_config(&cfg) {
            Ok(()) => self.say("Composio is connected. Your brief reads those apps each morning (read-only).", false),
            Err(e) => self.say(format!("Couldn't save: {e}"), true),
        }
        self.enter(Step::Tools);
    }

    fn research_key(&mut self, k: KeyEvent) {
        if !self.busy() && self.move_sel(&k, search::ALL.len() + 1) {
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Left if !self.busy() => match self.mode {
                Mode::Full => self.enter(Step::Tools),
                _ => self.quit = true,
            },
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') if !self.busy() => {
                let Some(&p) = search::ALL.get(self.sel) else { return self.enter(Step::Videos) };
                self.provider = p;
                if p == search::Provider::Parallel && !search::ready(&self.store, p) {
                    // One ⏎: install Parallel's CLI if needed and sign in through the browser.
                    let mut cmds = Vec::new();
                    if search::parallel_cli().is_none() {
                        cmds.push(sh(search::PARALLEL_CLI_INSTALL));
                    }
                    cmds.push(sh("export PATH=\"$HOME/.local/bin:$PATH\"; parallel-cli login"));
                    self.error = None;
                    self.outside = Some(Outside { title: "Signing in to Parallel (a browser page opens: approve brief there)".into(), cmds, after: After::Parallel });
                } else if p.key_url().is_none() {
                    if let Err(e) = search::choose(&self.store, p, None) {
                        return self.say(format!("Couldn't save: {e}"), true);
                    }
                    self.say("The brief will use your agent's own web search.", false);
                    self.sel = search::ALL.len();
                } else if search::key(&self.store, p).is_some() {
                    self.test_search(None);
                } else {
                    self.enter(Step::ResearchKey);
                }
            }
            KeyCode::Char('k') if !self.busy() && search::ALL.get(self.sel).is_some_and(|p| p.key_url().is_some()) => {
                self.provider = search::ALL[self.sel];
                self.enter(Step::ResearchKey);
            }
            _ => {}
        }
    }

    fn research_key_key(&mut self, k: KeyEvent) {
        if self.busy() {
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => {
                self.enter(Step::Research);
                self.sel = search::ALL.iter().position(|p| *p == self.provider).unwrap_or(0);
            }
            KeyCode::Char('o') if ctrl => {
                if let Some(url) = self.provider.key_url() {
                    connect::open(url);
                }
            }
            KeyCode::Enter if !self.key.is_blank() => {
                self.error = None;
                self.test_search(Some(self.key.text()));
            }
            _ => edit(&mut self.key, k),
        }
    }

    fn videos_key(&mut self, k: KeyEvent) {
        let installed = video::manimgx().is_some();
        if !installed && self.move_sel(&k, 2) {
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Left => self.enter(Step::Research),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                if !installed && self.sel == 0 {
                    self.outside = Some(Outside { title: "Installing ManimGX".into(), cmds: vec![sh(video::INSTALL)], after: After::Videos });
                } else if self.mode == Mode::Full {
                    self.enter(Step::Background);
                } else {
                    self.quit = true;
                }
            }
            _ => {}
        }
    }

    fn background_key(&mut self, k: KeyEvent) {
        let installed = service_installed();
        if !installed && self.move_sel(&k, 2) {
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Left => self.enter(Step::Videos),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                if !installed && self.sel == 0 {
                    match crate::install(&self.store) {
                        Ok(msg) => self.say(msg, false),
                        Err(e) => return self.say(format!("{e:#}"), true),
                    }
                }
                self.enter(Step::Brief);
            }
            _ => {}
        }
    }

    fn brief_key(&mut self, k: KeyEvent) {
        if self.working_on(Job::Brief) {
            if k.code == KeyCode::Esc {
                self.job = None;
                self.enter(Step::Background);
            }
            return;
        }
        if self.move_sel(&k, 2) {
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Left => self.enter(Step::Background),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => match (&self.brief, self.sel) {
                (Some(b), 0) => {
                    let date = b.date.unwrap_or_else(|| chrono::Local::now().date_naive());
                    let url = format!("http://127.0.0.1:{}/brief/{date}", self.store.config().port);
                    ensure_server(&self.store, &self.rt);
                    connect::open(&url);
                    self.say("Opened in your browser. Rate each card there, then come back here.", false);
                    self.sel = 1;
                }
                (Some(_), _) => self.enter(Step::Feedback),
                (None, 0) => {
                    self.error = None;
                    self.start_brief();
                }
                (None, _) => self.enter(Step::Done),
            },
            _ => {}
        }
    }

    fn feedback_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Up => self.rating = (self.rating + 2) % 3,
            KeyCode::Down | KeyCode::Tab => self.rating = (self.rating + 1) % 3,
            KeyCode::Esc => self.enter(Step::Brief),
            KeyCode::Enter => {
                let rating = ["up", "", "down"][self.rating];
                let mut comment = self.comment.text();
                if self.rating == 1 {
                    comment = format!("First brief: useful, but some misses. {comment}").trim().to_owned();
                }
                let date = self.brief.as_ref().and_then(|b| b.date).unwrap_or_else(|| chrono::Local::now().date_naive());
                let f = Feedback { ts: chrono::Local::now(), brief_date: date, item_id: String::new(), item_title: String::new(), rating: rating.into(), comment };
                match self.store.add_feedback(&f) {
                    Ok(()) => {
                        self.say("Thanks. Tomorrow's brief takes this into account.", false);
                        self.enter(Step::Done);
                    }
                    Err(e) => self.say(format!("Couldn't save: {e}"), true),
                }
            }
            _ => edit(&mut self.comment, k),
        }
    }

    // ── handing the terminal to installers and sign-ins ──────

    fn run_outside(&mut self, terminal: &mut DefaultTerminal, o: Outside) {
        leave_screen();
        let (accent, dim, bad, reset) = ("\x1b[38;5;208m", "\x1b[38;5;244m", "\x1b[38;5;203m", "\x1b[0m");
        println!("\n  {accent}brief{reset} › {}\n", o.title);
        let mut ok = true;
        for cmd in &o.cmds {
            println!("  {dim}$ {}{reset}\n", cmd.last().unwrap());
            let status = std::process::Command::new(&cmd[0]).args(&cmd[1..]).status();
            if !status.is_ok_and(|s| s.success()) {
                ok = false;
                println!("\n  {bad}That didn't finish.{reset}");
                break;
            }
        }
        print!("\n  {dim}Press ⏎ to go back to brief{reset} ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        *terminal = enter_screen();
        let _ = terminal.clear();
        match o.after {
            After::Agents => self.scan_agents(),
            After::ComposioApps if ok => self.enter(Step::ComposioApps),
            After::ComposioApps => {
                self.say("Composio isn't signed in yet. Pick it again to retry.", true);
                self.scan_tools();
            }
            After::ComposioDone => self.finish_composio(),
            After::Videos if video::manimgx().is_some() => {
                self.say("ManimGX is installed: your briefs will have videos.", false);
                self.sel = 0;
            }
            After::Videos => self.say("ManimGX still isn't there. Install it later; the brief picks it up.", true),
            After::Parallel => {
                search::recheck_parallel();
                if search::parallel_signed_in() {
                    self.provider = search::Provider::Parallel;
                    self.test_search(None);
                } else {
                    self.error = Some("Parallel isn't signed in yet. ⏎ to try again, or k to paste an API key instead.".into());
                }
            }
        }
    }

    // ── drawing ──────────────────────────────────────────────

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let width = area.width.saturating_sub(4).min(80);
        let [col] = Layout::horizontal([Constraint::Length(width)]).flex(Flex::Center).areas(area);
        let [header, body, toast, footer] =
            Layout::vertical([Constraint::Length(3), Constraint::Fill(1), Constraint::Length(1), Constraint::Length(1)]).areas(col.inner(ratatui::layout::Margin::new(0, 1)));

        if self.step != Step::Intro {
            self.draw_header(f, header);
        }
        let body = if self.step == Step::Intro { header.union(body) } else { body };
        match self.step {
            Step::Intro => self.draw_intro(f, body),
            Step::Agent => self.draw_agent(f, body),
            Step::Model => self.draw_model(f, body),
            Step::Interview => self.draw_interview(f, body),
            Step::Agents => self.draw_agents(f, body),
            Step::Tools => self.draw_tools(f, body),
            Step::ComposioApps => self.draw_composio(f, body),
            Step::Research => self.draw_research(f, body),
            Step::ResearchKey => self.draw_research_key(f, body),
            Step::Videos => self.draw_videos(f, body),
            Step::Background => self.draw_background(f, body),
            Step::Brief => self.draw_brief(f, body),
            Step::Feedback => self.draw_feedback(f, body),
            Step::Done => self.draw_done(f, body),
        }

        if let Some((label, started)) = self.job.as_ref().filter(|(k, ..)| !matches!(k, Job::Turn | Job::Brief)).map(|(_, l, s, _)| (l.clone(), *s)) {
            f.render_widget(Paragraph::new(Line::from(vec![Span::styled(format!("{} ", self.spinner()), Style::default().fg(ACCENT)), Span::styled(format!("{label}…{}", elapsed(started)), Style::default().fg(DIM))])), toast);
        } else if let Some((msg, err, at)) = &self.toast {
            if at.elapsed() < Duration::from_secs(7) {
                f.render_widget(Paragraph::new(Line::styled(msg.as_str(), Style::default().fg(if *err { BAD } else { GOOD }))), toast);
            }
        }

        let keys: &[(&str, &str)] = match self.step {
            Step::Intro => &[("⏎", "start"), ("q", "quit")],
            Step::Interview if self.asking_existing => &[("↑↓", "choose"), ("⏎", "select"), ("esc", "back")],
            Step::Interview if self.proposing => &[("←→", "choose"), ("⏎", "select"), ("↑↓", "scroll"), ("esc", "skip")],
            Step::Interview => &[("⏎", "send"), ("1-4", "pick a suggestion"), ("↑↓", "scroll"), ("esc", "skip")],
            Step::ComposioApps => &[("⏎", "link them"), ("esc", "back")],
            Step::ResearchKey => &[("⏎", "save and test"), ("ctrl-o", "open the key page"), ("esc", "back")],
            Step::Feedback => &[("↑↓", "rate"), ("type", "a comment"), ("⏎", "save"), ("esc", "back")],
            Step::Brief if self.working_on(Job::Brief) => &[("esc", "back")],
            Step::Done => &[("⏎", "finish")],
            _ => &[("↑↓", "move"), ("⏎", "select"), ("esc", "back"), ("ctrl-c", "quit")],
        };
        let mut spans = Vec::new();
        for (key, what) in keys {
            spans.push(Span::styled(*key, Style::default().fg(ACCENT)));
            spans.push(Span::styled(format!(" {what}   "), Style::default().fg(DIM)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), footer);
    }

    fn spinner(&self) -> &'static str {
        SPINNER[self.tick % SPINNER.len()]
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let stage = self.step.stage();
        let title = match self.mode {
            Mode::Full => "setup",
            Mode::InterviewOnly => "objectives",
            Mode::ResearchOnly => "research",
            Mode::AgentOnly => "your agent",
        };
        let mut lines = vec![Line::from(vec![
            Span::styled("brief ", Style::default().fg(ACCENT).bold()),
            Span::styled("› ", Style::default().fg(FAINT)),
            Span::styled(title, Style::default().fg(DIM)),
        ])];
        if self.mode == Mode::Full {
            let full: usize = STAGES.iter().map(|s| s.chars().count() + 4).sum::<usize>() - 2;
            let mut spans = Vec::new();
            for (i, name) in STAGES.iter().enumerate() {
                let n = i + 1;
                let (dot, style) = if n < stage {
                    ("●", Style::default().fg(GOOD))
                } else if n == stage {
                    ("●", Style::default().fg(ACCENT).bold())
                } else {
                    ("○", Style::default().fg(FAINT))
                };
                if i > 0 {
                    spans.push(Span::styled("  ", Style::default().fg(LINE)));
                }
                spans.push(Span::styled(format!("{dot} "), style));
                // Only the current stage's name when the screen is narrow.
                if full <= area.width as usize || n == stage {
                    spans.push(Span::styled(*name, if n == stage { style } else { Style::default().fg(if n < stage { DIM } else { FAINT }) }));
                }
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), area);
    }

    fn draw_intro(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = vec![Line::raw("")];
        if area.height >= 26 {
            for (row, color) in BANNER.iter().zip(BANNER_COLORS) {
                lines.push(Line::styled(*row, Style::default().fg(Color::Indexed(color))));
            }
            lines.push(Line::raw(""));
        } else {
            lines.push(Line::styled("brief", Style::default().fg(ACCENT).bold()));
        }
        lines.push(Line::styled("brief is a shared memory that keeps you accountable.", Style::default().bold()));
        lines.push(Line::raw(""));
        lines.push(Line::raw(
            "The AI agents you work with every day (Claude Code, Codex, Cline, ChatGPT…) write down what you work on. Every morning brief reads that, with your calendar, to-dos and meetings, holds it up against your goals and tells you, like a candid friend, what to do more of, less of, or stop.",
        ));
        lines.push(Line::raw(""));
        let steps = [
            "Pick the agent that keeps you accountable",
            "Set your goals with it",
            "Connect the agents you work in",
            "Connect your tools",
            "Choose how it researches",
            "Let it run every morning",
            "Read your first brief",
        ];
        for (i, name) in steps.iter().enumerate() {
            lines.push(Line::from(vec![Span::styled(format!("  {}  ", i + 1), Style::default().fg(ACCENT)), Span::raw(*name)]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled("About 15 minutes. Everything stays on this Mac.", Style::default().fg(DIM)));
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![Span::styled(" › ", Style::default().fg(ACCENT)), Span::styled("Let's start", Style::default().fg(ACCENT).bold()), Span::styled("  ⏎", Style::default().fg(FAINT))]));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn draw_agent(&self, f: &mut Frame, area: Rect) {
        let [title, rows_area, _, detail] = Layout::vertical([Constraint::Length(4), Constraint::Length(11), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(
            intro("Your accountability agent", "brief works through a coding agent you already use, on your own account. It runs your goals conversation now and writes your brief every morning."),
            title,
        );
        let scanning = self.found.is_empty();
        let mut rows: Vec<Row> = agent::ALL
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let state = match self.found.get(i) {
                    None => State::Off(String::new()),
                    Some((false, _)) => State::Off("not installed".into()),
                    Some((true, None)) => State::Warn("not signed in".into()),
                    Some((true, Some(who))) if who.is_empty() => State::On("signed in".into()),
                    Some((true, Some(who))) => State::On(format!("signed in · {who}")),
                };
                Row { name: a.name().into(), state: Some(state), action: "" }
            })
            .collect();
        rows.push(Row { name: "Suggest one".into(), state: None, action: "" });
        f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 20, self.spinner(), scanning)), rows_area);

        let dim = Style::default().fg(DIM);
        let text: Vec<Line> = if self.sel == agent::ALL.len() {
            vec![Line::raw("Use something else? Tell us which agent you'd like brief to work with."), Line::raw(""), Line::styled(format!("⏎ opens an email to {SUGGEST_EMAIL} in your mail app."), dim)]
        } else {
            let a = agent::ALL[self.sel];
            let mut t = vec![Line::raw(a.blurb().to_owned()), Line::raw("")];
            t.push(match self.found.get(self.sel) {
                None => Line::styled("Checking…", dim),
                Some((false, _)) => Line::from(vec![Span::styled("Not installed. ⏎ installs it: ", dim), Span::styled(a.install_cmd(), Style::default().fg(ACCENT))]),
                Some((true, None)) => Line::from(vec![Span::styled("Not signed in. ⏎ signs you in: ", dim), Span::styled(format!("{} {}", a.id(), a.login_args().join(" ")), Style::default().fg(ACCENT))]),
                Some((true, Some(_))) => Line::styled("Ready. ⏎ to choose its model.", Style::default().fg(GOOD)),
            });
            t
        };
        boxed(f, detail_box(text), detail);
    }

    fn draw_model(&self, f: &mut Frame, area: Rect) {
        let a = self.picked;
        let models = a.models();
        let [title, rows_area, _, detail] =
            Layout::vertical([Constraint::Length(4), Constraint::Length(models.len() as u16 * 2 + 1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(intro(&format!("Which model should {} use?", a.name()), "It holds your goals conversation now and writes a brief every morning, so judgement matters more than speed."), title);
        let rows: Vec<Row> = models
            .iter()
            .enumerate()
            .map(|(i, m)| Row { name: m.label.into(), state: (i == 0).then(|| State::On("recommended".into())), action: "" })
            .collect();
        f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 26, self.spinner(), false)), rows_area);
        let mut text = vec![Line::raw(models[self.sel].hint.to_owned())];
        if !models[self.sel].id.is_empty() {
            text.push(Line::styled(format!("model id: {}", models[self.sel].id), Style::default().fg(FAINT)));
        }
        if self.working_on(Job::Hello) {
            text.push(Line::raw(""));
            text.push(Line::styled(format!("{} Saying hello to {}…", self.spinner(), a.name()), Style::default().fg(ACCENT)));
        } else if let Some(e) = &self.error {
            text.push(Line::raw(""));
            text.push(Line::styled(e.clone(), Style::default().fg(BAD)));
            text.push(Line::styled(format!("Try another model, check `{}` works on its own, or go back and pick another agent.", a.id()), Style::default().fg(DIM)));
        }
        boxed(f, detail_box(text), detail);
    }

    fn draw_interview(&mut self, f: &mut Frame, area: Rect) {
        let a = self.picked;
        if self.asking_existing {
            let existing = self.store.objective_list();
            let [title, list, choices] = Layout::vertical([Constraint::Length(4), Constraint::Length(existing.len() as u16 + 1), Constraint::Fill(1)]).areas(area);
            f.render_widget(intro("Your objectives", &format!("You have {} already. {} can sharpen them with you, or you can keep them as they are.", existing.len(), a.name())), title);
            let lines: Vec<Line> = existing
                .iter()
                .enumerate()
                .map(|(i, o)| Line::from(vec![Span::styled(format!("  {}. ", i + 1), Style::default().fg(FAINT)), Span::raw(o.lines().next().unwrap_or_default().to_owned())]))
                .collect();
            f.render_widget(Paragraph::new(lines), list);
            let rows = [Row { name: format!("Sharpen them with {}", a.name()), state: None, action: "" }, Row { name: "Keep them".into(), state: None, action: "" }];
            f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 0, "", false)), choices);
            return;
        }

        let input_h = if self.proposing { 3 } else { (self.input.rows(area.width.saturating_sub(4) as usize).len() as u16).clamp(1, 4) + 2 };
        let [chat, input] = Layout::vertical([Constraint::Fill(1), Constraint::Length(input_h)]).areas(area);

        let lines = self.chat_lines(chat.width as usize);
        let max = (lines.len() as u16).saturating_sub(chat.height);
        if self.follow || self.scroll >= max {
            self.scroll = max;
            self.follow = true;
        }
        f.render_widget(Paragraph::new(lines).scroll((self.scroll, 0)), chat);

        if self.proposing {
            let options = ["Yes, hold me to these", "Change something", "Skip for now"];
            let mut spans = Vec::new();
            for (i, o) in options.iter().enumerate() {
                let on = i == self.sel;
                spans.push(Span::styled(if on { " › " } else { "   " }, Style::default().fg(ACCENT)));
                spans.push(Span::styled(*o, if on { Style::default().fg(ACCENT).bold() } else { Style::default().fg(DIM) }));
                spans.push(Span::raw("  "));
            }
            f.render_widget(
                Paragraph::new(Line::from(spans)).block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(ACCENT)).title(Span::styled(" save these objectives? ", Style::default().fg(ACCENT)))),
                input,
            );
            return;
        }
        let busy = self.busy();
        let inner_w = input.width.saturating_sub(4) as usize;
        let text: Vec<Line> = if self.input.is_blank() {
            let hint = if busy {
                format!("{} is thinking…", a.name())
            } else if self.turn_failed {
                "⏎ to try again".into()
            } else {
                "Answer in your own words…".into()
            };
            vec![Line::styled(hint, Style::default().fg(FAINT))]
        } else {
            self.input.rows(inner_w).into_iter().map(|r| Line::raw(self.input.row_text(r))).collect()
        };
        let border = if busy { LINE } else { ACCENT };
        f.render_widget(
            Paragraph::new(text).block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(border)).padding(Padding::horizontal(1)).title(Span::styled(" you ", Style::default().fg(border)))),
            input,
        );
        if !busy {
            let (r, c) = if self.input.is_blank() { (0, 0) } else { self.input.cursor_rc(inner_w) };
            f.set_cursor_position(Position::new(input.x + 2 + c as u16, input.y + 1 + r as u16));
        }
    }

    fn chat_lines(&self, w: usize) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let text_w = w.saturating_sub(8).max(20);
        let intro = format!(
            "{} asks a few questions, one at a time: where you are, what gives you energy, what matters most. Then you set short- and long-term goals together. About ten minutes.",
            self.picked.name()
        );
        for l in wrap(&intro, w) {
            lines.push(Line::styled(l, Style::default().fg(DIM)));
        }
        lines.push(Line::raw(""));
        for m in &self.t.messages {
            let assistant = m.role == "assistant";
            for (i, l) in wrap(m.text.trim(), text_w).into_iter().enumerate() {
                let label = match (i, assistant) {
                    (0, true) => Span::styled("brief  ", Style::default().fg(ACCENT).bold()),
                    (0, false) => Span::styled("  you  ", Style::default().fg(DIM)),
                    _ => Span::raw("       "),
                };
                lines.push(Line::from(vec![label, if assistant { Span::raw(l) } else { Span::styled(l, Style::default().fg(Color::Indexed(250))) }]));
            }
            lines.push(Line::raw(""));
        }
        if let Some((Job::Turn, label, started, _)) = &self.job {
            lines.push(Line::from(vec![
                Span::styled("brief  ", Style::default().fg(ACCENT).bold()),
                Span::styled(format!("{} {label}…{}", self.spinner(), elapsed(*started)), Style::default().fg(DIM)),
            ]));
            return lines;
        }
        if let Some(e) = &self.error {
            for l in wrap(&format!("{} didn't answer: {e}", self.picked.name()), text_w) {
                lines.push(Line::from(vec![Span::raw("       "), Span::styled(l, Style::default().fg(BAD))]));
            }
            lines.push(Line::from(vec![Span::raw("       "), Span::styled("Press ⏎ to try again, or esc to skip.", Style::default().fg(DIM))]));
            return lines;
        }
        let Some(last) = &self.last else { return lines };
        if self.proposing {
            for (i, o) in last.objectives.iter().enumerate() {
                let horizon = match o.horizon.trim() {
                    "short" => "short term",
                    "long" => "long term",
                    _ => "",
                };
                let mut head = vec![Span::styled(format!("  {:>2}  ", i + 1), Style::default().fg(ACCENT).bold()), Span::styled(o.title.trim().to_owned(), Style::default().bold())];
                if !horizon.is_empty() {
                    head.push(Span::styled(format!("  {horizon}"), Style::default().fg(FAINT)));
                }
                lines.push(Line::from(head));
                for (label, v) in [("why", &o.why), ("done when", &o.done_when), ("by", &o.deadline), ("time", &o.time), ("in the way", &o.obstacles)] {
                    if v.trim().is_empty() {
                        continue;
                    }
                    for (n, l) in wrap(v.trim(), text_w.saturating_sub(12)).into_iter().enumerate() {
                        lines.push(Line::from(vec![
                            Span::raw("      "),
                            Span::styled(format!("{:<11} ", if n == 0 { label } else { "" }), Style::default().fg(FAINT)),
                            Span::styled(l, Style::default().fg(DIM)),
                        ]));
                    }
                }
                lines.push(Line::raw(""));
            }
        } else if !last.suggestions.is_empty() {
            // Numbered chips, wrapped onto as many lines as they need.
            let mut spans = vec![Span::raw("       ")];
            let mut used = 7;
            for (i, s) in last.suggestions.iter().enumerate() {
                let chip = format!(" {s}   ");
                let len = chip.chars().count() + 3;
                if used > 7 && used + len > w {
                    lines.push(Line::from(std::mem::replace(&mut spans, vec![Span::raw("       ")])));
                    used = 7;
                }
                spans.push(Span::styled(format!(" {} ", i + 1), Style::default().fg(Color::Black).bg(ACCENT)));
                spans.push(Span::styled(chip, Style::default().fg(DIM)));
                used += len;
            }
            lines.push(Line::from(spans));
        }
        lines
    }

    fn draw_agents(&self, f: &mut Frame, area: Rect) {
        let [title, rows_area, _, detail] = Layout::vertical([Constraint::Length(4), Constraint::Length(13), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(
            intro("Connect your agents", "Let the agents you use every day write to brief, so it remembers what you work on. Each one gets brief's tools and logs your decisions, progress and blockers on its own."),
            title,
        );
        let scanning = self.agent_rows.is_empty();
        let mut rows: Vec<Row> = AGENT_ROWS
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let (state, action) = match self.agent_rows.get(i) {
                    None => (State::Off(String::new()), ""),
                    Some((_, true)) => (State::On("connected".into()), ""),
                    Some((true, false)) => (State::Warn("not connected".into()), "⏎ connect"),
                    Some((false, _)) => (State::Off("not installed".into()), ""),
                };
                Row { name: (*name).into(), state: Some(state), action }
            })
            .collect();
        let any = self.agent_rows.iter().any(|(_, on)| *on);
        rows.push(continue_row(any));
        f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 20, self.spinner(), scanning || self.working_on(Job::Connect))), rows_area);
        let text = match AGENT_ROWS.get(self.sel) {
            Some((_, about)) => vec![Line::raw(*about), Line::raw(""), Line::styled("ChatGPT, claude.ai and Grok run in the cloud: connect them later from `brief` → Connect.", Style::default().fg(FAINT))],
            None => vec![Line::raw(if any { "Next: the tools brief reads." } else { "You can connect them later from `brief` → Connect." })],
        };
        boxed(f, detail_box(text), detail);
    }

    fn draw_tools(&self, f: &mut Frame, area: Rect) {
        let [title, rows_area, _, detail] = Layout::vertical([Constraint::Length(4), Constraint::Length(15), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(intro("Connect your tools", "The more brief sees of your day, the more honest it gets. Everything stays on this Mac, and brief only reads."), title);
        let t = self.tools;
        let on = |b: bool, off: &str| if b { State::On("connected".into()) } else { State::Off(off.into()) };
        let act = |b: bool| if b { "" } else { "⏎ connect" };
        let rows = vec![
            Row { name: "Calendar".into(), state: Some(on(t.calendar, "not connected")), action: act(t.calendar) },
            Row { name: "Todoist".into(), state: Some(on(t.todoist, "not connected")), action: act(t.todoist) },
            Row { name: "Granola".into(), state: Some(on(t.granola, "not connected")), action: act(t.granola) },
            Row {
                name: "Browser extension".into(),
                state: Some(if t.browser_seen {
                    State::On("sending".into())
                } else if t.browser_installed {
                    State::Warn("not loaded yet".into())
                } else {
                    State::Off("not installed".into())
                }),
                action: act(t.browser_seen),
            },
            Row { name: "Anki".into(), state: Some(on(t.anki, "not running")), action: act(t.anki) },
            Row { name: "Composio".into(), state: Some(on(t.composio, "not connected")), action: act(t.composio) },
            continue_row(t.any()),
        ];
        f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 20, self.spinner(), self.working_on(Job::Connect))), rows_area);
        let about = match self.sel {
            0 => "Where your time goes: since the last brief, today's meetings and free blocks, and the week ahead. Reads every account in the Calendar app (Google, iCloud, Outlook); macOS asks once.",
            1 => "Your to-do list: what's overdue or due soon, and what you finished. Todoist opens in your browser, you click Agree. Your agents can also add and tick off to-dos when you ask.",
            2 => "Your Granola meeting notes: promises and follow-ups that no to-do picks up, and advice that changes a plan.",
            3 => "What you read in your browser: time per page, and what the pages you stayed on were about. Skips mail, banks and password managers.",
            4 => "Your Anki flashcards: what's due, and what you keep forgetting. Needs the AnkiConnect add-on.",
            5 => "Composio connects 1,000+ more apps: Gmail, Slack, Notion, GitHub, Linear… You sign in once (its CLI installs if needed), pick the apps, and your brief's agent reads them each morning with read-only tools.",
            _ if t.any() => "Next: how your brief researches.",
            _ => "You can connect them later from `brief` → Connect.",
        };
        boxed(f, detail_box(vec![Line::raw(about)]), detail);
    }

    fn draw_composio(&self, f: &mut Frame, area: Rect) {
        let [title, input, _, hint] = Layout::vertical([Constraint::Length(4), Constraint::Length(3), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(intro("Which apps should brief read?", "You're signed in to Composio. Type the apps to link, separated by spaces; each one opens its sign-in page."), title);
        let w = input.width.saturating_sub(4) as usize;
        let text = if self.apps.is_blank() { Line::styled("gmail slack notion github linear", Style::default().fg(FAINT)) } else { Line::raw(self.apps.text()) };
        f.render_widget(Paragraph::new(text).block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(ACCENT)).padding(Padding::horizontal(1))), input);
        let c = if self.apps.is_blank() { 0 } else { self.apps.cursor_rc(w.max(1)).1 };
        f.set_cursor_position(Position::new(input.x + 2 + c.min(w) as u16, input.y + 1));
        f.render_widget(Paragraph::new(Line::styled("Leave it empty to link apps later with `composio link <app>`.", Style::default().fg(DIM))), hint);
    }

    fn draw_research(&self, f: &mut Frame, area: Rect) {
        let [title, rows_area, _, detail] = Layout::vertical([Constraint::Length(5), Constraint::Length(11), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(
            intro(
                "How your brief researches",
                "Every brief researches your open problems on the web to find what you wouldn't find yourself: recent papers, small repos, write-ups with real numbers. Pick what it searches with.",
            ),
            title,
        );
        let active = search::active(&self.store);
        let mut rows: Vec<Row> = search::ALL
            .iter()
            .map(|p| {
                let state = if *p == active {
                    State::On("in use".into())
                } else if search::ready(&self.store, *p) {
                    State::Off("ready".into())
                } else if *p == search::Provider::Parallel {
                    State::Off("sign in".into())
                } else {
                    State::Off("needs a key".into())
                };
                let name = if *p == search::Provider::Parallel { format!("{} (recommended)", p.name()) } else { p.name().to_owned() };
                Row { name, state: Some(state), action: "" }
            })
            .collect();
        rows.push(Row { name: "Continue →".into(), state: None, action: "" });
        f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 26, self.spinner(), self.working_on(Job::Connect))), rows_area);
        let mut text = match search::ALL.get(self.sel) {
            Some(p) => {
                let mut t = vec![Line::raw(p.blurb().to_owned())];
                if let Some(url) = p.key_url() {
                    t.push(Line::raw(""));
                    let hint = if search::key(&self.store, *p).is_some() {
                        "Key saved. ⏎ uses it and runs a test search.".to_owned()
                    } else if *p == search::Provider::Parallel && search::parallel_signed_in() {
                        "Signed in with parallel-cli. ⏎ uses it and runs a test search.".to_owned()
                    } else if *p == search::Provider::Parallel {
                        "⏎ signs you in with your browser, no key to copy (installs Parallel's CLI if needed). k pastes an API key instead.".to_owned()
                    } else {
                        format!("Needs an API key from {url}. ⏎ to add it.")
                    };
                    t.push(Line::styled(hint, Style::default().fg(DIM)));
                }
                t
            }
            None => vec![Line::raw(format!("Searching with {}. Next: explainer videos.", active.name()))],
        };
        if let Some(e) = &self.error {
            text.push(Line::raw(""));
            text.push(Line::styled(e.clone(), Style::default().fg(BAD)));
        }
        boxed(f, detail_box(text), detail);
    }

    fn draw_research_key(&self, f: &mut Frame, area: Rect) {
        let p = self.provider;
        let [title, input, _, info] = Layout::vertical([Constraint::Length(4), Constraint::Length(3), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(intro(&format!("{} needs an API key", p.name()), &format!("Get one at {} (ctrl-o opens it), paste it here and press ⏎.", p.key_url().unwrap_or_default())), title);
        // Show only the end of the key.
        let key = self.key.text();
        let shown = if key.is_empty() {
            Line::styled("paste your key", Style::default().fg(FAINT))
        } else {
            let n = key.chars().count();
            Line::raw(format!("{}{}", "•".repeat(n.saturating_sub(4).min(40)), key.chars().skip(n.saturating_sub(4)).collect::<String>()))
        };
        f.render_widget(Paragraph::new(shown).block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(ACCENT)).padding(Padding::horizontal(1))), input);
        if !self.busy() {
            let n = key.chars().count();
            f.set_cursor_position(Position::new(input.x + 2 + n.min(44) as u16, input.y + 1));
        }
        let mut text = vec![Line::styled("It's kept in search.json in brief's folder, readable only by you.", Style::default().fg(DIM))];
        if let Some(e) = &self.error {
            text.push(Line::raw(""));
            text.push(Line::styled(e.clone(), Style::default().fg(BAD)));
        }
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), info);
    }

    fn draw_videos(&self, f: &mut Frame, area: Rect) {
        let [title, rows_area, _, detail] = Layout::vertical([Constraint::Length(5), Constraint::Length(5), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(
            intro(
                "Explainer videos",
                "Some items come with a short silent animation (made with ManimGX, in the style of 3Blue1Brown) that shows how a paper or idea works, so you get it in a minute instead of ten.",
            ),
            title,
        );
        let installed = video::manimgx().is_some();
        let rows = if installed {
            vec![Row { name: "Continue →".into(), state: Some(State::On("ManimGX installed".into())), action: "" }]
        } else {
            vec![Row { name: "Install ManimGX".into(), state: Some(State::On("recommended".into())), action: "runs the command below" }, Row { name: "No videos".into(), state: None, action: "the brief works without them" }]
        };
        f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 22, "", false)), rows_area);
        let dim = Style::default().fg(DIM);
        let mut text = Vec::new();
        if !installed {
            text.push(Line::from(vec![Span::styled("Installs with ", dim), Span::styled(video::INSTALL, Style::default().fg(ACCENT))]));
            if agent::find_bin("uv").is_none() {
                text.push(Line::styled("Needs uv first: brew install uv", dim));
            }
        } else {
            text.push(Line::styled("Your briefs will have videos.", Style::default().fg(GOOD)));
        }
        boxed(f, detail_box(text), detail);
    }

    fn draw_background(&self, f: &mut Frame, area: Rect) {
        let hour = self.store.config().brief_hour;
        let [title, rows_area] = Layout::vertical([Constraint::Length(4), Constraint::Fill(1)]).areas(area);
        f.render_widget(
            intro("Every morning", &format!("brief writes a new brief every morning after {hour:02}:00 (or when your Mac wakes up) and keeps your agents' log. For that it runs quietly in the background.")),
            title,
        );
        let rows = if service_installed() {
            vec![Row { name: "Already running · continue".into(), state: Some(State::On("starts at login".into())), action: "" }]
        } else {
            vec![
                Row { name: "Run in the background".into(), state: Some(State::On("recommended".into())), action: "starts at login" },
                Row { name: "Not now".into(), state: None, action: "`brief` → Run in background" },
            ]
        };
        f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 26, "", false)), rows_area);
    }

    fn draw_brief(&self, f: &mut Frame, area: Rect) {
        let a = self.picked;
        if let Some((Job::Brief, _, started, _)) = &self.job {
            let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(6), Constraint::Fill(2)]).areas(area);
            let lines = vec![
                Line::from(vec![Span::styled(format!("{}  ", self.spinner()), Style::default().fg(ACCENT)), Span::styled("Writing your first brief", Style::default().bold()), Span::styled(elapsed(*started), Style::default().fg(FAINT))]).centered(),
                Line::raw(""),
                Line::styled(format!("{} is reading your objectives and everything you connected,", a.name()), Style::default().fg(DIM)).centered(),
                Line::styled("and researching a few things worth your time. This takes a few minutes.", Style::default().fg(DIM)).centered(),
            ];
            f.render_widget(Paragraph::new(lines), mid);
            return;
        }
        let [title, body, rows_area] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1), Constraint::Length(4)]).areas(area);
        f.render_widget(Paragraph::new(Line::styled("Your first brief", Style::default().bold())), title);
        let Some(b) = &self.brief else {
            let text = vec![Line::styled(format!("Couldn't write it: {}", self.error.clone().unwrap_or_default()), Style::default().fg(BAD))];
            f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), body);
            let rows = [Row { name: "Try again".into(), state: None, action: "" }, Row { name: "Skip".into(), state: None, action: "it comes tomorrow morning" }];
            f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 20, "", false)), rows_area);
            return;
        };
        let mut lines = vec![Line::styled(b.headline.clone(), Style::default().fg(ACCENT).bold()), Line::raw("")];
        lines.push(Line::raw(strip_md(&b.summary)));
        for s in &b.sections {
            lines.push(Line::raw(""));
            lines.push(Line::styled(s.title.clone(), Style::default().fg(DIM).add_modifier(Modifier::BOLD)));
            for i in &s.items {
                lines.push(Line::from(vec![Span::styled("  • ", Style::default().fg(ACCENT)), Span::raw(i.title.clone())]));
            }
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
        let rows = [
            Row { name: "Open it in your browser".into(), state: None, action: "one card at a time, rate each" },
            Row { name: "Continue".into(), state: None, action: "tell brief how it was" },
        ];
        f.render_widget(Paragraph::new(row_lines(&rows, self.sel, 26, "", false)), rows_area);
    }

    fn draw_feedback(&self, f: &mut Frame, area: Rect) {
        let [title, rows_area, _, label, input] = Layout::vertical([Constraint::Length(3), Constraint::Length(6), Constraint::Length(1), Constraint::Length(1), Constraint::Length(5)]).areas(area);
        f.render_widget(intro("How was your first brief?", "Every brief learns from your feedback."), title);
        let rows: Vec<Row> = ["Spot on", "Useful, but some misses", "Not useful yet"].iter().map(|n| Row { name: (*n).into(), state: None, action: "" }).collect();
        f.render_widget(Paragraph::new(row_lines(&rows, self.rating, 0, "", false)), rows_area);
        f.render_widget(Paragraph::new(Line::styled("What should it do differently tomorrow? (optional)", Style::default().fg(DIM))), label);
        let w = input.width.saturating_sub(4) as usize;
        let text: Vec<Line> = if self.comment.is_blank() {
            vec![Line::styled("e.g. more concrete next steps, fewer links…", Style::default().fg(FAINT))]
        } else {
            self.comment.rows(w).into_iter().map(|r| Line::raw(self.comment.row_text(r))).collect()
        };
        f.render_widget(Paragraph::new(text).block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(ACCENT)).padding(Padding::horizontal(1))), input);
        let (r, c) = if self.comment.is_blank() { (0, 0) } else { self.comment.cursor_rc(w.max(1)) };
        f.set_cursor_position(Position::new(input.x + 2 + c as u16, input.y + 1 + (r as u16).min(2)));
    }

    fn draw_done(&self, f: &mut Frame, area: Rect) {
        let cfg = self.store.config();
        let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(10), Constraint::Fill(2)]).areas(area);
        let lines = vec![
            Line::styled("✔  You're all set.", Style::default().fg(GOOD).bold()).centered(),
            Line::raw(""),
            Line::raw("Now just use your agents normally and live your life normally.").centered(),
            Line::raw("brief will help you reach your goals and grow.").centered(),
            Line::raw(""),
            Line::styled(format!("Your next brief comes tomorrow after {:02}:00 at http://127.0.0.1:{}/", cfg.brief_hour, cfg.port), Style::default().fg(DIM)).centered(),
            Line::styled("Run `brief` any time for your objectives, journal and connections.", Style::default().fg(DIM)).centered(),
            Line::raw(""),
            Line::from(vec![Span::styled("› ", Style::default().fg(ACCENT)), Span::styled("Finish", Style::default().fg(ACCENT).bold()), Span::styled("  ⏎", Style::default().fg(FAINT))]).centered(),
        ];
        f.render_widget(Paragraph::new(lines), mid);
    }
}

// ── pieces ───────────────────────────────────────────────────

enum State {
    On(String),
    Warn(String),
    Off(String),
}

struct Row {
    name: String,
    state: Option<State>,
    action: &'static str,
}

fn continue_row(any: bool) -> Row {
    if any {
        Row { name: "Continue →".into(), state: None, action: "" }
    } else {
        Row { name: "Do this later".into(), state: None, action: "`brief` → Connect" }
    }
}

/// A list like the app's Connect screen: name, state dot, and the action for the selected row.
fn row_lines(rows: &[Row], sel: usize, name_w: usize, spin: &'static str, working: bool) -> Vec<Line<'static>> {
    let name_w = name_w.max(rows.iter().map(|r| r.name.chars().count() + 2).max().unwrap_or(0));
    let mut lines = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let on = i == sel;
        let mut spans = vec![
            Span::styled(if on { " › " } else { "   " }, Style::default().fg(ACCENT)),
            Span::styled(format!("{:<name_w$}", r.name), if on { Style::default().fg(ACCENT).bold() } else { Style::default() }),
        ];
        if let Some(st) = &r.state {
            let (dot, color, text) = match st {
                State::On(t) => ("●", GOOD, t.clone()),
                State::Warn(t) => ("○", ACCENT, t.clone()),
                State::Off(t) => ("○", DIM, t.clone()),
            };
            let (dot, color, text) = if working && on {
                (spin, ACCENT, "working…".to_owned())
            } else if working && text.is_empty() {
                (spin, ACCENT, String::new())
            } else {
                (dot, color, text)
            };
            spans.push(Span::styled(format!("{dot} {text:<24}"), Style::default().fg(color)));
        }
        if on {
            spans.push(Span::styled(r.action, Style::default().fg(DIM)));
        }
        lines.push(Line::from(spans));
        lines.push(Line::raw(""));
    }
    lines
}

fn intro(title: &str, text: &str) -> Paragraph<'static> {
    Paragraph::new(vec![Line::styled(title.to_owned(), Style::default().bold()), Line::styled(text.to_owned(), Style::default().fg(DIM))]).wrap(Wrap { trim: false })
}

/// Draw a bordered paragraph only as tall as its text.
fn boxed(f: &mut Frame, p: Paragraph<'static>, area: Rect) {
    let h = (p.line_count(area.width) as u16).min(area.height);
    f.render_widget(p, Rect { height: h, ..area });
}

fn detail_box(text: Vec<Line<'static>>) -> Paragraph<'static> {
    Paragraph::new(text).wrap(Wrap { trim: false }).block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(LINE)).padding(Padding::horizontal(1)))
}

/// Single-line editing keys shared by every text box.
pub(crate) fn edit(ed: &mut Editor, k: KeyEvent) {
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Backspace if alt || ctrl => ed.delete_word(),
        KeyCode::Char('w') if ctrl => ed.delete_word(),
        KeyCode::Char('u') if ctrl => ed.delete_to_line_start(),
        KeyCode::Backspace => ed.backspace(),
        KeyCode::Delete => ed.delete(),
        KeyCode::Left if alt || ctrl => ed.word_left(),
        KeyCode::Right if alt || ctrl => ed.word_right(),
        KeyCode::Char('b') if alt => ed.word_left(),
        KeyCode::Char('f') if alt => ed.word_right(),
        KeyCode::Left => ed.left(),
        KeyCode::Right => ed.right(),
        KeyCode::Home => ed.home(),
        KeyCode::Char('a') if ctrl => ed.home(),
        KeyCode::End => ed.end(),
        KeyCode::Char('e') if ctrl => ed.end(),
        KeyCode::Char(c) if !ctrl && !alt => ed.insert(c),
        _ => {}
    }
}

pub(crate) fn sh(cmd: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), cmd.into()]
}

fn enter_screen() -> DefaultTerminal {
    let t = ratatui::init();
    // Alternate scroll mode: the mouse wheel arrives as ↑/↓, so the chat scrolls without
    // capturing the mouse (text selection keeps working).
    let _ = std::io::stdout().write_all(b"\x1b[?1007h").and_then(|_| std::io::stdout().flush());
    t
}

fn leave_screen() {
    let _ = std::io::stdout().write_all(b"\x1b[?1007l").and_then(|_| std::io::stdout().flush());
    ratatui::restore();
}

fn service_installed() -> bool {
    crate::plist_path().is_ok_and(|p| p.exists())
}

fn suggest_agent() {
    let subject = "An agent for brief";
    let body = "Hi! I'd like brief to work with this agent:\n\n(name and link)\n\nWhy I use it:\n\n";
    connect::open(&format!("mailto:{SUGGEST_EMAIL}?subject={}&body={}", crate::html::url_encode(subject), crate::html::url_encode(body)));
}

/// The briefs website, served from this process if the background service isn't running.
fn ensure_server(store: &Store, rt: &tokio::runtime::Handle) {
    let port = store.config().port;
    if crate::server_up(port) {
        return;
    }
    rt.spawn(crate::server::serve(store.clone()));
    for _ in 0..30 {
        if crate::server_up(port) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// An app name safe to put in `sh -c`: only the characters app slugs use.
fn shell_word(s: &str) -> String {
    s.trim().to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect()
}

fn elapsed(since: Instant) -> String {
    let s = since.elapsed().as_secs();
    match s {
        0..=2 => String::new(),
        3..=59 => format!("  {s}s"),
        _ => format!("  {}:{:02}", s / 60, s % 60),
    }
}

fn one_line(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 300 {
        s.chars().take(299).collect::<String>() + "…"
    } else {
        s
    }
}

fn strip_md(s: &str) -> String {
    s.replace("**", "").replace('`', "").replace("__", "")
}

/// Word-wrap to `w` columns, keeping line breaks.
fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > w {
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
