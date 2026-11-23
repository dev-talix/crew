//! Background helpers: a familiar hands a long job to a short-lived helper with `start_helper` and
//! keeps chatting. The helper is the same familiar (same provider, model, workspace, and access)
//! working in a hidden side chat on a fresh CLI session, so it starts clean instead of from a copy of
//! the main chat, which may be mid-turn when the helper starts. When it ends, its result comes back to
//! the familiar as a new message labeled "Helper <label>", which starts the familiar's next turn.
//!
//! Limits: 3 running helpers per familiar, 30 minutes per helper, and no helpers from a side chat or
//! from inside a helper. Inside a helper only read-only Grimoire tools work, so it can't start helpers,
//! message other familiars, or use the vault in its familiar's name. Deleting the familiar stops its
//! helpers without telling anyone. Helpers still running when Grimoire quits show as "interrupted" at
//! the next launch, and the familiar isn't told: the person sees them in the chat's helpers list.

use crate::db::{now_ms, Agent, Db};
use crate::runtime::{Runtime, TurnOutcome};
use crate::side;
use crate::tools::Outcome;
use crate::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Notify;

/// The most helpers one familiar has running at once.
pub const MAX_RUNNING: i64 = 3;
/// How long a helper may run before Grimoire stops it.
pub const TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long a stopped helper gets to wind down before Grimoire lets it go.
const GRACE: Duration = Duration::from_secs(30);
/// The source label on the message that brings a helper's result back.
pub const SOURCE: &str = "Helper ";
pub const TOOL_NAMES: [&str; 3] = ["start_helper", "list_helpers", "stop_helper"];
/// The only Grimoire tools a helper can call. They read, and never act as its familiar toward others.
const HELPER_TOOLS: [&str; 4] = ["list_familiars", "list_spells", "list_themes", "read_memory"];
const LABEL_MAX: usize = 40;
const EVENT: &str = "helpers_changed";

/// Schema version 15: one row per helper, kept after it ends so the person can see it.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE helpers (
           id TEXT PRIMARY KEY,
           agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
           chat_id TEXT NOT NULL,
           label TEXT NOT NULL,
           task TEXT NOT NULL,
           status TEXT NOT NULL,
           result TEXT,
           started_at INTEGER NOT NULL,
           finished_at INTEGER
         );
         CREATE INDEX helpers_by_agent ON helpers(agent_id, started_at);
         PRAGMA user_version = 18;
         COMMIT;",
    )
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Helper {
    pub id: String,
    /// The familiar it works for.
    pub agent_id: String,
    /// Its hidden side chat, which holds the transcript.
    pub chat_id: String,
    pub label: String,
    pub task: String,
    /// "running", then "done", "failed", "stopped", "timeout", or "interrupted".
    pub status: String,
    /// The final reply, or why it didn't finish.
    pub result: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

const COLUMNS: &str = "id, agent_id, chat_id, label, task, status, result, started_at, finished_at";

fn row_to_helper(r: &rusqlite::Row) -> rusqlite::Result<Helper> {
    Ok(Helper {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        chat_id: r.get(2)?,
        label: r.get(3)?,
        task: r.get(4)?,
        status: r.get(5)?,
        result: r.get(6)?,
        started_at: r.get(7)?,
        finished_at: r.get(8)?,
    })
}

pub fn get(db: &Db, id: &str) -> Result<Option<Helper>, String> {
    db.with(|c| c.query_row(&format!("SELECT {COLUMNS} FROM helpers WHERE id = ?1"), params![id], row_to_helper).optional())
}

/// A familiar's helpers, newest first.
pub fn list(db: &Db, agent_id: &str, limit: i64) -> Result<Vec<Helper>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached(&format!("SELECT {COLUMNS} FROM helpers WHERE agent_id = ?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2"))?;
        let rows = stmt.query_map(params![agent_id, limit], row_to_helper)?;
        rows.collect()
    })
}

