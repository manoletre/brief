//! `brief` with no arguments: a full-screen terminal app.

use crate::{anki, browsing, calendar, connect, editor::Editor, generate, search, store::{Entry, Store}, granola, todoist, video};
use anyhow::Result;
use chrono::{Local, NaiveDate};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Flex, Layout, Position, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, List, ListItem, ListState, Padding, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// 256-colour palette so it looks the same in Terminal.app, iTerm and others.
pub(crate) const ACCENT: Color = Color::Indexed(208);
pub(crate) const DIM: Color = Color::Indexed(244);
pub(crate) const FAINT: Color = Color::Indexed(240);
pub(crate) const LINE: Color = Color::Indexed(238);
pub(crate) const GOOD: Color = Color::Indexed(114);
pub(crate) const BAD: Color = Color::Indexed(203);
pub(crate) const BANNER_COLORS: [u8; 6] = [202, 202, 208, 208, 214, 214];
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) const BANNER: [&str; 6] = [
    "██████╗ ██████╗ ██╗███████╗███████╗",
    "██╔══██╗██╔══██╗██║██╔════╝██╔════╝",
    "██████╔╝██████╔╝██║█████╗  █████╗  ",
    "██╔══██╗██╔══██╗██║██╔══╝  ██╔══╝  ",
    "██████╔╝██║  ██║██║███████╗██║     ",
    "╚═════╝ ╚═╝  ╚═╝╚═╝╚══════╝╚═╝     ",
];

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Home,
    Objectives,
    Journal,
    /// Every part of brief you can set up, one panel per section.
    Setup,
    /// One section opened: its rows, and what the selected one does.
    Panel(Sec),
}

#[derive(Clone, Copy)]
enum Action {
    ReadBrief,
    Generate,
    Objectives,
    Journal,
    Setup,
    Service,
    Quit,
}

/// The sections of Setup.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sec {
    Agent,
    Chatbots,
    Reads,
    Research,
}
const SECS: [Sec; 4] = [Sec::Agent, Sec::Chatbots, Sec::Reads, Sec::Research];

impl Sec {
    fn title(self) -> &'static str {
        match self {
            Sec::Agent => "Your agent",
            Sec::Chatbots => "Chatbots that log your work",
            Sec::Reads => "What the brief reads",
            Sec::Research => "Research & videos",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Sec::Agent => "The agent that writes your brief, when it writes it, and the background service that runs it.",
            Sec::Chatbots => "Give these brief's tools, so they log your decisions, progress and blockers on their own.",
            Sec::Reads => "The more the brief sees of your day, the more honest it gets. Everything stays on this Mac; brief only reads.",
            Sec::Research => "What the brief searches the web with to find hidden gems for your open problems, and its explainer videos.",
        }
    }

    fn rows(self) -> Vec<Conn> {
        match self {
            Sec::Agent => vec![Conn::Agent, Conn::Model, Conn::Hour, Conn::Service],
            Sec::Chatbots => vec![Conn::ClaudeCode, Conn::ClaudeDesktop, Conn::Codex, Conn::Cline, Conn::OpenCode, Conn::Tunnel, Conn::ChatGpt, Conn::ClaudeAi, Conn::Grok],
            Sec::Reads => vec![Conn::Calendar, Conn::Todoist, Conn::Granola, Conn::Browser, Conn::Anki, Conn::Composio],
            Sec::Research => search::ALL.iter().map(|p| Conn::Search(*p)).chain([Conn::Videos]).collect(),
        }
    }
}

/// One row of a Setup panel.
#[derive(Clone, Copy, PartialEq)]
enum Conn {
    Agent,
    Model,
    Hour,
    Service,
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Cline,
    OpenCode,
    Tunnel,
    ChatGpt,
    ClaudeAi,
    Grok,
    Calendar,
    Todoist,
    Granola,
    Browser,
    Anki,
    Composio,
    Search(search::Provider),
    Videos,
}

const COMPOSIO_INSTALL: &str = "curl -fsSL https://composio.dev/install | sh";
/// Where installers put commands, which a fresh shell may not have on PATH yet.
const LOCAL_BIN: &str = "export PATH=\"$HOME/.local/bin:$HOME/.composio:$PATH\";";

/// Commands that need the real terminal (installs and sign-ins), and what to do after them.
struct Outside {
    title: String,
    cmds: Vec<Vec<String>>,
    after: After,
}

#[derive(Clone, Copy)]
enum After {
    Composio,
    Parallel,
    Videos,
}

#[derive(Default)]
struct Status {
    goals: usize,
    entries: usize,
    last_entry: Option<Entry>,
    today: Option<String>,
    server_up: bool,
    server_generating: bool,
    service_installed: bool,
    tunnel_on: bool,
    tunnel_url: Option<String>,
    code_installed: bool,
    code_connected: bool,
    desktop_installed: bool,
    desktop_extension: bool,
    desktop_config_entry: bool,
    tailscale: bool,
    /// The connected Todoist account ("" if its name is unknown).
    todoist: Option<String>,
    /// Connected before brief asked to change to-dos, so chatbots can only read them.
    todoist_read_only: bool,
    granola: Option<String>,
    calendar: bool,
    /// The browser extension's files are written; `chrome_seen` once it has sent something lately.
    chrome_installed: bool,
    chrome_seen: bool,
    /// (installed, connected) for the agent CLIs that can log to brief.
    codex: (bool, bool),
    cline: (bool, bool),
    opencode: (bool, bool),
    /// Only checked on the Setup screens (it asks AnkiConnect).
    anki: bool,
    composio: bool,
    search: Option<search::Provider>,
    manimgx: bool,
}

type Job = mpsc::Receiver<Result<String, String>>;

/// Why the app closed: to quit, or to run the objectives interview in the terminal and come back.
pub enum Exit {
    Quit,
    Interview,
    /// Pick the agent and model with onboarding's screens, then come back to Setup.
    Agent,
}

struct App {
    store: Store,
    rt: tokio::runtime::Handle,
    screen: Screen,
    menu: ListState,
    toast: Option<(String, bool, Instant)>,
    status: Status,
    tick: usize,
    refreshed: Option<Instant>,
    generating: Option<Job>,
    // Objectives
    objs: Vec<Editor>,
    osel: usize,
    ofirst: usize,
    editing: bool,
    wrap_w: usize,
    deleted: Option<(usize, String)>,
    // Journal / setup
    scroll: u16,
    /// The selected panel on Setup, and the selected row in an open panel.
    psel: usize,
    rsel: usize,
    /// Typing an API key for a search provider.
    key_input: Option<(search::Provider, Editor)>,
    outside: Option<Outside>,
    job: Option<(Conn, Job)>,
    exit: Exit,
}

/// `start`: open Setup on that section (coming back from a step that needed the whole terminal).
pub fn run(store: Store, start: Option<Sec>) -> Result<Exit> {
    let mut app = App {
        store,
        rt: tokio::runtime::Handle::current(),
        screen: Screen::Home,
        menu: ListState::default().with_selected(Some(0)),
        toast: None,
        status: Status::default(),
        tick: 0,
        refreshed: None,
        generating: None,
        objs: Vec::new(),
        osel: 0,
        ofirst: 0,
        editing: false,
        wrap_w: 60,
        deleted: None,
        scroll: 0,
        psel: 0,
        rsel: 0,
        key_input: None,
        outside: None,
        job: None,
        exit: Exit::Quit,
    };
    if let Some(sec) = start {
        app.psel = SECS.iter().position(|s| *s == sec).unwrap_or(0);
        app.screen = Screen::Panel(sec);
    }
    app.refresh();
    if app.status.goals == 0 && start.is_none() {
        app.open_objectives();
    }
    let mut terminal = ratatui::init();
    // Coming back from onboarding or the interview: start from a blank screen.
    let _ = terminal.clear();
    // Alternate scroll mode: the mouse wheel arrives as ↑/↓ keys, without capturing
    // the mouse, so text selection keeps working.
    let _ = std::io::stdout().write_all(b"\x1b[?1007h").and_then(|_| std::io::stdout().flush());
    let result = app.event_loop(&mut terminal);
    let _ = std::io::stdout().write_all(b"\x1b[?1007l").and_then(|_| std::io::stdout().flush());
    ratatui::restore();
    result.map(|()| app.exit)
}

