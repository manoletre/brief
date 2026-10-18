//! `brief` with no arguments: a full-screen terminal app.

use crate::{browsing, calendar, connect, editor::Editor, generate, store::{Entry, Store}, granola, todoist};
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
const ACCENT: Color = Color::Indexed(208);
const DIM: Color = Color::Indexed(244);
const FAINT: Color = Color::Indexed(240);
const LINE: Color = Color::Indexed(238);
const GOOD: Color = Color::Indexed(114);
const BAD: Color = Color::Indexed(203);
const BANNER_COLORS: [u8; 6] = [202, 202, 208, 208, 214, 214];
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];

const BANNER: [&str; 6] = [
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
    Settings,
    Connect,
}

#[derive(Clone, Copy)]
enum Action {
    ReadBrief,
    Generate,
    Objectives,
    Journal,
    Connect,
    Settings,
    Service,
    Quit,
}

#[derive(Clone, Copy, PartialEq)]
enum Conn {
    ClaudeCode,
    ClaudeDesktop,
    Tunnel,
    ChatGpt,
    ClaudeAi,
    Grok,
    Todoist,
    Granola,
    Calendar,
    Browser,
}
const CONNS: [Conn; 10] = [
    Conn::ClaudeCode,
    Conn::ClaudeDesktop,
    Conn::Tunnel,
    Conn::ChatGpt,
    Conn::ClaudeAi,
    Conn::Grok,
    Conn::Todoist,
    Conn::Granola,
    Conn::Calendar,
    Conn::Browser,
];

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
}

type Job = mpsc::Receiver<Result<String, String>>;

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
    // Journal / settings / connect
    scroll: u16,
    settings_row: usize,
    csel: usize,
    job: Option<(Conn, Job)>,
}

