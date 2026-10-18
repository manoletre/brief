//! Plain-file storage under ~/.brief (override with BRIEF_HOME).
//!
//!   config.json        port, brief hour, model, optional access key
//!   objectives.md      your goals, written at `brief init`
//!   entries.jsonl      everything your chatbots logged via MCP
//!   feedback.jsonl     your reactions to brief items
//!   briefs/DATE.json   one generated brief per day
//!   anki_*.jsonl       cards proposed for Anki, and the user's buries (see anki.rs)
//!   todoist.json       Todoist access token (see todoist.rs)
//!   calendar.json      set when the calendar is connected (see calendar.rs)
//!   browsing.jsonl     pages you looked at in Chrome, chrome.json its key (see browsing.rs)

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// Local hour (0-23) after which the daily brief is generated.
    pub brief_hour: u32,
    /// Model alias passed to `claude --model`.
    pub model: String,
    /// When set, HTTP MCP requests must carry `?key=` or `Authorization: Bearer`.
    /// Needed only if you expose the server through a tunnel.
    #[serde(default)]
    pub access_key: Option<String>,
    /// Serve /mcp on `public_port()` for Tailscale Funnel, so cloud chatbots (ChatGPT,
    /// claude.ai, Grok) can reach it.
    #[serde(default)]
    pub tunnel: bool,
    /// This Mac's Funnel address, e.g. https://mac.tail1234.ts.net.
    #[serde(default)]
    pub public_url: Option<String>,
    /// Model alias for the reviewer that checks every Anki card before it's added.
    #[serde(default = "default_anki_model")]
    pub anki_model: String,
}

fn default_anki_model() -> String {
    "sonnet".into()
}

