//! How the brief writer searches the web for things worth the user's time. Four options:
//!
//!   parallel     Parallel's Search and Extract APIs (parallel.ai): objective-driven search
//!                that returns the relevant excerpts of each page, and reads pages in full
//!   brave        the Brave Search API: an independent index, plain results
//!   browserbase  Browserbase's Search API (the team behind browse.sh)
//!   claude       the agent's own web search, nothing to set up
//!
//! The agent reaches the first three through `brief search`, which it may run during a brief.
//! The choice and the API keys are kept in search.json (readable only by you); keys can also
//! come from PARALLEL_API_KEY, BRAVE_API_KEY and BROWSERBASE_API_KEY. Parallel needs no key when
//! its own CLI is signed in (`parallel-cli login`, a browser sign-in): brief then searches through it.

use crate::oauth;
use crate::store::Store;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Provider {
    Parallel,
    Brave,
    Browserbase,
    Claude,
}

/// In the order onboarding shows them.
pub const ALL: [Provider; 4] = [Provider::Parallel, Provider::Brave, Provider::Browserbase, Provider::Claude];

impl Provider {
    pub fn id(self) -> &'static str {
        match self {
            Provider::Parallel => "parallel",
            Provider::Brave => "brave",
            Provider::Browserbase => "browserbase",
            Provider::Claude => "claude",
        }
    }

    pub fn from_id(id: &str) -> Option<Provider> {
        match id {
            "browser" | "browser.sh" | "browse.sh" => Some(Provider::Browserbase),
            _ => ALL.into_iter().find(|p| p.id() == id),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Parallel => "Parallel",
            Provider::Brave => "Brave Search",
            Provider::Browserbase => "Browserbase",
            Provider::Claude => "Just your agent",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Provider::Parallel => "search built for agents: finds the passages that answer the question, reads whole pages",
            Provider::Brave => "an independent web index, fast plain results",
            Provider::Browserbase => "Browserbase Search (browse.sh), web results for agents",
            Provider::Claude => "the agent's built-in web search, nothing to set up",
        }
    }

    /// Where to get an API key (None: needs none).
    pub fn key_url(self) -> Option<&'static str> {
        match self {
            Provider::Parallel => Some("https://platform.parallel.ai"),
            Provider::Brave => Some("https://api-dashboard.search.brave.com/app/keys"),
            Provider::Browserbase => Some("https://www.browserbase.com/settings"),
            Provider::Claude => None,
        }
    }

    fn env(self) -> &'static str {
        match self {
            Provider::Parallel => "PARALLEL_API_KEY",
            Provider::Brave => "BRAVE_API_KEY",
            Provider::Browserbase => "BROWSERBASE_API_KEY",
            Provider::Claude => "",
        }
    }

    /// Can it read a page's text (`brief search --read`)? Otherwise the agent fetches pages itself.
    pub fn reads(self) -> bool {
        self == Provider::Parallel
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}

fn path(store: &Store) -> std::path::PathBuf {
    store.root.join("search.json")
}

pub fn settings(store: &Store) -> Settings {
    std::fs::read_to_string(path(store)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(store: &Store, s: &Settings) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path(store))?;
    f.write_all(serde_json::to_string_pretty(s)?.as_bytes())?;
    Ok(())
}

pub fn key(store: &Store, p: Provider) -> Option<String> {
    settings(store).keys.get(p.id()).cloned().filter(|k| !k.is_empty()).or_else(|| std::env::var(p.env()).ok().filter(|k| !k.is_empty()))
}

/// The provider the brief uses: the one chosen, if it can be used; otherwise the agent's own.
pub fn active(store: &Store) -> Provider {
    match Provider::from_id(&settings(store).provider) {
        Some(Provider::Claude) | None => Provider::Claude,
        Some(p) if ready(store, p) => p,
        Some(_) => Provider::Claude,
    }
}

/// It has what it needs: an API key, or for Parallel a signed-in `parallel-cli`.
pub fn ready(store: &Store, p: Provider) -> bool {
    p == Provider::Claude || key(store, p).is_some() || (p == Provider::Parallel && parallel_signed_in())
}

/// Installs Parallel's CLI (`parallel-cli`), which signs in through the browser.
pub const PARALLEL_CLI_INSTALL: &str = "uv tool install 'parallel-web-tools[cli]'";