fn running_count(db: &Db, agent_id: &str) -> Result<i64, String> {
    db.with(|c| c.query_row("SELECT COUNT(*) FROM helpers WHERE agent_id = ?1 AND status = 'running'", params![agent_id], |r| r.get(0)))
}

/// Records how a running helper ended. False when it had already ended, say because its familiar was
/// deleted, so nobody hears about it twice.
pub fn settle(db: &Db, id: &str, status: &str, result: Option<&str>) -> Result<bool, String> {
    db.with(|c| {
        c.execute(
            "UPDATE helpers SET status = ?2, result = ?3, finished_at = ?4 WHERE id = ?1 AND status = 'running'",
            params![id, status, result, now_ms()],
        )
        .map(|n| n > 0)
    })
}

/// Marks helpers left running by the last launch as interrupted. Their CLI sessions ended with the app.
pub fn interrupt_leftovers(db: &Db) -> Result<usize, String> {
    db.with(|c| {
        c.execute(
            "UPDATE helpers SET status = 'interrupted', result = 'Grimoire quit while it was working.', finished_at = ?1 WHERE status = 'running'",
            params![now_ms()],
        )
    })
}

/// Whether `chat_id` is a helper's chat.
pub fn is_helper_chat(db: &Db, chat_id: &str) -> Result<bool, String> {
    db.with(|c| c.query_row("SELECT EXISTS(SELECT 1 FROM helpers WHERE chat_id = ?1)", params![chat_id], |r| r.get(0)))
}

/// Refuses a tool a helper isn't allowed, when the call comes from a helper's chat (`lane`).
pub fn check_lane(db: &Db, lane: Option<&str>, tool: &str) -> Result<(), String> {
    match lane {
        Some(chat) if !HELPER_TOOLS.contains(&tool) && is_helper_chat(db, chat)? => Err(format!(
            "Helpers can't use {tool}. Finish the task and put everything your familiar needs in your final reply."
        )),
        _ => Ok(()),
    }
}

fn short(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", flat[..at].trim_end()),
        None => flat,
    }
}

/// Starting helpers one at a time keeps the running count honest when calls arrive together.
static STARTING: Mutex<()> = Mutex::new(());

/// Checks a `start_helper` call from `caller` and saves the helper and its hidden chat.
pub fn create(db: &Db, caller: &str, lane: Option<&str>, arguments: &Value) -> Result<(Agent, Helper), String> {
    if lane.is_some() {
        return Err("Start helpers from your main chat. Helpers and side conversations can't start their own.".into());
    }
    let agent = db.get_agent(caller)?.ok_or("Only a familiar can start a helper.")?;
    let task = arguments.get("task").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if task.is_empty() {
        return Err("Write the task for the helper.".into());
    }
    let label = match arguments.get("label").and_then(Value::as_str).map(str::trim).filter(|l| !l.is_empty()) {
        Some(label) => short(label, LABEL_MAX),
        None => short(task, LABEL_MAX),
    };
    let _starting = STARTING.lock().unwrap_or_else(|e| e.into_inner());
    if running_count(db, caller)? >= MAX_RUNNING {
        return Err(format!(
            "You already have {MAX_RUNNING} helpers running, which is Grimoire's limit. Wait for one to finish, or stop one with stop_helper."
        ));
    }
    let chat = side::create(db, caller, caller, task)?;
    let helper = Helper {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent.id.clone(),
        chat_id: chat.id,
        label,
        task: task.to_string(),
        status: "running".into(),
        result: None,
        started_at: now_ms(),
        finished_at: None,
    };
    db.with(|c| {
        c.execute(
            &format!("INSERT INTO helpers ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, NULL)"),
            params![helper.id, helper.agent_id, helper.chat_id, helper.label, helper.task, helper.status, helper.started_at],
        )
    })?;
    Ok((agent, helper))
}

