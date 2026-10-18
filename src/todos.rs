//! The user's to-do list, whichever app it lives in. Chatbots see one list of to-dos through the
//! MCP tools (`get_todos`, `add_todo`, `update_todo`) and never learn which app is behind it;
//! brief talks to the connected app. Today that's Todoist (todoist.rs); another app only needs
//! to implement `TodoApp` and be added to `connected`.

use crate::store::Store;
use anyhow::Result;

/// A to-do to create. Lists and sections are named as the user sees them.
#[derive(Debug, Default)]
pub struct NewTodo {
    pub content: String,
    pub description: String,
    /// The list (project) to put it in; the app's default list when empty.
    pub list: String,
    pub section: String,
    /// When it's due, in plain words: "tomorrow", "friday 9am", "every monday".
    pub due: String,
    /// A hard deadline, YYYY-MM-DD.
    pub deadline: String,
    /// 1 (most urgent) to 4 (normal).
    pub priority: Option<u8>,
    pub labels: Vec<String>,
    /// Make it a sub-to-do of this one.
    pub parent_id: String,
}

/// Changes to an existing to-do. `None` leaves a field as it is.
#[derive(Debug, Default)]
pub struct Change {
    pub id: String,
    pub content: Option<String>,
    pub description: Option<String>,
    /// Plain words, or "none" to clear it.
    pub due: Option<String>,
    /// YYYY-MM-DD, or "none" to clear it.
    pub deadline: Option<String>,
    pub priority: Option<u8>,
    /// Replaces all labels.
    pub labels: Option<Vec<String>>,
    /// Move to this list (and `section`, if given).
    pub list: Option<String>,
    pub section: Option<String>,
    pub status: Option<Status>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Done,
    Open,
}

/// A to-do app brief can connect to. Each call answers in text for a chatbot, never naming the app.
pub trait TodoApp: Sync {
    fn connected(&self, store: &Store) -> bool;
    /// Open to-dos matching `query` (words in the text or note) in `list`, both optional, with ids.
    fn find(&self, store: &Store, query: &str, list: &str, limit: usize) -> Result<String>;
    fn add(&self, store: &Store, todo: NewTodo) -> Result<String>;
    fn update(&self, store: &Store, change: Change) -> Result<String>;
}

const APPS: &[&dyn TodoApp] = &[&crate::todoist::App];

/// The to-do app the user connected, if any.
pub fn connected(store: &Store) -> Option<&'static dyn TodoApp> {
    APPS.iter().copied().find(|a| a.connected(store))
}

fn app(store: &Store) -> Result<&'static dyn TodoApp> {
    connected(store).ok_or_else(|| {
        anyhow::anyhow!("The user hasn't connected a to-do list to brief yet. They can do it in `brief` → Connect.")
    })
}

pub fn find(store: &Store, query: &str, list: &str, limit: usize) -> Result<String> {
    app(store)?.find(store, query, list, limit)
}

pub fn add(store: &Store, todo: NewTodo) -> Result<String> {
    anyhow::ensure!(!todo.content.trim().is_empty(), "`content` is required");
    check_priority(todo.priority)?;
    app(store)?.add(store, todo)
}

pub fn update(store: &Store, change: Change) -> Result<String> {
    anyhow::ensure!(!change.id.trim().is_empty(), "`id` is required (find it with get_todos)");
    check_priority(change.priority)?;
    app(store)?.update(store, change)
}

fn check_priority(p: Option<u8>) -> Result<()> {
    anyhow::ensure!(p.map_or(true, |p| (1..=4).contains(&p)), "`priority` must be 1 (most urgent) to 4 (normal)");
    Ok(())
}
