//! Ledger: each familiar's goal, status, what it's waiting on, and its next step, kept in the
//! `ledgers` table so it survives restarts. A familiar gets its ledger when a chat starts and keeps it
//! current with the read_ledger and update_ledger tools; the person can edit it in the chat view.
//!
//! Claude reuses a chat's system prompt when it resumes, and Codex may too, so a resumed chat doesn't
//! see the ledger again. The familiar knows its own changes from the conversation, so only the
//! person's edits need passing on: the next turn after one starts with a short note (`turn_note`).

use crate::db::{now_ms, Agent, Db};
use crate::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter, State};

pub const TOOL_NAMES: [&str; 2] = ["read_ledger", "update_ledger"];
const CHANGED: &str = "ledger_changed";
pub const STATUSES: [&str; 5] = ["idle", "working", "waiting", "blocked", "done"];
const GOAL_LIMIT: usize = 300;
const WAITING_LIMIT: usize = 300;
const NEXT_LIMIT: usize = 1000;

/// The database the chat-start prompt reads from, set once in setup.
static DB: OnceLock<Arc<Db>> = OnceLock::new();

pub fn init(db: Arc<Db>) {
    let _ = DB.set(db);
}

/// Schema version 15: one ledger row per familiar, removed with it.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE ledgers (
           agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
           goal TEXT NOT NULL,
           status TEXT NOT NULL,
           waiting_on TEXT NOT NULL,
           next_step TEXT NOT NULL,
           updated_at INTEGER NOT NULL,
           updated_by TEXT NOT NULL,
           told_at INTEGER NOT NULL DEFAULT 0
         );
         PRAGMA user_version = 17;
         COMMIT;",
    )
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Ledger {
    pub goal: String,
    /// One of STATUSES.
    pub status: String,
    pub waiting_on: String,
    pub next_step: String,
    /// None until someone saves it.
    pub updated_at: Option<i64>,
    /// "familiar" or "you".
    pub updated_by: Option<String>,
}

impl Ledger {
    fn empty() -> Self {
        Ledger { goal: String::new(), status: "idle".into(), waiting_on: String::new(), next_step: String::new(), updated_at: None, updated_by: None }
    }

    fn is_blank(&self) -> bool {
        self.goal.is_empty() && self.status == "idle" && self.waiting_on.is_empty() && self.next_step.is_empty()
    }

    /// The ledger as plain lines, for the familiar to read.
    fn describe(&self) -> String {
        let or_none = |s: &str| if s.is_empty() { "(none)".to_string() } else { s.to_string() };
        let mut text = format!("goal: {}\nstatus: {}\nwaiting on: {}", or_none(&self.goal), self.status, or_none(&self.waiting_on));
        if !self.next_step.is_empty() {
            text.push_str(&format!("\nnext step: {}", self.next_step));
        }
        text
    }
}

/// A change to some fields. None leaves a field as it is; an empty string clears it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    pub goal: Option<String>,
    pub status: Option<String>,
    pub waiting_on: Option<String>,
    pub next_step: Option<String>,
}

impl Patch {
    /// Reads update_ledger's arguments, which use snake_case like the tool schema.
    fn from_tool(arguments: &Value) -> Result<Self, String> {
        let field = |name: &str| match arguments.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("{name} must be text.")),
        };
        Ok(Patch { goal: field("goal")?, status: field("status")?, waiting_on: field("waiting_on")?, next_step: field("next_step")? })
    }
}

fn row_to_ledger(r: &rusqlite::Row) -> rusqlite::Result<Ledger> {
    Ok(Ledger { goal: r.get(0)?, status: r.get(1)?, waiting_on: r.get(2)?, next_step: r.get(3)?, updated_at: Some(r.get(4)?), updated_by: Some(r.get(5)?) })
}

pub fn get(db: &Db, agent_id: &str) -> Result<Ledger, String> {
    let row = db.with(|c| {
        c.query_row(
            "SELECT goal, status, waiting_on, next_step, updated_at, updated_by FROM ledgers WHERE agent_id = ?1",
            params![agent_id],
            row_to_ledger,
        )
        .optional()
    })?;
    Ok(row.unwrap_or_else(Ledger::empty))
}

