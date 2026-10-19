//! Hooking brief up to chatbots: detection and one-step connect/disconnect.

use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const CHATGPT_SETTINGS: &str = "https://chatgpt.com/#settings/Connectors";
pub const CLAUDE_AI_SETTINGS: &str = "https://claude.ai/settings/connectors";
pub const GROK_SETTINGS: &str = "https://grok.com/connectors";
const TAILSCALE_DOWNLOAD: &str = "https://tailscale.com/download/mac";

/// Path chatbots should launch. Prefer the ~/.cargo/bin link so configs keep working across rebuilds.
pub fn stable_exe() -> PathBuf {
    let link = dirs::home_dir().map(|h| h.join(".cargo/bin/brief"));
    match link {
        Some(l) if l.exists() => l,
        _ => std::env::current_exe().unwrap_or_else(|_| "brief".into()),
    }
}

fn find(name: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend([home.join(".local/bin"), "/opt/homebrew/bin".into(), "/usr/local/bin".into()]);
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// The Tailscale CLI: on PATH, or the one inside the Mac app.
pub fn tailscale() -> Option<PathBuf> {
    find("tailscale").or_else(|| {
        let app = PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
        app.is_file().then_some(app)
    })
}

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("couldn't run {:?}", cmd.get_program()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = if err.trim().is_empty() { String::from_utf8_lossy(&out.stdout).into_owned() } else { err.into_owned() };
        bail!("{}", msg.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read_json(path: &PathBuf) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

// ── Claude Code ──────────────────────────────────────────────

pub fn claude_code_installed() -> bool {
    crate::generate::find_claude().is_some()
}

pub fn claude_code_connected() -> bool {
    dirs::home_dir()
        .and_then(|h| read_json(&h.join(".claude.json")))
        .is_some_and(|v| v.pointer("/mcpServers/brief").is_some())
}

pub fn set_claude_code(on: bool) -> Result<String> {
    let claude = crate::generate::find_claude().context("Claude Code isn't installed")?;
    if on {
        let _ = Command::new(&claude).args(["mcp", "remove", "--scope", "user", "brief"]).output();
        run(Command::new(&claude).args(["mcp", "add", "--scope", "user", "brief", "--"]).arg(stable_exe()).arg("mcp"))?;
        Ok("Connected to Claude Code. New Claude Code sessions will log to brief.".into())
    } else {
        run(Command::new(&claude).args(["mcp", "remove", "--scope", "user", "brief"]))?;
        Ok("Disconnected from Claude Code.".into())
    }
}

// ── Codex, Cline, OpenCode ───────────────────────────────────

fn codex_config() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".codex/config.toml"))
}

pub fn codex_connected() -> bool {
    codex_config().and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|t| t.contains("[mcp_servers.brief]"))
}

pub fn set_codex(on: bool) -> Result<String> {
    let codex = crate::agent::Agent::Codex.find().context("Codex isn't installed")?;
    let _ = Command::new(&codex).args(["mcp", "remove", "brief"]).output();
    if on {
        run(Command::new(&codex).args(["mcp", "add", "brief", "--"]).arg(stable_exe()).arg("mcp"))?;
        Ok("Connected to Codex. New Codex sessions will log to brief.".into())
    } else {
        Ok("Disconnected from Codex.".into())
    }
}

fn cline_config() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".cline/data/settings/cline_mcp_settings.json"))
}

pub fn cline_connected() -> bool {
    cline_config().and_then(|p| read_json(&p)).is_some_and(|v| v.pointer("/mcpServers/brief").is_some())
}

pub fn set_cline(on: bool) -> Result<String> {
    let path = cline_config().context("no home directory")?;
    let entry = json!({ "command": stable_exe(), "args": ["mcp"], "disabled": false });
    edit_json(&path, "mcpServers", on.then_some(entry))?;
    Ok(if on { "Connected to Cline. New Cline tasks will log to brief." } else { "Disconnected from Cline." }.into())
}

fn opencode_config() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".config/opencode/opencode.json"))
}

pub fn opencode_connected() -> bool {
    opencode_config().and_then(|p| read_json(&p)).is_some_and(|v| v.pointer("/mcp/brief").is_some())
}

pub fn set_opencode(on: bool) -> Result<String> {
    let path = opencode_config().context("no home directory")?;
    let entry = json!({ "type": "local", "command": [stable_exe(), "mcp"], "enabled": true });
    edit_json(&path, "mcp", on.then_some(entry))?;
    Ok(if on { "Connected to OpenCode. New OpenCode sessions will log to brief." } else { "Disconnected from OpenCode." }.into())
}