impl App {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            self.update();
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(80))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press && !self.on_key(k) {
                        return Ok(());
                    }
                }
                if let Some(o) = self.outside.take() {
                    self.run_outside(terminal, o);
                }
            }
        }
    }

    // ── state ────────────────────────────────────────────────

    fn update(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if let Some(Ok(result)) = self.generating.as_ref().map(|rx| rx.try_recv()) {
            self.generating = None;
            match result {
                Ok(headline) => self.say(format!("Today's brief is ready: {headline}"), false),
                Err(e) => self.say(format!("Couldn't write the brief: {e}"), true),
            }
            self.refreshed = None;
        }
        if let Some(Ok(result)) = self.job.as_ref().map(|(_, rx)| rx.try_recv()) {
            self.job = None;
            match result {
                Ok(msg) => self.say(msg, false),
                Err(e) => self.say(e, true),
            }
            self.refreshed = None;
        }
        let every = if self.status.tunnel_on && self.status.tunnel_url.is_none() { 1 } else { 2 };
        if self.refreshed.map_or(true, |t| t.elapsed() > Duration::from_secs(every)) {
            self.refresh();
        }
    }

    fn refresh(&mut self) {
        let entries = self.store.entries();
        let cfg = self.store.config();
        let server_up = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], cfg.port).into(), Duration::from_millis(80)).is_ok();
        let api = if server_up {
            http(cfg.port, "GET", "/api/status").and_then(|b| serde_json::from_str::<serde_json::Value>(&b).ok())
        } else {
            None
        };
        let api = api.unwrap_or_default();
        self.status = Status {
            goals: self.store.objective_list().len(),
            entries: entries.len(),
            last_entry: entries.last().cloned(),
            today: self.store.brief(today()).map(|b| b.headline),
            server_up,
            server_generating: api.get("generating").and_then(|v| v.as_bool()).unwrap_or(false),
            service_installed: crate::plist_path().is_ok_and(|p| p.exists()),
            tunnel_on: cfg.tunnel,
            tunnel_url: api.get("tunnel_url").and_then(|v| v.as_str()).map(str::to_owned),
            code_installed: connect::claude_code_installed(),
            code_connected: connect::claude_code_connected(),
            desktop_installed: connect::claude_desktop_installed(),
            desktop_extension: connect::claude_desktop_extension(),
            desktop_config_entry: connect::claude_desktop_config_entry(),
            tailscale: connect::tailscale().is_some(),
            todoist: todoist::auth(&self.store).map(|a| a.account),
            todoist_read_only: todoist::auth(&self.store).is_some_and(|a| !a.scope.contains("read_write")),
            granola: granola::auth(&self.store).map(|a| a.account),
            calendar: calendar::connected(&self.store),
            chrome_installed: browsing::installed(&self.store),
            chrome_seen: browsing::last_seen(&self.store).is_some_and(|t| chrono::Local::now() - t < chrono::Duration::days(3)),
            codex: (crate::agent::Agent::Codex.find().is_some(), connect::codex_connected()),
            cline: (crate::agent::Agent::Cline.find().is_some(), connect::cline_connected()),
            opencode: (crate::agent::Agent::OpenCode.find().is_some(), connect::opencode_connected()),
            anki: if matches!(self.screen, Screen::Setup | Screen::Panel(_)) { anki::available(false) } else { self.status.anki },
            composio: cfg.composio,
            search: Some(search::active(&self.store)),
            manimgx: video::manimgx().is_some(),
        };
        self.refreshed = Some(Instant::now());
    }

    fn say(&mut self, msg: impl Into<String>, error: bool) {
        self.toast = Some((msg.into(), error, Instant::now()));
    }

    fn busy(&self) -> bool {
        self.generating.is_some() || self.status.server_generating
    }

    /// Run blocking setup work on a thread so the screen keeps animating.
    fn start_job(&mut self, conn: Conn, f: impl FnOnce() -> anyhow::Result<String> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f().map_err(|e| format!("{e:#}")));
        });
        self.job = Some((conn, rx));
    }

    fn menu_items(&self) -> Vec<(Action, String, String)> {
        let s = &self.status;
        let cfg = self.store.config();
        let connected = [s.code_connected, s.desktop_extension || s.desktop_config_entry, s.tunnel_url.is_some(), s.todoist.is_some(), s.granola.is_some(), s.calendar, s.chrome_seen].iter().filter(|b| **b).count();
        vec![
            (Action::ReadBrief, "Read today's brief".into(), match &s.today {
                Some(h) => h.clone(),
                None => "not written yet · opens your past briefs".into(),
            }),
            (Action::Generate, "Write today's brief now".into(), format!("a few minutes, with {}", crate::agent::Agent::from_id(&cfg.agent).name())),
            (Action::Objectives, "Objectives".into(), match s.goals {
                0 => "not set yet · start here".into(),
                1 => "1 objective".into(),
                n => format!("{n} objectives"),
            }),
            (Action::Journal, "Journal".into(), format!("{} entries from your chatbots", s.entries)),
            (Action::Setup, "Setup".into(), {
                let agent = crate::agent::Agent::from_id(&cfg.agent);
                let search = s.search.map_or("", |p| if p == search::Provider::Claude { "agent's search" } else { p.name() });
                format!("{} · {} at {:02}:00 · {connected} connected · {search}", agent.name(), agent.model_label(&cfg.model), cfg.brief_hour)
            }),
            (Action::Service, if s.service_installed { "Stop running in background".into() } else { "Run in background".into() },
                if s.service_installed { "removes the login item".into() } else { "starts at login, writes the brief daily".into() }),
            (Action::Quit, "Quit".into(), String::new()),
        ]
    }

    // ── keys ─────────────────────────────────────────────────

    /// Returns false to quit.
    fn on_key(&mut self, k: KeyEvent) -> bool {
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            if self.screen == Screen::Objectives {
                self.save_objectives();
            }
            return false;
        }
        match self.screen {
            Screen::Home => return self.home_key(k),
            Screen::Objectives if self.editing => self.edit_key(k),
            Screen::Objectives => return self.objectives_key(k),
            Screen::Setup => self.setup_key(k),
            Screen::Panel(sec) => return self.panel_key(sec, k),
            Screen::Journal => match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Backspace => self.screen = Screen::Home,
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = self.scroll.saturating_add(15),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(15),
                KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
                // Clamped to the last page on the next draw.
                KeyCode::End | KeyCode::Char('G') => self.scroll = u16::MAX,
                _ => {}
            },
        }
        true
    }

    fn home_key(&mut self, k: KeyEvent) -> bool {
        let items = self.menu_items();
        let i = self.menu.selected().unwrap_or(0);
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.menu.select(Some((i + 1) % items.len())),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.menu.select(Some((i + items.len() - 1) % items.len())),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => return self.activate(items[i].0),
            KeyCode::Char(c @ '1'..='9') => {
                let n = c as usize - '1' as usize;
                if n < items.len() {
                    self.menu.select(Some(n));
                    return self.activate(items[n].0);
                }
            }
            _ => {}
        }
        true
    }

    fn activate(&mut self, action: Action) -> bool {
        let port = self.store.config().port;
        match action {
            Action::Quit => return false,
            Action::ReadBrief => {
                if !self.status.server_up {
                    self.say("The background service isn't running. Choose “Run in background” first.", true);
                } else {
                    let path = if self.status.today.is_some() { format!("/brief/{}", today()) } else { "/".into() };
                    connect::open(&format!("http://127.0.0.1:{port}{path}"));
                    self.say("Opened in your browser", false);
                }
            }
            Action::Generate => {
                if self.busy() {
                    self.say("Already writing today's brief…", false);
                } else if self.status.server_up {
                    http(port, "POST", "/api/generate");
                    self.say("Writing in the background. You can close this; it'll show up in your briefs.", false);
                    self.refreshed = None;
                } else {
                    let (tx, rx) = mpsc::channel();
                    let store = self.store.clone();
                    self.rt.spawn(async move {
                        let r = generate::generate(&store).await.map(|b| b.headline).map_err(|e| format!("{e:#}"));
                        let _ = tx.send(r);
                    });
                    self.generating = Some(rx);
                    self.say("Writing… keep this open until it's done (or run in background).", false);
                }
            }
            Action::Objectives => self.open_objectives(),
            Action::Journal => {
                self.scroll = 0;
                self.screen = Screen::Journal;
            }
            Action::Setup => {
                self.screen = Screen::Setup;
                self.refreshed = None;
            }
            Action::Service => {
                let result = if self.status.service_installed { crate::uninstall() } else { crate::install(&self.store) };
                match result {
                    Ok(msg) => self.say(msg, false),
                    Err(e) => self.say(format!("{e:#}"), true),
                }
                std::thread::sleep(Duration::from_millis(400));
                self.refresh();
            }
        }
        true
    }

    // ── objectives ───────────────────────────────────────────

    fn open_objectives(&mut self) {
        self.objs = self.store.objective_list().iter().map(|s| Editor::new(s)).collect();
        self.osel = 0;
        self.ofirst = 0;
        self.editing = false;
        self.deleted = None;
        self.screen = Screen::Objectives;
        if self.objs.is_empty() {
            self.add_objective(0);
        }
    }


    fn save_objectives(&mut self) {
        let items: Vec<String> = self.objs.iter().map(Editor::text).collect();
        if let Err(e) = self.store.save_objective_list(&items) {
            self.say(format!("Couldn't save: {e}"), true);
        }
        self.refreshed = None;
    }

    fn add_objective(&mut self, at: usize) {
        self.objs.insert(at, Editor::new(""));
        self.osel = at;
        self.editing = true;
    }

    /// Leave edit mode; drops the objective if it ended up empty. Returns true if it was dropped.
    fn finish_edit(&mut self) -> bool {
        self.editing = false;
        let dropped = self.objs.get(self.osel).is_some_and(Editor::is_blank);
        if dropped {
            self.objs.remove(self.osel);
        }
        self.save_objectives();
        dropped
    }

    fn edit_at(&mut self, i: usize, at_start: bool) {
        self.osel = i;
        self.editing = true;
        if at_start {
            self.objs[i].cursor_to_start();
        } else {
            self.objs[i].end_of_text();
        }
    }

    /// Returns false to leave the app (for the interview).
    fn objectives_key(&mut self, k: KeyEvent) -> bool {
        let n = self.objs.len();
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => {
                self.save_objectives();
                self.screen = Screen::Home;
            }
            KeyCode::Up | KeyCode::Char('K') if (shift || k.code == KeyCode::Char('K')) && self.osel > 0 && self.osel < n => {
                self.objs.swap(self.osel, self.osel - 1);
                self.osel -= 1;
                self.save_objectives();
            }
            KeyCode::Down | KeyCode::Char('J') if (shift || k.code == KeyCode::Char('J')) && self.osel + 1 < n => {
                self.objs.swap(self.osel, self.osel + 1);
                self.osel += 1;
                self.save_objectives();
            }
            KeyCode::Up | KeyCode::Char('k') => self.osel = self.osel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.osel = (self.osel + 1).min(n),
            KeyCode::BackTab => self.osel = self.osel.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('e') | KeyCode::Right | KeyCode::Char(' ') => {
                if self.osel >= n {
                    self.add_objective(n);
                } else {
                    self.edit_at(self.osel, false);
                }
            }
            KeyCode::Char('n') | KeyCode::Char('a') | KeyCode::Char('o') => self.add_objective((self.osel + 1).min(n)),
            KeyCode::Char('i') => {
                self.save_objectives();
                self.exit = Exit::Interview;
                return false;
            }
            KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete | KeyCode::Backspace if self.osel < n => {
                let gone = self.objs.remove(self.osel);
                self.deleted = Some((self.osel, gone.text()));
                self.osel = self.osel.min(self.objs.len().saturating_sub(1));
                self.save_objectives();
                self.say("Deleted. Press u to undo.", false);
            }
            KeyCode::Char('u') => {
                if let Some((i, text)) = self.deleted.take() {
                    let i = i.min(self.objs.len());
                    self.objs.insert(i, Editor::new(&text));
                    self.osel = i;
                    self.save_objectives();
                    self.say("Restored", false);
                }
            }
            _ => {}
        }
        true
    }

    fn edit_key(&mut self, k: KeyEvent) {
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let w = self.wrap_w;
        let i = self.osel;
        match k.code {
            KeyCode::Esc => {
                self.finish_edit();
                self.say("Saved", false);
            }
            // ⌥⏎ / ⌃J: line break inside this objective (for notes).
            KeyCode::Enter if alt || ctrl => self.objs[i].insert('\n'),
            KeyCode::Char('j') if ctrl => self.objs[i].insert('\n'),
            // ⏎: start the next objective (or stop, on an empty one).
            KeyCode::Enter => {
                if self.objs[i].is_blank() {
                    self.finish_edit();
                } else {
                    self.finish_edit();
                    self.add_objective(i + 1);
                }
            }
            KeyCode::Tab => {
                let dropped = self.finish_edit();
                let next = if dropped { i } else { i + 1 };
                if next < self.objs.len() {
                    self.edit_at(next, false);
                } else {
                    self.add_objective(self.objs.len());
                }
            }
            KeyCode::BackTab => {
                self.finish_edit();
                if i > 0 {
                    self.edit_at(i - 1, false);
                }
            }
            KeyCode::Up => {
                if !self.objs[i].up(w) && i > 0 {
                    self.finish_edit();
                    self.edit_at(i - 1, false);
                }
            }
            KeyCode::Down => {
                if !self.objs[i].down(w) && i + 1 < self.objs.len() {
                    let dropped = self.finish_edit();
                    self.edit_at(if dropped { i } else { i + 1 }, true);
                }
            }
            // Backspace on an empty objective removes it and goes back to the previous one.
            KeyCode::Backspace if self.objs[i].is_blank() && i > 0 => {
                self.objs.remove(i);
                self.edit_at(i - 1, false);
            }
            KeyCode::Backspace if alt || ctrl => self.objs[i].delete_word(),
            KeyCode::Char('w') if ctrl => self.objs[i].delete_word(),
            KeyCode::Char('u') if ctrl => self.objs[i].delete_to_line_start(),
            KeyCode::Backspace => self.objs[i].backspace(),
            KeyCode::Delete => self.objs[i].delete(),
            KeyCode::Left if alt || ctrl => self.objs[i].word_left(),
            KeyCode::Right if alt || ctrl => self.objs[i].word_right(),
            KeyCode::Char('b') if alt => self.objs[i].word_left(),
            KeyCode::Char('f') if alt => self.objs[i].word_right(),
            KeyCode::Left => self.objs[i].left(),
            KeyCode::Right => self.objs[i].right(),
            KeyCode::Home => self.objs[i].home(),
            KeyCode::Char('a') if ctrl => self.objs[i].home(),
            KeyCode::End => self.objs[i].end(),
            KeyCode::Char('e') if ctrl => self.objs[i].end(),
            KeyCode::Char(c) if !ctrl && !alt => self.objs[i].insert(c),
            _ => {}
        }
    }

    // ── setup ────────────────────────────────────────────────

    fn setup_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => self.screen = Screen::Home,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => self.psel = (self.psel + 1) % SECS.len(),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => self.psel = (self.psel + SECS.len() - 1) % SECS.len(),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k') => self.psel = (self.psel + 2) % SECS.len(),
            KeyCode::Char(c @ '1'..='4') => self.open_panel(c as usize - '1' as usize),
            KeyCode::Enter | KeyCode::Char(' ') => self.open_panel(self.psel),
            _ => {}
        }
    }

    fn open_panel(&mut self, i: usize) {
        self.psel = i;
        self.rsel = 0;
        self.key_input = None;
        self.screen = Screen::Panel(SECS[i]);
    }

    /// Returns false to leave the app (to run a step that needs the whole terminal).
    fn panel_key(&mut self, sec: Sec, k: KeyEvent) -> bool {
        if self.key_input.is_some() {
            self.key_input_key(k);
            return true;
        }
        let rows = sec.rows();
        self.rsel = self.rsel.min(rows.len() - 1);
        let conn = rows[self.rsel];
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => self.screen = Screen::Setup,
            KeyCode::Down | KeyCode::Char('j') => self.rsel = (self.rsel + 1) % rows.len(),
            KeyCode::Up | KeyCode::Char('k') if !matches!(conn, Conn::Search(_)) || k.code == KeyCode::Up => self.rsel = (self.rsel + rows.len() - 1) % rows.len(),
            KeyCode::Tab => self.open_panel((self.psel + 1) % SECS.len()),
            KeyCode::BackTab => self.open_panel((self.psel + SECS.len() - 1) % SECS.len()),
            KeyCode::Left | KeyCode::Char('h') if matches!(conn, Conn::Model | Conn::Hour) => self.change(conn, -1),
            KeyCode::Right | KeyCode::Char('l') if matches!(conn, Conn::Model | Conn::Hour) => self.change(conn, 1),
            KeyCode::Char('c') if sec == Sec::Chatbots => {
                let url = self.status.tunnel_url.clone().unwrap_or_else(|| format!("http://127.0.0.1:{}/mcp", self.store.config().port));
                match connect::copy(&url) {
                    Ok(()) => self.say(format!("Copied {}", redact(&url)), false),
                    Err(e) => self.say(format!("Couldn't copy: {e}"), true),
                }
            }
            KeyCode::Char('k') => {
                if let Conn::Search(p) = conn {
                    if p.key_url().is_some() {
                        self.key_input = Some((p, Editor::new("")));
                    }
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                if self.job.is_some() {
                    self.say("One moment, still working on the last step…", false);
                } else {
                    return self.connect_action(conn);
                }
            }
            _ => {}
        }
        true
    }

    fn key_input_key(&mut self, k: KeyEvent) {
        let Some((p, ed)) = self.key_input.as_mut() else { return };
        let p = *p;
        match k.code {
            KeyCode::Esc => self.key_input = None,
            KeyCode::Char('o') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(url) = p.key_url() {
                    connect::open(url);
                }
            }
            KeyCode::Enter if !ed.is_blank() => {
                let key = ed.text().trim().to_owned();
                self.key_input = None;
                if let Err(e) = search::choose(&self.store, p, Some(&key)) {
                    return self.say(format!("Couldn't save: {e}"), true);
                }
                self.test_search(p);
            }
            _ => crate::onboard::edit(ed, k),
        }
    }

    /// ←→ on a row with a value.
    fn change(&mut self, conn: Conn, delta: i32) {
        let mut cfg = self.store.config();
        match conn {
            Conn::Hour => cfg.brief_hour = (cfg.brief_hour as i32 + delta).rem_euclid(24) as u32,
            Conn::Model => {
                let models = crate::agent::Agent::from_id(&cfg.agent).models();
                let i = models.iter().position(|m| m.id == cfg.model).unwrap_or(0) as i32;
                cfg.model = models[(i + delta).rem_euclid(models.len() as i32) as usize].id.into();
            }
            _ => return,
        }
        match self.store.save_config(&cfg) {
            Ok(()) => self.say("Saved", false),
            Err(e) => self.say(format!("Couldn't save: {e}"), true),
        }
    }

    /// Use `p` for the brief's research, after a test search (back to the agent's own if it fails).
    fn test_search(&mut self, p: search::Provider) {
        let store = self.store.clone();
        self.start_job(Conn::Search(p), move || {
            search::choose(&store, p, None)?;
            search::test(&store).inspect_err(|_| {
                let mut st = search::settings(&store);
                st.provider = search::Provider::Claude.id().into();
                let _ = search::save(&store, &st);
            })
        });
    }

    /// ⏎ on a row. Returns false to leave the app for a step that needs the whole terminal.
    fn connect_action(&mut self, conn: Conn) -> bool {
        let s = &self.status;
        match conn {
            Conn::Agent => {
                self.exit = Exit::Agent;
                return false;
            }
            Conn::Model | Conn::Hour => self.change(conn, 1),
            Conn::Service => {
                let result = if s.service_installed { crate::uninstall() } else { crate::install(&self.store) };
                match result {
                    Ok(msg) => self.say(msg, false),
                    Err(e) => self.say(format!("{e:#}"), true),
                }
                std::thread::sleep(Duration::from_millis(400));
                self.refresh();
            }
            Conn::ClaudeCode if !s.code_installed => self.say("Claude Code isn't installed on this Mac.", true),
            Conn::ClaudeCode => {
                let on = !s.code_connected;
                self.start_job(conn, move || connect::set_claude_code(on));
            }
            Conn::ClaudeDesktop => {
                let store = self.store.clone();
                self.start_job(conn, move || connect::install_claude_desktop(&store));
            }
            Conn::Codex | Conn::Cline | Conn::OpenCode => {
                let (installed, on) = match conn {
                    Conn::Codex => s.codex,
                    Conn::Cline => s.cline,
                    _ => s.opencode,
                };
                if !installed {
                    self.say("It isn't installed on this Mac.", true);
                } else {
                    self.start_job(conn, move || match conn {
                        Conn::Codex => connect::set_codex(!on),
                        Conn::Cline => connect::set_cline(!on),
                        _ => connect::set_opencode(!on),
                    });
                }
            }
            Conn::Tunnel if !s.tailscale => self.start_job(conn, connect::get_tailscale),
            Conn::Tunnel => {
                let (store, on) = (self.store.clone(), !s.tunnel_on);
                if on {
                    self.say("Setting up the public link… if a Tailscale page opens, allow Funnel there.", false);
                }
                self.start_job(conn, move || connect::set_tunnel(&store, on));
            }
            Conn::ChatGpt | Conn::ClaudeAi | Conn::Grok => match s.tunnel_url.clone() {
                None => self.say("Turn on the public link first (the Public link row).", true),
                Some(url) => {
                    let _ = connect::copy(&url);
                    connect::open(match conn {
                        Conn::ChatGpt => connect::CHATGPT_SETTINGS,
                        Conn::ClaudeAi => connect::CLAUDE_AI_SETTINGS,
                        _ => connect::GROK_SETTINGS,
                    });
                    self.say("Link copied. Add a custom connector and paste it as the URL.", false);
                }
            },
            Conn::Todoist if s.todoist_read_only => {
                let store = self.store.clone();
                self.say("Opening Todoist in your browser: click Agree there to let your chatbots add and edit to-dos.", false);
                self.start_job(conn, move || todoist::connect(&store));
            }
            Conn::Todoist if s.todoist.is_some() => {
                let store = self.store.clone();
                self.start_job(conn, move || todoist::disconnect(&store));
            }
            Conn::Todoist => {
                let store = self.store.clone();
                self.say("Opening Todoist in your browser: click Agree there.", false);
                self.start_job(conn, move || todoist::connect(&store));
            }
            Conn::Granola if s.granola.is_some() => {
                let store = self.store.clone();
                self.start_job(conn, move || granola::disconnect(&store));
            }
            Conn::Granola => {
                let store = self.store.clone();
                self.say("Opening Granola in your browser: sign in and allow brief there.", false);
                self.start_job(conn, move || granola::connect(&store));
            }
            Conn::Calendar if s.calendar => {
                let store = self.store.clone();
                self.start_job(conn, move || calendar::disconnect(&store));
            }
            Conn::Calendar => {
                let store = self.store.clone();
                self.say("Building the Calendar helper… then macOS asks whether brief may see your calendar: click Allow.", false);
                self.start_job(conn, move || calendar::connect(&store));
            }
            Conn::Browser => {
                let store = self.store.clone();
                if !s.chrome_seen {
                    self.say(format!("Opening {}'s extensions page. Turn on Developer mode, Load unpacked, ⌘⇧G, paste, ⏎.", browsing::browser_name()), false);
                }
                self.start_job(conn, move || browsing::install(&store).map(|t| t.lines().next().unwrap_or_default().to_owned()));
            }
            Conn::Anki if s.anki => self.say("Anki is connected through AnkiConnect.", false),
            Conn::Anki => self.start_job(conn, || {
                if anki::available(true) {
                    Ok("Anki is connected.".into())
                } else {
                    anyhow::bail!("Anki needs the AnkiConnect add-on: in Anki, Tools → Add-ons → Get Add-ons…, paste 2055492159, restart Anki")
                }
            }),
            Conn::Composio if s.composio => {
                let mut cfg = self.store.config();
                cfg.composio = false;
                match self.store.save_config(&cfg) {
                    Ok(()) => self.say("The brief no longer reads your Composio apps. ⏎ again to set it up.", false),
                    Err(e) => self.say(format!("Couldn't save: {e}"), true),
                }
                self.refreshed = None;
            }
            Conn::Composio => {
                let mut cmds = Vec::new();
                if crate::agent::find_bin("composio").is_none() {
                    cmds.push(crate::onboard::sh(COMPOSIO_INSTALL));
                }
                cmds.push(crate::onboard::sh(&format!("{LOCAL_BIN} composio login")));
                self.outside = Some(Outside { title: "Signing in to Composio (a browser page opens)".into(), cmds, after: After::Composio });
            }
            Conn::Search(p) if search::ready(&self.store, p) => {
                if p == search::Provider::Claude {
                    match search::choose(&self.store, p, None) {
                        Ok(()) => self.say("The brief will use your agent's own web search.", false),
                        Err(e) => self.say(format!("Couldn't save: {e}"), true),
                    }
                    self.refreshed = None;
                } else {
                    self.test_search(p);
                }
            }
            Conn::Search(search::Provider::Parallel) => {
                // One ⏎: install Parallel's CLI if needed and sign in through the browser.
                let mut cmds = Vec::new();
                if search::parallel_cli().is_none() {
                    cmds.push(crate::onboard::sh(search::PARALLEL_CLI_INSTALL));
                }
                cmds.push(crate::onboard::sh(&format!("{LOCAL_BIN} parallel-cli login")));
                self.outside = Some(Outside { title: "Signing in to Parallel (a browser page opens: approve brief there)".into(), cmds, after: After::Parallel });
            }
            Conn::Search(p) => self.key_input = Some((p, Editor::new(""))),
            Conn::Videos if s.manimgx => self.say("ManimGX is installed: your briefs have explainer videos.", false),
            Conn::Videos => {
                self.outside = Some(Outside { title: "Installing ManimGX".into(), cmds: vec![crate::onboard::sh(&format!("{LOCAL_BIN} {}", video::INSTALL))], after: After::Videos });
            }
        }
        true
    }

    /// Hand the terminal to installers and sign-ins, then come back.
    fn run_outside(&mut self, terminal: &mut DefaultTerminal, o: Outside) {
        let _ = std::io::stdout().write_all(b"\x1b[?1007l").and_then(|_| std::io::stdout().flush());
        ratatui::restore();
        let (accent, dim, bad, reset) = ("\x1b[38;5;208m", "\x1b[38;5;244m", "\x1b[38;5;203m", "\x1b[0m");
        println!("\n  {accent}brief{reset} › {}\n", o.title);
        let mut ok = true;
        for cmd in &o.cmds {
            println!("  {dim}$ {}{reset}\n", cmd.last().map(|c| c.trim_start_matches(LOCAL_BIN).trim()).unwrap_or_default());
            if !std::process::Command::new(&cmd[0]).args(&cmd[1..]).status().is_ok_and(|s| s.success()) {
                ok = false;
                println!("\n  {bad}That didn't finish.{reset}");
                break;
            }
        }
        let line = |prompt: &str| -> String {
            print!("{prompt}");
            let _ = std::io::stdout().flush();
            let mut l = String::new();
            let _ = std::io::stdin().read_line(&mut l);
            l.trim().to_owned()
        };
        match o.after {
            After::Composio if ok => {
                println!("\n  Which apps should brief read? Their names, separated by spaces (gmail slack notion github linear).");
                let apps = line(&format!("  {accent}›{reset} "));
                for app in apps.split([' ', ',']).map(str::trim).filter(|a| !a.is_empty()) {
                    let app: String = app.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
                    println!("\n  {dim}$ composio link {app}{reset}\n");
                    let _ = std::process::Command::new("/bin/sh").args(["-c", &format!("{LOCAL_BIN} composio link {app}")]).status();
                }
                let mut cfg = self.store.config();
                cfg.composio = true;
                match self.store.save_config(&cfg) {
                    Ok(()) => self.say("Composio is connected. The brief reads those apps each morning (read-only).", false),
                    Err(e) => self.say(format!("Couldn't save: {e}"), true),
                }
            }
            After::Composio => self.say("Composio isn't signed in yet. ⏎ to try again.", true),
            After::Parallel => {
                search::recheck_parallel();
                if search::parallel_signed_in() {
                    self.test_search(search::Provider::Parallel);
                } else {
                    self.say("Parallel isn't signed in yet. ⏎ to try again, or k to paste an API key.", true);
                }
            }
            After::Videos if video::manimgx().is_some() => self.say("ManimGX is installed: your briefs have explainer videos.", false),
            After::Videos => self.say("ManimGX still isn't there; the brief works without videos.", true),
        }
        if !matches!(o.after, After::Composio) || !ok {
            line(&format!("\n  {dim}Press ⏎ to go back to brief{reset} "));
        }
        *terminal = ratatui::init();
        let _ = terminal.clear();
        let _ = std::io::stdout().write_all(b"\x1b[?1007h").and_then(|_| std::io::stdout().flush());
        self.refreshed = None;
    }

    // ── drawing ──────────────────────────────────────────────

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        let width = area.width.saturating_sub(4).min(78);
        let [col] = Layout::horizontal([Constraint::Length(width)]).flex(Flex::Center).areas(area);
        let [body, toast, footer] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Length(1)]).areas(col.inner(ratatui::layout::Margin::new(0, 1)));

        match self.screen {
            Screen::Home => self.draw_home(f, body),
            Screen::Objectives => self.draw_objectives(f, body),
            Screen::Journal => self.draw_journal(f, body),
            Screen::Setup => self.draw_setup(f, body),
            Screen::Panel(sec) => self.draw_panel(f, body, sec),
        }

        if let Some((msg, err, at)) = &self.toast {
            if at.elapsed() < Duration::from_secs(6) {
                let style = Style::default().fg(if *err { BAD } else { GOOD });
                f.render_widget(Paragraph::new(Line::styled(msg.as_str(), style)), toast);
            }
        }

        let keys: &[(&str, &str)] = match self.screen {
            Screen::Home => &[("↑↓", "move"), ("⏎", "select"), ("1-8", "jump"), ("q", "quit")],
            Screen::Objectives if self.editing => &[("esc", "done"), ("⏎", "next objective"), ("⌥⏎", "line break"), ("↑↓", "move"), ("⌥⌫", "del word")],
            Screen::Objectives => &[("↑↓", "move"), ("⏎", "edit"), ("n", "new"), ("d", "delete"), ("⇧↑↓", "reorder"), ("i", "interview"), ("esc", "back")],
            Screen::Setup => &[("←→↑↓", "move"), ("⏎", "open"), ("1-4", "jump"), ("esc", "back")],
            Screen::Panel(_) if self.key_input.is_some() => &[("⏎", "save & test"), ("ctrl-o", "get a key"), ("esc", "cancel")],
            Screen::Panel(Sec::Agent) => &[("↑↓", "move"), ("⏎", "set up"), ("←→", "change"), ("tab", "next panel"), ("esc", "back")],
            Screen::Panel(Sec::Chatbots) => &[("↑↓", "move"), ("⏎", "connect / disconnect"), ("c", "copy link"), ("tab", "next panel"), ("esc", "back")],
            Screen::Panel(Sec::Research) => &[("↑↓", "move"), ("⏎", "use / sign in"), ("k", "paste a key"), ("tab", "next panel"), ("esc", "back")],
            Screen::Panel(_) => &[("↑↓", "move"), ("⏎", "connect / disconnect"), ("tab", "next panel"), ("esc", "back")],
            Screen::Journal => &[("↑↓", "scroll"), ("space", "page"), ("g/G", "top/bottom"), ("esc", "back")],
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

    fn draw_home(&mut self, f: &mut Frame, area: Rect) {
        let big = area.height >= 30 && area.width >= 40;
        let header_h = if big { 9 } else { 3 };
        let items = self.menu_items();
        let [header, status, _, menu] = Layout::vertical([
            Constraint::Length(header_h),
            Constraint::Length(7),
            Constraint::Length(1),
            Constraint::Length(items.len() as u16 + 2),
        ])
        .areas(area);

        let mut lines: Vec<Line> = Vec::new();
        if big {
            lines.push(Line::raw(""));
            for (row, color) in BANNER.iter().zip(BANNER_COLORS) {
                lines.push(Line::styled(*row, Style::default().fg(Color::Indexed(color))));
            }
        } else {
            lines.push(Line::styled("brief", Style::default().fg(ACCENT).bold()));
        }
        let agent = crate::agent::Agent::from_id(&self.store.config().agent).name();
        lines.push(Line::styled(format!("your agents log the work · {agent} writes you a brief each morning"), Style::default().fg(DIM)));
        f.render_widget(Paragraph::new(lines), header);

        let s = &self.status;
        let cfg = self.store.config();
        let spin = self.spinner();
        let row = |ok: Option<bool>, label: &str, value: String, value_style: Style| -> Line {
            let (dot, color) = match ok {
                Some(true) => ("●", GOOD),
                Some(false) => ("○", DIM),
                None => (spin, ACCENT),
            };
            Line::from(vec![
                Span::styled(format!("{dot} "), Style::default().fg(color)),
                Span::styled(format!("{label:<16}"), Style::default().fg(DIM)),
                Span::styled(value, value_style),
            ])
        };
        let plain = Style::default();
        let today_line = if self.busy() {
            row(None, "today's brief", "writing…".into(), Style::default().fg(ACCENT))
        } else if let Some(h) = &s.today {
            row(Some(true), "today's brief", truncate(h, width_left(area)), plain)
        } else {
            row(Some(false), "today's brief", format!("not written yet · scheduled after {:02}:00", cfg.brief_hour), Style::default().fg(DIM))
        };
        let service = if s.server_up {
            row(Some(true), "background", format!("running · http://127.0.0.1:{}", cfg.port), plain)
        } else if s.service_installed {
            row(Some(false), "background", format!("installed but not responding · see {}", self.store.root.join("logs").display()), Style::default().fg(BAD))
        } else {
            row(Some(false), "background", "not running".into(), Style::default().fg(DIM))
        };
        let goals = row(Some(s.goals > 0), "objectives", match s.goals {
            0 => "not set".into(),
            1 => "1 objective".into(),
            n => format!("{n} objectives"),
        }, plain);
        let journal = row(
            Some(s.entries > 0),
            "journal",
            match &s.last_entry {
                Some(e) => truncate(&format!("{} entries · last: {} · {}", s.entries, e.project, ago(e.ts)), width_left(area)),
                None => "empty · connect a chatbot".into(),
            },
            plain,
        );
        let apps: Vec<&str> = [("Todoist", s.todoist.is_some()), ("Granola", s.granola.is_some()), ("Calendar", s.calendar), ("Browser", s.chrome_seen)]
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(n, _)| n)
            .collect();
        let todo = if apps.is_empty() {
            row(Some(false), "apps", "none · Setup → What the brief reads".into(), Style::default().fg(DIM))
        } else {
            row(Some(true), "apps", format!("{} · read for the brief", apps.join(", ")), plain)
        };
        f.render_widget(
            Paragraph::new(vec![today_line, service, goals, journal, todo])
                .block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(LINE)).padding(Padding::horizontal(1))),
            status,
        );

        let label_w = items.iter().map(|(_, l, _)| l.chars().count()).max().unwrap_or(0) + 3;
        let hint_w = (menu.width as usize).saturating_sub(label_w + 6);
        let list = List::new(items.iter().enumerate().map(|(i, (_, label, hint))| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", i + 1), Style::default().fg(FAINT)),
                Span::raw(format!("{label:<label_w$}")),
                Span::styled(truncate(hint, hint_w), Style::default().fg(DIM)),
            ]))
        }))
        .highlight_symbol(" › ")
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
        .block(Block::default().padding(Padding::vertical(1)));
        f.render_stateful_widget(list, menu, &mut self.menu);
    }

    fn draw_objectives(&mut self, f: &mut Frame, area: Rect) {
        let [title, help, list] = Layout::vertical([Constraint::Length(2), Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        f.render_widget(screen_title("Objectives"), title);
        f.render_widget(
            Paragraph::new(Line::styled(
                "One box per objective. First line is the goal; add notes underneath with ⌥⏎. Press i to talk them through with your agent instead.",
                Style::default().fg(DIM),
            ))
            .wrap(Wrap { trim: true }),
            help,
        );

        // Text width inside a card: borders (2) + padding (2), minus one column for the cursor.
        self.wrap_w = (list.width as usize).saturating_sub(5).max(8);
        let w = self.wrap_w;
        let heights: Vec<u16> = self.objs.iter().map(|e| e.rows(w).len() as u16 + 2).chain([3]).collect();

        // Keep the selected card on screen.
        self.ofirst = self.ofirst.min(self.osel);
        while self.ofirst < self.osel && heights[self.ofirst..=self.osel].iter().sum::<u16>() > list.height {
            self.ofirst += 1;
        }

        let mut y = list.y;
        for (i, &h) in heights.iter().enumerate().skip(self.ofirst) {
            if y >= list.bottom() {
                break;
            }
            let rect = Rect::new(list.x, y, list.width, h.min(list.bottom() - y));
            let selected = i == self.osel;
            let border = Style::default().fg(if selected { ACCENT } else { LINE });

            if i == self.objs.len() {
                let label = if selected { "+ add an objective  (⏎)" } else { "+ add an objective" };
                f.render_widget(
                    Paragraph::new(Line::styled(label, Style::default().fg(if selected { ACCENT } else { FAINT })))
                        .block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(if selected { ACCENT } else { LINE })).padding(Padding::horizontal(1))),
                    rect,
                );
                break;
            }

            let ed = &self.objs[i];
            let title_end = ed.title_end();
            let lines: Vec<Line> = ed
                .rows(w)
                .into_iter()
                .map(|r| {
                    let style = if r.0 < title_end || (r.0 == 0 && r.1 == 0) { Style::default().bold() } else { Style::default().fg(DIM) };
                    Line::styled(ed.row_text(r), style)
                })
                .collect();
            let number = Span::styled(format!(" {} ", i + 1), Style::default().fg(if selected { ACCENT } else { FAINT }));
            let mut block = Block::bordered().border_type(BorderType::Rounded).border_style(border).padding(Padding::horizontal(1)).title(number);
            if selected && self.editing {
                block = block.title_bottom(Line::styled(" editing ", Style::default().fg(ACCENT)).right_aligned());
            }
            f.render_widget(Paragraph::new(lines).block(block), rect);

            if selected && self.editing {
                let (r, c) = ed.cursor_rc(w);
                let pos = Position::new(rect.x + 2 + c as u16, rect.y + 1 + r as u16);
                if rect.contains(pos) {
                    f.set_cursor_position(pos);
                }
            }
            y += h;
        }
    }

    fn draw_journal(&mut self, f: &mut Frame, area: Rect) {
        let [title, body] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        f.render_widget(screen_title("Journal"), title);
        let entries = self.store.entries();
        let mut lines: Vec<Line> = Vec::new();
        if entries.is_empty() {
            lines.push(Line::styled("Nothing logged yet. Connect a chatbot from the main menu.", Style::default().fg(DIM)));
        }
        let mut day = String::new();
        for e in entries.iter().rev() {
            let d = e.ts.format("%A, %B %-d").to_string();
            if d != day {
                if !day.is_empty() {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::styled(d.clone(), Style::default().fg(DIM).add_modifier(Modifier::BOLD)));
                day = d;
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", e.ts.format("%H:%M")), Style::default().fg(DIM)),
                Span::styled(e.project.clone(), Style::default().fg(ACCENT)),
                Span::raw(" "),
                Span::styled(e.kind.clone(), Style::default().fg(kind_color(&e.kind))),
                Span::styled(format!("  via {}", e.source), Style::default().fg(FAINT)),
            ]));
            lines.push(Line::raw(format!("      {}", e.summary)));
        }
        // Clamp on the wrapped height so long summaries can still be scrolled into view.
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        self.clamp_scroll(para.line_count(body.width), body.height);
        f.render_widget(para.scroll((self.scroll, 0)), body);
    }

    /// A row's name, state, state colour and what ⏎ does.
    fn row_info(&self, conn: Conn) -> (String, String, Color, &'static str) {
        let s = &self.status;
        let cfg = self.store.config();
        let spin = self.spinner();
        let onoff = |name: &str, on: bool| -> (String, String, Color, &'static str) {
            (name.into(), if on { "connected".into() } else { "not connected".into() }, if on { GOOD } else { DIM }, if on { "⏎ disconnect" } else { "⏎ connect" })
        };
        let cli = |name: &str, (installed, on): (bool, bool)| -> (String, String, Color, &'static str) {
            if !installed {
                (name.into(), "not installed".into(), FAINT, "")
            } else {
                (name.into(), if on { "connected".into() } else { "not connected".into() }, if on { GOOD } else { DIM }, if on { "⏎ disconnect" } else { "⏎ connect" })
            }
        };
        match conn {
            Conn::Agent => {
                let a = crate::agent::Agent::from_id(&cfg.agent);
                ("Agent".into(), a.name().into(), if a.find().is_some() { GOOD } else { BAD }, "⏎ change agent")
            }
            Conn::Model => ("Model".into(), crate::agent::Agent::from_id(&cfg.agent).model_label(&cfg.model), ACCENT, "←→ change"),
            Conn::Hour => ("Brief after".into(), format!("{:02}:00", cfg.brief_hour), ACCENT, "←→ change"),
            Conn::Service => (
                "Background service".into(),
                if s.server_up { "running".into() } else if s.service_installed { "not responding".into() } else { "off".into() },
                if s.server_up { GOOD } else if s.service_installed { BAD } else { DIM },
                if s.service_installed { "⏎ stop" } else { "⏎ start at login" },
            ),
            Conn::ClaudeCode if !s.code_installed => ("Claude Code".into(), "not installed".into(), FAINT, ""),
            Conn::ClaudeCode => onoff("Claude Code", s.code_connected),
            Conn::ClaudeDesktop => (
                "Claude Desktop".into(),
                if s.desktop_extension {
                    "installed".into()
                } else if s.desktop_config_entry {
                    "old dev setup".into()
                } else if s.desktop_installed {
                    "not connected".into()
                } else {
                    "not installed".into()
                },
                if s.desktop_extension { GOOD } else if s.desktop_config_entry { ACCENT } else { DIM },
                if s.desktop_extension { "⏎ reinstall" } else { "⏎ install" },
            ),
            Conn::Codex => cli("Codex", s.codex),
            Conn::Cline => cli("Cline", s.cline),
            Conn::OpenCode => cli("OpenCode", s.opencode),
            Conn::Tunnel => {
                let (state, color) = if !s.tailscale {
                    ("needs Tailscale".into(), DIM)
                } else if !s.tunnel_on {
                    ("off".into(), DIM)
                } else if s.tunnel_url.is_some() {
                    ("live".into(), GOOD)
                } else if s.server_up {
                    (format!("{spin} starting"), ACCENT)
                } else {
                    ("needs the service".into(), BAD)
                };
                ("Public link".into(), state, color, if !s.tailscale { "⏎ get Tailscale" } else if s.tunnel_on { "⏎ turn off" } else { "⏎ turn on" })
            }
            Conn::ChatGpt | Conn::ClaudeAi | Conn::Grok => (
                match conn {
                    Conn::ChatGpt => "ChatGPT",
                    Conn::ClaudeAi => "claude.ai & phone",
                    _ => "Grok",
                }
                .into(),
                if s.tunnel_url.is_some() { "ready to add".into() } else { "needs public link".into() },
                if s.tunnel_url.is_some() { ACCENT } else { FAINT },
                if s.tunnel_url.is_some() { "⏎ copy link & open settings" } else { "" },
            ),
            Conn::Calendar => onoff("Calendar", s.calendar),
            Conn::Todoist if s.todoist_read_only => ("Todoist".into(), "read-only".into(), ACCENT, "⏎ allow changes"),
            Conn::Todoist => onoff("Todoist", s.todoist.is_some()),
            Conn::Granola => onoff("Granola", s.granola.is_some()),
            Conn::Browser => (
                "Browser extension".into(),
                if s.chrome_seen { "sending".into() } else if s.chrome_installed { "not loaded yet".into() } else { "not installed".into() },
                if s.chrome_seen { GOOD } else if s.chrome_installed { ACCENT } else { DIM },
                if s.chrome_seen { "⏎ update" } else { "⏎ install" },
            ),
            Conn::Anki => ("Anki".into(), if s.anki { "connected".into() } else { "not running".into() }, if s.anki { GOOD } else { DIM }, if s.anki { "" } else { "⏎ open Anki" }),
            Conn::Composio => onoff("Composio", s.composio),
            Conn::Search(p) => {
                let name = if p == search::Provider::Parallel { "Parallel ★".to_owned() } else { p.name().to_owned() };
                if s.search == Some(p) {
                    (name, "in use".into(), GOOD, if p == search::Provider::Claude { "" } else { "⏎ test again" })
                } else if search::ready(&self.store, p) {
                    (name, "ready".into(), DIM, "⏎ use it")
                } else if p == search::Provider::Parallel {
                    (name, "not signed in".into(), DIM, "⏎ sign in with your browser")
                } else {
                    (name, "needs a key".into(), DIM, "⏎ paste a key")
                }
            }
            Conn::Videos => (
                "Videos".into(),
                if s.manimgx { "on (ManimGX)".into() } else { "off".into() },
                if s.manimgx { GOOD } else { DIM },
                if s.manimgx { "" } else { "⏎ install ManimGX" },
            ),
        }
    }

    /// What the selected row does, for the box under a panel.
    fn detail(&self, conn: Conn) -> Vec<Line<'static>> {
        let s = &self.status;
        let dim = Style::default().fg(DIM);
        let good = Style::default().fg(GOOD);
        let port = self.store.config().port;
        let url_line = |url: &Option<String>| -> Line<'static> {
            match url {
                Some(u) => Line::from(vec![Span::styled("Link  ", dim), Span::styled(redact(u), Style::default().fg(ACCENT)), Span::styled("   c copy", Style::default().fg(FAINT))]),
                None => Line::raw(""),
            }
        };
        match conn {
            Conn::Agent => vec![
                Line::raw("The coding agent that writes your brief every morning and runs your objectives conversation, on your own account: Claude Code, Codex, Cline or OpenCode."),
                Line::raw(""),
                Line::styled("⏎ opens the agent picker: it shows which are installed and signed in, and can install or sign one in.", dim),
            ],
            Conn::Model => vec![Line::raw("The model your agent writes the brief with. The first one is the recommended one."), Line::styled("←→ to change.", dim)],
            Conn::Hour => vec![Line::raw("The brief is written after this hour, local time. If your Mac was asleep, it catches up when it wakes."), Line::styled("←→ to change.", dim)],
            Conn::Service => vec![
                Line::raw("Runs brief in the background at login: it writes the brief every morning, serves your briefs at http://127.0.0.1, and keeps your chatbots' log."),
                Line::raw(""),
                Line::styled(format!("Your data lives in {}.", tilde(&self.store.root)), dim),
            ],
            Conn::ClaudeCode => vec![Line::raw("Adds brief to Claude Code for all your projects. New sessions can log your work and read your objectives.")],
            Conn::ClaudeDesktop => vec![
                Line::raw("Installs brief as a Claude Desktop extension, with its logo and description. Claude Desktop opens an install dialog: click Install."),
                Line::styled("Remove it in Claude Desktop → Settings → Extensions.", dim),
            ],
            Conn::Codex => vec![Line::raw("Adds brief to Codex's MCP servers (codex mcp add), so Codex logs your work.")],
            Conn::Cline => vec![Line::raw("Adds brief to Cline's MCP servers, for the CLI and the editor extensions.")],
            Conn::OpenCode => vec![Line::raw("Adds brief to OpenCode's config as a local MCP server.")],
            Conn::Tunnel => vec![
                Line::raw("ChatGPT, claude.ai and Grok run in the cloud, so they can't reach your Mac directly. This gives brief's MCP endpoint, and only that, a permanent https link through Tailscale Funnel, locked with a secret key."),
                Line::raw(""),
                Line::styled("Needs Tailscale running and signed in. The first time, a Tailscale page opens where you allow Funnel.", dim),
                url_line(&s.tunnel_url),
            ],
            Conn::ChatGpt => vec![
                Line::raw("Copies your link and opens ChatGPT's connector settings. Turn on Developer mode, create a connector named “brief”, paste the link and choose “No authentication” (the key is in the link)."),
                url_line(&s.tunnel_url),
            ],
            Conn::ClaudeAi => vec![
                Line::raw("Copies your link and opens claude.ai's connector settings. Add a custom connector named “brief” and paste the link. It then works in the phone apps too."),
                url_line(&s.tunnel_url),
            ],
            Conn::Grok => vec![
                Line::raw("Copies your link and opens Grok's connectors. Add a custom connector named “brief”, paste the link, no authentication."),
                Line::styled("Custom connectors may need a paid Grok plan.", dim),
                url_line(&s.tunnel_url),
            ],
            Conn::Calendar => vec![
                Line::raw("Where your time went, today's meetings and free blocks, and the week ahead, from every account in the Calendar app."),
                if s.calendar { Line::styled("Connected.", good) } else { Line::styled("⏎: macOS asks whether brief may see your calendar; click Allow. Needs the Xcode command line tools.", dim) },
            ],
            Conn::Todoist => vec![
                Line::raw("Your to-do list: projects, what's overdue or due soon, what you finished. Chatbots can add, reschedule or tick off to-dos when you ask."),
                match &s.todoist {
                    Some(who) if !who.is_empty() => Line::styled(format!("Connected as {who}."), good),
                    Some(_) => Line::styled("Connected.", good),
                    None => Line::styled("⏎: Todoist opens in your browser, you click Agree. No token to copy.", dim),
                },
            ],
            Conn::Granola => vec![
                Line::raw("Your Granola meeting notes: promises and follow-ups nobody wrote down, advice that changes a plan."),
                match &s.granola {
                    Some(who) if !who.is_empty() => Line::styled(format!("Connected as {who}."), good),
                    Some(_) => Line::styled("Connected.", good),
                    None => Line::styled("⏎: Granola opens in your browser; sign in and allow brief.", dim),
                },
            ],
            Conn::Browser => vec![
                Line::raw(format!("An extension for {} that tells the brief what you read: time per site and page, and what the pages you stayed on were about.", browsing::browser_name())),
                if s.chrome_seen {
                    Line::styled("Installed and sending.", good)
                } else {
                    Line::styled("⏎: the extensions page opens and the folder path is copied. Developer mode, Load unpacked, ⌘⇧G, paste, ⏎. Once only.", dim)
                },
            ],
            Conn::Anki => vec![
                Line::raw("Your flashcards: what's due, what you keep forgetting. Chatbots can add cards after a reviewer checks them."),
                Line::styled("Needs the AnkiConnect add-on: in Anki, Tools → Add-ons → Get Add-ons…, code 2055492159.", dim),
            ],
            Conn::Composio => vec![
                Line::raw("1,000+ more apps (Gmail, Slack, Notion, GitHub, Linear…) through Composio. The brief writer reads them with Composio's CLI, read-only."),
                if s.composio { Line::styled("Connected. ⏎ turns it off; add apps with `composio link <app>`.", good) } else { Line::styled("⏎ installs Composio's CLI if needed, signs you in (a browser page opens) and asks which apps to link.", dim) },
            ],
            Conn::Search(p) => {
                let mut t = vec![Line::raw(p.blurb().to_owned())];
                t.push(Line::raw(""));
                t.push(match p {
                    search::Provider::Parallel if search::key(&self.store, p).is_some() => Line::styled("Using your API key.", good),
                    search::Provider::Parallel if search::parallel_signed_in() => Line::styled("Signed in with parallel-cli.", good),
                    search::Provider::Parallel => Line::styled("⏎ signs you in with your browser, no key to copy (installs Parallel's CLI if needed). k pastes an API key instead.", dim),
                    search::Provider::Claude => Line::styled("Nothing to set up.", dim),
                    _ if search::key(&self.store, p).is_some() => Line::styled("Key saved.", good),
                    _ => Line::styled(format!("Needs an API key from {}. ⏎ to paste it.", p.key_url().unwrap_or_default()), dim),
                });
                if s.search == Some(p) {
                    t.push(Line::styled("Your brief researches with this.", good));
                }
                t
            }
            Conn::Videos => vec![
                Line::raw("Some items come with a short animation (made with ManimGX) that shows how a paper or idea works, in a few steps you click through."),
                if s.manimgx { Line::styled("Installed: your briefs have videos.", good) } else { Line::styled(format!("⏎ installs it: {}", video::INSTALL), dim) },
            ],
        }
        .into_iter()
        .chain(matches!(conn, Conn::ClaudeCode | Conn::ClaudeDesktop | Conn::Codex | Conn::Cline | Conn::OpenCode).then(|| Line::styled(format!("Other MCP apps on this Mac can use http://127.0.0.1:{port}/mcp"), Style::default().fg(FAINT))))
        .collect()
    }

    fn draw_setup(&mut self, f: &mut Frame, area: Rect) {
        let [title, help, top, bottom] = Layout::vertical([Constraint::Length(2), Constraint::Length(2), Constraint::Length(11), Constraint::Length(9)]).areas(area);
        f.render_widget(screen_title("Setup"), title);
        f.render_widget(Paragraph::new(Line::styled("Everything brief uses, by section. Pick one and press ⏎.", Style::default().fg(DIM))), help);
        let [a, b] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).spacing(1).areas(top);
        let [c, d] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).spacing(1).areas(bottom);
        for (i, cell) in [a, b, c, d].into_iter().enumerate() {
            let sec = SECS[i];
            let on = i == self.psel;
            let w = cell.width.saturating_sub(4) as usize;
            let lines: Vec<Line> = sec
                .rows()
                .into_iter()
                .map(|conn| {
                    let (name, state, color, _) = self.row_info(conn);
                    let dot = if self.job.as_ref().is_some_and(|(c, _)| *c == conn) { self.spinner() } else if color == GOOD { "●" } else { "○" };
                    let name_w = w.saturating_sub(state.chars().count() + 3).min(20);
                    Line::from(vec![
                        Span::styled(format!("{dot} "), Style::default().fg(color)),
                        Span::raw(format!("{:<name_w$} ", truncate(&name, name_w))),
                        Span::styled(truncate(&state, w.saturating_sub(name_w + 3)), Style::default().fg(color)),
                    ])
                })
                .collect();
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(if on { ACCENT } else { LINE }))
                .title(Line::from(vec![
                    Span::styled(format!(" {} ", i + 1), Style::default().fg(FAINT)),
                    Span::styled(format!("{} ", sec.title()), if on { Style::default().fg(ACCENT).bold() } else { Style::default().bold() }),
                ]))
                .padding(Padding::horizontal(1));
            f.render_widget(Paragraph::new(lines).block(block), cell);
        }
    }

    fn draw_panel(&mut self, f: &mut Frame, area: Rect, sec: Sec) {
        let rows = sec.rows();
        self.rsel = self.rsel.min(rows.len() - 1);
        let [title, help, list, _, detail] =
            Layout::vertical([Constraint::Length(2), Constraint::Length(3), Constraint::Length(rows.len() as u16 * 2), Constraint::Length(1), Constraint::Max(11)]).areas(area);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("brief ", Style::default().fg(DIM)),
                Span::styled("› ", Style::default().fg(FAINT)),
                Span::styled("Setup ", Style::default().fg(DIM)),
                Span::styled("› ", Style::default().fg(FAINT)),
                Span::styled(sec.title(), Style::default().fg(ACCENT).bold()),
            ])),
            title,
        );
        f.render_widget(Paragraph::new(Line::styled(sec.blurb(), Style::default().fg(DIM))).wrap(Wrap { trim: false }), help);
        let spin = self.spinner();
        let mut lines: Vec<Line> = Vec::new();
        for (i, conn) in rows.iter().enumerate() {
            let selected = i == self.rsel;
            let working = self.job.as_ref().is_some_and(|(c, _)| c == conn);
            let (name, state, color, action) = self.row_info(*conn);
            let dot = if working { spin } else if color == GOOD { "●" } else { "○" };
            let name_style = if selected { Style::default().fg(ACCENT).bold() } else { Style::default() };
            lines.push(Line::from(vec![
                Span::styled(if selected { " › " } else { "   " }, Style::default().fg(ACCENT)),
                Span::styled(format!("{name:<22}"), name_style),
                Span::styled(format!("{dot} "), Style::default().fg(if working { ACCENT } else { color })),
                Span::styled(format!("{:<20}", if working { "working…".into() } else { state }), Style::default().fg(if working { ACCENT } else { color })),
                Span::styled(if selected { action } else { "" }, Style::default().fg(DIM)),
            ]));
            lines.push(Line::raw(""));
        }
        f.render_widget(Paragraph::new(lines), list);

        let boxed = |text: Vec<Line<'static>>| {
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(LINE)).padding(Padding::horizontal(1)))
        };
        if let Some((p, ed)) = &self.key_input {
            let key = ed.text();
            let n = key.chars().count();
            let shown = if key.is_empty() {
                Line::styled("paste your key", Style::default().fg(FAINT))
            } else {
                Line::raw(format!("{}{}", "•".repeat(n.saturating_sub(4).min(40)), key.chars().skip(n.saturating_sub(4)).collect::<String>()))
            };
            let [input, info] = Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(detail);
            f.render_widget(
                Paragraph::new(shown).block(
                    Block::bordered()
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(ACCENT))
                        .title(format!(" {} API key ", p.name()))
                        .padding(Padding::horizontal(1)),
                ),
                input,
            );
            f.set_cursor_position(Position::new(input.x + 2 + n.min(44) as u16, input.y + 1));
            f.render_widget(
                Paragraph::new(vec![
                    Line::styled(format!("Get one at {} (ctrl-o opens it), paste it and press ⏎.", p.key_url().unwrap_or_default()), Style::default().fg(DIM)),
                    Line::styled("It's kept in search.json in brief's folder, readable only by you.", Style::default().fg(FAINT)),
                ])
                .wrap(Wrap { trim: false }),
                info,
            );
        } else {
            f.render_widget(boxed(self.detail(rows[self.rsel])), detail);
        }
    }

    fn clamp_scroll(&mut self, lines: usize, height: u16) {
        let max = (lines as u16).saturating_sub(height.saturating_sub(1));
        self.scroll = self.scroll.min(max);
    }
}