fn checked(name: &str, value: String, limit: usize) -> Result<String, String> {
    let value = value.trim().to_string();
    let length = value.chars().count();
    if length > limit {
        return Err(format!("The {name} can be up to {limit} characters, and this is {length}. Shorten it and try again."));
    }
    Ok(value)
}

/// Applies `patch` and saves it. `by` is "familiar" or "you".
pub fn update(db: &Db, agent_id: &str, patch: Patch, by: &str) -> Result<Ledger, String> {
    if patch.goal.is_none() && patch.status.is_none() && patch.waiting_on.is_none() && patch.next_step.is_none() {
        return Err("Nothing to change. Give a goal, status, waiting_on, or next_step.".into());
    }
    let mut ledger = get(db, agent_id)?;
    if let Some(goal) = patch.goal {
        ledger.goal = checked("goal", goal, GOAL_LIMIT)?;
    }
    if let Some(status) = patch.status {
        let status = status.trim().to_lowercase();
        if !STATUSES.contains(&status.as_str()) {
            return Err(format!("Unknown status \"{status}\". Use one of {}.", STATUSES.join(", ")));
        }
        ledger.status = status;
    }
    if let Some(waiting_on) = patch.waiting_on {
        ledger.waiting_on = checked("waiting_on", waiting_on, WAITING_LIMIT)?;
    }
    if let Some(next_step) = patch.next_step {
        ledger.next_step = checked("next_step", next_step, NEXT_LIMIT)?;
    }
    let at = now_ms();
    db.with(|c| {
        c.execute(
            "INSERT INTO ledgers (agent_id, goal, status, waiting_on, next_step, updated_at, updated_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(agent_id) DO UPDATE SET goal = ?2, status = ?3, waiting_on = ?4, next_step = ?5, updated_at = ?6, updated_by = ?7",
            params![agent_id, ledger.goal, ledger.status, ledger.waiting_on, ledger.next_step, at, by],
        )
    })?;
    Ok(Ledger { updated_at: Some(at), updated_by: Some(by.into()), ..ledger })
}

/// The ledger part of a familiar's chat-start prompt.
pub fn session_prompt(agent: &Agent) -> Option<String> {
    let ledger = get(DB.get()?, &agent.id).ok()?;
    prompt_text(&ledger, crate::tools::server_command(&agent.id).is_some())
}

/// None when there's no ledger to show and no tool to start one.
fn prompt_text(ledger: &Ledger, tools: bool) -> Option<String> {
    if ledger.is_blank() && !tools {
        return None;
    }
    let mut prompt = String::from("Grimoire ledger: your goal, status, and what you're waiting on, kept across chats and restarts. The user can see and edit it.");
    if tools {
        prompt.push_str(
            " Keep it current: call update_ledger when your goal, status, or blocker changes, and read_ledger for the latest. \
             Statuses: idle, working, waiting, blocked, done.",
        );
    }
    if !ledger.is_blank() {
        prompt.push_str(&format!("\n\n<ledger>\n{}\n</ledger>", ledger.describe()));
    }
    Some(prompt)
}