pub fn run(store: Store, start_in_editor: bool) -> Result<()> {
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
        settings_row: 0,
        csel: 0,
        job: None,
    };
    app.refresh();
    if start_in_editor || app.status.goals == 0 {
        app.open_objectives();
        if app.status.server_up {
            app.open_interview();
        }
    }
    let mut terminal = ratatui::init();
    // Alternate scroll mode: the mouse wheel arrives as ↑/↓ keys, without capturing
    // the mouse, so text selection keeps working.
    let _ = std::io::stdout().write_all(b"\x1b[?1007h").and_then(|_| std::io::stdout().flush());
    let result = app.event_loop(&mut terminal);
    let _ = std::io::stdout().write_all(b"\x1b[?1007l").and_then(|_| std::io::stdout().flush());
    ratatui::restore();
    result
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
            (Action::Generate, "Write today's brief now".into(), "1–3 minutes, uses your Claude subscription".into()),
            (Action::Objectives, "Objectives".into(), match s.goals {
                0 => "not set yet · start here".into(),
                1 => "1 objective".into(),
                n => format!("{n} objectives"),
            }),
            (Action::Journal, "Journal".into(), format!("{} entries from your chatbots", s.entries)),
            (Action::Connect, "Connect chatbots & apps".into(), match connected {
                0 => "nothing connected yet".into(),
                n => format!("{n} connected"),
            }),
            (Action::Settings, "Settings".into(), format!("brief at {:02}:00 · {}", cfg.brief_hour, cfg.model)),
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
            Screen::Objectives => self.objectives_key(k),
            Screen::Connect => self.connect_key(k),
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
            Screen::Settings => self.settings_key(k),
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
            Action::Connect => self.screen = Screen::Connect,
            Action::Settings => {
                self.settings_row = 0;
                self.screen = Screen::Settings;
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

    /// The objectives interview runs on the briefs website, where it can be a real conversation.
    fn open_interview(&mut self) {
        if self.status.server_up {
            connect::open(&format!("http://127.0.0.1:{}/onboarding", self.store.config().port));
            self.say("The interview opened in your browser. Your objectives here update when you save there.", false);
        } else {
            self.say("The interview needs the background service: Home → start it, or run `brief init`.", true);
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

    fn objectives_key(&mut self, k: KeyEvent) {
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
            KeyCode::Char('i') => self.open_interview(),
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

    // ── connect ──────────────────────────────────────────────

    fn connect_key(&mut self, k: KeyEvent) {
        let conn = CONNS[self.csel];
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Backspace => self.screen = Screen::Home,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.csel = (self.csel + 1) % CONNS.len(),
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => self.csel = (self.csel + CONNS.len() - 1) % CONNS.len(),
            KeyCode::Char('c') => {
                let url = self.status.tunnel_url.clone().unwrap_or_else(|| format!("http://127.0.0.1:{}/mcp", self.store.config().port));
                match connect::copy(&url) {
                    Ok(()) => self.say(format!("Copied {}", redact(&url)), false),
                    Err(e) => self.say(format!("Couldn't copy: {e}"), true),
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                if self.job.is_some() {
                    self.say("One moment, still working on the last step…", false);
                } else {
                    self.connect_action(conn);
                }
            }
            _ => {}
        }
    }

    fn connect_action(&mut self, conn: Conn) {
        let s = &self.status;
        match conn {
            Conn::ClaudeCode if !s.code_installed => self.say("Claude Code isn't installed on this Mac.", true),
            Conn::ClaudeCode => {
                let on = !s.code_connected;
                self.start_job(conn, move || connect::set_claude_code(on));
            }
            Conn::ClaudeDesktop => {
                let store = self.store.clone();
                self.start_job(conn, move || connect::install_claude_desktop(&store));
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
        }
    }

    fn settings_key(&mut self, k: KeyEvent) {
        let mut cfg = self.store.config();
        let delta: i32 = match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                self.screen = Screen::Home;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab | KeyCode::Up | KeyCode::Char('k') => {
                self.settings_row = (self.settings_row + 1) % 2;
                return;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter | KeyCode::Char(' ') => 1,
            KeyCode::Left | KeyCode::Char('h') => -1,
            _ => return,
        };
        match self.settings_row {
            0 => cfg.brief_hour = (cfg.brief_hour as i32 + delta).rem_euclid(24) as u32,
            _ => {
                let i = MODELS.iter().position(|m| *m == cfg.model).unwrap_or(0) as i32;
                cfg.model = MODELS[(i + delta).rem_euclid(MODELS.len() as i32) as usize].into();
            }
        }
        match self.store.save_config(&cfg) {
            Ok(()) => self.say("Saved", false),
            Err(e) => self.say(format!("Couldn't save: {e}"), true),
        }
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
            Screen::Settings => self.draw_settings(f, body),
            Screen::Connect => self.draw_connect(f, body),
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
            Screen::Objectives => &[("↑↓", "move"), ("⏎", "edit"), ("n", "new"), ("d", "delete"), ("⇧↑↓", "reorder"), ("esc", "back")],
            Screen::Settings => &[("↑↓", "choose"), ("←→", "change"), ("esc", "back")],
            Screen::Connect => &[("↑↓", "move"), ("⏎", "connect / disconnect"), ("c", "copy link"), ("esc", "back")],
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
        lines.push(Line::styled("your chatbots log the work · Claude writes you a brief each morning", Style::default().fg(DIM)));
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
            row(Some(false), "background", "installed but not responding · see ~/.brief/logs".into(), Style::default().fg(BAD))
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
            row(Some(false), "apps", "none · Connect → Todoist, Granola, Calendar, Browser".into(), Style::default().fg(DIM))
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
                "One box per objective. First line is the goal; add notes underneath with ⌥⏎. Press i to let Claude interview you instead.",
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

    fn draw_connect(&mut self, f: &mut Frame, area: Rect) {
        let [title, help, rows, _, detail] =
            Layout::vertical([Constraint::Length(2), Constraint::Length(2), Constraint::Length(CONNS.len() as u16 * 2 + 2), Constraint::Length(1), Constraint::Fill(1)])
                .areas(area);
        f.render_widget(screen_title("Connect"), title);
        f.render_widget(Paragraph::new(Line::styled("Pick one and press ⏎. brief does the setup for you.", Style::default().fg(DIM))), help);

        let s = &self.status;
        let spin = self.spinner();
        let mut lines: Vec<Line> = Vec::new();
        for (i, conn) in CONNS.iter().enumerate() {
            if *conn == Conn::Todoist {
                lines.push(Line::styled("   apps the brief reads", Style::default().fg(FAINT)));
                lines.push(Line::raw(""));
            }
            let selected = i == self.csel;
            let working = self.job.as_ref().is_some_and(|(c, _)| c == conn);
            let (name, state, color, action): (&str, String, Color, &str) = match conn {
                Conn::ClaudeCode => (
                    "Claude Code",
                    if !s.code_installed { "not installed".into() } else if s.code_connected { "connected".into() } else { "not connected".into() },
                    if s.code_connected { GOOD } else { DIM },
                    if !s.code_installed { "" } else if s.code_connected { "⏎ disconnect" } else { "⏎ connect" },
                ),
                Conn::ClaudeDesktop => (
                    "Claude Desktop",
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
                        ("waiting for background service".into(), BAD)
                    };
                    ("Public link", state, color, if !s.tailscale { "⏎ get Tailscale" } else if s.tunnel_on { "⏎ turn off" } else { "⏎ turn on" })
                }
                Conn::ChatGpt | Conn::ClaudeAi | Conn::Grok => (
                    match conn {
                        Conn::ChatGpt => "ChatGPT",
                        Conn::ClaudeAi => "claude.ai, web & phone",
                        _ => "Grok",
                    },
                    if s.tunnel_url.is_some() { "ready to add".into() } else { "needs the public link".into() },
                    if s.tunnel_url.is_some() { ACCENT } else { FAINT },
                    if s.tunnel_url.is_some() { "⏎ copy link & open settings" } else { "" },
                ),
                Conn::Todoist if s.todoist_read_only => ("Todoist", "read-only".into(), ACCENT, "⏎ allow changes"),
                Conn::Browser => (
                    "Browser extension",
                    if s.chrome_seen { "sending".into() } else if s.chrome_installed { "not loaded in Chrome".into() } else { "not installed".into() },
                    if s.chrome_seen { GOOD } else if s.chrome_installed { ACCENT } else { DIM },
                    if s.chrome_seen { "⏎ update" } else { "⏎ install" },
                ),
                Conn::Todoist | Conn::Granola | Conn::Calendar => {
                    let (name, on) = match conn {
                        Conn::Todoist => ("Todoist", s.todoist.is_some()),
                        Conn::Granola => ("Granola", s.granola.is_some()),
                        _ => ("Calendar", s.calendar),
                    };
                    (
                        name,
                        if on { "connected".into() } else { "not connected".into() },
                        if on { GOOD } else { DIM },
                        if on { "⏎ disconnect" } else { "⏎ connect" },
                    )
                }
            };
            let dot = if working { spin } else if color == GOOD { "●" } else { "○" };
            let name_style = if selected { Style::default().fg(ACCENT).bold() } else { Style::default() };
            lines.push(Line::from(vec![
                Span::styled(if selected { " › " } else { "   " }, Style::default().fg(ACCENT)),
                Span::styled(format!("{name:<24}"), name_style),
                Span::styled(format!("{dot} "), Style::default().fg(if working { ACCENT } else { color })),
                Span::styled(format!("{:<20}", if working { "working…".into() } else { state }), Style::default().fg(if working { ACCENT } else { color })),
                Span::styled(if selected { action } else { "" }, Style::default().fg(DIM)),
            ]));
            lines.push(Line::raw(""));
        }
        f.render_widget(Paragraph::new(lines), rows);

        let port = self.store.config().port;
        let url_line = |url: &Option<String>| -> Line {
            match url {
                Some(u) => Line::from(vec![Span::styled("Link  ", Style::default().fg(DIM)), Span::styled(redact(u), Style::default().fg(ACCENT)), Span::styled("   c copy", Style::default().fg(FAINT))]),
                None => Line::raw(""),
            }
        };
        let dim = Style::default().fg(DIM);
        let text: Vec<Line> = match CONNS[self.csel] {
            Conn::ClaudeCode => vec![Line::raw(
                "Adds brief to Claude Code for all your projects. New sessions can log your work and read your objectives.",
            )],
            Conn::ClaudeDesktop => vec![
                Line::raw("Installs brief as a Claude Desktop extension, with its logo and description. Claude Desktop opens an install dialog: click Install."),
                Line::raw("It runs the same `brief` command, so `make` reaches it without reinstalling. Remove it in Claude Desktop → Settings → Extensions."),
            ],
            Conn::Tunnel => vec![
                Line::raw("ChatGPT, claude.ai and Grok run in the cloud, so they can't reach your Mac directly. This gives brief's MCP endpoint, and only that, a permanent https link through Tailscale Funnel, locked with a secret key. Your briefs and journal stay local."),
                Line::raw(""),
                Line::styled("Needs Tailscale running and signed in. The first time, a Tailscale page opens where you allow Funnel. The link stays the same across restarts; it only works while this Mac is awake.", dim),
                Line::raw(""),
                url_line(&s.tunnel_url),
            ],
            Conn::ChatGpt => vec![
                Line::raw("Copies your link and opens ChatGPT's connector settings. There, turn on Developer mode, create a connector named “brief”, paste the link as its URL and choose “No authentication” (the key is in the link)."),
                Line::raw(""),
                url_line(&s.tunnel_url),
            ],
            Conn::ClaudeAi => vec![
                Line::raw("Copies your link and opens claude.ai's connector settings. Add a custom connector named “brief” and paste the link."),
                Line::raw(""),
                Line::styled("It then also works in the phone apps and Claude Desktop, so you won't need the Claude Desktop row.", dim),
                Line::raw(""),
                url_line(&s.tunnel_url),
            ],
            Conn::Grok => vec![
                Line::raw("Copies your link and opens Grok's connectors. Add a new custom connector named “brief” and paste the link as its URL, with no authentication (the key is in the link)."),
                Line::raw(""),
                Line::styled("Custom connectors may need a paid Grok plan.", dim),
                Line::raw(""),
                url_line(&s.tunnel_url),
            ],
            Conn::Todoist => vec![
                Line::raw("Your to-do list. The daily brief reads it: your projects, what's overdue or due soon, and what you finished, tied to your objectives and journal (goals with no tasks, tasks that keep slipping, work you did that no task planned). Your chatbots see it as your to-do list and can add, reschedule or tick off to-dos when you ask them to."),
                Line::raw(""),
                match &s.todoist {
                    Some(who) if !who.is_empty() => Line::styled(format!("Connected as {who}."), Style::default().fg(GOOD)),
                    Some(_) => Line::styled("Connected.", Style::default().fg(GOOD)),
                    None => Line::styled("Press ⏎: Todoist opens in your browser, you click Agree, done. No token to copy.", dim),
                },
                if s.todoist_read_only {
                    Line::styled("Connected read-only, so chatbots can't add or change to-dos yet. Press ⏎ and click Agree to allow it.", Style::default().fg(ACCENT))
                } else {
                    Line::styled("Chatbots change to-dos only when you ask or agree. Run `brief todoist` to see exactly what the brief sees.", dim)
                },
            ],
            Conn::Granola => vec![
                Line::raw("Lets the daily brief read your Granola meeting notes. It catches promises and follow-ups from meetings that aren't in your to-do list or journal, advice that changes a plan, and people who connect to another project."),
                Line::raw(""),
                match &s.granola {
                    Some(who) if !who.is_empty() => Line::styled(format!("Connected as {who}."), Style::default().fg(GOOD)),
                    Some(_) => Line::styled("Connected.", Style::default().fg(GOOD)),
                    None => Line::styled("Press ⏎: Granola opens in your browser, you sign in and allow brief, done.", dim),
                },
                Line::styled("Read-only, through Granola's official connector. Run `brief granola` to see exactly what the brief sees.", dim),
            ],
            Conn::Calendar => vec![
                Line::raw("Lets the daily brief read your calendar: where your time went since the last brief, today's meetings and free blocks, and the week ahead. It weighs that against your objectives, so it can say what to move, drop or protect time for."),
                Line::raw(""),
                if s.calendar {
                    Line::styled("Connected.", Style::default().fg(GOOD))
                } else {
                    Line::styled("Press ⏎: macOS asks whether brief may see your calendar; click Allow. Reads every account in the Calendar app (Google, iCloud, Outlook).", dim)
                },
                Line::styled("Read-only. Needs the Xcode command line tools. Run `brief calendar` to see exactly what the brief sees.", dim),
            ],
            Conn::Browser => vec![
                Line::raw(format!(
                    "An extension for {} that tells the brief what you read and worked on there: time per site and page, and what the pages you stayed on were about. The brief checks that against your objectives, calendar and to-dos.",
                    browsing::browser_name()
                )),
                Line::raw(""),
                if s.chrome_seen {
                    Line::styled("Installed and sending. ⏎ rewrites it from this build of brief; it reloads itself within a minute.", Style::default().fg(GOOD))
                } else {
                    Line::styled("Press ⏎: your browser's extensions page opens and the folder path is copied. Turn on Developer mode, click Load unpacked, press ⌘⇧G, paste, ⏎. Only once: updates are automatic after that.", dim)
                },
                Line::styled("Stays on this Mac. Skips mail, banks and password managers; pause it or block a site from its toolbar button. `brief browser status` shows what it sent.", dim),
            ],
        };
        let mut text = text;
        if !matches!(CONNS[self.csel], Conn::Todoist | Conn::Granola | Conn::Calendar | Conn::Browser) {
            text.push(Line::raw(""));
            text.push(Line::styled(format!("Other MCP apps on this Mac can use http://127.0.0.1:{port}/mcp"), Style::default().fg(FAINT)));
        }
        f.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(LINE)).padding(Padding::horizontal(1))),
            detail,
        );
    }

    fn draw_settings(&mut self, f: &mut Frame, area: Rect) {
        let [title, body] = Layout::vertical([Constraint::Length(2), Constraint::Fill(1)]).areas(area);
        f.render_widget(screen_title("Settings"), title);
        let cfg = self.store.config();
        let rows = [
            ("Write the brief after", format!("{:02}:00", cfg.brief_hour), "local time; catches up if your Mac was asleep"),
            ("Claude model", cfg.model.clone(), "opus is best; sonnet and haiku are lighter on your usage"),
        ];
        let mut lines = Vec::new();
        for (i, (label, value, hint)) in rows.iter().enumerate() {
            let on = i == self.settings_row;
            let arrow = if on { Style::default().fg(ACCENT) } else { Style::default().fg(LINE) };
            lines.push(Line::from(vec![
                Span::styled(if on { " › " } else { "   " }, Style::default().fg(ACCENT)),
                Span::styled(format!("{label:<24}"), if on { Style::default().bold() } else { Style::default() }),
                Span::styled("‹ ", arrow),
                Span::styled(format!("{value:^8}"), if on { Style::default().fg(ACCENT).bold() } else { Style::default() }),
                Span::styled(" ›", arrow),
            ]));
            lines.push(Line::styled(format!("   {hint}"), Style::default().fg(DIM)));
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            format!("   Your data lives in {}. Port ({}) is set in config.json there.", tilde(&self.store.root), cfg.port),
            Style::default().fg(FAINT),
        ));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
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
