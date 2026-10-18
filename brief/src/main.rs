mod generate;
mod html;
mod mcp;
mod server;
mod store;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::{BufRead, Write};
use store::Store;

#[derive(Parser)]
#[command(name = "brief", version, about = "Your chatbots log what you work on; Claude writes you a daily brief.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set your objectives and preferences (run this first).
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
    /// Log an entry from the terminal: brief log <project> <summary…>
    Log { project: String, summary: Vec<String> },
    /// Print how to connect Claude Desktop, Claude Code, ChatGPT and others.
    Connect,
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
    match Cli::parse().cmd {
        Cmd::Init => init(&store),
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
        Cmd::Connect => {
            connect(&store);
            Ok(())
        }
        Cmd::Install => install(&store),
        Cmd::Uninstall => uninstall(),
        Cmd::Key => {
            let mut cfg = store.config();
            let key: String = {
                use rand::Rng;
                rand::thread_rng().sample_iter(&rand::distributions::Alphanumeric).take(32).map(char::from).collect()
            };
            cfg.access_key = Some(key.clone());
            store.save_config(&cfg)?;
            println!("Access key: {key}\nRemote MCP URL: https://<your-tunnel-host>/mcp?key={key}");
            Ok(())
        }
    }
}

fn prompt(q: &str, default: &str) -> Result<String> {
    print!("{q} [{default}]: ");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    let s = s.trim();
    Ok(if s.is_empty() { default.to_owned() } else { s.to_owned() })
}

fn init(store: &Store) -> Result<()> {
    println!("brief keeps its data in {}\n", store.root.display());
    let existing = store.objectives();
    if !existing.trim().is_empty() {
        println!("Current objectives:\n\n{}\n", existing.trim());
    }
    if existing.trim().is_empty() || prompt("Rewrite them? (y/n)", "n")?.starts_with('y') {
        println!("What are you working towards? Projects, goals, things you want to find or learn.");
        println!("One per line; an empty line finishes.\n");
        let mut lines = Vec::new();
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            if line.trim().is_empty() {
                break;
            }
            let l = line.trim();
            lines.push(if l.starts_with('-') || l.starts_with('#') { l.to_owned() } else { format!("- {l}") });
        }
        store.save_objectives(&(lines.join("\n") + "\n"))?;
    }

    let mut cfg = store.config();
    cfg.brief_hour = prompt("Hour of day to write the brief (0-23)", &cfg.brief_hour.to_string())?.parse().unwrap_or(cfg.brief_hour);
    cfg.model = prompt("Claude model (opus / sonnet / haiku)", &cfg.model)?;
    store.save_config(&cfg)?;

    println!("\nSaved. Next:\n  brief install    # run in the background at login\n  brief connect    # hook up your chatbots\n  brief open       # read your briefs");
    Ok(())
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

fn open_url(url: &str) -> Result<()> {
    std::process::Command::new("open").arg(url).status()?;
    Ok(())
}

fn exe() -> String {
    std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "brief".into())
}

fn connect(store: &Store) {
    let port = store.config().port;
    let exe = exe();
    println!(
        r#"brief exposes two MCP tools: log_work (write) and get_context (read).

── Claude Code ─────────────────────────────────────────────
  claude mcp add --scope user brief -- "{exe}" mcp

── Claude Desktop ──────────────────────────────────────────
  Add to ~/Library/Application Support/Claude/claude_desktop_config.json:

  {{ "mcpServers": {{ "brief": {{ "command": "{exe}", "args": ["mcp"] }} }} }}

  then restart Claude Desktop.

── Any client that speaks MCP over HTTP, on this machine ───
  http://127.0.0.1:{port}/mcp      (needs `brief serve` running)

── ChatGPT, claude.ai web, phone apps ──────────────────────
  These call MCP servers from the cloud, so they can't see localhost.
  Expose only /mcp through a tunnel, protected by a key:

    brief key
    cloudflared tunnel --url http://127.0.0.1:{port}

  Then add https://<tunnel-host>/mcp?key=<key> as a custom connector
  (ChatGPT: Settings → Connectors → Developer mode; Claude: Settings → Connectors).

Tip: the tool descriptions ask assistants to log proactively, but adding
"Log meaningful work to brief" to each assistant's custom instructions helps a lot.
"#
    );
}

const PLIST_LABEL: &str = "com.brief.serve";

fn plist_path() -> Result<std::path::PathBuf> {
    Ok(dirs::home_dir().context("no home")?.join(format!("Library/LaunchAgents/{PLIST_LABEL}.plist")))
}

fn install(store: &Store) -> Result<()> {
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
    let uid = std::process::Command::new("id").arg("-u").output()?.stdout;
    let domain = format!("gui/{}", String::from_utf8_lossy(&uid).trim());
    let _ = std::process::Command::new("launchctl").args(["bootout", &domain, &path.display().to_string()]).status();
    std::process::Command::new("launchctl").args(["bootstrap", &domain, &path.display().to_string()]).status()?;
    println!("Installed. brief now runs at login: http://127.0.0.1:{}/", store.config().port);
    println!("Logs: {}", logs.join("serve.log").display());
    Ok(())
}

fn uninstall() -> Result<()> {
    let path = plist_path()?;
    let uid = std::process::Command::new("id").arg("-u").output()?.stdout;
    let domain = format!("gui/{}", String::from_utf8_lossy(&uid).trim());
    let _ = std::process::Command::new("launchctl").args(["bootout", &domain, &path.display().to_string()]).status();
    let _ = std::fs::remove_file(&path);
    println!("Removed.");
    Ok(())
}
