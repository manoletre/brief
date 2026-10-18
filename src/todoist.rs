//! The user's Todoist: what they still plan to do, next to what they log and what they aim for.
//! Connecting is one ⏎ and a click on Agree (see oauth.rs). The brief only reads it; chatbots can
//! also add and edit to-dos through the MCP tools, when the user asks them to.
//!
//!   todoist.json   client id and tokens (readable only by you)

use crate::oauth::{self, json_ok, Provider};
use crate::store::Store;
use crate::todos;
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fmt::Write as _;

const API: &str = "https://api.todoist.com/api/v1";

pub const TODOIST: Provider = Provider {
    name: "Todoist",
    file: "todoist.json",
    register: "https://todoist.com/oauth/register",
    authorize: "https://todoist.com/oauth/authorize",
    token: "https://todoist.com/oauth/access_token",
    revoke: Some("https://todoist.com/api/v1/revoke"),
    scope: "data:read_write",
    resource: None,
};

pub fn auth(store: &Store) -> Option<oauth::Auth> {
    TODOIST.auth(store)
}

/// Opens Todoist's consent page and waits until the user allows (or denies) brief.
pub fn connect(store: &Store) -> Result<String> {
    let (mut a, _) = TODOIST.connect(store)?;
    if let Ok(user) = json_ok("GET", &format!("{API}/user"), Some(&a.access_token), &[], None) {
        a.account = oauth::account_name(user["full_name"].as_str().unwrap_or(""), user["email"].as_str().unwrap_or(""));
        TODOIST.save(store, &a)?;
    }
    // Read the list the way chatbots will, so a broken connection shows up now, not tomorrow.
    let open = snapshot(&a.access_token, true).with_context(|| "Connected to Todoist, but couldn't read your to-dos")?.tasks.len();
    let who = if a.account.is_empty() { String::new() } else { format!(" as {}", a.account) };
    Ok(format!("Connected to Todoist{who}: {open} open to-dos readable, and your chatbots can add and edit them."))
}

pub fn disconnect(store: &Store) -> Result<String> {
    TODOIST.disconnect(store)
}

fn token(store: &Store) -> Result<String> {
    TODOIST.access_token(store, false)
}

/// A token that may change tasks. Connections made when brief only asked to read stay read-only
/// until the user connects again.
fn write_token(store: &Store) -> Result<String> {
    let a = auth(store).context("The user hasn't connected a to-do list to brief yet. They can do it in `brief` → Connect.")?;
    ensure!(
        a.scope.contains("read_write"),
        "The user's to-do list is connected to brief read-only, so it can't be changed yet. To allow it, the user \
opens `brief` → Connect, picks their to-do app and presses ⏎."
    );
    token(store)
}

#[derive(Debug)]
struct Task {
    id: String,
    content: String,
    description: String,
    project: String,
    section: String,
    is_subtask: bool,
    priority: i64,
    due: Option<NaiveDate>,
    due_text: String,
    recurring: bool,
    deadline: Option<NaiveDate>,
    labels: Vec<String>,
    order: (i64, i64),
}

impl Task {
    fn line(&self, today: NaiveDate, with_project: bool) -> String {
        self.line_with(today, with_project, false)
    }

    fn line_with(&self, today: NaiveDate, with_project: bool, with_id: bool) -> String {
        let mut s = String::from(if self.is_subtask { "  - ↳ " } else { "- " });
        if with_project {
            let _ = write!(s, "[{}] ", self.project);
        }
        if !self.section.is_empty() {
            let _ = write!(s, "{} / ", self.section);
        }
        s.push_str(&one_line(&self.content, 140));
        if let Some(d) = self.due {
            let _ = write!(s, " · due {}", when(d, today));
            if self.recurring {
                let _ = write!(s, " ({})", self.due_text);
            }
        }
        if let Some(d) = self.deadline {
            let _ = write!(s, " · deadline {}", when(d, today));
        }
        if self.priority > 1 {
            let _ = write!(s, " · p{}", 5 - self.priority);
        }
        for l in &self.labels {
            let _ = write!(s, " @{l}");
        }
        if with_id {
            let _ = write!(s, " · id {}", self.id);
        }
        if !self.description.trim().is_empty() {
            let _ = write!(s, "\n    note: {}", one_line(&self.description, 160));
        }
        s
    }
}

