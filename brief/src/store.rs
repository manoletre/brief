//! Plain-file storage under ~/.brief (override with BRIEF_HOME).
//!
//!   config.json        port, brief hour, model, optional access key
//!   objectives.md      your goals, written at `brief init`
//!   entries.jsonl      everything your chatbots logged via MCP
//!   feedback.jsonl     your reactions to brief items
//!   briefs/DATE.json   one generated brief per day

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
}

impl Default for Config {
    fn default() -> Self {
        Config { port: 7878, brief_hour: 7, model: "opus".into(), access_key: None }
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
    #[serde(default)]
    pub cost_usd: Option<f64>,
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

fn append_jsonl<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    // One write() per line with O_APPEND, so the stdio and HTTP servers can share the file.
    OpenOptions::new().create(true).append(true).open(path)?.write_all(line.as_bytes())?;
    Ok(())
}

fn read_jsonl<T: DeserializeOwned>(path: &PathBuf) -> Vec<T> {
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