fn screen_title(t: &str) -> Paragraph<'_> {
    Paragraph::new(Text::from(Line::from(vec![
        Span::styled("brief ", Style::default().fg(DIM)),
        Span::styled("› ", Style::default().fg(FAINT)),
        Span::styled(t, Style::default().fg(ACCENT).bold()),
    ])))
}

/// Show a link without its secret key.
fn redact(url: &str) -> String {
    match url.split_once("key=") {
        Some((head, key)) if key.len() > 4 => format!("{head}key={}…", &key[..4]),
        _ => url.to_owned(),
    }
}

fn kind_color(kind: &str) -> Color {
    match kind {
        "blocker" => BAD,
        "decision" | "progress" => GOOD,
        "idea" => Color::Indexed(141),
        "learning" | "question" => Color::Indexed(75),
        _ => DIM,
    }
}

fn width_left(area: Rect) -> usize {
    area.width.saturating_sub(24) as usize
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn ago(t: chrono::DateTime<Local>) -> String {
    let m = (Local::now() - t).num_minutes();
    match m {
        ..=0 => "just now".into(),
        1..=59 => format!("{m}m ago"),
        60..=1439 => format!("{}h ago", m / 60),
        _ => format!("{}d ago", m / 1440),
    }
}

fn tilde(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    match dirs::home_dir().map(|h| h.display().to_string()) {
        Some(h) if s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// Tiny HTTP/1.1 client for talking to our own server on localhost.
fn http(port: u16, method: &str, path: &str) -> Option<String> {
    let mut s = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(200)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    write!(s, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    buf.split_once("\r\n\r\n").map(|(_, body)| body.to_owned())
}