/// The Todoist part of the brief prompt: what's late, due soon, planned per project, and done
/// since the last brief. Empty when Todoist isn't connected.
pub fn brief_context(store: &Store, since: DateTime<Local>) -> String {
    if auth(store).is_none() {
        return String::new();
    }
    match context(store, since) {
        Ok(text) => text,
        Err(e) => format!("(Todoist is connected but couldn't be read today: {e:#}. Mention this in one short line.)\n"),
    }
}

/// Projects, sections and open tasks, as of now.
struct Snapshot {
    projects: Vec<Value>,
    /// Project id → name, with the inbox called "Inbox".
    project_name: HashMap<String, String>,
    /// Section id → (name, project id, order).
    sections: HashMap<String, (String, String, i64)>,
    tasks: Vec<Task>,
}

fn id(v: &Value) -> String {
    v["id"].as_str().map(str::to_owned).unwrap_or_else(|| v["id"].to_string())
}

fn snapshot(token: &str, with_tasks: bool) -> Result<Snapshot> {
    let types = if with_tasks { r#"["projects","sections","items"]"# } else { r#"["projects","sections"]"# };
    let data = json_ok("POST", &format!("{API}/sync"), Some(token), &[("sync_token", "*"), ("resource_types", types)], None)?;

    let list = |k: &str| data[k].as_array().cloned().unwrap_or_default();
    let alive = |v: &Value| !v["is_deleted"].as_bool().unwrap_or(false) && !v["is_archived"].as_bool().unwrap_or(false);

    let projects: Vec<Value> = list("projects").into_iter().filter(alive).collect();
    let project_name: HashMap<String, String> = projects
        .iter()
        .map(|p| (id(p), if p["inbox_project"].as_bool() == Some(true) { "Inbox".into() } else { p["name"].as_str().unwrap_or("?").to_owned() }))
        .collect();
    let sections: HashMap<String, (String, String, i64)> = list("sections")
        .into_iter()
        .filter(alive)
        .map(|s| {
            let project = s["project_id"].as_str().unwrap_or_default().to_owned();
            (id(&s), (s["name"].as_str().unwrap_or("").to_owned(), project, s["section_order"].as_i64().unwrap_or(0)))
        })
        .collect();

    let tasks: Vec<Task> = list("items")
        .into_iter()
        .filter(|t| !t["checked"].as_bool().unwrap_or(false) && !t["is_deleted"].as_bool().unwrap_or(false))
        .filter_map(|t| to_task(&t, &project_name, &sections))
        .collect();
    Ok(Snapshot { projects, project_name, sections, tasks })
}

fn to_task(t: &Value, project_name: &HashMap<String, String>, sections: &HashMap<String, (String, String, i64)>) -> Option<Task> {
    let date = |v: &Value| v["date"].as_str().and_then(|d| NaiveDate::parse_from_str(d.get(..10)?, "%Y-%m-%d").ok());
    let project = project_name.get(t["project_id"].as_str().unwrap_or_default())?.clone();
    let section = sections.get(t["section_id"].as_str().unwrap_or_default());
    Some(Task {
        id: id(t),
        content: t["content"].as_str().unwrap_or("").to_owned(),
        description: t["description"].as_str().unwrap_or("").to_owned(),
        project,
        section: section.map(|s| s.0.clone()).unwrap_or_default(),
        is_subtask: !t["parent_id"].is_null(),
        priority: t["priority"].as_i64().unwrap_or(1),
        due: date(&t["due"]),
        due_text: t["due"]["string"].as_str().unwrap_or("").to_owned(),
        recurring: t["due"]["is_recurring"].as_bool().unwrap_or(false),
        deadline: date(&t["deadline"]),
        labels: t["labels"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_owned)).collect(),
        order: (section.map_or(-1, |s| s.2), t["child_order"].as_i64().unwrap_or(0)),
    })
}

impl Snapshot {
    /// One to-do as a line for a chatbot, with its id.
    fn describe(&self, t: &Value) -> String {
        match to_task(t, &self.project_name, &self.sections) {
            Some(t) => t.line_with(Local::now().date_naive(), true, true).trim_start_matches("- ").to_owned(),
            None => format!("{} · id {}", t["content"].as_str().unwrap_or("?"), id(t)),
        }
    }

    /// The id of the project called `name` (case-insensitive; "Inbox" is the inbox).
    fn project_id(&self, name: &str) -> Result<String> {
        if let Some((id, _)) = self.project_name.iter().find(|(_, n)| n.eq_ignore_ascii_case(name.trim())) {
            return Ok(id.clone());
        }
        let mut names: Vec<&str> = self.project_name.values().map(String::as_str).collect();
        names.sort_unstable();
        bail!("no Todoist project called {name:?}. Projects: {}", names.join(", "))
    }

