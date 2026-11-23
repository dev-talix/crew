//! Memory: markdown notes a familiar keeps across chats, one file per familiar and one per coven, in
//! `memory/` in the app data folder. A familiar gets its memory, and its coven's, when a chat starts,
//! and changes them with the read_memory, remember, and rewrite_memory tools. Who last changed a
//! memory, and when, is kept in the settings table.

use crate::db::{now_ms, Agent, Db};
use crate::AppState;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, State};

/// Each memory holds at most this many bytes of UTF-8.
pub const LIMIT: usize = 16 * 1024;
pub const TOOL_NAMES: [&str; 3] = ["read_memory", "remember", "rewrite_memory"];
const CHANGED: &str = "memory_changed";

static DIR: OnceLock<PathBuf> = OnceLock::new();
/// Appends read, change, and write a file, so two familiars in one coven take turns.
static WRITING: Mutex<()> = Mutex::new(());

pub fn init(data_dir: &Path) {
    let _ = DIR.set(data_dir.join("memory"));
}

/// A shared memory folder for provider tests, which read memory the way the app does.
#[cfg(test)]
pub fn test_dir() -> &'static Path {
    init(&std::env::temp_dir().join(format!("grimoire-memory-test-{}", std::process::id())));
    dir().unwrap()
}

fn dir() -> Result<&'static Path, String> {
    DIR.get().map(PathBuf::as_path).ok_or_else(|| "Memory isn't set up yet.".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Familiar,
    Coven,
}

/// Whose memory: a familiar's own, or a coven's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Owner<'a> {
    pub scope: Scope,
    pub id: &'a str,
}

impl Owner<'_> {
    /// The file name, refusing ids that could leave the memory folder.
    fn stem(&self) -> Result<String, String> {
        let safe = !self.id.is_empty() && self.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !safe {
            return Err("That memory doesn't exist.".into());
        }
        Ok(match self.scope {
            Scope::Familiar => self.id.to_string(),
            Scope::Coven => format!("coven-{}", self.id),
        })
    }

    fn path(&self, dir: &Path) -> Result<PathBuf, String> {
        Ok(dir.join(format!("{}.md", self.stem()?)))
    }

    fn meta_key(&self) -> Result<String, String> {
        Ok(format!("memory:{}", self.stem()?))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub text: String,
    pub limit: usize,
    /// The familiar that last changed it; None when the user did, or nobody has yet.
    pub updated_by: Option<String>,
    pub updated_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    by: Option<String>,
    at: i64,
}

fn kb(bytes: usize) -> String {
    format!("{:.1} KB", bytes as f64 / 1024.0)
}

fn check_size(text: &str) -> Result<(), String> {
    if text.len() > LIMIT {
        return Err(format!("A memory holds up to {}, and this is {}. Shorten it and try again.", kb(LIMIT), kb(text.len())));
    }
    Ok(())
}

pub fn read(db: &Db, dir: &Path, owner: Owner) -> Result<Memory, String> {
    let text = match std::fs::read_to_string(owner.path(dir)?) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Could not read the memory: {e}")),
    };
    let meta: Option<Meta> = db.setting(&owner.meta_key()?)?.and_then(|m| serde_json::from_str(&m).ok());
    Ok(Memory {
        text,
        limit: LIMIT,
        updated_by: meta.as_ref().and_then(|m| m.by.clone()),
        updated_at: meta.map(|m| m.at),
    })
}

fn write_file(db: &Db, dir: &Path, owner: Owner, text: &str, by: Option<&str>) -> Result<Memory, String> {
    check_size(text)?;
    let path = owner.path(dir)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create the memory folder: {e}"))?;
    // Write a temporary file and rename it, so a crash never leaves half a memory.
    let temp = path.with_extension("md.tmp");
    std::fs::write(&temp, text).and_then(|_| std::fs::rename(&temp, &path)).map_err(|e| format!("Could not save the memory: {e}"))?;
    let meta = Meta { by: by.map(String::from), at: now_ms() };
    db.set_setting(&owner.meta_key()?, &serde_json::to_string(&meta).map_err(|e| e.to_string())?)?;
    read(db, dir, owner)
}

