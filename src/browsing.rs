//! What you looked at in your browser (Dia, Arc, Chrome or another Chromium browser), from brief's extension (`extension/`): each visit is a
//! page with the seconds it was actually in front of you, plus the start of its text for pages
//! you stayed on. The extension posts visits to /api/browsing once a minute with the key from
//! `chrome.json`; they're kept in `browsing.jsonl`.
//!
//! The extension is loaded unpacked from ~/.brief/chrome-extension, so updating it is rewriting
//! that folder: `brief chrome`, `make` and `brief serve` do, and the extension reloads itself
//! when the server reports a newer build than the one it's running.

use crate::store::{append_jsonl, read_jsonl, Store};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Local};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

const FILES: [(&str, &str); 5] = [
    ("manifest.json", include_str!("../extension/manifest.json")),
    ("background.js", include_str!("../extension/background.js")),
    ("content.js", include_str!("../extension/content.js")),
    ("popup.html", include_str!("../extension/popup.html")),
    ("popup.js", include_str!("../extension/popup.js")),
];
const ICON: &[u8] = include_bytes!("../assets/icon.png");

/// The page text kept per visit, and in the brief prompt overall.
const MAX_EXCERPT: usize = 1500;
const MAX_PROMPT_CHARS: usize = 14_000;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Visit {
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub domain: String,
    pub start: DateTime<Local>,
    pub seconds: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub excerpt: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    key: String,
    #[serde(default)]
    last_seen: Option<DateTime<Local>>,
}

/// Hash of the extension's code: changes whenever a new brief build ships different files.
pub fn build() -> String {
    let mut h = Sha256::new();
    for (name, text) in FILES {
        h.update(name.as_bytes());
        h.update(text.as_bytes());
    }
    format!("{:x}", h.finalize())[..12].to_owned()
}

pub fn dir(store: &Store) -> PathBuf {
    store.root.join("chrome-extension")
}

fn settings_path(store: &Store) -> PathBuf {
    store.root.join("chrome.json")
}

fn settings(store: &Store) -> Option<Settings> {
    serde_json::from_str(&fs::read_to_string(settings_path(store)).ok()?).ok()
}