/// The helper `wanted` names, by id or label, among `caller`'s own.
fn find_own(db: &Db, caller: &str, wanted: &str) -> Result<Helper, String> {
    let wanted = wanted.trim();
    let mine = list(db, caller, 200)?;
    if let Some(helper) = mine.iter().find(|h| h.id == wanted) {
        return Ok(helper.clone());
    }
    let named: Vec<&Helper> = mine.iter().filter(|h| h.status == "running" && h.label.eq_ignore_ascii_case(wanted)).collect();
    match named.as_slice() {
        [helper] => Ok((*helper).clone()),
        [] => Err(format!("You have no running helper called \"{wanted}\". Use list_helpers to see yours.")),
        _ => Err(format!("More than one of your helpers is called \"{wanted}\". Use its id instead.")),
    }
}

/// How a helper ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ending {
    Done(String),
    Failed(String),
    Stopped,
    TimedOut,
}

impl Ending {
    pub fn status(&self) -> &'static str {
        match self {
            Ending::Done(_) => "done",
            Ending::Failed(_) => "failed",
            Ending::Stopped => "stopped",
            Ending::TimedOut => "timeout",
        }
    }

    fn result(&self) -> Option<&str> {
        match self {
            Ending::Done(text) | Ending::Failed(text) => Some(text),
            Ending::Stopped | Ending::TimedOut => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Waiting,
    Starting,
    Started,
    Cancelled,
}

/// Where a helper's turn is in starting its CLI turn, so a stop can cancel a turn that hasn't started
/// and interrupt one that has, but never interrupt too early and let the turn start afterwards.
pub struct Startup {
    phase: Mutex<Phase>,
    started: Notify,
}

impl Default for Startup {
    fn default() -> Self {
        Startup { phase: Mutex::new(Phase::Waiting), started: Notify::new() }
    }
}

impl Startup {
    fn phase(&self) -> std::sync::MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Called by the turn right before it starts the CLI turn. False when the helper already ended.
    pub fn begin(&self) -> bool {
        let mut phase = self.phase();
        if *phase == Phase::Cancelled {
            return false;
        }
        *phase = Phase::Starting;
        true
    }

    /// Called once the CLI turn has started, or failed to.
    pub fn started(&self) {
        *self.phase() = Phase::Started;
        self.started.notify_one();
    }

    /// Stops a turn that hasn't begun from ever starting. Returns the phase it was in.
    fn cancel(&self) -> Phase {
        let mut phase = self.phase();
        let was = *phase;
        if was == Phase::Waiting {
            *phase = Phase::Cancelled;
        }
        was
    }
}

/// Runs `turn` until it ends, `stop` fires, or `limit` passes. A turn that hasn't started its CLI turn
/// is dropped and never starts. Otherwise `halt` interrupts the CLI, once the turn has finished
/// starting, and the turn gets `GRACE` to wind down and save what it wrote.
pub async fn supervise<Turn, Halt, Halted>(turn: Turn, stop: &Notify, limit: Duration, startup: &Startup, halt: Halt) -> Ending
where
    Turn: Future<Output = TurnOutcome>,
    Halt: FnOnce() -> Halted,
    Halted: Future<Output = ()>,
{
    tokio::pin!(turn);
    let ending = tokio::select! {
        // A stop sent before the turn was first polled wins, so the turn never starts.
        biased;
        _ = stop.notified() => Ending::Stopped,
        _ = tokio::time::sleep(limit) => Ending::TimedOut,
        outcome = &mut turn => return match outcome {
            TurnOutcome::Done(text) => Ending::Done(text),
            TurnOutcome::Failed(reason) => Ending::Failed(reason),
            TurnOutcome::Stopped => Ending::Stopped,
        },
    };
    match startup.cancel() {
        Phase::Waiting | Phase::Cancelled => return ending,
        // An interrupt sent mid-start finds no turn to stop, so let the start finish first, however slow,
        // up to the helper's own limit. A start that outlasts that is left to the CLI's own timeouts.
        Phase::Starting => {
            let started = async {
                tokio::select! {
                    _ = &mut turn => false,
                    _ = startup.started.notified() => true,
                }
            };
            if !tokio::time::timeout(limit, started).await.unwrap_or(false) {
                return ending;
            }
        }
        Phase::Started => {}
    }
    halt().await;
    let _ = tokio::time::timeout(GRACE, &mut turn).await;
    ending
}

/// What the helper's CLI sees: whose helper it is, the rules, and the task.
pub fn framed(familiar: &str, task: &str) -> String {
    format!(
        "You are a background helper for {familiar}, one of the person's familiars in Grimoire. {familiar} handed you \
         the task below and is carrying on with other work. Work on it on your own; nobody will answer questions. \
         When you're done, end with a reply that gives {familiar} the whole result, since that reply is all it gets \
         back. You can't start helpers, message other familiars, or use the vault.\n\nTask:\n{task}"
    )
}

/// The message that brings a helper's result back to its familiar.
pub fn result_text(helper: &Helper, ending: &Ending) -> String {
    let label = &helper.label;
    let body = match ending {
        Ending::Done(text) if text.trim().is_empty() => format!("Helper {label} finished without writing a result."),
        Ending::Done(text) => format!("Helper {label} finished:\n\n{text}"),
        Ending::Failed(reason) => format!("Helper {label} failed: {reason}"),
        Ending::Stopped => format!("Helper {label} was stopped before it finished."),
        Ending::TimedOut => format!("Helper {label} ran out of time after {} minutes and was stopped.", TIMEOUT.as_secs() / 60),
    };
    format!("{body}\n\n(Helper {}. The person can read its whole transcript in Grimoire.)", helper.id)
}

fn summary(helper: &Helper) -> Value {
    let elapsed = helper.finished_at.unwrap_or_else(now_ms) - helper.started_at;
    json!({
        "id": helper.id,
        "label": helper.label,
        "status": helper.status,
        "minutes": elapsed / 60_000,
        "task": short(&helper.task, 200),
        "result": helper.result.as_deref().map(|r| short(r, 400)),
    })
}

/// Stop signals for running helpers, by helper id.
static RUNNING: LazyLock<Mutex<HashMap<String, Arc<Notify>>>> = LazyLock::new(Default::default);

fn running() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Notify>>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Asks a running helper to stop. One with no task behind it, say after a crash, is settled here.
fn signal_stop(db: &Db, helper: &Helper) -> Result<(), String> {
    if helper.status != "running" {
        return Err(format!("Helper {} already ended ({}).", helper.label, helper.status));
    }
    match running().get(&helper.id) {
        Some(stop) => stop.notify_one(),
        None => {
            settle(db, &helper.id, "stopped", None)?;
        }
    }
    Ok(())
}

/// Stops a familiar's helpers before it's deleted. They end without telling anyone.
pub fn stop_for(db: &Db, agent_id: &str) {
    let Ok(helpers) = list(db, agent_id, i64::MAX) else { return };
    for helper in helpers.iter().filter(|h| h.status == "running") {
        let _ = settle(db, &helper.id, "stopped", None);
        if let Some(stop) = running().get(&helper.id) {
            stop.notify_one();
        }
    }
}

/// Runs a saved helper in the background and reports back when it ends.
fn launch(app: AppHandle, db: Arc<Db>, runtime: Arc<Runtime>, agent: Agent, helper: Helper) {
    let stop = Arc::new(Notify::new());
    let startup = Startup::default();
    running().insert(helper.id.clone(), stop.clone());
    let home = crate::chats::current(&db, &agent.id).ok().flatten();
    tauri::async_runtime::spawn(async move {
        // A stop that came between saving the helper and registering it above settled the row with no one
        // to signal, so check before starting anything.
        if !get(&db, &helper.id).is_ok_and(|h| h.is_some_and(|h| h.status == "running")) {
            running().remove(&helper.id);
            return;
        }
        let key = side::lane_key(&agent.id, &helper.chat_id);
        let prompt = framed(&agent.name, &helper.task);
        let turn = runtime.helper_turn(&helper.chat_id, &helper.task, &prompt, &startup);
        let ending = supervise(turn, &stop, TIMEOUT, &startup, || runtime.stop_lane(&key)).await;
        runtime.forget_lane(&key);
        running().remove(&helper.id);
        let settled = settle(&db, &helper.id, ending.status(), ending.result());
        let _ = app.emit(EVENT, ());
        if settled == Ok(true) {
            // Like a `message_familiar` reply: it starts the familiar's next turn in the chat the helper
            // started from, or waits there and comes with its next turn.
            runtime.deliver(&agent.id, home, result_text(&helper, &ending), &format!("{SOURCE}{}", helper.label));
        }
    });
}

pub fn definitions() -> Value {
    json!([
        {
            "name": "start_helper",
            "description": "Start a background helper for a long job and carry on right away. The helper is you, in a fresh session: same workspace, access, and model, but it doesn't see this conversation, so put everything it needs in the task. When it ends, its result comes to you as a new message labeled \"Helper <label>\", which starts your next turn. You can have 3 helpers running at once, each runs at most 30 minutes, and helpers can't start helpers or message other familiars.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "The whole job, with the context and the result you want back." },
                    "label": { "type": "string", "description": "Optional short name, like \"test run\". Defaults to the start of the task." }
                },
                "required": ["task"]
            }
        },
        {
            "name": "list_helpers",
            "description": "List your helpers, newest first, with their status (running, done, failed, stopped, timeout, or interrupted), minutes so far, and results.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "stop_helper",
            "description": "Stop one of your running helpers. You still get a message saying it was stopped.",
            "inputSchema": {
                "type": "object",
                "properties": { "helper": { "type": "string", "description": "Helper id, or the label of a running one, from list_helpers." } },
                "required": ["helper"]
            }
        }
    ])
}