    /// The id of the section called `name` in that project.
    fn section_id(&self, project_id: &str, name: &str) -> Result<String> {
        let in_project = || self.sections.iter().filter(|(_, s)| s.1 == project_id);
        if let Some((id, _)) = in_project().find(|(_, s)| s.0.eq_ignore_ascii_case(name.trim())) {
            return Ok(id.clone());
        }
        let names: Vec<&str> = in_project().map(|(_, s)| s.0.as_str()).collect();
        let project = self.project_name.get(project_id).map_or("?", String::as_str);
        match names.is_empty() {
            true => bail!("project {project:?} has no sections"),
            false => bail!("no section called {name:?} in {project:?}. Sections: {}", names.join(", ")),
        }
    }
}

fn context(store: &Store, since: DateTime<Local>) -> Result<String> {
    let token = token(store)?;
    let Snapshot { projects, project_name, tasks, .. } = snapshot(&token, true)?;
    let today = Local::now().date_naive();

    let mut p = String::new();
    let _ = writeln!(p, "{} open tasks in {} projects. Priorities: p1 is the most urgent.\n", tasks.len(), projects.len());

    let soonest = |t: &Task| [t.due, t.deadline].into_iter().flatten().min();
    let mut dated: Vec<&Task> = tasks.iter().filter(|t| soonest(t).is_some_and(|d| d <= today + Duration::days(7))).collect();
    dated.sort_by_key(|t| (soonest(t), -t.priority));
    let groups: [(&str, Box<dyn Fn(NaiveDate) -> bool>); 3] = [
        ("Overdue", Box::new(move |d| d < today)),
        ("Due today", Box::new(move |d| d == today)),
        ("Due in the next 7 days", Box::new(move |d| d > today)),
    ];
    for (title, pick) in groups {
        let group: Vec<_> = dated.iter().filter(|t| soonest(t).is_some_and(&pick)).collect();
        if group.is_empty() {
            continue;
        }
        let _ = writeln!(p, "{title} ({}):", group.len());
        for t in group.iter().take(40) {
            let _ = writeln!(p, "{}", t.line(today, true));
        }
        if group.len() > 40 {
            let _ = writeln!(p, "- …and {} more", group.len() - 40);
        }
        p.push('\n');
    }

    p.push_str("Every project with its open tasks (undated ones are plans not yet scheduled):\n");
    let mut by_project: Vec<(&String, Vec<&Task>)> = Vec::new();
    for proj in &projects {
        let name = &project_name[&id(proj)];
        let mut ts: Vec<&Task> = tasks.iter().filter(|t| &t.project == name).collect();
        ts.sort_by_key(|t| t.order);
        by_project.push((name, ts));
    }
    for (name, ts) in &by_project {
        let late = ts.iter().filter(|t| soonest(t).is_some_and(|d| d < today)).count();
        let _ = write!(p, "\n## {name} ({} open", ts.len());
        if late > 0 {
            let _ = write!(p, ", {late} overdue");
        }
        p.push_str(")\n");
        for t in ts.iter().take(25) {
            let _ = writeln!(p, "{}", t.line(today, false));
        }
        if ts.len() > 25 {
            let _ = writeln!(p, "- …and {} more", ts.len() - 25);
        }
    }

    // Completed since the last brief (at most a week back, at least a day).
    let from = since.max(Local::now() - Duration::days(7)).min(Local::now() - Duration::days(1));
    let done = completed(&token, from).unwrap_or_default();
    if !done.is_empty() {
        let _ = writeln!(p, "\nCompleted since {} ({}):", from.format("%a %b %-d"), done.len());
        for (at, project_id, content) in done.iter().take(40) {
            let project = project_name.get(project_id).map_or("?", String::as_str);
            let _ = writeln!(p, "- {} [{project}] {}", at.format("%a %H:%M"), one_line(content, 140));
        }
    }
    Ok(p)
}