fn save_settings(store: &Store, s: &Settings) -> Result<()> {
    let path = settings_path(store);
    fs::write(&path, serde_json::to_string_pretty(s)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// The key the extension sends, if it was ever installed.
pub fn key(store: &Store) -> Option<String> {
    settings(store).map(|s| s.key).filter(|k| !k.is_empty())
}

/// The build currently written to the extension folder.
pub fn installed_build(store: &Store) -> Option<String> {
    let cfg = fs::read_to_string(dir(store).join("config.js")).ok()?;
    let start = cfg.find("build: \"")? + 8;
    Some(cfg[start..].split('"').next()?.to_owned())
}

pub fn installed(store: &Store) -> bool {
    dir(store).join("manifest.json").exists()
}

/// When the extension last sent anything (it does every minute while the browser is open).
pub fn last_seen(store: &Store) -> Option<DateTime<Local>> {
    settings(store)?.last_seen
}

/// Write the extension's files (creating its key the first time). Returns true if anything changed.
pub fn write_files(store: &Store) -> Result<bool> {
    let mut s = settings(store).unwrap_or_default();
    if s.key.is_empty() {
        s.key = crate::new_key();
        save_settings(store, &s)?;
    }
    let build = build();
    if installed_build(store).as_deref() == Some(build.as_str()) && dir(store).join("icon.png").exists() {
        return Ok(false);
    }
    let d = dir(store);
    fs::create_dir_all(&d)?;
    for (name, text) in FILES {
        fs::write(d.join(name), text)?;
    }
    fs::write(d.join("icon.png"), ICON)?;
    // config.js last: the extension only reloads once it names the new build.
    let port = store.config().port;
    fs::write(
        d.join("config.js"),
        format!("// Written by `brief browser`. The key lets the extension talk to brief on this Mac.\nself.BRIEF = {{ port: {port}, key: \"{}\", build: \"{build}\" }};\n", s.key),
    )?;
    Ok(true)
}

/// Keep the extension folder in step with this build of brief, if it was ever installed.
pub fn refresh(store: &Store) {
    if installed(store) {
        if let Err(e) = write_files(store) {
            eprintln!("brief: couldn't update the browser extension: {e:#}");
        }
    }
}

/// Store visits from the extension; remembers when it was last heard from.
pub fn ingest(store: &Store, visits: Vec<Visit>) -> Result<()> {
    for mut v in visits {
        if v.seconds == 0 || v.url.is_empty() {
            continue;
        }
        v.excerpt = cut(&v.excerpt, MAX_EXCERPT);
        v.description = cut(&v.description, 300);
        v.title = cut(&v.title, 200);
        append_jsonl(&store.root.join("browsing.jsonl"), &v)?;
    }
    if let Some(mut s) = settings(store) {
        s.last_seen = Some(Local::now());
        save_settings(store, &s)?;
    }
    Ok(())
}

pub fn visits(store: &Store) -> Vec<Visit> {
    read_jsonl(&store.root.join("browsing.jsonl"))
}

/// Drop visits older than 60 days, so the file doesn't grow forever.
pub fn prune(store: &Store) -> Result<()> {
    let path = store.root.join("browsing.jsonl");
    let all = visits(store);
    let cutoff = Local::now() - Duration::days(60);
    if all.first().map_or(true, |v| v.start > cutoff) {
        return Ok(());
    }
    let mut text = String::new();
    for v in all.iter().filter(|v| v.start > cutoff) {
        text.push_str(&serde_json::to_string(v)?);
        text.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path).context("couldn't prune browsing.jsonl")
}

fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn hm(seconds: u64) -> String {
    let m = (seconds + 30) / 60;
    if m >= 60 { format!("{}h{:02}", m / 60, m % 60) } else { format!("{m} min") }
}

/// What the brief writer sees: time per site and per page since the last brief, what the pages
/// they read were about, and a lighter outline of the week before.
pub fn brief_context(store: &Store, since: DateTime<Local>) -> String {
    let all = visits(store);
    if all.is_empty() {
        return String::new();
    }
    let fresh: Vec<&Visit> = all.iter().filter(|v| v.start > since).collect();
    let mut p = String::new();
    if fresh.is_empty() {
        p.push_str("(Nothing recorded since the last brief: the browser was closed, paused, or the extension isn't running.)\n");
    } else {
        let total: u64 = fresh.iter().map(|v| v.seconds as u64).sum();
        let _ = writeln!(p, "Since the last brief: {} in front of the browser, across {} visits.\n", hm(total), fresh.len());

        // Per site.
        let mut sites: HashMap<&str, u64> = HashMap::new();
        for v in &fresh {
            *sites.entry(v.domain.as_str()).or_default() += v.seconds as u64;
        }
        let mut sites: Vec<_> = sites.into_iter().collect();
        sites.sort_by(|a, b| b.1.cmp(&a.1));
        p.push_str("Time per site:\n");
        for (d, s) in sites.iter().take(20) {
            let _ = writeln!(p, "- {d}: {}", hm(*s));
        }

        // Per page, merged across visits, longest first.
        struct Page<'a> { v: &'a Visit, seconds: u64, first: DateTime<Local>, excerpt: &'a str }
        let mut pages: HashMap<&str, Page> = HashMap::new();
        for v in &fresh {
            let e = pages.entry(v.url.as_str()).or_insert(Page { v, seconds: 0, first: v.start, excerpt: "" });
            e.seconds += v.seconds as u64;
            e.first = e.first.min(v.start);
            if e.excerpt.is_empty() && !v.excerpt.is_empty() {
                e.excerpt = &v.excerpt;
            }
            if e.v.title.is_empty() {
                e.v = v;
            }
        }
        let mut pages: Vec<Page> = pages.into_values().filter(|p| p.seconds >= 30).collect();
        pages.sort_by(|a, b| b.seconds.cmp(&a.seconds));
        p.push_str("\nPages they spent time on (longest first; text is the start of the page):\n");
        for pg in pages.iter().take(60) {
            if p.len() > MAX_PROMPT_CHARS {
                p.push_str("- … (more, shorter visits left out)\n");
                break;
            }
            let _ = writeln!(p, "- {} | {} | {} | {}", pg.first.format("%a %H:%M"), hm(pg.seconds), if pg.v.title.is_empty() { "(no title)" } else { &pg.v.title }, pg.v.url);
            if !pg.v.description.is_empty() {
                let _ = writeln!(p, "  about: {}", pg.v.description);
            }
            if !pg.excerpt.is_empty() {
                let _ = writeln!(p, "  text: {}", cut(pg.excerpt, 600));
            }
        }
    }

    // The week before, by day and site, so drift over several days shows.
    let week_start = Local::now() - Duration::days(7);
    let mut days: std::collections::BTreeMap<String, HashMap<&str, u64>> = Default::default();
    for v in all.iter().filter(|v| v.start > week_start && v.start <= since) {
        *days.entry(v.start.format("%a %b %-d").to_string()).or_default().entry(v.domain.as_str()).or_default() += v.seconds as u64;
    }
    if !days.is_empty() {
        p.push_str("\nEarlier this week, top sites per day:\n");
        for (day, sites) in days {
            let mut sites: Vec<_> = sites.into_iter().collect();
            sites.sort_by(|a, b| b.1.cmp(&a.1));
            let total: u64 = sites.iter().map(|s| s.1).sum();
            let top: Vec<String> = sites.iter().take(6).map(|(d, s)| format!("{d} {}", hm(*s))).collect();
            let _ = writeln!(p, "- {day} ({}): {}", hm(total), top.join(", "));
        }
    }
    p
}

/// A Chromium browser the extension runs in: its name, app bundle id and extensions page.
pub struct Browser {
    pub name: &'static str,
    bundle: &'static str,
    extensions: &'static str,
}

const BROWSERS: [Browser; 7] = [
    Browser { name: "Dia", bundle: "company.thebrowser.dia", extensions: "chrome://extensions" },
    Browser { name: "Arc", bundle: "company.thebrowser.browser", extensions: "arc://extensions" },
    Browser { name: "Google Chrome", bundle: "com.google.chrome", extensions: "chrome://extensions" },
    Browser { name: "Brave", bundle: "com.brave.browser", extensions: "brave://extensions" },
    Browser { name: "Microsoft Edge", bundle: "com.microsoft.edgemac", extensions: "edge://extensions" },
    Browser { name: "Vivaldi", bundle: "com.vivaldi.vivaldi", extensions: "vivaldi://extensions" },
    Browser { name: "Opera", bundle: "com.operasoftware.opera", extensions: "opera://extensions" },
];

/// The default browser's app bundle id, from Launch Services (lowercased).
fn default_bundle() -> Option<String> {
    let plist = dirs::home_dir()?.join("Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist");
    let out = std::process::Command::new("plutil").args(["-convert", "json", "-o", "-"]).arg(plist).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("LSHandlers")?.as_array()?.iter().find_map(|h| {
        (h.get("LSHandlerURLScheme")?.as_str()? == "https").then(|| h.get("LSHandlerRoleAll")?.as_str().map(str::to_lowercase))?
    })
}

/// The browser to install into: the default browser if it can run the extension, else the
/// first supported one installed. `Err` names the default browser when none fits.
pub fn browser() -> Result<&'static Browser, String> {
    let default = default_bundle();
    if let Some(b) = BROWSERS.iter().find(|b| default.as_deref() == Some(b.bundle)) {
        return Ok(b);
    }
    let installed = |b: &&Browser| {
        std::process::Command::new("mdfind")
            .arg(format!("kMDItemCFBundleIdentifier == '{}'c", b.bundle))
            .output()
            .is_ok_and(|o| !o.stdout.is_empty())
    };
    BROWSERS.iter().find(installed).ok_or_else(|| default.unwrap_or_else(|| "unknown".into()))
}

/// The browser's display name, for messages ("your browser" when unsure).
pub fn browser_name() -> &'static str {
    browser().map_or("your browser", |b| b.name)
}