/// Runs one helper tool for `caller`, who calls from its main chat or from `lane`.
pub fn call_tool(ctx: &crate::tools::Context, caller: &str, lane: Option<&str>, tool: &str, arguments: Value) -> Result<Outcome, String> {
    let changed = (tool != "list_helpers").then_some(EVENT);
    let text = match tool {
        "start_helper" => {
            let (agent, helper) = create(&ctx.db, caller, lane, &arguments)?;
            let link = format!("{SOURCE}{}", helper.label);
            side::link(&ctx.app, &ctx.db, None, caller, &link, &helper.chat_id)?;
            let text = format!(
                "Started helper {} (id {}). It works in the background, and its result will come to you as a new message \
                 when it's done, so carry on now.",
                helper.label, helper.id
            );
            launch(ctx.app.clone(), ctx.db.clone(), ctx.runtime.clone(), agent, helper);
            text
        }
        "list_helpers" => serde_json::to_string_pretty(&list(&ctx.db, caller, 20)?.iter().map(summary).collect::<Vec<_>>()).unwrap_or_default(),
        "stop_helper" => {
            let wanted = arguments.get("helper").and_then(Value::as_str).ok_or("Name the helper to stop.")?;
            let helper = find_own(&ctx.db, caller, wanted)?;
            signal_stop(&ctx.db, &helper)?;
            format!("Stopping helper {}. You'll get a message when it has stopped.", helper.label)
        }
        _ => return Err(format!("Grimoire has no tool named {tool}.")),
    };
    Ok(Outcome { text, changed })
}