impl Config {
    /// The port Tailscale Funnel forwards to. It serves only /mcp, and only with the key.
    pub fn public_port(&self) -> u16 {
        self.port + 1
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 7878,
            brief_hour: 7,
            model: "opus".into(),
            access_key: None,
            tunnel: false,
            public_url: None,
            anki_model: default_anki_model(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub ts: DateTime<Local>,
    pub project: String,
    pub kind: String,
    pub summary: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Which chatbot wrote it (from MCP clientInfo or the tool argument).
    #[serde(default)]
    pub source: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Feedback {
    pub ts: DateTime<Local>,
    pub brief_date: NaiveDate,
    /// Empty for feedback on the brief as a whole.
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub item_title: String,
    /// "up", "down" or "" (comment only).
    #[serde(default)]
    pub rating: String,
    #[serde(default)]
    pub comment: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Brief {
    #[serde(default)]
    pub date: Option<NaiveDate>,
    #[serde(default)]
    pub generated_at: Option<DateTime<Local>>,
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub reading_minutes: u32,
    #[serde(default)]
    pub sections: Vec<Section>,
    /// Optional "think first" question shown before the brief, answer revealed on click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub think_first: Option<ThinkFirst>,
    /// Plain-language meanings for the few technical words the brief can't avoid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<Term>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ThinkFirst {
    pub question: String,
    #[serde(default)]
    pub answer: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Term {
    pub term: String,
    #[serde(default)]
    pub meaning: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Section {
    pub title: String,
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Item {
    #[serde(default)]
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub project: String,
    /// The one thing to take away, in a sentence.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub takeaway: String,
    /// Why this is in the brief: the evidence it's based on (calendar, browsing, journal, to-dos).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// The objective it serves, as the user wrote it ("" when it serves none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub objective: String,
    /// For things to read or watch: a paragraph on what it is about.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub about: String,
    /// Short lines revealed one at a time, in sync with the visual.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
    /// A small diagram (flow, compare, bars, timeline, stat), drawn by the page's JS.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub visual: serde_json::Value,
    #[serde(default)]
    pub links: Vec<Link>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Link {
    pub title: String,
    pub url: String,
    #[serde(default, rename = "type")]
    pub kind: String,
}

impl Store {
    pub fn open() -> Result<Self> {
        let root = match std::env::var_os("BRIEF_HOME") {
            Some(p) => PathBuf::from(p),
            None => dirs::home_dir().context("no home directory")?.join(".brief"),
        };
        fs::create_dir_all(root.join("briefs"))?;
        fs::create_dir_all(root.join("logs"))?;
        fs::create_dir_all(root.join("work"))?;
        Ok(Store { root })
    }

    pub fn config(&self) -> Config {
        fs::read_to_string(self.root.join("config.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_config(&self, c: &Config) -> Result<()> {
        fs::write(self.root.join("config.json"), serde_json::to_string_pretty(c)?)?;
        Ok(())
    }

    pub fn objectives(&self) -> String {
        fs::read_to_string(self.root.join("objectives.md")).unwrap_or_default()
    }

    pub fn save_objectives(&self, text: &str) -> Result<()> {
        fs::write(self.root.join("objectives.md"), text)?;
        Ok(())
    }

    /// Objectives as separate items. In the file each is a `- ` bullet whose first line
    /// is the goal, followed by indented note lines.
    pub fn objective_list(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for line in self.objectives().lines() {
            let t = line.trim_end();
            if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
                out.push(rest.trim().to_owned());
            } else if t.trim().is_empty() {
                continue;
            } else if let Some(last) = out.last_mut().filter(|_| line.starts_with([' ', '\t'])) {
                last.push('\n');
                last.push_str(t.trim_start());
            } else {
                out.push(t.trim().to_owned());
            }
        }
        out
    }

    pub fn save_objective_list(&self, items: &[String]) -> Result<()> {
        let mut text = String::new();
        for item in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
            for (n, line) in item.lines().enumerate() {
                text.push_str(if n == 0 { "- " } else { "  " });
                text.push_str(line.trim());
                text.push('\n');
            }
        }
        self.save_objectives(&text)
    }

    pub fn add_entry(&self, e: &Entry) -> Result<()> {
        append_jsonl(&self.root.join("entries.jsonl"), e)
    }

    pub fn entries(&self) -> Vec<Entry> {
        read_jsonl(&self.root.join("entries.jsonl"))
    }

    pub fn add_feedback(&self, f: &Feedback) -> Result<()> {
        append_jsonl(&self.root.join("feedback.jsonl"), f)
    }

    pub fn feedback(&self) -> Vec<Feedback> {
        read_jsonl(&self.root.join("feedback.jsonl"))
    }

    fn brief_path(&self, date: NaiveDate) -> PathBuf {
        self.root.join("briefs").join(format!("{date}.json"))
    }

    pub fn brief(&self, date: NaiveDate) -> Option<Brief> {
        let s = fs::read_to_string(self.brief_path(date)).ok()?;
        serde_json::from_str(&s).ok()
    }

    pub fn save_brief(&self, b: &Brief) -> Result<()> {
        let date = b.date.context("brief without date")?;
        fs::write(self.brief_path(date), serde_json::to_string_pretty(b)?)?;
        Ok(())
    }

    /// Dates with a brief, newest first.
    pub fn brief_dates(&self) -> Vec<NaiveDate> {
        let mut dates: Vec<NaiveDate> = fs::read_dir(self.root.join("briefs"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                NaiveDate::parse_from_str(name.strip_suffix(".json")?, "%Y-%m-%d").ok()
            })
            .collect();
        dates.sort_unstable_by(|a, b| b.cmp(a));
        dates
    }
}

pub fn append_jsonl<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    // One write() per line with O_APPEND, so the stdio and HTTP servers can share the file.
    OpenOptions::new().create(true).append(true).open(path)?.write_all(line.as_bytes())?;
    Ok(())
}

pub fn read_jsonl<T: DeserializeOwned>(path: &PathBuf) -> Vec<T> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn new_id() -> String {
    use rand::Rng;
    let n: u32 = rand::thread_rng().gen();
    format!("{}-{:08x}", Local::now().format("%Y%m%d%H%M%S"), n)
}