/// `brief browser`: write (or update) the extension, and walk through loading it the first time.
pub fn install(store: &Store) -> Result<String> {
    let first = !installed(store);
    let changed = write_files(store)?;
    let path = dir(store);
    let seen = last_seen(store).is_some_and(|t| Local::now() - t < Duration::days(3));
    if !first && seen {
        return Ok(if changed {
            format!("Updated the browser extension (build {}). It reloads itself within a minute.", build())
        } else {
            format!("The browser extension is up to date (build {}) and sending to brief.", build())
        });
    }
    let b = browser().map_err(|default| {
        anyhow::anyhow!(
            "The extension needs a Chromium browser (Dia, Arc, Chrome, Brave, Edge…), and your default browser ({default}) isn't one.\n\
             The files are in {}; load them in a Chromium browser by hand.",
            path.display()
        )
    })?;
    let _ = crate::connect::copy(&path.display().to_string());
    let _ = std::process::Command::new("open").args(["-b", b.bundle, b.extensions]).status();
    if let Err(e) = show_guide(store, b) {
        eprintln!("(Couldn't show the install guide window: {e:#})");
    }
    Ok(format!(
        "brief's browser extension is in {path}\n\n\
         {name} only loads local extensions by hand, once. A small guide window is open on top of {name}'s\n\
         extensions page ({ext}; if the page didn't open, paste that into the address bar):\n  \
         1. Turn on Developer mode.\n  \
         2. Click Load unpacked.\n  \
         3. Press ⌘⇧G, paste (the folder path is on your clipboard), press ⏎, then Select.\n\n\
         After that, updates are automatic: `make`, `brief browser` and the background service rewrite\n\
         the folder and the extension reloads itself. Run `brief browser status` to check it's sending.",
        path = path.display(),
        name = b.name,
        ext = b.extensions,
    ))
}

