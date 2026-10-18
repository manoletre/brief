//! Your calendar, read from the Mac's Calendar app: every account added there (Google, iCloud,
//! Exchange) with no sign-in of its own. EventKit needs a real app for macOS to ask for access,
//! so `brief calendar` compiles `calendar.swift` into ~/.brief/calendar/BriefCalendar.app (see
//! macapp.rs) and runs it with `open`. macOS asks once, for "brief".
//!
//! `calendar.json` marks it connected; deleting it (`brief calendar off`) stops the brief from
//! reading the calendar. Access itself is revoked in System Settings → Privacy → Calendars.

use crate::store::Store;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDate, NaiveTime, TimeZone};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const SWIFT: &str = include_str!("calendar.swift");
const PRIVACY_PANE: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars";
/// Free time is counted between these hours.
const DAY_START: u32 = 9;
const DAY_END: u32 = 18;

#[derive(Deserialize)]
struct Reply {
    status: String,
    #[serde(default)]
    calendars: Vec<String>,
    #[serde(default)]
    events: Vec<Event>,
}

#[derive(Deserialize, Clone)]
pub struct Event {
    pub title: String,
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub calendar: String,
    #[serde(default)]
    pub attendees: usize,
    #[serde(default)]
    pub free: bool,
    #[serde(default)]
    pub tentative: bool,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub notes: String,
}

impl Event {
    fn hours(&self) -> f64 {
        (self.end - self.start).num_minutes() as f64 / 60.0
    }

    /// Takes up time: not all-day and not marked free.
    fn busy(&self) -> bool {
        !self.all_day && !self.free
    }
}

fn dir(store: &Store) -> PathBuf {
    store.root.join("calendar")
}

pub fn connected(store: &Store) -> bool {
    store.root.join("calendar.json").exists()
}

/// Compile the helper app the first time (and when its code changes).
fn ensure_helper(store: &Store) -> Result<PathBuf> {
    const USAGE: &str = "brief reads your calendar so your daily brief knows where your time went and what's coming up.";
    crate::macapp::ensure(
        &dir(store),
        &crate::macapp::Spec {
            name: "BriefCalendar",
            bundle_id: "com.brief.calendar",
            source: SWIFT,
            plist_extra: &format!(
                "  <key>NSCalendarsFullAccessUsageDescription</key><string>{USAGE}</string>\n  <key>NSCalendarsUsageDescription</key><string>{USAGE}</string>\n"
            ),
            purpose: "the Calendar connector",
        },
    )
}