/// A note for the start of the familiar's next turn when the person changed its ledger since the
/// familiar last saw it. A turn that starts a new chat gets the ledger in its prompt, so it only
/// marks the ledger seen.
pub fn turn_note(db: &Db, agent: &Agent) -> Option<String> {
    let (updated_at, updated_by, told_at): (i64, String, i64) = db
        .with(|c| {
            c.query_row("SELECT updated_at, updated_by, told_at FROM ledgers WHERE agent_id = ?1", params![agent.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()
        })
        .ok()??;
    let unseen = agent.session_id.is_some() && updated_by == "you" && updated_at > told_at;
    if told_at < updated_at {
        let _ = db.with(|c| c.execute("UPDATE ledgers SET told_at = ?2 WHERE agent_id = ?1", params![agent.id, updated_at]));
    }
    let ledger = get(db, &agent.id).ok()?;
    unseen.then(|| format!("(Grimoire: the user updated your ledger.)\n<ledger>\n{}\n</ledger>", ledger.describe()))
}

pub fn definitions() -> Vec<Value> {
    let text = |description: &str| json!({ "type": "string", "description": description });
    vec![
        json!({
            "name": "read_ledger",
            "description": "Read your Grimoire ledger: your goal, status, what you're waiting on, and your next step. The user may have changed it since this chat started.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "update_ledger",
            "description": "Update your Grimoire ledger when your goal, status, or blocker changes, so you can pick up where you left off after a restart. Give only the fields that changed; an empty string clears one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "goal": text("What you're working toward, in a sentence. Up to 300 characters."),
                    "status": { "type": "string", "enum": STATUSES, "description": "idle, working, waiting (on someone or something), blocked, or done." },
                    "waiting_on": text("Who or what you're waiting on or blocked by. Up to 300 characters."),
                    "next_step": text("Your next step, or short notes for picking the work back up. Up to 1000 characters.")
                }
            }
        }),
    ]
}

/// Runs a ledger tool for the familiar `caller`, which the tool socket took from the connection.
pub fn call_tool(db: &Db, caller: &str, tool: &str, arguments: &Value) -> Result<String, String> {
    db.get_agent(caller)?.ok_or("Only a familiar has a ledger.")?;
    match tool {
        "read_ledger" => {
            let ledger = get(db, caller)?;
            Ok(if ledger.is_blank() { "Your ledger is empty.".into() } else { ledger.describe() })
        }
        "update_ledger" => {
            let ledger = update(db, caller, Patch::from_tool(arguments)?, "familiar")?;
            Ok(format!("Updated your ledger.\n{}", ledger.describe()))
        }
        _ => Err(format!("Grimoire has no tool named {tool}.")),
    }
}

/// The event that tells the app's windows a familiar changed its ledger.
pub fn changed_event(tool: &str) -> Option<&'static str> {
    (tool == "update_ledger").then_some(CHANGED)
}

#[tauri::command]
pub fn get_ledger(state: State<AppState>, agent_id: String) -> Result<Ledger, String> {
    get(&state.db, &agent_id)
}