/// The floating window over the browser that walks through loading the extension and closes
/// once it's sending (guide.swift).
fn show_guide(store: &Store, b: &Browser) -> Result<()> {
    let app = crate::macapp::ensure(
        &store.root.join("guide"),
        &crate::macapp::Spec { name: "BriefGuide", bundle_id: "com.brief.guide", source: include_str!("guide.swift"), plist_extra: "", purpose: "the install guide" },
    )?;
    // Give the browser a moment to come forward, so the guide lands on top of it.
    std::thread::sleep(std::time::Duration::from_millis(700));
    std::process::Command::new("open")
        .arg("-n")
        .arg(&app)
        .arg("--args")
        .args(["--browser", b.name, "--bundle", b.bundle, "--page", b.extensions])
        .arg("--folder")
        .arg(dir(store))
        .arg("--watch")
        .arg(settings_path(store))
        .arg("--icon")
        .arg(dir(store).join("icon.png"))
        .status()
        .context("couldn't open the install guide")?;
    Ok(())
}

/// `brief browser status`.
pub fn status_text(store: &Store) -> String {
    if !installed(store) {
        return "The browser extension isn't installed. Run `brief browser`.".into();
    }
    let today = Local::now().date_naive();
    let secs: u64 = visits(store).iter().filter(|v| v.start.date_naive() == today).map(|v| v.seconds as u64).sum();
    match last_seen(store) {
        Some(t) => format!(
            "Browser extension build {} · last heard from {} · {} recorded today.\nIt sends once a minute while the browser is open.",
            installed_build(store).unwrap_or_default(),
            t.format("%a %H:%M"),
            hm(secs)
        ),
        None => format!("The extension files are in {}, but the browser hasn't sent anything yet. Run `brief browser` for the steps.", dir(store).display()),
    }
}