/// Replaces a memory. `by` is the familiar making the change, or None for the user.
pub fn write(db: &Db, dir: &Path, owner: Owner, text: &str, by: Option<&str>) -> Result<Memory, String> {
    let _lock = WRITING.lock().map_err(|_| "memory lock poisoned")?;
    write_file(db, dir, owner, text, by)
}

/// Adds a note dated `date` (YYYY-MM-DD) as a list item at the end of a memory.
pub fn append(db: &Db, dir: &Path, owner: Owner, note: &str, by: Option<&str>, date: &str) -> Result<Memory, String> {
    let note = note.trim();
    if note.is_empty() {
        return Err("The note is empty.".into());
    }
    let _lock = WRITING.lock().map_err(|_| "memory lock poisoned")?;
    let mut text = read(db, dir, owner)?.text;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("- {date}: {}\n", note.lines().collect::<Vec<_>>().join("\n  ")));
    if text.len() > LIMIT {
        return Err(format!(
            "This memory is full: it holds up to {} and the note would make it {}. Tidy it with rewrite_memory first, keeping what still matters.",
            kb(LIMIT),
            kb(text.len())
        ));
    }
    write_file(db, dir, owner, &text, by)
}

/// Removes a memory when its familiar or coven is deleted.
pub fn forget(db: &Db, owner: Owner) -> Result<(), String> {
    match dir() {
        Ok(dir) => forget_in(db, dir, owner),
        Err(_) => Ok(()),
    }
}

fn forget_in(db: &Db, dir: &Path, owner: Owner) -> Result<(), String> {
    match std::fs::remove_file(owner.path(dir)?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("Could not delete the memory: {e}")),
        _ => {}
    }
    let key = owner.meta_key()?;
    db.with(|c| c.execute("DELETE FROM settings WHERE key = ?1", params![key]).map(|_| ()))
}

/// The memory a familiar starts a chat with, or None when it has nothing to read or no way to write.
pub fn session_prompt(agent: &Agent) -> Option<String> {
    let dir = dir().ok()?;
    prompt_in(dir, agent, crate::tools::server_command(&agent.id).is_some())
}

fn prompt_in(dir: &Path, agent: &Agent, tools: bool) -> Option<String> {
    let load = |owner: Owner| owner.path(dir).ok().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let own = load(Owner { scope: Scope::Familiar, id: &agent.id });
    let coven = agent.coven_id.as_deref().map(|id| load(Owner { scope: Scope::Coven, id }));
    if !tools && own.trim().is_empty() && coven.as_deref().is_none_or(|c| c.trim().is_empty()) {
        return None;
    }
    let mut prompt = String::from(
        "Grimoire memory: notes kept across your chats, loaded when this chat started. The user can read and edit them in Grimoire.",
    );
    if tools {
        prompt.push_str(
            " They may have changed since, so call read_memory for the latest. Call remember to add a short dated note when you learn \
             something worth knowing in later chats, and rewrite_memory to tidy.",
        );
        if coven.is_some() {
            prompt.push_str(" Pass scope \"coven\" for the memory your coven shares.");
        }
    }
    let section = |who: &str, text: &str| {
        let text = text.trim();
        format!("\n\n<memory of=\"{who}\">\n{}\n</memory>", if text.is_empty() { "(empty)" } else { text })
    };
    prompt.push_str(&section("you", &own));
    if let Some(coven) = coven {
        prompt.push_str(&section("your coven", &coven));
    }
    Some(prompt)
}