/// Set (or remove, with None) `<section>.brief` in a JSON config file, keeping a .bak of the original.
fn edit_json(path: &Path, section: &str, entry: Option<Value>) -> Result<()> {
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let mut cfg: Value = if original.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&original).with_context(|| format!("{} isn't plain JSON; add brief there by hand", path.display()))?
    };
    let servers = cfg.as_object_mut().context("config isn't a JSON object")?.entry(section).or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().with_context(|| format!("`{section}` in {} isn't an object", path.display()))?;
    match entry {
        Some(e) => servers.insert("brief".into(), e),
        None => servers.remove("brief"),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if !original.is_empty() {
        std::fs::write(path.with_extension("json.bak"), &original)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&cfg)? + "\n")?;
    Ok(())
}

// ── Claude Desktop ───────────────────────────────────────────

fn desktop_config() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join("Library/Application Support/Claude/claude_desktop_config.json"))
}

pub fn claude_desktop_installed() -> bool {
    desktop_config().and_then(|p| p.parent().map(|d| d.exists())).unwrap_or(false)
        || std::path::Path::new("/Applications/Claude.app").exists()
}

/// Installed as a Desktop Extension (shows brief's name, logo and description in
/// Settings → Extensions).
pub fn claude_desktop_extension() -> bool {
    let Some(dir) = desktop_config().and_then(|p| Some(p.parent()?.join("Claude Extensions"))) else { return false };
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
        read_json(&e.path().join("manifest.json")).is_some_and(|m| m["name"] == "brief")
    })
}

/// The older way: a plain entry in claude_desktop_config.json.
pub fn claude_desktop_config_entry() -> bool {
    desktop_config().and_then(|p| read_json(&p)).is_some_and(|v| v.pointer("/mcpServers/brief").is_some())
}

/// Builds brief.mcpb, opens it in Claude Desktop (which shows its install dialog) and reveals
/// the file in Finder. Also drops the old config entry so brief doesn't show up twice.
pub fn install_claude_desktop(store: &Store) -> Result<String> {
    let bundle = build_mcpb(store)?;
    if claude_desktop_config_entry() {
        remove_desktop_config_entry()?;
    }
    run(Command::new("open").args(["-a", "Claude"]).arg(&bundle))?;
    let _ = Command::new("open").arg("-R").arg(&bundle).output();
    Ok("Opened the install dialog in Claude Desktop: click Install.".into())
}

fn remove_desktop_config_entry() -> Result<()> {
    let path = desktop_config().context("no home directory")?;
    let Ok(original) = std::fs::read_to_string(&path) else { return Ok(()) };
    let mut cfg: Value = serde_json::from_str(&original).context("claude_desktop_config.json isn't valid JSON; fix it first")?;
    if let Some(servers) = cfg.get_mut("mcpServers").and_then(Value::as_object_mut) {
        servers.remove("brief");
    }
    std::fs::write(path.with_extension("json.bak"), original)?;
    std::fs::write(&path, serde_json::to_string_pretty(&cfg)? + "\n")?;
    Ok(())
}

/// Writes ~/.brief/brief.mcpb: a manifest with brief's name, logo and descriptions, plus a
/// launcher that runs the `brief` on PATH, so rebuilds with `make` reach Claude Desktop
/// without reinstalling the extension.
pub fn build_mcpb(store: &Store) -> Result<PathBuf> {
    let dir = store.root.join("mcpb");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("server"))?;

    let exe = stable_exe().display().to_string().replace('\'', r"'\''");
    std::fs::write(dir.join("server/launch.sh"), format!("#!/bin/sh\nexec '{exe}' mcp\n"))?;
    std::fs::write(dir.join("icon.png"), crate::mcp::ICON_PNG)?;

    let tools: Vec<Value> = crate::mcp::tool_list()
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| json!({ "name": t["name"], "description": t["description"] }))
        .collect();
    let manifest = json!({
        "manifest_version": "0.3",
        "name": "brief",
        "display_name": "Brief",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Your work journal: Claude logs what you work on, and every morning you get a daily brief.",
        "long_description": MCPB_LONG_DESCRIPTION,
        "author": { "name": "manoletre" },
        "icon": "icon.png",
        "icons": [{ "src": "icon.png", "size": "512x512" }],
        "homepage": format!("http://127.0.0.1:{}", store.config().port),
        "server": {
            "type": "binary",
            "entry_point": "server/launch.sh",
            "mcp_config": { "command": "/bin/sh", "args": ["${__dirname}/server/launch.sh"] }
        },
        "tools": tools,
        "keywords": ["journal", "productivity", "daily brief", "memory", "notes", "anki", "flashcards"],
        "license": "MIT",
        "compatibility": { "platforms": ["darwin"] }
    });
    std::fs::write(dir.join("manifest.json"), serde_json::to_string_pretty(&manifest)? + "\n")?;

    let bundle = store.root.join("brief.mcpb");
    let _ = std::fs::remove_file(&bundle);
    run(Command::new("zip").current_dir(&dir).args(["-q", "-r", "-X"]).arg(&bundle).arg("."))?;
    Ok(bundle)
}

