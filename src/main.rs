mod anki;
mod browsing;
mod calendar;
mod connect;
mod editor;
mod generate;
mod granola;
mod html;
mod interview;
mod macapp;
mod mcp;
mod oauth;
mod server;
mod store;
mod todoist;
mod todos;
mod tui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use store::Store;

#[derive(Parser)]
#[command(name = "brief", version, about = "Your chatbots log what you work on; Claude writes you a daily brief.")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set your objectives: Claude interviews you in the browser (the app's editor is there too).
    Init,
    /// Run the background server: MCP over HTTP, the briefs website, and the daily scheduler.
    Serve,
    /// Speak MCP over stdin/stdout (for clients that launch the server, e.g. Claude Desktop).
    Mcp,
    /// Write today's brief right now.
    Generate,
    /// Open the briefs website in your browser.
    Open,
    /// Edit your objectives in $EDITOR.
    Objectives,
    /// Show your Anki decks and cards (search with Anki syntax: brief anki 'deck:ML attention').
    Anki { query: Vec<String> },
    /// Connect Todoist (opens its "Allow brief?" page), or show what the brief sees there.
    /// `brief todoist off` disconnects.
    Todoist { action: Option<String> },
    /// Connect Granola (opens its "Allow brief?" page), or show the meeting notes the brief sees.
    /// `brief granola off` disconnects.
    Granola { action: Option<String> },
    /// Connect your calendar (the Mac's Calendar app), or show what the brief sees there.
    /// `brief calendar off` disconnects.
    Calendar { action: Option<String> },
    /// Install or update brief's extension in your browser (Dia, Arc, Chrome…).
    /// `brief browser status` shows what it sends.
    #[command(alias = "chrome")]
    Browser { action: Option<String> },
    /// Log an entry from the terminal: brief log <project> <summary…>
    Log { project: String, summary: Vec<String> },
    /// Print how to connect Claude Desktop, Claude Code, ChatGPT and others.
    Connect,
    /// Build ~/.brief/brief.mcpb (brief as a Claude Desktop extension) and open it in Claude Desktop.
    Extension {
        /// Only build the file.
        #[arg(long)]
        no_open: bool,
    },
    /// Start `brief serve` at login (macOS launchd).
    Install,
    /// Remove the launchd agent.
    Uninstall,
    /// Create (or rotate) the access key needed to reach /mcp through a tunnel.
    Key,
}