#[tauri::command]
pub fn list_helpers(state: State<AppState>, agent_id: String) -> Result<Vec<Helper>, String> {
    crate::link::local_only(&agent_id)?;
    list(&state.db, &agent_id, 20)
}

#[tauri::command]
pub fn stop_helper(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    let helper = get(&state.db, &id)?.ok_or("That helper is gone.")?;
    signal_stop(&state.db, &helper)?;
    let _ = app.emit(EVENT, ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Avatar, Provider};

    fn familiar(db: &Db, id: &str, name: &str) -> Agent {
        let agent = Agent {
            id: id.into(),
            name: name.into(),
            provider: Provider::Claude,
            model: None,
            workspace: format!("/tmp/{id}"),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        agent
    }

    fn task(text: &str) -> Value {
        json!({ "task": text })
    }

    #[test]
    fn starts_helpers_in_hidden_chats_up_to_the_limit() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", "Researcher");
        familiar(&db, "b", "Scribe");
        db.insert_message("a", "user", "Main chat", "done").unwrap();
        let (agent, first) = create(&db, "a", None, &json!({ "task": "  Run the whole test suite and report failures  ", "label": " tests " })).unwrap();
        assert_eq!((agent.id.as_str(), first.label.as_str(), first.status.as_str()), ("a", "tests", "running"));
        assert_eq!(first.task, "Run the whole test suite and report failures");
        // Its chat is a hidden side chat of the familiar itself, out of the chat list.
        let chat = side::get(&db, &first.chat_id).unwrap().unwrap();
        assert_eq!((chat.asker_id.as_str(), chat.target_id.as_str()), ("a", "a"));
        assert_eq!(crate::chats::list(&db, "a").unwrap().len(), 1);
        assert!(is_helper_chat(&db, &first.chat_id).unwrap());

        let (_, second) = create(&db, "a", None, &task("Summarize every file in docs, one paragraph each, as long as it takes")).unwrap();
        assert!(second.label.ends_with('…') && second.label.chars().count() == LABEL_MAX + 1, "{}", second.label);
        create(&db, "a", None, &task("third")).unwrap();
        let err = create(&db, "a", None, &task("fourth")).unwrap_err();
        assert!(err.contains("already have 3 helpers running"), "{err}");
        // Others have their own limit, and an ended helper frees a place.
        assert!(create(&db, "b", None, &task("mine")).is_ok());
        assert!(settle(&db, &first.id, "done", Some("All green.")).unwrap());
        assert!(create(&db, "a", None, &task("fourth")).is_ok());
        assert_eq!(list(&db, "a", 20).unwrap().len(), 4);
    }

    #[test]
    fn refuses_bad_starts() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", "Researcher");
        assert_eq!(create(&db, "a", None, &task("  ")).unwrap_err(), "Write the task for the helper.");
        assert!(create(&db, "ghost", None, &task("x")).unwrap_err().contains("Only a familiar"));
        // Not from a side conversation, and so not from inside a helper either.
        let (_, helper) = create(&db, "a", None, &task("x")).unwrap();
        let err = create(&db, "a", Some(&helper.chat_id), &task("again")).unwrap_err();
        assert!(err.contains("main chat"), "{err}");
        assert_eq!(list(&db, "a", 20).unwrap().len(), 1);
    }

    #[test]
    fn helpers_only_get_read_only_tools() {
        let db = Db::open_in_memory().unwrap();
        let a = familiar(&db, "a", "Researcher");
        let b = familiar(&db, "b", "Scribe");
        let (_, helper) = create(&db, "a", None, &task("x")).unwrap();
        let lane = Some(helper.chat_id.as_str());
        for tool in ["start_helper", "stop_helper", "list_helpers", "ask_familiar", "message_familiar", "vault_use", "remember", "create_spell", "set_theme"] {
            assert!(check_lane(&db, lane, tool).unwrap_err().contains(&format!("Helpers can't use {tool}")), "{tool}");
        }
        for tool in HELPER_TOOLS {
            assert!(check_lane(&db, lane, tool).is_ok(), "{tool}");
        }
        // The main chat and ordinary side conversations keep every tool.
        let side = side::create(&db, &a.id, &b.id, "hi").unwrap();
        assert!(check_lane(&db, None, "ask_familiar").is_ok());
        assert!(check_lane(&db, Some(&side.id), "ask_familiar").is_ok());
    }

    #[test]
    fn settles_once_and_stops_with_the_familiar() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", "Researcher");
        let (_, h1) = create(&db, "a", None, &task("one")).unwrap();
        let (_, h2) = create(&db, "a", None, &task("two")).unwrap();
        assert!(settle(&db, &h1.id, "failed", Some("no network")).unwrap());
        assert!(!settle(&db, &h1.id, "done", Some("late")).unwrap(), "an ended helper stays ended");
        let h1 = get(&db, &h1.id).unwrap().unwrap();
        assert_eq!((h1.status.as_str(), h1.result.as_deref()), ("failed", Some("no network")));
        assert!(h1.finished_at.is_some());

        stop_for(&db, "a");
        assert_eq!(get(&db, &h2.id).unwrap().unwrap().status, "stopped");
        assert!(!settle(&db, &h2.id, "done", Some("x")).unwrap(), "so its result isn't delivered");
        db.delete_agent("a").unwrap();
        assert!(get(&db, &h2.id).unwrap().is_none(), "rows go with the familiar");
    }

    #[test]
    fn a_restart_interrupts_running_helpers() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", "Researcher");
        let (_, running) = create(&db, "a", None, &task("one")).unwrap();
        let (_, done) = create(&db, "a", None, &task("two")).unwrap();
        settle(&db, &done.id, "done", Some("ok")).unwrap();
        assert_eq!(interrupt_leftovers(&db).unwrap(), 1);
        assert_eq!(get(&db, &running.id).unwrap().unwrap().status, "interrupted");
        assert_eq!(get(&db, &done.id).unwrap().unwrap().status, "done");
    }

    #[test]
    fn stops_only_your_own_helpers() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", "Researcher");
        familiar(&db, "b", "Scribe");
        let (_, mine) = create(&db, "a", None, &json!({ "task": "x", "label": "Build" })).unwrap();
        assert_eq!(find_own(&db, "a", "build").unwrap().id, mine.id);
        assert_eq!(find_own(&db, "a", &mine.id).unwrap().id, mine.id);
        assert!(find_own(&db, "b", &mine.id).unwrap_err().contains("no running helper"));
        // With no task behind it, a stop settles the row at once.
        signal_stop(&db, &mine).unwrap();
        let mine = get(&db, &mine.id).unwrap().unwrap();
        assert_eq!(mine.status, "stopped");
        assert!(signal_stop(&db, &mine).unwrap_err().contains("already ended"));
    }

    /// A helper turn that takes `starting` to start its CLI turn, then `working` to finish it.
    async fn helper_turn(startup: &Startup, starting: Duration, working: Duration) -> TurnOutcome {
        if !startup.begin() {
            return TurnOutcome::Stopped;
        }
        tokio::time::sleep(starting).await;
        startup.started();
        tokio::time::sleep(working).await;
        TurnOutcome::Done("result".into())
    }

    #[tokio::test(start_paused = true)]
    async fn supervises_turns_with_a_timeout_and_a_stop() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let halted = AtomicUsize::new(0);
        let halt = || async {
            halted.fetch_add(1, SeqCst);
        };
        let stop = Notify::new();
        let startup = Startup::default();
        let done = std::future::ready(TurnOutcome::Done("result".into()));
        assert_eq!(supervise(done, &stop, TIMEOUT, &startup, halt).await, Ending::Done("result".into()));
        let failed = std::future::ready(TurnOutcome::Failed("boom".into()));
        assert_eq!(supervise(failed, &stop, TIMEOUT, &startup, halt).await, Ending::Failed("boom".into()));
        assert_eq!(halted.load(SeqCst), 0);

        // A started turn that runs too long is interrupted and gets to wind down.
        let startup = Startup::default();
        let slow = helper_turn(&startup, Duration::ZERO, TIMEOUT * 2);
        assert_eq!(supervise(slow, &stop, TIMEOUT, &startup, halt).await, Ending::TimedOut);
        assert_eq!(halted.load(SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_helper_stopped_before_it_starts_never_starts() {
        let stop = Notify::new();
        let startup = Startup::default();
        let began = std::sync::atomic::AtomicBool::new(false);
        let turn = async {
            began.store(true, std::sync::atomic::Ordering::SeqCst);
            helper_turn(&startup, Duration::ZERO, Duration::from_secs(5)).await
        };
        stop.notify_one();
        let ending = supervise(turn, &stop, TIMEOUT, &startup, || async { panic!("nothing to interrupt") }).await;
        assert_eq!(ending, Ending::Stopped);
        assert!(!began.load(std::sync::atomic::Ordering::SeqCst), "the turn was never polled");
        assert!(!startup.begin(), "and can't start later");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_mid_start_interrupts_once_the_turn_has_started() {
        let stop = Notify::new();
        let startup = Startup::default();
        let began = tokio::time::Instant::now();
        let halted_at = Mutex::new(None);
        let turn = helper_turn(&startup, Duration::from_secs(10), Duration::from_secs(3600));
        let supervised = supervise(turn, &stop, TIMEOUT, &startup, || async {
            *halted_at.lock().unwrap() = Some((began.elapsed(), *startup.phase()));
        });
        let stopper = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            stop.notify_one();
        };
        let (ending, ()) = tokio::join!(supervised, stopper);
        assert_eq!(ending, Ending::Stopped);
        let (at, phase) = halted_at.into_inner().unwrap().expect("the started turn is interrupted");
        assert_eq!((at, phase), (Duration::from_secs(10), Phase::Started), "not before it started");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_during_a_slow_start_still_interrupts() {
        let stop = Notify::new();
        let startup = Startup::default();
        let began = tokio::time::Instant::now();
        let halted_at = Mutex::new(None);
        // The CLI takes longer to start than GRACE, the wind-down allowance.
        let turn = helper_turn(&startup, GRACE + Duration::from_secs(15), Duration::from_secs(3600));
        let supervised = supervise(turn, &stop, TIMEOUT, &startup, || async {
            *halted_at.lock().unwrap() = Some(began.elapsed());
        });
        let stopper = async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            stop.notify_one();
        };
        let (ending, ()) = tokio::join!(supervised, stopper);
        assert_eq!(ending, Ending::Stopped);
        assert_eq!(halted_at.into_inner().unwrap(), Some(GRACE + Duration::from_secs(15)), "interrupted once started");
    }

    #[test]
    fn words_results_for_the_familiar() {
        let helper = Helper {
            id: "h1".into(),
            agent_id: "a".into(),
            chat_id: "c1".into(),
            label: "tests".into(),
            task: "Run tests".into(),
            status: "running".into(),
            result: None,
            started_at: 0,
            finished_at: None,
        };
        let done = result_text(&helper, &Ending::Done("All green.".into()));
        assert!(done.starts_with("Helper tests finished:\n\nAll green.\n\n(Helper h1."), "{done}");
        assert!(result_text(&helper, &Ending::Done(" ".into())).starts_with("Helper tests finished without writing a result."));
        assert!(result_text(&helper, &Ending::Failed("offline".into())).starts_with("Helper tests failed: offline"));
        assert!(result_text(&helper, &Ending::Stopped).starts_with("Helper tests was stopped"));
        assert!(result_text(&helper, &Ending::TimedOut).starts_with("Helper tests ran out of time after 30 minutes"));
        assert!(framed("Researcher", "Run tests").ends_with("Task:\nRun tests"));
        assert_eq!(summary(&Helper { finished_at: Some(120_000), ..helper })["minutes"], 2);
    }

    #[test]
    fn offers_the_helper_tools() {
        let names: Vec<String> = definitions().as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, TOOL_NAMES);
    }
}
