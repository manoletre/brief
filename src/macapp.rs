//! Tiny native macOS apps brief compiles from Swift source on first use (needs the Xcode command
//! line tools): the Calendar reader (calendar.swift) and the browser-extension install guide
//! (guide.swift). An app is only rebuilt when its source changes, because a rebuild changes its
//! signature and macOS then asks again for any permission it had.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Spec<'a> {
    /// Executable and bundle name, e.g. "BriefCalendar" → BriefCalendar.app.
    pub name: &'a str,
    pub bundle_id: &'a str,
    pub source: &'a str,
    /// Extra Info.plist entries (`<key>…</key><string>…</string>` lines).
    pub plist_extra: &'a str,
    /// For error messages: "the Calendar connector".
    pub purpose: &'a str,
}

/// Build `dir/<name>.app` unless it's already built from this exact source; returns its path.
pub fn ensure(dir: &Path, spec: &Spec) -> Result<PathBuf> {
    let app = dir.join(format!("{}.app", spec.name));
    let hash = format!("{:x}", Sha256::digest(spec.source.as_bytes()));
    let stamp = dir.join("build.sha256");
    let bin = app.join("Contents/MacOS").join(spec.name);
    if bin.exists() && fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == hash) {
        return Ok(app);
    }
    let swiftc = ["/usr/bin/swiftc", "/opt/homebrew/bin/swiftc"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .with_context(|| format!("{} needs the Xcode command line tools: run `xcode-select --install`, then try again", spec.purpose))?;
    fs::create_dir_all(bin.parent().unwrap())?;
    let src = dir.join(format!("{}.swift", spec.name));
    fs::write(&src, spec.source)?;
    let out = Command::new(swiftc).args(["-O", "-o"]).arg(&bin).arg(&src).output().context("couldn't run swiftc")?;
    if !out.status.success() {
        bail!("couldn't build {}: {}", spec.purpose, String::from_utf8_lossy(&out.stderr).trim());
    }
    fs::write(
        app.join("Contents/Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>{id}</string>
  <key>CFBundleName</key><string>brief</string>
  <key>CFBundleDisplayName</key><string>brief</string>
  <key>CFBundleExecutable</key><string>{name}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSUIElement</key><true/>
{extra}</dict></plist>
"#,
            id = spec.bundle_id,
            name = spec.name,
            extra = spec.plist_extra
        ),
    )?;
    let _ = Command::new("codesign").args(["--force", "--sign", "-"]).arg(&app).output();
    fs::write(&stamp, hash)?;
    Ok(app)
}
