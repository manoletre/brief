//! The agent brief works through: a coding agent's CLI run headless (Claude Code, Codex, Cline or
//! OpenCode), on the user's own subscription or account. It drives the objectives interview and
//! writes the daily brief. Chosen during onboarding and kept in config.json (`agent`, `model`).

use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Agent {
    Claude,
    Codex,
    Cline,
    OpenCode,
}

pub const ALL: [Agent; 4] = [Agent::Claude, Agent::Codex, Agent::Cline, Agent::OpenCode];

/// A model the user can pick, best for brief first. `id` "" means the agent's own default.
pub struct Model {
    pub id: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
}

/// What the agent may do on a run.
#[derive(Clone, Copy, PartialEq)]
pub enum Tools {
    /// Just answer (the interview).
    None,
    /// Search and read the web, and run the `composio` CLI when `composio` is set (the brief).
    /// With `search`, searching goes through `brief search` (Parallel, Brave or Browserbase)
    /// instead of the agent's own web search.
    Research { composio: bool, search: bool },
}

pub struct Reply {
    pub text: String,
    pub cost_usd: Option<f64>,
}

impl Agent {
    pub fn id(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Cline => "cline",
            Agent::OpenCode => "opencode",
        }
    }

    pub fn from_id(id: &str) -> Agent {
        ALL.into_iter().find(|a| a.id() == id).unwrap_or(Agent::Claude)
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Cline => "Cline",
            Agent::OpenCode => "OpenCode",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Agent::Claude => "Anthropic's CLI · your Claude subscription",
            Agent::Codex => "OpenAI's CLI · your ChatGPT plan",
            Agent::Cline => "open source · any model through your Cline account",
            Agent::OpenCode => "open source · any provider you've signed in to",
        }
    }

    /// The command that installs it (npm, as all four ship there).
    pub fn install_cmd(self) -> &'static str {
        match self {
            Agent::Claude => "npm install -g @anthropic-ai/claude-code",
            Agent::Codex => "npm install -g @openai/codex",
            Agent::Cline => "npm install -g cline",
            Agent::OpenCode => "npm install -g opencode-ai",
        }
    }

    /// The interactive sign-in, run in the user's terminal.
    pub fn login_args(self) -> &'static [&'static str] {
        match self {
            Agent::Claude => &["auth", "login"],
            Agent::Codex => &["login"],
            Agent::Cline => &["auth"],
            Agent::OpenCode => &["auth", "login"],
        }
    }

    pub fn models(self) -> &'static [Model] {
        match self {
            Agent::Claude => &[
                Model { id: "opus", label: "Opus", hint: "the sharpest judgement; the brief runs once a day, so it's worth it" },
                Model { id: "sonnet", label: "Sonnet", hint: "nearly as good, lighter on your usage limits" },
                Model { id: "haiku", label: "Haiku", hint: "fast and light, shallower briefs" },
            ],
            Agent::Codex => &[
                Model { id: "", label: "Codex's default", hint: "whatever model Codex is set to (codex /model)" },
                Model { id: "gpt-5", label: "GPT-5", hint: "strong general reasoning" },
                Model { id: "gpt-5-codex", label: "GPT-5 Codex", hint: "tuned for code, a bit terse for a brief" },
            ],
            Agent::Cline => &[
                Model { id: "anthropic/claude-sonnet-5.5", label: "Claude Sonnet 5.5", hint: "the best value for honest, well-written feedback" },
                Model { id: "anthropic/claude-opus-5.5", label: "Claude Opus 5.5", hint: "the sharpest, about twice the cost per brief" },
                Model { id: "openai/gpt-5", label: "GPT-5", hint: "strong reasoning, a different voice" },
                Model { id: "", label: "Cline's current model", hint: "whatever you picked in Cline (cline auth)" },
            ],
            Agent::OpenCode => &[
                Model { id: "", label: "OpenCode's default", hint: "the model set in your OpenCode config" },
                Model { id: "anthropic/claude-sonnet-5-5", label: "Claude Sonnet 5.5", hint: "needs Anthropic signed in to OpenCode" },
                Model { id: "openai/gpt-5", label: "GPT-5", hint: "needs OpenAI signed in to OpenCode" },
            ],
        }
    }

    /// Can it run `brief search` during a brief? Codex's read-only sandbox has no network for commands.
    pub fn runs_commands(self) -> bool {
        self != Agent::Codex
    }

    pub fn model_label(self, id: &str) -> String {
        match self.models().iter().find(|m| m.id == id) {
            Some(m) => m.label.to_owned(),
            None => id.to_owned(),
        }
    }

    pub fn find(self) -> Option<PathBuf> {
        if self == Agent::Claude {
            return crate::generate::find_claude();
        }
        find_bin(self.id())
    }

    /// Who it's signed in as: Some("") when signed in but the account is unknown, None when not.
    /// Blocking; runs the CLI's own status command where it has one.
    pub fn account(self) -> Option<String> {
        let bin = self.find()?;
        let out = |args: &[&str]| std::process::Command::new(&bin).args(args).stdin(Stdio::null()).output().ok();
        match self {
            Agent::Claude => {
                let o = out(&["auth", "status", "--json"])?;
                let v: Value = serde_json::from_slice(&o.stdout).ok()?;
                (v["loggedIn"] == true).then(|| v["email"].as_str().unwrap_or_default().to_owned())
            }
            Agent::Codex => {
                let o = out(&["login", "status"])?;
                let text = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
                (o.status.success() && !text.to_lowercase().contains("not logged")).then(String::new)
            }
            Agent::Cline => {
                let path = dirs::home_dir()?.join(".cline/data/settings/providers.json");
                let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
                let provider = v["lastUsedProvider"].as_str()?;
                let settings = &v["providers"][provider]["settings"];
                let signed_in = !settings["auth"].is_null() || settings["apiKey"].is_string();
                signed_in.then(|| settings["auth"]["metadata"]["userInfo"]["email"].as_str().unwrap_or(provider).to_owned())
            }
            Agent::OpenCode => {
                let o = out(&["auth", "list"])?;
                let text = String::from_utf8_lossy(&o.stdout);
                (o.status.success() && text.lines().any(|l| l.contains('●') || l.contains("oauth") || l.contains("api"))).then(String::new)
            }
        }
    }

    /// Run one prompt to completion and return the agent's final message.
    pub async fn ask(self, store: &Store, model: &str, prompt: &str, tools: Tools, timeout: std::time::Duration) -> Result<Reply> {
        let bin = self.find().with_context(|| format!("couldn't find the `{}` CLI; install it with `{}`", self.id(), self.install_cmd()))?;
        // An empty folder, so no project instructions (CLAUDE.md, AGENTS.md…) leak in.
        let work = store.root.join("work");
        let mut cmd = Command::new(&bin);
        cmd.current_dir(&work).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        // `brief search` must find this build and this store.
        cmd.env("BRIEF_HOME", &store.root).env("PATH", tool_path());
        let mut stdin_prompt = true;
        let mut last_message: Option<PathBuf> = None;
        match self {
            Agent::Claude => {
                cmd.args(["-p", "--output-format", "json"]);
                if !model.is_empty() {
                    cmd.args(["--model", model]);
                }
                match tools {
                    Tools::None => {
                        cmd.args(["--tools", ""]);
                    }
                    Tools::Research { composio, search } => {
                        let mut allowed = vec!["WebFetch"];
                        allowed.push(if search { "Bash(brief search:*)" } else { "WebSearch" });
                        if composio {
                            allowed.push("Bash(composio:*)");
                        }
                        let mut denied = vec!["Edit", "Write", "NotebookEdit"];
                        if search {
                            denied.push("WebSearch");
                        }
                        if !search && !composio {
                            denied.push("Bash");
                        }
                        cmd.args(["--allowedTools", &allowed.join(",")]);
                        cmd.args(["--disallowedTools", &denied.join(",")]);
                    }
                }
            }
            Agent::Codex => {
                let file = work.join(format!("codex-last-{}.txt", std::process::id()));
                cmd.args(["exec", "--skip-git-repo-check", "--sandbox", "read-only", "--color", "never"]);
                cmd.arg("--output-last-message").arg(&file);
                if !model.is_empty() {
                    cmd.args(["--model", model]);
                }
                if matches!(tools, Tools::Research { .. }) {
                    cmd.args(["-c", "tools.web_search=true"]);
                }
                cmd.arg("-");
                last_message = Some(file);
            }
            Agent::Cline => {
                // Cline's JSON mode wants the prompt as an argument; plan mode keeps it from
                // editing files, and the run ends with a `run_result` line holding the answer.
                cmd.arg("--json").arg("-c").arg(&work).args(["-t", &timeout.as_secs().to_string()]);
                if tools == Tools::None {
                    cmd.arg("-p");
                }
                if !model.is_empty() {
                    cmd.args(["-m", model]);
                }
                cmd.arg(prompt);
                cmd.stdin(Stdio::null());
                stdin_prompt = false;
            }
            Agent::OpenCode => {
                cmd.arg("run");
                if !model.is_empty() {
                    cmd.args(["-m", model]);
                }
                cmd.arg(prompt);
                cmd.stdin(Stdio::null());
                stdin_prompt = false;
            }
        }
        if stdin_prompt {
            cmd.stdin(Stdio::piped());
        }
        let mut child = cmd.spawn().with_context(|| format!("failed to start {}", self.id()))?;
        if stdin_prompt {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(prompt.as_bytes()).await?;
            drop(stdin);
        }
        let out = tokio::time::timeout(timeout, child.wait_with_output()).await.with_context(|| format!("{} timed out", self.name()))??;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let failed = |detail: &str| anyhow::anyhow!("{} failed ({}): {}", self.name(), out.status, detail.trim().chars().take(600).collect::<String>());

        match self {
            Agent::Claude => {
                if !out.status.success() {
                    return Err(failed(if stderr.trim().is_empty() { &stdout } else { &stderr }));
                }
                let v: Value = serde_json::from_str(&stdout).context("unexpected output from claude")?;
                if v["is_error"] == true {
                    bail!("Claude reported an error: {}", v["result"]);
                }
                Ok(Reply { text: v["result"].as_str().unwrap_or_default().to_owned(), cost_usd: v["total_cost_usd"].as_f64() })
            }
            Agent::Codex => {
                let file = last_message.unwrap();
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                let _ = std::fs::remove_file(&file);
                if !out.status.success() || text.trim().is_empty() {
                    return Err(failed(&stderr));
                }
                Ok(Reply { text, cost_usd: None })
            }
            Agent::Cline => {
                let result = stdout.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find(|v| v["type"] == "run_result");
                let Some(r) = result else {
                    let err = stdout.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find(|v| v["type"] == "error");
                    return Err(failed(err.as_ref().and_then(|e| e["message"].as_str()).unwrap_or(&stderr)));
                };
                if r["finishReason"] != "completed" {
                    return Err(failed(r["text"].as_str().unwrap_or("no answer")));
                }
                Ok(Reply { text: r["text"].as_str().unwrap_or_default().to_owned(), cost_usd: r["aggregateUsage"]["totalCost"].as_f64() })
            }
            Agent::OpenCode => {
                if !out.status.success() {
                    return Err(failed(&stderr));
                }
                Ok(Reply { text: strip_ansi(&stdout), cost_usd: None })
            }
        }
    }
}