pub fn definitions() -> Vec<Value> {
    let scope = json!({
        "type": "string",
        "enum": ["mine", "coven"],
        "description": "mine (the default) for your own memory, or coven for the memory your coven shares."
    });
    vec![
        json!({
            "name": "read_memory",
            "description": "Read your Grimoire memory: notes you keep across chats. You got a copy when this chat started, but the user or other familiars may have changed it since; this returns the current text.",
            "inputSchema": { "type": "object", "properties": { "scope": scope } }
        }),
        json!({
            "name": "remember",
            "description": "Add a short dated note to your Grimoire memory, for things worth knowing in later chats: the user's preferences, decisions, facts about ongoing work. Each memory holds up to 16 KB.",
            "inputSchema": {
                "type": "object",
                "properties": { "note": { "type": "string", "description": "The note, in a sentence or two." }, "scope": scope },
                "required": ["note"]
            }
        }),
        json!({
            "name": "rewrite_memory",
            "description": "Replace your whole Grimoire memory with new markdown, to tidy it: merge duplicates, drop what's out of date. Call read_memory first so you keep what the user or others added. Up to 16 KB.",
            "inputSchema": {
                "type": "object",
                "properties": { "text": { "type": "string", "description": "The complete new memory." }, "scope": scope },
                "required": ["text"]
            }
        }),
    ]
}

/// Runs a memory tool for the familiar `caller`.
pub fn call_tool(db: &Db, caller: &str, tool: &str, arguments: &Value) -> Result<String, String> {
    tool_in(db, dir()?, caller, tool, arguments, &chrono::Local::now().format("%Y-%m-%d").to_string())
}

fn tool_in(db: &Db, dir: &Path, caller: &str, tool: &str, arguments: &Value, today: &str) -> Result<String, String> {
    let agent = db.get_agent(caller)?.ok_or("Only a familiar has a memory.")?;
    let (owner, whose) = match arguments.get("scope").and_then(Value::as_str).unwrap_or("mine") {
        "mine" => (Owner { scope: Scope::Familiar, id: &agent.id }, "your memory".to_string()),
        "coven" => {
            let id = agent.coven_id.as_deref().ok_or("You aren't in a coven, so there's no coven memory.")?;
            (Owner { scope: Scope::Coven, id }, "your coven's memory".to_string())
        }
        other => return Err(format!("Unknown scope \"{other}\". Use mine or coven.")),
    };
    let text_arg = |name: &str| arguments.get(name).and_then(Value::as_str).ok_or(format!("Give the {name}."));
    let usage = |m: &Memory| format!("{} of {} used", kb(m.text.len()), kb(LIMIT));
    match tool {
        "read_memory" => {
            let memory = read(db, dir, owner)?;
            Ok(if memory.text.trim().is_empty() { format!("{whose} is empty.") } else { format!("{}\n\n({})", memory.text.trim_end(), usage(&memory)) })
        }
        "remember" => {
            let memory = append(db, dir, owner, text_arg("note")?, Some(caller), today)?;
            Ok(format!("Added the note to {whose} ({}).", usage(&memory)))
        }
        "rewrite_memory" => {
            let memory = write(db, dir, owner, text_arg("text")?, Some(caller))?;
            Ok(format!("Rewrote {whose} ({}).", usage(&memory)))
        }
        _ => Err(format!("Grimoire has no tool named {tool}.")),
    }
}

/// Checks that the familiar or coven exists before the user saves its memory.
fn owner_exists(db: &Db, owner: Owner) -> Result<(), String> {
    let exists = match owner.scope {
        Scope::Familiar => db.get_agent(owner.id)?.is_some(),
        Scope::Coven => crate::covens::exists(db, owner.id)?,
    };
    if exists { Ok(()) } else { Err("That memory's owner no longer exists.".into()) }
}

#[tauri::command]
pub fn get_memory(state: State<AppState>, scope: Scope, id: String) -> Result<Memory, String> {
    read(&state.db, dir()?, Owner { scope, id: &id })
}

#[tauri::command]
pub fn save_memory(app: AppHandle, state: State<AppState>, scope: Scope, id: String, text: String) -> Result<Memory, String> {
    let owner = Owner { scope, id: &id };
    owner_exists(&state.db, owner)?;
    let saved = write(&state.db, dir()?, owner, &text, None)?;
    let _ = app.emit(CHANGED, ());
    Ok(saved)
}