#[tauri::command]
pub fn save_ledger(app: AppHandle, state: State<AppState>, agent_id: String, patch: Patch) -> Result<Ledger, String> {
    state.db.get_agent(&agent_id)?.ok_or("That familiar no longer exists.")?;
    let saved = update(&state.db, &agent_id, patch, "you")?;
    let _ = app.emit(CHANGED, ());
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Avatar, Provider};

    fn familiar(db: &Db, id: &str) -> Agent {
        let agent = Agent {
            id: id.into(),
            name: id.into(),
            provider: Provider::Claude,
            model: None,
            workspace: format!("/tmp/{id}"),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        agent
    }

    fn patch(goal: Option<&str>, status: Option<&str>, waiting_on: Option<&str>) -> Patch {
        Patch { goal: goal.map(String::from), status: status.map(String::from), waiting_on: waiting_on.map(String::from), next_step: None }
    }

    #[test]
    fn saves_partial_updates_and_validates() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "scribe");
        assert_eq!(get(&db, "scribe").unwrap(), Ledger::empty());

        let saved = update(&db, "scribe", patch(Some(" Ship the ledger "), Some("Working"), None), "you").unwrap();
        assert_eq!((saved.goal.as_str(), saved.status.as_str(), saved.updated_by.as_deref()), ("Ship the ledger", "working", Some("you")));

        let saved = update(&db, "scribe", patch(None, Some("waiting"), Some("Nik's review")), "familiar").unwrap();
        assert_eq!((saved.goal.as_str(), saved.waiting_on.as_str()), ("Ship the ledger", "Nik's review"), "untouched fields stay");
        assert_eq!(get(&db, "scribe").unwrap(), saved);

        let cleared = update(&db, "scribe", patch(None, Some("done"), Some("")), "familiar").unwrap();
        assert_eq!(cleared.waiting_on, "");

        assert!(update(&db, "scribe", patch(None, Some("napping"), None), "you").unwrap_err().contains("Unknown status"));
        assert!(update(&db, "scribe", Patch::default(), "you").unwrap_err().contains("Nothing to change"));
        let long = "é".repeat(GOAL_LIMIT + 1);
        assert!(update(&db, "scribe", patch(Some(&long), None, None), "you").unwrap_err().contains("up to 300 characters"));
        assert!(update(&db, "scribe", patch(Some(&"é".repeat(GOAL_LIMIT)), None, None), "you").is_ok(), "counts characters, not bytes");

        db.delete_agent("scribe").unwrap();
        assert_eq!(get(&db, "scribe").unwrap(), Ledger::empty(), "deleted with the familiar");
    }

    #[test]
    fn tools_act_on_the_caller_only() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "scribe");
        familiar(&db, "other");
        assert_eq!(call_tool(&db, "scribe", "read_ledger", &json!({})).unwrap(), "Your ledger is empty.");
        let text = call_tool(&db, "scribe", "update_ledger", &json!({ "goal": "Fix the build", "status": "blocked", "waiting_on": "CI access" })).unwrap();
        assert!(text.contains("goal: Fix the build") && text.contains("status: blocked") && text.contains("waiting on: CI access"), "{text}");
        assert_eq!(get(&db, "scribe").unwrap().updated_by.as_deref(), Some("familiar"));
        assert!(get(&db, "other").unwrap().is_blank());

        assert!(call_tool(&db, "scribe", "update_ledger", &json!({ "status": 3 })).unwrap_err().contains("must be text"));
        assert!(call_tool(&db, "scribe", "update_ledger", &json!({})).is_err());
        assert!(call_tool(&db, "ghost", "read_ledger", &json!({})).is_err());
        assert_eq!(changed_event("update_ledger"), Some(CHANGED));
        assert_eq!(changed_event("read_ledger"), None);
    }

    #[test]
    fn builds_the_prompt_text() {
        let blank = Ledger::empty();
        assert_eq!(prompt_text(&blank, false), None, "nothing to show and no tools");
        let hint = prompt_text(&blank, true).unwrap();
        assert!(hint.contains("update_ledger") && !hint.contains("<ledger>"));

        let ledger = Ledger { goal: "Port the parser".into(), status: "waiting".into(), waiting_on: "the user".into(), next_step: "Run the fuzzer".into(), ..blank };
        let prompt = prompt_text(&ledger, false).unwrap();
        assert!(prompt.contains("<ledger>\ngoal: Port the parser\nstatus: waiting\nwaiting on: the user\nnext step: Run the fuzzer\n</ledger>"), "{prompt}");
        assert!(!prompt.contains("update_ledger"), "no tools, so no tool hints");
        assert!(prompt.len() < 400, "the wrapper stays short");
    }

    #[test]
    fn tells_a_resumed_chat_about_the_users_edits_once() {
        let db = Db::open_in_memory().unwrap();
        let mut agent = familiar(&db, "scribe");
        assert_eq!(turn_note(&db, &agent), None, "no ledger yet");

        update(&db, "scribe", patch(Some("Draft the README"), None, None), "you").unwrap();
        assert_eq!(turn_note(&db, &agent), None, "a new chat reads it in its prompt");

        agent.session_id = Some("s1".into());
        std::thread::sleep(std::time::Duration::from_millis(2));
        update(&db, "scribe", patch(None, Some("working"), None), "you").unwrap();
        let note = turn_note(&db, &agent).unwrap();
        assert!(note.contains("the user updated your ledger") && note.contains("status: working"), "{note}");
        assert_eq!(turn_note(&db, &agent), None, "only once");

        std::thread::sleep(std::time::Duration::from_millis(2));
        update(&db, "scribe", patch(None, Some("done"), None), "you").unwrap();
        call_tool(&db, "scribe", "update_ledger", &json!({ "next_step": "Nothing" })).unwrap();
        assert_eq!(turn_note(&db, &agent), None, "the familiar's own change shows it the ledger");
    }
}