/// The agent and model from config.json.
pub fn configured(store: &Store) -> (Agent, String) {
    let cfg = store.config();
    (Agent::from_id(&cfg.agent), cfg.model)
}

/// PATH for the agent: brief's own folder first, then the usual install spots (launchd's PATH is bare).
pub fn tool_path() -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = crate::connect::stable_exe().parent().map(|d| vec![d.to_path_buf()]).unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.extend([home.join(".local/bin"), home.join(".npm-global/bin")]);
    }
    dirs.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
    dirs.extend(["/opt/homebrew/bin".into(), "/usr/local/bin".into(), "/usr/bin".into(), "/bin".into()]);
    std::env::join_paths(dirs).unwrap_or_default()
}

pub fn find_bin(name: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    // launchd starts us with a bare PATH, so also check the usual install spots.
    dirs.extend([home.join(".local/bin"), home.join(".npm-global/bin"), home.join(".opencode/bin"), "/opt/homebrew/bin".into(), "/usr/local/bin".into()]);
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Take the JSON object out of an agent's reply. They sometimes wrap it in prose or fences, or
/// write one, notice a mistake and write a corrected one after it: the last complete object wins.
pub fn json_object(text: &str) -> &str {
    let mut found = "";
    let mut i = 0;
    while let Some(off) = text[i..].find('{') {
        let start = i + off;
        let mut objects = serde_json::Deserializer::from_str(&text[start..]).into_iter::<serde::de::IgnoredAny>();
        match objects.next() {
            Some(Ok(_)) => {
                let end = start + objects.byte_offset();
                found = &text[start..end];
                i = end;
            }
            _ => i = start + 1,
        }
    }
    if found.is_empty() {
        // Nothing parses on its own: hand back the outermost braces for the caller's fallback.
        if let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) {
            if b > a {
                return &text[a..=b];
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::json_object;

    #[test]
    fn last_complete_object_wins() {
        let text = r#"{"message": "a and b?", "suggestions": []}

Wait, that's two questions. Corrected:

{"message": "b?", "suggestions": ["{x}"]}"#;
        assert_eq!(json_object(text), r#"{"message": "b?", "suggestions": ["{x}"]}"#);
        assert_eq!(json_object("Here you go: ```json\n{\"a\": {\"b\": 1}}\n```"), r#"{"a": {"b": 1}}"#);
        assert_eq!(json_object("no json"), "");
    }
}