/// The event that tells the app's windows a familiar changed a memory.
pub fn changed_event(tool: &str) -> Option<&'static str> {
    (tool != "read_memory").then_some(CHANGED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Avatar, Provider};

    fn familiar(db: &Db, id: &str, coven: Option<&str>) -> Agent {
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
            coven_id: coven.map(String::from),
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        agent
    }

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("grimoire-memory-{}", uuid::Uuid::new_v4()))
    }

    const MINE: fn(&str) -> Owner<'_> = |id| Owner { scope: Scope::Familiar, id };

    #[test]
    fn stores_files_and_records_who_changed_them() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        familiar(&db, "scribe", None);
        let empty = read(&db, &dir, MINE("scribe")).unwrap();
        assert_eq!((empty.text.as_str(), empty.updated_at), ("", None));

        let saved = write(&db, &dir, MINE("scribe"), "Likes short answers.", Some("scribe")).unwrap();
        assert_eq!(saved.updated_by.as_deref(), Some("scribe"));
        assert!(saved.updated_at.is_some());
        assert_eq!(std::fs::read_to_string(dir.join("scribe.md")).unwrap(), "Likes short answers.");

        let by_user = write(&db, &dir, Owner { scope: Scope::Coven, id: "c1" }, "Ship on Fridays.", None).unwrap();
        assert_eq!(by_user.updated_by, None);
        assert!(dir.join("coven-c1.md").exists());

        assert!(MINE("../etc/passwd").path(&dir).is_err());
        assert!(MINE("").path(&dir).is_err());

        forget_in(&db, &dir, MINE("scribe")).unwrap();
        let gone = read(&db, &dir, MINE("scribe")).unwrap();
        assert_eq!((gone.text.as_str(), gone.updated_at), ("", None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn appends_dated_notes_and_enforces_the_limit() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        familiar(&db, "scribe", None);
        write(&db, &dir, MINE("scribe"), "# Notes", None).unwrap();
        let memory = append(&db, &dir, MINE("scribe"), " Uses bun.\nNot npm. ", Some("scribe"), "2026-11-23").unwrap();
        assert_eq!(memory.text, "# Notes\n- 2026-11-23: Uses bun.\n  Not npm.\n");
        assert!(append(&db, &dir, MINE("scribe"), "  ", None, "2026-11-23").is_err());

        let big = "x".repeat(LIMIT + 1);
        let error = write(&db, &dir, MINE("scribe"), &big, None).unwrap_err();
        assert!(error.contains("up to 16.0 KB"), "{error}");
        assert!(write(&db, &dir, MINE("scribe"), &"x".repeat(LIMIT), None).is_ok());
        let full = append(&db, &dir, MINE("scribe"), "one more", None, "2026-11-23").unwrap_err();
        assert!(full.contains("rewrite_memory"), "{full}");
        // A refused change leaves the file as it was.
        assert_eq!(read(&db, &dir, MINE("scribe")).unwrap().text.len(), LIMIT);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tools_reach_own_and_coven_memory() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        let coven = crate::covens::save(&db, None, "Research", &[]).unwrap();
        familiar(&db, "scribe", Some(&coven.id));
        familiar(&db, "loner", None);
        let call = |caller: &str, tool: &str, args: Value| tool_in(&db, &dir, caller, tool, &args, "2026-11-23");

        assert_eq!(call("scribe", "read_memory", json!({})).unwrap(), "your memory is empty.");
        assert!(call("scribe", "remember", json!({ "note": "The user is Nik." })).unwrap().starts_with("Added the note to your memory"));
        assert!(call("scribe", "read_memory", json!({ "scope": "mine" })).unwrap().contains("- 2026-11-23: The user is Nik."));

        call("scribe", "rewrite_memory", json!({ "scope": "coven", "text": "Cite sources." })).unwrap();
        let shared = read(&db, &dir, Owner { scope: Scope::Coven, id: &coven.id }).unwrap();
        assert_eq!((shared.text.as_str(), shared.updated_by.as_deref()), ("Cite sources.", Some("scribe")));

        assert!(call("loner", "read_memory", json!({ "scope": "coven" })).unwrap_err().contains("aren't in a coven"));
        assert!(call("scribe", "read_memory", json!({ "scope": "everyone" })).is_err());
        assert!(call("ghost", "read_memory", json!({})).is_err());
        assert!(call("scribe", "remember", json!({})).unwrap_err().contains("Give the note"));
        assert_eq!(changed_event("remember"), Some(CHANGED));
        assert_eq!(changed_event("read_memory"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn builds_the_session_prompt() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        let loner = familiar(&db, "loner", None);
        assert_eq!(prompt_in(&dir, &loner, false), None, "nothing to read and no tools");

        let prompt = prompt_in(&dir, &loner, true).unwrap();
        assert!(prompt.contains("remember") && prompt.contains("<memory of=\"you\">\n(empty)\n</memory>"));
        assert!(!prompt.contains("coven"));

        let scribe = familiar(&db, "scribe", Some("c1"));
        write(&db, &dir, MINE("scribe"), "Likes tables.\n", None).unwrap();
        write(&db, &dir, Owner { scope: Scope::Coven, id: "c1" }, "Cite sources.", None).unwrap();
        let prompt = prompt_in(&dir, &scribe, false).unwrap();
        assert!(prompt.contains("<memory of=\"you\">\nLikes tables.\n</memory>"));
        assert!(prompt.contains("<memory of=\"your coven\">\nCite sources.\n</memory>"));
        assert!(!prompt.contains("read_memory"), "no tools, so no tool hints");
        assert!(prompt.len() < 600, "the wrapper stays short");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Starts a new chat with a memory and a coven memory, and returns the familiar's reply.
    async fn live_recall(provider: Provider, model: &str) -> String {
        use crate::providers::{claude::Claude, codex::Codex, ProviderEvent};
        let dir = test_dir();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("live-memory.md"), "- 2026-11-20: The user's cat is named Miso.\n").unwrap();
        std::fs::write(dir.join("coven-live-coven.md"), "- The coven meets on Thursdays.\n").unwrap();
        let workspace = std::env::temp_dir().join(format!("grimoire-live-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let agent = Agent {
            id: "live-memory".into(),
            name: "Scribe".into(),
            provider: provider.clone(),
            model: Some(model.into()),
            workspace: workspace.to_string_lossy().into_owned(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: Some("live-coven".into()),
            created_at: now_ms(),
        };
        let prompt = "What do you remember about me and about my coven? Answer in one short sentence and don't use any tools.";
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (codex, claude) = (std::sync::Arc::new(Codex::default()), Claude::default());
        match provider {
            Provider::Codex => codex.start_turn(&agent, prompt, &[], tx).await.unwrap(),
            Provider::Claude => claude.start_turn(&agent, prompt, &[], tx).await.unwrap(),
        }
        let mut reply = String::new();
        let collect = async {
            while let Some(event) = rx.recv().await {
                match event {
                    ProviderEvent::Delta(text) => reply.push_str(&text),
                    ProviderEvent::Done => break,
                    ProviderEvent::Failed(e) => panic!("turn failed: {e}"),
                    _ => {}
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(180), collect).await.expect("no reply in time");
        claude.forget("live-memory");
        std::fs::remove_dir_all(workspace).unwrap();
        println!("{provider:?} replied: {reply}");
        reply
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real claude CLI"]
    async fn live_claude_reads_its_memory() {
        let reply = live_recall(Provider::Claude, "claude-haiku-5-5").await;
        assert!(reply.contains("Miso") && reply.contains("Thursday"), "{reply}");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real codex CLI"]
    async fn live_codex_reads_its_memory() {
        let reply = live_recall(Provider::Codex, "gpt-6.2-sol").await;
        assert!(reply.contains("Miso") && reply.contains("Thursday"), "{reply}");
    }
}