#[tokio::main]
async fn main() -> Result<()> {
    let store = Store::open()?;
    let cmd = match Cli::parse().cmd {
        Some(cmd) => cmd,
        None if std::io::stdout().is_terminal() => return app(store, false).await,
        None => {
            <Cli as clap::CommandFactory>::command().print_help()?;
            return Ok(());
        }
    };
    match cmd {
        Cmd::Init => {
            // The interview runs on the briefs website; serve it from here if the service is off.
            let port = store.config().port;
            if !server_up(port) {
                tokio::spawn(server::serve(store.clone()));
                for _ in 0..20 {
                    if server_up(port) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
            app(store, true).await
        }
        Cmd::Serve => server::serve(store).await,
        Cmd::Mcp => mcp::run_stdio(store).await,
        Cmd::Generate => {
            eprintln!("Asking Claude to write today's brief (this can take a few minutes)…");
            let b = generate::generate(&store).await?;
            println!("{}", b.headline);
            println!("Open http://127.0.0.1:{}/brief/{}", store.config().port, b.date.unwrap());
            Ok(())
        }
        Cmd::Open => open_url(&format!("http://127.0.0.1:{}/", store.config().port)),
        Cmd::Objectives => edit_objectives(&store),
        Cmd::Log { project, summary } => {
            let e = store::Entry {
                id: store::new_id(),
                ts: chrono::Local::now(),
                project,
                kind: "note".into(),
                summary: summary.join(" "),
                details: String::new(),
                tags: vec![],
                source: "terminal".into(),
            };
            store.add_entry(&e)?;
            println!("Logged.");
            Ok(())
        }
        Cmd::Anki { query } => {
            let text = tokio::task::spawn_blocking(move || anki::search_text(&query.join(" "), None, 50)).await??;
            print!("{text}");
            Ok(())
        }
        Cmd::Todoist { action } => {
            let app = App { name: "todoist", auth: todoist::auth, connect: todoist::connect, disconnect: todoist::disconnect, context: todoist::brief_context };
            connected_app(store, app, action).await
        }
        Cmd::Granola { action } => {
            let app = App { name: "granola", auth: granola::auth, connect: granola::connect, disconnect: granola::disconnect, context: granola::brief_context };
            connected_app(store, app, action).await
        }
        Cmd::Calendar { action } => {
            let text = tokio::task::spawn_blocking(move || match action.as_deref() {
                Some("off" | "disconnect") => calendar::disconnect(&store),
                Some(other) => anyhow::bail!("unknown action {other:?}; use `brief calendar` or `brief calendar off`"),
                None if !calendar::connected(&store) => {
                    eprintln!("Building the Calendar helper; macOS will ask whether brief may see your calendar…");
                    calendar::connect(&store)
                }
                None => Ok(calendar::preview(&store)),
            })
            .await??;
            println!("{}", text.trim_end());
            Ok(())
        }
        Cmd::Browser { action } => {
            let text = match action.as_deref() {
                None | Some("install") => browsing::install(&store)?,
                // Just rewrite the files (what `make` runs); the extension reloads itself.
                Some("update") if browsing::installed(&store) => match browsing::write_files(&store)? {
                    true => format!("Updated the browser extension to build {}; it reloads within a minute.", browsing::build()),
                    false => "The browser extension is up to date.".into(),
                },
                Some("update") => "The browser extension isn't installed yet. Run `brief browser`.".into(),
                Some("status") => browsing::status_text(&store),
                Some("context") => browsing::brief_context(&store, chrono::Local::now() - chrono::Duration::days(1)),
                Some(other) => anyhow::bail!("unknown action {other:?}; use `brief browser`, `brief browser status` or `brief browser context`"),
            };
            println!("{}", text.trim_end());
            Ok(())
        }
        Cmd::Extension { no_open: true } => {
            println!("{}", connect::build_mcpb(&store)?.display());
            Ok(())
        }
        Cmd::Extension { no_open: false } => {
            println!("{}", connect::install_claude_desktop(&store)?);
            Ok(())
        }
        Cmd::Connect => {
            print!("{}", connect_text(&store));
            Ok(())
        }
        Cmd::Install => Ok(println!("{}", install(&store)?)),
        Cmd::Uninstall => Ok(println!("{}", uninstall()?)),
        Cmd::Key => {
            let mut cfg = store.config();
            let key = new_key();
            cfg.access_key = Some(key.clone());
            store.save_config(&cfg)?;
            let host = cfg.public_url.as_deref().unwrap_or("https://<your-funnel-host>");
            println!("Access key: {key}\nRemote MCP URL: {host}/mcp?key={key}\nRe-paste it in every connector; the old link stops working.");
            Ok(())
        }
    }
}

/// An app brief reads for the brief (Todoist, Granola).
struct App {
    name: &'static str,
    auth: fn(&Store) -> Option<oauth::Auth>,
    connect: fn(&Store) -> Result<String>,
    disconnect: fn(&Store) -> Result<String>,
    context: fn(&Store, chrono::DateTime<chrono::Local>) -> String,
}

/// `brief <app>`: connect if needed, else print what the brief sees. `brief <app> off` disconnects.
async fn connected_app(store: Store, app: App, action: Option<String>) -> Result<()> {
    let text = tokio::task::spawn_blocking(move || match action.as_deref() {
        Some("off" | "disconnect") => (app.disconnect)(&store),
        Some(other) => anyhow::bail!("unknown action {other:?}; use `brief {0}` or `brief {0} off`", app.name),
        None if (app.auth)(&store).is_none() => {
            eprintln!("Opening {} in your browser: allow brief there…", app.name);
            (app.connect)(&store)
        }
        None => Ok((app.context)(&store, chrono::Local::now() - chrono::Duration::days(1))),
    })
    .await??;
    println!("{}", text.trim_end());
    Ok(())
}

async fn app(store: Store, start_in_editor: bool) -> Result<()> {
    tokio::task::spawn_blocking(move || tui::run(store, start_in_editor)).await?
}

fn edit_objectives(store: &Store) -> Result<()> {
    let path = store.root.join("objectives.md");
    if !path.exists() {
        store.save_objectives("")?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
    std::process::Command::new(editor).arg(&path).status().context("could not start $EDITOR")?;
    Ok(())
}

fn server_up(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), std::time::Duration::from_millis(80)).is_ok()
}

fn open_url(url: &str) -> Result<()> {
    std::process::Command::new("open").arg(url).status()?;
    Ok(())
}

fn exe() -> String {
    connect::stable_exe().display().to_string()
}

pub fn new_key() -> String {
    use rand::Rng;
    rand::thread_rng().sample_iter(&rand::distributions::Alphanumeric).take(32).map(char::from).collect()
}

fn gui_domain() -> Result<String> {
    let uid = std::process::Command::new("id").arg("-u").output()?.stdout;
    Ok(format!("gui/{}", String::from_utf8_lossy(&uid).trim()))
}

pub fn restart_service() -> Result<()> {
    let out = std::process::Command::new("launchctl").args(["kickstart", "-k", &format!("{}/{PLIST_LABEL}", gui_domain()?)]).output()?;
    anyhow::ensure!(out.status.success(), "couldn't restart the service: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(())
}

pub fn connect_text(store: &Store) -> String {
    let port = store.config().port;
    let exe = exe();
    format!(
        r#"brief exposes four MCP tools: log_work and get_context (your journal), and
anki_search and anki_add_cards (your Anki; needs the AnkiConnect add-on, code 2055492159).

── Claude Code ─────────────────────────────────────────────
  claude mcp add --scope user brief -- "{exe}" mcp

── Claude Desktop ──────────────────────────────────────────
  Run `brief`, open "Connect chatbots" and press ⏎ on "Claude Desktop": it
  builds ~/.brief/brief.mcpb and opens Claude Desktop's install dialog.
  Or add to ~/Library/Application Support/Claude/claude_desktop_config.json:

  {{ "mcpServers": {{ "brief": {{ "command": "{exe}", "args": ["mcp"] }} }} }}

── Any client that speaks MCP over HTTP, on this machine ───
  http://127.0.0.1:{port}/mcp      (needs `brief serve` running)

── ChatGPT, claude.ai web, Grok, phone apps ────────────────
  These call MCP servers from the cloud, so they can't see localhost.
  Run `brief`, open "Connect chatbots" and press ⏎ on "Public link": it
  publishes only /mcp through Tailscale Funnel, locked with a key, and
  copies the link. Then add it as a custom connector with no authentication
  (ChatGPT: Settings → Connectors → Developer mode; Claude: Settings →
  Connectors; Grok: Connectors → New → Custom).

Tip: the tool descriptions ask assistants to log proactively, but adding
"Log meaningful work to brief" to each assistant's custom instructions helps a lot.
"#
    )
}

const PLIST_LABEL: &str = "com.brief.serve";

pub fn plist_path() -> Result<std::path::PathBuf> {
    Ok(dirs::home_dir().context("no home")?.join(format!("Library/LaunchAgents/{PLIST_LABEL}.plist")))
}

pub fn install(store: &Store) -> Result<String> {
    let path = plist_path()?;
    let logs = store.root.join("logs");
    let env_path = std::env::var("PATH").unwrap_or_default();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{PLIST_LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>serve</string></array>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{out}</string>
  <key>StandardErrorPath</key><string>{err}</string>
</dict></plist>
"#,
        exe = html::esc(&exe()),
        path = html::esc(&env_path),
        out = html::esc(&logs.join("serve.log").display().to_string()),
        err = html::esc(&logs.join("serve.log").display().to_string()),
    );
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, plist)?;
    let domain = gui_domain()?;
    let _ = std::process::Command::new("launchctl").args(["bootout", &domain, &path.display().to_string()]).output();
    let out = std::process::Command::new("launchctl").args(["bootstrap", &domain, &path.display().to_string()]).output()?;
    anyhow::ensure!(out.status.success(), "launchctl failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    Ok(format!("Running in the background at http://127.0.0.1:{}/ (starts at login)", store.config().port))
}

pub fn uninstall() -> Result<String> {
    let path = plist_path()?;
    let domain = gui_domain()?;
    let _ = std::process::Command::new("launchctl").args(["bootout", &domain, &path.display().to_string()]).output();
    let _ = std::fs::remove_file(&path);
    Ok("Stopped. brief no longer runs in the background.".into())
}