pub fn parallel_cli() -> Option<std::path::PathBuf> {
    crate::agent::find_bin("parallel-cli")
}

static SIGNED_IN: std::sync::Mutex<Option<(std::time::Instant, bool)>> = std::sync::Mutex::new(None);

/// `parallel-cli auth` says it's signed in. Remembered for a minute (screens ask on every frame).
pub fn parallel_signed_in() -> bool {
    if let Some((at, yes)) = *SIGNED_IN.lock().unwrap() {
        if at.elapsed().as_secs() < 60 {
            return yes;
        }
    }
    let yes = parallel_cli()
        .and_then(|bin| std::process::Command::new(bin).args(["auth", "--json"]).stdin(std::process::Stdio::null()).output().ok())
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .is_some_and(|v| v["authenticated"] == true && v["selected_org_id"] != "legacy");
    *SIGNED_IN.lock().unwrap() = Some((std::time::Instant::now(), yes));
    yes
}

/// Forget the remembered sign-in state (after a `parallel-cli login`).
pub fn recheck_parallel() {
    *SIGNED_IN.lock().unwrap() = None;
}

/// Run `parallel-cli … --json` and return its JSON.
fn parallel_cli_json(args: &[String]) -> Result<Value> {
    let bin = parallel_cli().context("parallel-cli isn't installed")?;
    let out = std::process::Command::new(bin).args(args).arg("--json").stdin(std::process::Stdio::null()).output().context("couldn't run parallel-cli")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
        bail!("parallel-cli failed: {}", err.trim().chars().take(400).collect::<String>());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(crate::agent::json_object(&text)).context("unexpected output from parallel-cli")
}

pub fn choose(store: &Store, p: Provider, api_key: Option<&str>) -> Result<()> {
    let mut s = settings(store);
    s.provider = p.id().into();
    if let Some(k) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        s.keys.insert(p.id().into(), k.into());
    }
    save(store, &s)
}

/// One result, whichever provider found it.
#[derive(Clone)]
struct Hit {
    title: String,
    url: String,
    date: String,
    text: String,
}