/// Tasks completed after `from`: (when, project id, text), oldest first.
fn completed(token: &str, from: DateTime<Local>) -> Result<Vec<(DateTime<Local>, String, String)>> {
    let fmt = |t: DateTime<Local>| t.with_timezone(&Utc).format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let (since, until) = (fmt(from), fmt(Local::now()));
    let mut out = Vec::new();
    let mut cursor = String::new();
    for _ in 0..5 {
        let mut form = vec![("since", since.as_str()), ("until", until.as_str()), ("limit", "200")];
        if !cursor.is_empty() {
            form.push(("cursor", &cursor));
        }
        let page = json_ok("GET", &format!("{API}/tasks/completed/by_completion_date"), Some(token), &form, None)?;
        for t in page["items"].as_array().into_iter().flatten() {
            let Some(at) = t["completed_at"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()) else { continue };
            out.push((at.with_timezone(&Local), t["project_id"].as_str().unwrap_or("").to_owned(), t["content"].as_str().unwrap_or("").to_owned()));
        }
        match page["next_cursor"].as_str() {
            Some(c) if !c.is_empty() => cursor = c.to_owned(),
            _ => break,
        }
    }
    out.sort_by_key(|t| t.0);
    Ok(out)
}

// ── Changing to-dos (for the MCP tools, through todos.rs) ──

/// Todoist as the user's to-do list.
pub struct App;

impl todos::TodoApp for App {
    fn connected(&self, store: &Store) -> bool {
        auth(store).is_some()
    }