/// Run the helper and read its reply. `ask` lets it show the permission prompt.
fn read(store: &Store, back: u32, forward: u32, ask: bool) -> Result<Reply> {
    let app = ensure_helper(store)?;
    let out = dir(store).join(format!("events-{}.json", std::process::id()));
    let _ = fs::remove_file(&out);
    let mut child = Command::new("/usr/bin/open")
        .args(["-W", "-g", "-n"])
        .arg(&app)
        .arg("--args")
        .arg("--out")
        .arg(&out)
        .args(["--back", &back.to_string(), "--forward", &forward.to_string()])
        .args(if ask { &["--ask"][..] } else { &[] })
        .spawn()
        .context("couldn't start the Calendar helper")?;
    // The permission prompt waits for you; reading alone takes a second.
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(if ask { 300 } else { 60 });
    while child.try_wait()?.is_none() {
        if std::time::Instant::now() > limit {
            let _ = child.kill();
            bail!("the Calendar helper didn't answer");
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let text = fs::read_to_string(&out).context("the Calendar helper wrote nothing")?;
    let _ = fs::remove_file(&out);
    serde_json::from_str(&text).context("unexpected reply from the Calendar helper")
}

pub fn connect(store: &Store) -> Result<String> {
    let reply = read(store, 0, 7, true)?;
    if reply.status != "granted" {
        crate::connect::open(PRIVACY_PANE);
        bail!(
            "macOS didn't give brief access to your calendar ({}). In System Settings → Privacy & Security → Calendars, \
             set brief to Full Access, then connect again.",
            reply.status.replace('_', " ")
        );
    }
    fs::write(
        store.root.join("calendar.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "connected_at": Local::now(), "calendars": reply.calendars }))?,
    )?;
    let week = reply.events.iter().filter(|e| e.busy()).count();
    Ok(format!("Calendar connected: {} calendars, {week} events in the next 7 days.", reply.calendars.len()))
}

pub fn disconnect(store: &Store) -> Result<String> {
    let _ = fs::remove_file(store.root.join("calendar.json"));
    Ok("Calendar disconnected. (To revoke access completely: System Settings → Privacy & Security → Calendars.)".into())
}

/// What the brief writer sees: where time went since the last brief, today with its free
/// blocks, and the week ahead.
pub fn brief_context(store: &Store, since: DateTime<Local>) -> String {
    if !connected(store) {
        return String::new();
    }
    let back = (Local::now().date_naive() - since.date_naive()).num_days().clamp(1, 7) as u32;
    match read(store, back, 7, false) {
        Ok(r) if r.status == "granted" => format_context(&r.events, since),
        Ok(r) => format!("(The calendar is connected but macOS access is {}. Mention in one short line that `brief calendar` fixes it.)\n", r.status.replace('_', " ")),
        Err(e) => format!("(The calendar is connected but couldn't be read today: {e:#}. Mention this in one short line.)\n"),
    }
}

fn format_context(events: &[Event], since: DateTime<Local>) -> String {
    let now = Local::now();
    let today = now.date_naive();
    let mut p = String::new();

    let past: Vec<&Event> = events.iter().filter(|e| e.busy() && e.end > since && e.start < now && e.start.date_naive() < today).collect();
    if !past.is_empty() {
        p.push_str("Since the last brief (where their time went):\n");
        let mut by_day: BTreeMap<NaiveDate, Vec<&Event>> = BTreeMap::new();
        for e in past {
            by_day.entry(e.start.date_naive()).or_default().push(e);
        }
        for (day, evs) in by_day {
            let total: f64 = evs.iter().map(|e| e.hours()).sum();
            let _ = writeln!(p, "- {} — {total:.1}h in {} event(s)", day.format("%a %b %-d"), evs.len());
            for e in evs {
                line(&mut p, "  ", e);
            }
        }
        p.push('\n');
    }

    let todays: Vec<&Event> = events
        .iter()
        .filter(|e| e.start.date_naive() == today || (e.all_day && e.start.date_naive() < today && e.end > day_at(today, 0)))
        .collect();
    let _ = writeln!(p, "Today ({}):", today.format("%A %b %-d"));
    if todays.is_empty() {
        p.push_str("- nothing on the calendar\n");
    }
    for e in &todays {
        line(&mut p, "- ", e);
    }
    let busy: f64 = todays.iter().filter(|e| e.busy()).map(|e| e.hours()).sum();
    let free = free_blocks(events, today);
    let free_h: f64 = free.iter().map(|(a, b)| (*b - *a).num_minutes() as f64 / 60.0).sum();
    let _ = writeln!(
        p,
        "Meetings today: {busy:.1}h. Free blocks of 30+ min between {DAY_START}:00 and {DAY_END}:00 ({free_h:.1}h): {}\n",
        if free.is_empty() { "none".into() } else { free.iter().map(|(a, b)| format!("{}–{}", a.format("%H:%M"), b.format("%H:%M"))).collect::<Vec<_>>().join(", ") }
    );

    let ahead: Vec<&Event> = events.iter().filter(|e| e.start.date_naive() > today).take(60).collect();
    if !ahead.is_empty() {
        p.push_str("Next 7 days:\n");
        for e in ahead {
            line(&mut p, "- ", e);
        }
    }
    p
}

fn line(p: &mut String, indent: &str, e: &Event) {
    let when = if e.all_day {
        format!("{} all day", e.start.format("%a %b %-d"))
    } else {
        format!("{} {}–{}", e.start.format("%a %b %-d"), e.start.format("%H:%M"), e.end.format("%H:%M"))
    };
    let _ = write!(p, "{indent}{when} | {}", if e.title.is_empty() { "(no title)" } else { &e.title });
    if !e.calendar.is_empty() {
        let _ = write!(p, " | {}", e.calendar);
    }
    if e.attendees > 1 {
        let _ = write!(p, " | {} people", e.attendees);
    }
    if e.tentative {
        p.push_str(" | not yet accepted");
    }
    if !e.location.is_empty() {
        let _ = write!(p, " | at {}", e.location.replace('\n', " "));
    }
    p.push('\n');
    if !e.notes.is_empty() {
        let notes: String = e.notes.split_whitespace().collect::<Vec<_>>().join(" ");
        let _ = writeln!(p, "{indent}  notes: {}", notes.chars().take(200).collect::<String>());
    }
}

fn day_at(day: NaiveDate, hour: u32) -> DateTime<Local> {
    Local.from_local_datetime(&day.and_time(NaiveTime::from_hms_opt(hour, 0, 0).unwrap())).earliest().unwrap_or_else(Local::now)
}

/// Gaps of 30+ minutes between busy events on `day`, within working hours and not in the past.
fn free_blocks(events: &[Event], day: NaiveDate) -> Vec<(DateTime<Local>, DateTime<Local>)> {
    let mut cursor = day_at(day, DAY_START).max(Local::now());
    let end = day_at(day, DAY_END);
    let mut busy: Vec<(DateTime<Local>, DateTime<Local>)> = events.iter().filter(|e| e.busy() && e.end > cursor && e.start < end).map(|e| (e.start, e.end)).collect();
    busy.sort();
    let mut out = Vec::new();
    for (s, e) in busy {
        if s - cursor >= Duration::minutes(30) {
            out.push((cursor, s));
        }
        cursor = cursor.max(e);
    }
    if end - cursor >= Duration::minutes(30) {
        out.push((cursor, end));
    }
    out
}

/// `brief calendar` when connected: exactly what the brief sees.
pub fn preview(store: &Store) -> String {
    let since = Local::now() - Duration::days(1);
    brief_context(store, since)
}