const MCPB_LONG_DESCRIPTION: &str = "**brief** is a personal work journal that lives on your Mac.

While you chat, Claude logs the work that matters (decisions, progress, ideas, blockers, open questions, things you learned) without you having to ask. Every morning, your local Claude Code reads your objectives, that log and your feedback on earlier briefs, and writes you a 10–20 minute brief: feedback on your work, connections between projects, blind spots, things to read or watch, and next steps.

Everything stays in plain files in `~/.brief`. Read your briefs at http://127.0.0.1:7878.

**Tools**
- `log_work`: record a decision, progress, idea, blocker, question or learning on one of your projects.
- `get_context`: read your objectives, projects and recent work.
- `anki_search`: see what you're already memorizing in Anki, and how well.
- `anki_add_cards`: add flashcards for facts you've learned; a reviewer rewrites or turns down cards that aren't atomic and clear.";

// ── Public link via Tailscale Funnel (ChatGPT, claude.ai, Grok) ──

pub fn get_tailscale() -> Result<String> {
    open(TAILSCALE_DOWNLOAD);
    Ok("Install Tailscale, open it and sign in, then press ⏎ here again.".into())
}

/// This Mac's Funnel hostname, e.g. mac.tail1234.ts.net.
fn tailscale_host(bin: &Path) -> Result<String> {
    let out = run(Command::new(bin).args(["status", "--json"]))
        .context("Tailscale isn't running. Open Tailscale and sign in, then press ⏎ again")?;
    let v: Value = serde_json::from_str(&out).context("unexpected output from `tailscale status`")?;
    let state = v["BackendState"].as_str().unwrap_or("unknown");
    if state != "Running" {
        bail!("Tailscale isn't connected ({state}). Open Tailscale and sign in, then press ⏎ again.");
    }
    let name = v.pointer("/Self/DNSName").and_then(Value::as_str).unwrap_or("").trim_end_matches('.');
    if name.is_empty() {
        bail!("Tailscale has no name for this Mac; turn on MagicDNS in the Tailscale admin console");
    }
    Ok(name.to_owned())
}

/// `tailscale funnel --bg <port>`. The first time, tailscale prints a link where you allow
/// Funnel for your tailnet and waits until you do, so open that link and keep waiting.
fn start_funnel(bin: &Path, port: u16) -> Result<()> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::mpsc::{self, RecvTimeoutError};
    let mut child = Command::new(bin)
        .args(["funnel", "--bg", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run tailscale")?;
    let (tx, rx) = mpsc::channel();
    let pipes: [Box<dyn Read + Send>; 2] = [Box::new(child.stdout.take().unwrap()), Box::new(child.stderr.take().unwrap())];
    for pipe in pipes {
        let tx = tx.clone();
        std::thread::spawn(move || BufReader::new(pipe).lines().map_while(Result::ok).for_each(|l| drop(tx.send(l))));
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_secs(300);
    let (mut output, mut opened) = (Vec::new(), false);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => {
                if let Some(url) = line.split_whitespace().find(|w| w.starts_with("https://login.tailscale.com/")).filter(|_| !opened) {
                    open(url);
                    opened = true;
                }
                output.push(line);
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                bail!("Gave up waiting for Funnel to be allowed. Press ⏎ to try again.");
            }
        }
    }
    if !child.wait()?.success() {
        bail!("tailscale funnel failed: {}", output.join(" ").trim());
    }
    Ok(())
}

/// Turn the public link on or off. The background service serves the public port, so this
/// (re)starts the service.
pub fn set_tunnel(store: &Store, on: bool) -> Result<String> {
    let mut cfg = store.config();
    if on {
        let bin = tailscale().context("Tailscale isn't installed")?;
        let host = tailscale_host(&bin)?;
        start_funnel(&bin, cfg.public_port())?;
        cfg.public_url = Some(format!("https://{host}"));
        if cfg.access_key.is_none() {
            cfg.access_key = Some(crate::new_key());
        }
    } else if let Some(bin) = tailscale() {
        let _ = Command::new(bin).args(["funnel", "--https=443", "off"]).output();
    }
    cfg.tunnel = on;
    store.save_config(&cfg)?;
    if crate::plist_path().is_ok_and(|p| p.exists()) {
        crate::restart_service()?;
    } else {
        crate::install(store)?;
    }
    if !on {
        return Ok("Public link turned off.".into());
    }
    let link = format!("{}/mcp?key={}", cfg.public_url.unwrap(), cfg.access_key.unwrap());
    copy(&link)?;
    Ok("Public link is live and copied. Paste it as a custom connector in ChatGPT, claude.ai or Grok.".into())
}

pub fn copy(text: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn()?;
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    child.wait()?;
    Ok(())
}

pub fn open(url: &str) {
    let _ = Command::new("open").arg(url).output();
}