/// Search with several queries towards one objective. Blocking.
pub fn search(store: &Store, queries: &[String], objective: &str) -> Result<String> {
    let p = active(store);
    if p == Provider::Claude {
        bail!("no search provider is set up; use your own web search");
    }
    let api_key = key(store, p).unwrap_or_default();
    let hits = match p {
        Provider::Parallel if api_key.is_empty() => {
            let mut args: Vec<String> = vec!["search".into()];
            if !objective.is_empty() {
                args.push(objective.into());
            }
            for q in queries {
                args.extend(["-q".into(), q.clone()]);
            }
            args.extend(["--mode", "advanced", "--max-results", "8", "--excerpt-max-chars-per-result", "1500", "--excerpt-max-chars-total", "14000"].map(String::from));
            parallel_hits(&parallel_cli_json(&args)?)
        }
        Provider::Parallel => parallel_search(&api_key, queries, objective)?,
        Provider::Brave => queries.iter().map(|q| brave(&api_key, q)).collect::<Result<Vec<_>>>()?.concat(),
        Provider::Browserbase => queries.iter().map(|q| browserbase(&api_key, q)).collect::<Result<Vec<_>>>()?.concat(),
        Provider::Claude => unreachable!(),
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = format!("{} results for {}\n\n", p.name(), queries.iter().map(|q| format!("\"{q}\"")).collect::<Vec<_>>().join(", "));
    for (n, h) in hits.into_iter().filter(|h| seen.insert(h.url.clone())).enumerate() {
        let _ = writeln!(out, "[{}] {}\n    {}{}", n + 1, h.title.trim(), h.url, if h.date.is_empty() { String::new() } else { format!(" ({})", h.date) });
        if !h.text.trim().is_empty() {
            for line in h.text.trim().lines().filter(|l| !l.trim().is_empty()) {
                let _ = writeln!(out, "    {}", line.trim());
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// The relevant parts of pages (Parallel only). Blocking.
pub fn read(store: &Store, urls: &[String], objective: &str) -> Result<String> {
    let p = active(store);
    if !p.reads() {
        bail!("{} can't read pages; fetch them with your own tools", p.name());
    }
    let v = match key(store, p) {
        Some(api_key) => {
            let mut body = json!({"urls": urls, "advanced_settings": {"excerpt_settings": {"max_chars_per_result": 6000}}, "max_chars_total": 20000});
            if !objective.is_empty() {
                body["objective"] = objective.into();
            }
            post("https://api.parallel.ai/v1/extract", &format!("x-api-key: {api_key}"), &body)?
        }
        None => {
            let mut args: Vec<String> = vec!["extract".into()];
            args.extend(urls.iter().cloned());
            if !objective.is_empty() {
                args.extend(["--objective".into(), objective.into()]);
            }
            args.extend(["--excerpt-max-chars-per-result", "6000", "--excerpt-max-chars-total", "20000"].map(String::from));
            parallel_cli_json(&args)?
        }
    };
    let mut out = String::new();
    for r in v["results"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "# {}\n{}\n", r["title"].as_str().unwrap_or_default(), r["url"].as_str().unwrap_or_default());
        for e in r["excerpts"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let _ = writeln!(out, "{}\n", e.trim());
        }
    }
    for e in v["errors"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "couldn't read {}: {}", e["url"].as_str().unwrap_or_default(), e["error_type"].as_str().unwrap_or_default());
    }
    Ok(out)
}

fn parallel_search(api_key: &str, queries: &[String], objective: &str) -> Result<Vec<Hit>> {
    let mut body = json!({
        "search_queries": queries,
        "mode": "advanced",
        "max_chars_total": 14000,
        "advanced_settings": {"max_results": 8, "excerpt_settings": {"max_chars_per_result": 1500}},
    });
    if !objective.is_empty() {
        body["objective"] = objective.into();
    }
    let v = post("https://api.parallel.ai/v1/search", &format!("x-api-key: {api_key}"), &body)?;
    Ok(parallel_hits(&v))
}

/// Results from Parallel's API or its CLI (both give url, title, publish_date, excerpts).
fn parallel_hits(v: &Value) -> Vec<Hit> {
    v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Hit {
            title: r["title"].as_str().unwrap_or_default().into(),
            url: r["url"].as_str().unwrap_or_default().into(),
            date: r["publish_date"].as_str().unwrap_or_default().into(),
            text: r["excerpts"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join("\n…\n"),
        })
        .collect()
}

fn brave(api_key: &str, q: &str) -> Result<Vec<Hit>> {
    let url = format!("https://api.search.brave.com/res/v1/web/search?count=8&extra_snippets=1&q={}", crate::html::url_encode(q));
    let r = oauth::request("GET", &url, None, &["Accept: application/json", &format!("X-Subscription-Token: {api_key}")], &[], None, false)?;
    let v = ok_json("Brave", r)?;
    Ok(v["web"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let mut text = strip_tags(r["description"].as_str().unwrap_or_default());
            for s in r["extra_snippets"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                text.push('\n');
                text.push_str(&strip_tags(s));
            }
            Hit {
                title: strip_tags(r["title"].as_str().unwrap_or_default()),
                url: r["url"].as_str().unwrap_or_default().into(),
                date: r["page_age"].as_str().or(r["age"].as_str()).unwrap_or_default().chars().take(10).collect(),
                text,
            }
        })
        .collect())
}

fn browserbase(api_key: &str, q: &str) -> Result<Vec<Hit>> {
    let q: String = q.chars().take(200).collect();
    let v = post("https://api.browserbase.com/v1/search", &format!("x-bb-api-key: {api_key}"), &json!({"query": q, "numResults": 8}))?;
    Ok(v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| Hit {
            title: r["title"].as_str().unwrap_or_default().into(),
            url: r["url"].as_str().unwrap_or_default().into(),
            date: r["publishedDate"].as_str().unwrap_or_default().chars().take(10).collect(),
            text: r["author"].as_str().map(|a| format!("by {a}")).unwrap_or_default(),
        })
        .collect())
}

fn post(url: &str, auth_header: &str, body: &Value) -> Result<Value> {
    let host = url.split('/').nth(2).unwrap_or(url);
    ok_json(host, oauth::request("POST", url, None, &[auth_header], &[], Some(body), false)?)
}

fn ok_json(who: &str, r: oauth::Response) -> Result<Value> {
    if !(200..300).contains(&r.code) {
        bail!("{who} answered {}: {}", r.code, r.body.chars().take(300).collect::<String>().trim());
    }
    serde_json::from_str(&r.body).with_context(|| format!("unexpected answer from {who}"))
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#x27;", "'").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">")
}

/// A small search to check a key works. Blocking.
pub fn test(store: &Store) -> Result<String> {
    let p = active(store);
    if p == Provider::Claude {
        return Ok("The brief writer will use its own web search.".into());
    }
    let out = search(store, &["Liquid AI LFM2 technical report".into()], "Check the search connection works")?;
    let n = out.lines().filter(|l| l.starts_with('[')).count();
    anyhow::ensure!(n > 0, "{} answered, but with no results", p.name());
    Ok(format!("{} works: {n} results for a test search.", p.name()))
}

/// What the brief prompt says about searching with `p`.
pub fn prompt_section_for(_store: &Store, p: Provider) -> String {
    if p == Provider::Claude {
        return "Search with your web search tool and read pages with your fetch tool.".into();
    }
    let exe = "brief";
    let mut s = format!(
        "Search with {name}, through the `{exe} search` command (run it with your shell tool; your own web search is \
off for this brief). Give it 2-4 short keyword queries for ONE question at a time, and an objective in plain words \
that says what you're really looking for and why:\n\n\
    {exe} search \"chess LLM UCI tokenizer\" \"move tokenization language model chess\" --objective \"Evidence on whether \
one-token-per-move vocabularies beat BPE for small chess LLMs; papers or repos from 2025-2026\"\n\n\
Each result comes with its title, address, date and {what}. Run it many times, one question each.\n",
        name = p.name(),
        what = if p == Provider::Parallel { "the passages most relevant to your objective" } else { "a snippet" }
    );
    if p.reads() {
        let _ = write!(
            s,
            "Read a page (or several, space-separated) with:\n\n    {exe} search --read https://arxiv.org/abs/2501.00001 --objective \"what they found about X\"\n\n\
It returns the parts of the page that matter for the objective, as markdown. Read every source you recommend.\n"
        );
    } else {
        s.push_str("Read pages with your fetch tool. Read every source you recommend.\n");
    }
    s
}

/// `brief search …` from the command line (what the brief writer runs).
pub fn cli(store: &Store, args: &[String]) -> Result<()> {
    let mut queries = Vec::new();
    let mut urls = Vec::new();
    let mut objective = String::new();
    let mut reading = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--objective" | "-o" => objective = it.next().cloned().unwrap_or_default(),
            "--read" | "-r" => reading = true,
            _ if reading => urls.push(a.clone()),
            _ => queries.push(a.clone()),
        }
    }
    if !urls.is_empty() {
        print!("{}", read(store, &urls, &objective)?);
        return Ok(());
    }
    match queries.first().map(String::as_str) {
        None | Some("status") => {
            let p = active(store);
            let chosen = Provider::from_id(&settings(store).provider);
            let how = match p {
                Provider::Parallel if key(store, p).is_none() => " (signed in with parallel-cli)",
                _ => "",
            };
            println!("Search: {}{how}", p.name());
            if let Some(c) = chosen.filter(|c| *c != p) {
                if c == Provider::Parallel {
                    println!("(Parallel is chosen but not signed in: `brief search setup`, or `parallel-cli login`)");
                } else {
                    println!("({} is chosen but has no API key: set one with `brief search use {} <key>`)", c.name(), c.id());
                }
            }
            println!("Options: {}", ALL.map(|p| p.id()).join(", "));
            println!("Change it with `brief search use <provider> [api-key]`, or in `brief init`.");
            Ok(())
        }
        Some("use") if queries.len() >= 2 => {
            let p = Provider::from_id(&queries[1]).with_context(|| format!("unknown provider {}; options: {}", queries[1], ALL.map(|p| p.id()).join(", ")))?;
            choose(store, p, queries.get(2).map(String::as_str))?;
            if !ready(store, p) {
                if p == Provider::Parallel {
                    bail!("Parallel needs you signed in: run `parallel-cli login` (install: {PARALLEL_CLI_INSTALL}), or pass an API key");
                }
                bail!("{} needs an API key: brief search use {} <key>  (get one at {})", p.name(), p.id(), p.key_url().unwrap_or_default());
            }
            println!("{}", test(store)?);
            Ok(())
        }
        Some("test") => {
            println!("{}", test(store)?);
            Ok(())
        }
        Some(_) => {
            print!("{}", search(store, &queries, &objective)?);
            Ok(())
        }
    }
}