    fn find(&self, store: &Store, query: &str, list: &str, limit: usize) -> Result<String> {
        let snap = snapshot(&token(store)?, true)?;
        let list = if list.trim().is_empty() { None } else { Some(&snap.project_name[&snap.project_id(list)?]) };
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let today = Local::now().date_naive();
        let soonest = |t: &Task| [t.due, t.deadline].into_iter().flatten().min().unwrap_or(NaiveDate::MAX);
        let project_order: HashMap<&str, usize> = snap.projects.iter().enumerate().map(|(i, p)| (snap.project_name[&id(p)].as_str(), i)).collect();
        let mut hits: Vec<&Task> = snap
            .tasks
            .iter()
            .filter(|t| list.map_or(true, |l| &t.project == l))
            .filter(|t| {
                let text = format!("{} {}", t.content, t.description).to_lowercase();
                words.iter().all(|w| text.contains(w))
            })
            .collect();
        hits.sort_by_key(|t| (soonest(t), project_order.get(t.project.as_str()).copied(), t.order));

        let mut names: Vec<&str> = snap.projects.iter().map(|p| snap.project_name[&id(p)].as_str()).collect();
        names.dedup();
        let mut out = format!("Lists: {}

", names.join(", "));
        let filtered = !words.is_empty() || list.is_some();
        let what = match (hits.len(), filtered) {
            (1, true) => "open to-do matches",
            (1, false) => "open to-do",
            (_, true) => "open to-dos match",
            (_, false) => "open to-dos",
        };
        let _ = writeln!(out, "{} {what}, soonest first (p1 is the most urgent):", hits.len());
        for t in hits.iter().take(limit) {
            let _ = writeln!(out, "{}", t.line_with(today, true, true));
        }
        if hits.len() > limit {
            let _ = writeln!(out, "- …and {} more (narrow with `query` or `list`)", hits.len() - limit);
        }
        Ok(out)
    }

    fn add(&self, store: &Store, todo: todos::NewTodo) -> Result<String> {
        let token = write_token(store)?;
        let snap = snapshot(&token, false)?;
        let mut body = Map::new();
        body.insert("content".into(), json!(todo.content.trim()));
        if !todo.description.trim().is_empty() {
            body.insert("description".into(), json!(todo.description.trim()));
        }
        if !todo.parent_id.trim().is_empty() {
            body.insert("parent_id".into(), json!(todo.parent_id.trim()));
        }
        let project = match todo.list.trim() {
            "" => None,
            l => Some(snap.project_id(l)?),
        };
        if let Some(p) = &project {
            body.insert("project_id".into(), json!(p));
        }
        if !todo.section.trim().is_empty() {
            let p = project.clone().or_else(|| inbox(&snap)).context("say which `list` the section is in")?;
            body.insert("section_id".into(), json!(snap.section_id(&p, &todo.section)?));
        }
        if !todo.due.trim().is_empty() {
            body.insert("due_string".into(), json!(todo.due.trim()));
        }
        if !todo.deadline.trim().is_empty() {
            body.insert("deadline_date".into(), json!(deadline(&todo.deadline)?));
        }
        if let Some(p) = todo.priority {
            body.insert("priority".into(), json!(5 - p as i64));
        }
        if !todo.labels.is_empty() {
            body.insert("labels".into(), json!(todo.labels));
        }
        let task = call("POST", &format!("{API}/tasks"), &token, Some(&Value::Object(body)))?;
        Ok(format!("Added: {}", snap.describe(&task)))
    }

    fn update(&self, store: &Store, c: todos::Change) -> Result<String> {
        let token = write_token(store)?;
        let snap = snapshot(&token, false)?;
        let task_url = format!("{API}/tasks/{}", oauth::enc(c.id.trim()));
        let mut task = call("GET", &task_url, &token, None).map_err(|e| anyhow::anyhow!("no to-do with id {:?} ({e})", c.id))?;
        let mut done = Vec::new();

        let mut body = Map::new();
        if let Some(v) = &c.content {
            ensure!(!v.trim().is_empty(), "`content` can't be empty");
            body.insert("content".into(), json!(v.trim()));
        }
        if let Some(v) = &c.description {
            body.insert("description".into(), json!(v.trim()));
        }
        if let Some(v) = &c.due {
            body.insert("due_string".into(), json!(if is_none(v) { "no date" } else { v.trim() }));
        }
        if let Some(v) = &c.deadline {
            body.insert("deadline_date".into(), if is_none(v) { Value::Null } else { json!(deadline(v)?) });
        }
        if let Some(p) = c.priority {
            body.insert("priority".into(), json!(5 - p as i64));
        }
        if let Some(l) = &c.labels {
            body.insert("labels".into(), json!(l));
        }
        if !body.is_empty() {
            task = call("POST", &task_url, &token, Some(&Value::Object(body)))?;
            done.push("updated");
        }

        let list = c.list.as_deref().map(str::trim).filter(|l| !l.is_empty());
        let section = c.section.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if list.is_some() || section.is_some() {
            let project = match list {
                Some(l) => snap.project_id(l)?,
                None => task["project_id"].as_str().unwrap_or_default().to_owned(),
            };
            let target = match section {
                Some(s) => json!({ "section_id": snap.section_id(&project, s)? }),
                None => json!({ "project_id": project }),
            };
            call("POST", &format!("{task_url}/move"), &token, Some(&target))?;
            done.push("moved");
        }

        match c.status {
            Some(todos::Status::Done) => {
                call("POST", &format!("{task_url}/close"), &token, None)?;
                done.push("marked done");
            }
            Some(todos::Status::Open) => {
                call("POST", &format!("{task_url}/reopen"), &token, None)?;
                done.push("reopened");
            }
            None => {}
        }
        ensure!(!done.is_empty(), "nothing to change: give at least one field to update, `list`/`section` to move, or `status`");
        if done.iter().any(|d| *d == "moved" || *d == "reopened") {
            task = call("GET", &task_url, &token, None).unwrap_or(task);
        }
        let mut what = done.join(", ");
        what[..1].make_ascii_uppercase();
        Ok(format!("{what}: {}", snap.describe(&task)))
    }
}

fn inbox(snap: &Snapshot) -> Option<String> {
    snap.project_name.iter().find(|(_, n)| *n == "Inbox").map(|(id, _)| id.clone())
}

/// A request that may answer with no body (closing a task answers 204).
fn call(method: &str, url: &str, token: &str, body: Option<&Value>) -> Result<Value> {
    let r = oauth::request(method, url, Some(token), &[], &[], body, false)?;
    match r.code {
        200..=299 if r.body.trim().is_empty() => Ok(Value::Null),
        200..=299 => serde_json::from_str(&r.body).context("unexpected answer from the to-do app"),
        403 => bail!("the to-do app refused the change (403): {}", r.body.trim()),
        code => bail!("the to-do app answered {code}: {}", r.body.chars().take(300).collect::<String>().trim()),
    }
}

fn deadline(s: &str) -> Result<String> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map(|d| d.to_string()).map_err(|_| anyhow::anyhow!("`deadline` must be YYYY-MM-DD (or \"none\" to clear it)"))
}

fn is_none(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "" | "none" | "no date" | "null")
}

// ── Helpers ──

fn when(d: NaiveDate, today: NaiveDate) -> String {
    let days = (d - today).num_days();
    let date = d.format("%a %b %-d");
    match days {
        0 => "today".into(),
        1 => "tomorrow".into(),
        n if n < 0 => format!("{date} ({} days late)", -n),
        _ => date.to_string(),
    }
}

fn one_line(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= n {
        s
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}
