//! A familiar's chats. Each chat keeps its own messages and CLI session (the Codex thread id or the
//! Claude session id). `agents.chat_id` points at the current chat, and `agents.session_id` mirrors that
//! chat's session, which is what the providers resume. Messages are also indexed for full-text search.

use crate::db::{now_ms, Db};
use crate::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Chat {
    pub id: String,
    pub agent_id: String,
    /// The first message sent in it, or a name given later. Empty until the first message.
    pub title: String,
    pub session_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub message_id: i64,
    pub agent_id: String,
    pub chat_id: String,
    pub chat_title: String,
    pub role: String,
    /// The text around the match, with each matched term between \u{2} and \u{3}.
    pub snippet: String,
    pub created_at: i64,
}

const TITLE_MAX: usize = 80;
const BUSY: &str = "Wait for the familiar to finish, or stop it first.";

/// Schema version 10: chats, the current chat per familiar, and the message search index.
const SCHEMA: &str = "
    CREATE TABLE chats (
      id TEXT PRIMARY KEY,
      agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
      title TEXT NOT NULL DEFAULT '',
      session_id TEXT,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL
    );
    CREATE INDEX chats_by_agent ON chats(agent_id, updated_at);
    ALTER TABLE agents ADD COLUMN chat_id TEXT;
    ALTER TABLE messages ADD COLUMN chat_id TEXT REFERENCES chats(id) ON DELETE CASCADE;
    CREATE INDEX messages_by_chat ON messages(chat_id, id);
    CREATE VIRTUAL TABLE messages_fts USING fts5(
      text, content = 'messages', content_rowid = 'id', tokenize = 'unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages WHEN new.role IN ('user', 'assistant') BEGIN
      INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages WHEN old.role IN ('user', 'assistant') BEGIN
      INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TRIGGER messages_fts_update AFTER UPDATE OF text ON messages WHEN old.role IN ('user', 'assistant') BEGIN
      INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
      INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
    END;
    INSERT INTO messages_fts(rowid, text) SELECT id, text FROM messages WHERE role IN ('user', 'assistant');
";

/// Upgrades to version 10. Before chats, "New chat" left a divider message and dropped the session, so
/// each run of messages between dividers becomes a chat, and the last one stays current with the session.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(SCHEMA)?;
    split_at_dividers(&tx)?;
    tx.execute_batch("PRAGMA user_version = 10;")?;
    tx.commit()
}

struct Segment {
    first: Option<i64>,
    last: i64,
    title: String,
    created_at: i64,
    updated_at: i64,
}

impl Segment {
    fn starting(at: i64) -> Self {
        Segment { first: None, last: 0, title: String::new(), created_at: at, updated_at: at }
    }
}

fn split_at_dividers(c: &Connection) -> rusqlite::Result<()> {
    let agents: Vec<(String, Option<String>, i64)> = c
        .prepare("SELECT id, session_id, created_at FROM agents")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (agent_id, session, agent_created) in agents {
        let mut segments = vec![Segment::starting(agent_created)];
        let mut stmt = c.prepare(
            "SELECT id, role, text, created_at, COALESCE(updated_at, created_at) FROM messages WHERE agent_id = ?1 ORDER BY id",
        )?;
        let mut rows = stmt.query(params![agent_id])?;
        while let Some(r) = rows.next()? {
            let (id, role, created): (i64, String, i64) = (r.get(0)?, r.get(1)?, r.get(3)?);
            if role == "divider" {
                segments.push(Segment::starting(created));
                continue;
            }
            let segment = segments.last_mut().expect("starts with one segment");
            if segment.first.is_none() {
                segment.first = Some(id);
                segment.created_at = created;
            }
            segment.last = id;
            segment.updated_at = segment.updated_at.max(r.get(4)?);
            if segment.title.is_empty() && role == "user" {
                segment.title = title_from(&r.get::<_, String>(2)?);
            }
        }
        let latest = segments.len() - 1;
        for (i, segment) in segments.iter().enumerate() {
            // Two dividers in a row left nothing between them; only the latest chat may be empty.
            if segment.first.is_none() && i != latest {
                continue;
            }
            let chat_id = uuid::Uuid::new_v4().to_string();
            c.execute(
                "INSERT INTO chats (id, agent_id, title, session_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![chat_id, agent_id, segment.title, if i == latest { session.as_deref() } else { None }, segment.created_at, segment.updated_at],
            )?;
            if let Some(first) = segment.first {
                c.execute(
                    "UPDATE messages SET chat_id = ?1 WHERE agent_id = ?2 AND id BETWEEN ?3 AND ?4",
                    params![chat_id, agent_id, first, segment.last],
                )?;
            }
            if i == latest {
                c.execute("UPDATE agents SET chat_id = ?1 WHERE id = ?2", params![chat_id, agent_id])?;
            }
        }
        c.execute("DELETE FROM messages WHERE agent_id = ?1 AND role = 'divider'", params![agent_id])?;
    }
    Ok(())
}

/// A chat title from the first message: whitespace collapsed, cut to 80 characters.
pub fn title_from(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= TITLE_MAX {
        return flat;
    }
    let cut: String = flat.chars().take(TITLE_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The familiar's current chat, created if it has none yet, like a newly summoned familiar.
pub(crate) fn ensure_current(c: &Connection, agent_id: &str) -> rusqlite::Result<String> {
    let current: Option<String> = c
        .query_row(
            "SELECT chats.id FROM agents JOIN chats ON chats.id = agents.chat_id WHERE agents.id = ?1",
            params![agent_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = current {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    c.execute(
        "INSERT INTO chats (id, agent_id, title, session_id, created_at, updated_at)
         SELECT ?1, id, '', session_id, ?2, ?2 FROM agents WHERE id = ?3",
        params![id, now, agent_id],
    )?;
    c.execute("UPDATE agents SET chat_id = ?1 WHERE id = ?2", params![id, agent_id])?;
    Ok(id)
}

/// Marks a chat as just used, and titles it from its first message you send.
pub(crate) fn touch(c: &Connection, chat_id: &str, role: &str, text: &str, at: i64) -> rusqlite::Result<()> {
    let title = if role == "user" { title_from(text) } else { String::new() };
    c.prepare_cached(
        "UPDATE chats SET updated_at = MAX(updated_at, ?2), title = CASE WHEN title = '' THEN ?3 ELSE title END WHERE id = ?1",
    )?
    .execute(params![chat_id, at, title])
    .map(|_| ())
}

const CHAT_COLUMNS: &str = "chats.id, chats.agent_id, chats.title, chats.session_id, chats.created_at, chats.updated_at,
     chats.id = (SELECT chat_id FROM agents WHERE agents.id = chats.agent_id)";

fn row_to_chat(r: &rusqlite::Row) -> rusqlite::Result<Chat> {
    Ok(Chat {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        title: r.get(2)?,
        session_id: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        current: r.get::<_, Option<bool>>(6)?.unwrap_or(false),
    })
}

fn get(c: &Connection, chat_id: &str) -> rusqlite::Result<Option<Chat>> {
    c.query_row(&format!("SELECT {CHAT_COLUMNS} FROM chats WHERE id = ?1"), params![chat_id], row_to_chat)
        .optional()
}

/// A familiar's chats, the most recently used first.
pub fn list(db: &Db, agent_id: &str) -> Result<Vec<Chat>, String> {
    db.with(|c| {
        ensure_current(c, agent_id)?;
        let mut stmt = c.prepare(&format!(
            "SELECT {CHAT_COLUMNS} FROM chats WHERE agent_id = ?1 AND side_of IS NULL ORDER BY updated_at DESC, created_at DESC"
        ))?;
        let rows = stmt.query_map(params![agent_id], row_to_chat)?;
        rows.collect()
    })
}

/// Makes `chat_id` current and loads its session for the next turn.
fn make_current(c: &Connection, agent_id: &str, chat_id: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE agents SET chat_id = ?2, session_id = (SELECT session_id FROM chats WHERE id = ?2) WHERE id = ?1",
        params![agent_id, chat_id],
    )
    .map(|_| ())
}

/// The familiar's current chat, if it has one yet.
pub fn current(db: &Db, agent_id: &str) -> Result<Option<String>, String> {
    db.with(|c| current_id(c, agent_id))
}

fn current_id(c: &Connection, agent_id: &str) -> rusqlite::Result<Option<String>> {
    c.query_row("SELECT chat_id FROM agents WHERE id = ?1", params![agent_id], |r| r.get(0))
        .optional()
        .map(Option::flatten)
}

fn insert_empty(c: &Connection, agent_id: &str) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    c.execute(
        "INSERT INTO chats (id, agent_id, title, session_id, created_at, updated_at) VALUES (?1, ?2, '', NULL, ?3, ?3)",
        params![id, agent_id, now],
    )?;
    Ok(id)
}

/// Starts a fresh chat with no session. A current chat with no messages yet is reused.
pub fn start(db: &Db, agent_id: &str) -> Result<Chat, String> {
    db.with(|c| {
        let current = ensure_current(c, agent_id)?;
        let empty: bool = c.query_row(
            "SELECT NOT EXISTS (SELECT 1 FROM messages WHERE chat_id = ?1)",
            params![current],
            |r| r.get(0),
        )?;
        let id = if empty { current } else { insert_empty(c, agent_id)? };
        make_current(c, agent_id, &id)?;
        Ok(get(c, &id)?.expect("just made"))
    })
}

/// Opens one of the familiar's chats; the next message resumes its session.
pub fn open(db: &Db, agent_id: &str, chat_id: &str) -> Result<Chat, String> {
    let main = crate::side::get(db, chat_id)?.is_none();
    let chat = db.with(|c| get(c, chat_id))?.filter(|chat| chat.agent_id == agent_id && main).ok_or("That chat no longer exists.")?;
    db.with(|c| make_current(c, agent_id, chat_id))?;
    Ok(Chat { current: true, ..chat })
}

pub fn rename(db: &Db, chat_id: &str, title: &str) -> Result<Chat, String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() || title.chars().count() > TITLE_MAX {
        return Err(format!("Give the chat a title of 1 to {TITLE_MAX} characters."));
    }
    db.with(|c| {
        c.execute("UPDATE chats SET title = ?2 WHERE id = ?1", params![chat_id, title])?;
        get(c, chat_id)
    })?
    .ok_or_else(|| "That chat no longer exists.".into())
}

/// Deletes a chat and its messages. Deleting the current chat opens the newest one left, or a fresh one.
/// Returns the familiar and whether its current chat changed.
pub fn delete(db: &Db, chat_id: &str) -> Result<(String, bool), String> {
    db.with(|c| {
        let Some(chat) = get(c, chat_id)? else { return Ok((String::new(), false)) };
        c.execute("DELETE FROM chats WHERE id = ?1", params![chat_id])?;
        if !chat.current {
            return Ok((chat.agent_id, false));
        }
        let newest: Option<String> = c
            .query_row(
                "SELECT id FROM chats WHERE agent_id = ?1 AND side_of IS NULL ORDER BY updated_at DESC, created_at DESC LIMIT 1",
                params![chat.agent_id],
                |r| r.get(0),
            )
            .optional()?;
        let next = match newest {
            Some(id) => id,
            None => insert_empty(c, &chat.agent_id)?,
        };
        make_current(c, &chat.agent_id, &next)?;
        Ok((chat.agent_id, true))
    })
}

/// Turns typed words into an FTS5 query: each word quoted, all required, the last one as a prefix.
pub fn fts_query(input: &str) -> Option<String> {
    let words: Vec<String> = input
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    let last = words.last()?;
    let mut query = words[..words.len() - 1].join(" ");
    if !query.is_empty() {
        query.push(' ');
    }
    query.push_str(last);
    query.push('*');
    Some(query)
}

/// Messages you sent and replies, best matches first.
pub fn search(db: &Db, input: &str, limit: i64) -> Result<Vec<SearchHit>, String> {
    let Some(query) = fts_query(input) else { return Ok(Vec::new()) };
    db.with(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT m.id, m.agent_id, m.chat_id, chats.title, m.role,
                    snippet(messages_fts, 0, char(2), char(3), '…', 14), m.created_at
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             JOIN chats ON chats.id = m.chat_id
             WHERE messages_fts MATCH ?1 AND chats.side_of IS NULL
             ORDER BY bm25(messages_fts), m.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit], |r| {
            Ok(SearchHit {
                message_id: r.get(0)?,
                agent_id: r.get(1)?,
                chat_id: r.get(2)?,
                chat_title: r.get(3)?,
                role: r.get(4)?,
                snippet: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        rows.collect()
    })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ChatsChanged<'a> {
    agent_id: &'a str,
    /// The current chat changed, so open chat views reload.
    switched: bool,
}

fn changed(app: &AppHandle, agent_id: &str, switched: bool) {
    let _ = app.emit("chats_changed", ChatsChanged { agent_id, switched });
    if switched {
        // Linked Macs show the current chat, so they reload it.
        crate::link::record("chat", || serde_json::json!({ "agentId": agent_id }));
    }
}

/// Switches the familiar's current chat, dropping the live CLI session so the next turn resumes the new one.
fn switch(app: &AppHandle, state: &AppState, agent_id: &str, to: impl FnOnce() -> Result<Chat, String>) -> Result<Chat, String> {
    if state.runtime.is_busy(agent_id) {
        return Err(BUSY.into());
    }
    let before = state.db.with(|c| current_id(c, agent_id))?;
    let chat = to()?;
    let switched = before.as_deref() != Some(chat.id.as_str());
    if switched {
        state.runtime.reset_session(agent_id);
    }
    changed(app, agent_id, switched);
    Ok(chat)
}

#[tauri::command]
pub fn list_chats(state: State<AppState>, agent_id: String) -> Result<Vec<Chat>, String> {
    crate::link::local_only(&agent_id)?;
    list(&state.db, &agent_id)
}

#[tauri::command]
pub fn new_chat(app: AppHandle, state: State<AppState>, agent_id: String) -> Result<Chat, String> {
    crate::link::local_only(&agent_id)?;
    let left = state.db.with(|c| current_id(c, &agent_id))?;
    let chat = switch(&app, &state, &agent_id, || start(&state.db, &agent_id))?;
    if let Some(left) = left.filter(|left| *left != chat.id) {
        crate::checkins::after_new_chat(&state.db, &state.runtime, &agent_id, &left);
    }
    Ok(chat)
}

#[tauri::command]
pub fn open_chat(app: AppHandle, state: State<AppState>, agent_id: String, chat_id: String) -> Result<Chat, String> {
    crate::link::local_only(&agent_id)?;
    // Opening the chat that is already current changes nothing, so it's fine mid-turn, like a search jump.
    if state.db.with(|c| current_id(c, &agent_id))?.as_deref() == Some(chat_id.as_str()) {
        return open(&state.db, &agent_id, &chat_id);
    }
    switch(&app, &state, &agent_id, || open(&state.db, &agent_id, &chat_id))
}

#[tauri::command]
pub fn rename_chat(app: AppHandle, state: State<AppState>, chat_id: String, title: String) -> Result<Chat, String> {
    let chat = rename(&state.db, &chat_id, &title)?;
    changed(&app, &chat.agent_id, false);
    Ok(chat)
}

#[tauri::command]
pub fn delete_chat(app: AppHandle, state: State<AppState>, chat_id: String) -> Result<(), String> {
    let chat = state.db.with(|c| get(c, &chat_id))?.ok_or("That chat no longer exists.")?;
    if chat.current && state.runtime.is_busy(&chat.agent_id) {
        return Err(BUSY.into());
    }
    let (agent_id, switched) = delete(&state.db, &chat_id)?;
    if switched {
        state.runtime.reset_session(&agent_id);
    }
    changed(&app, &agent_id, switched);
    Ok(())
}

#[tauri::command]
pub fn search_messages(state: State<AppState>, query: String, limit: Option<i64>) -> Result<Vec<SearchHit>, String> {
    search(&state.db, &query, limit.unwrap_or(30).clamp(1, 100))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Agent, Avatar, Provider};

    fn agent(id: &str) -> Agent {
        Agent {
            id: id.into(),
            name: "Ada".into(),
            provider: Provider::Codex,
            model: None,
            workspace: "/tmp/ada".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: 1,
        }
    }

    fn texts(db: &Db, agent_id: &str) -> Vec<String> {
        db.list_messages(agent_id, None, 50).unwrap().into_iter().map(|m| m.text).collect()
    }

    #[test]
    fn migration_splits_messages_at_dividers() {
        let dir = std::env::temp_dir().join(format!("grimoire-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grimoire.db");
        {
            // A version 1 database with dividers; opening it runs every migration up to chats.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE agents (id TEXT PRIMARY KEY, name TEXT NOT NULL, provider TEXT NOT NULL, model TEXT,
                   workspace TEXT NOT NULL, access TEXT NOT NULL, session_id TEXT, created_at INTEGER NOT NULL);
                 CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                   role TEXT NOT NULL, text TEXT NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL);
                 CREATE INDEX messages_by_agent ON messages(agent_id, id);
                 INSERT INTO agents (id, name, provider, workspace, access, session_id, created_at) VALUES
                   ('a', 'Ada', 'claude', '/tmp', 'workspace', 'live-session', 1),
                   ('b', 'Bo', 'codex', '/tmp', 'workspace', NULL, 1);
                 INSERT INTO messages (agent_id, role, text, status, created_at) VALUES
                   ('a', 'user', '  Plan the   launch  ', 'done', 10),
                   ('a', 'assistant', 'Here is a plan.', 'done', 11),
                   ('a', 'divider', 'New chat', 'done', 12),
                   ('a', 'divider', 'New chat', 'done', 13),
                   ('a', 'assistant', 'A ritual reply first', 'done', 14),
                   ('a', 'user', 'Write the notes', 'done', 15),
                   ('b', 'user', 'Old question', 'done', 20),
                   ('b', 'divider', 'New chat', 'done', 21);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();

        let chats = list(&db, "a").unwrap();
        let titles: Vec<&str> = chats.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Write the notes", "Plan the launch"], "the empty run between dividers is skipped");
        assert!(chats[0].current);
        assert_eq!(chats[0].session_id.as_deref(), Some("live-session"));
        assert_eq!(chats[1].session_id, None);
        assert_eq!((chats[1].created_at, chats[1].updated_at), (10, 11));
        assert_eq!(texts(&db, "a"), ["A ritual reply first", "Write the notes"]);

        // A trailing divider means "New chat" was pressed and nothing sent yet: an empty current chat.
        let chats = list(&db, "b").unwrap();
        assert_eq!(chats.len(), 2);
        assert!(chats.iter().any(|c| c.current && c.title.is_empty()));
        assert!(texts(&db, "b").is_empty());

        let roles: i64 = db.with(|c| c.query_row("SELECT COUNT(*) FROM messages WHERE role = 'divider' OR chat_id IS NULL", [], |r| r.get(0))).unwrap();
        assert_eq!(roles, 0);
        assert_eq!(search(&db, "launch", 10).unwrap().len(), 1, "old messages are searchable");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chats_keep_their_own_sessions() {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("a")).unwrap();
        db.insert_message("a", "user", "first topic", "done").unwrap();
        // The provider reports its thread or session id during the turn.
        db.set_session("a", Some("thread-1")).unwrap();
        let first = list(&db, "a").unwrap().remove(0);
        assert_eq!(first.session_id.as_deref(), Some("thread-1"));

        let second = start(&db, "a").unwrap();
        assert_ne!(second.id, first.id);
        assert_eq!(db.get_agent("a").unwrap().unwrap().session_id, None, "a new chat starts a new session");
        assert_eq!(start(&db, "a").unwrap().id, second.id, "an empty chat is reused");
        db.insert_message("a", "user", "second topic", "done").unwrap();
        db.set_session("a", Some("thread-2")).unwrap();

        open(&db, "a", &first.id).unwrap();
        assert_eq!(db.get_agent("a").unwrap().unwrap().session_id.as_deref(), Some("thread-1"));
        assert_eq!(texts(&db, "a"), ["first topic"]);
        open(&db, "a", &second.id).unwrap();
        assert_eq!(db.get_agent("a").unwrap().unwrap().session_id.as_deref(), Some("thread-2"));
        assert_eq!(texts(&db, "a"), ["second topic"]);

        db.insert_agent(&agent("other")).unwrap();
        assert!(open(&db, "other", &first.id).is_err(), "a chat opens only for its own familiar");
    }

    #[test]
    fn renames_and_deletes_chats() {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("a")).unwrap();
        db.insert_message("a", "user", "older", "done").unwrap();
        let older = list(&db, "a").unwrap().remove(0);
        std::thread::sleep(std::time::Duration::from_millis(2));
        start(&db, "a").unwrap();
        db.insert_message("a", "user", "newer", "done").unwrap();
        db.set_session("a", Some("s-new")).unwrap();
        let newer = list(&db, "a").unwrap().remove(0);
        assert_eq!(newer.title, "newer");

        assert_eq!(rename(&db, &older.id, "  Launch   plan ").unwrap().title, "Launch plan");
        assert!(rename(&db, &older.id, "   ").is_err());

        // Open the older chat, then delete it: the newest remaining becomes current with its session.
        open(&db, "a", &older.id).unwrap();
        assert_eq!(delete(&db, &older.id).unwrap(), ("a".to_string(), true));
        assert_eq!(db.get_agent("a").unwrap().unwrap().session_id.as_deref(), Some("s-new"));
        assert_eq!(texts(&db, "a"), ["newer"]);

        // Deleting the last chat leaves a fresh one.
        assert_eq!(delete(&db, &newer.id).unwrap(), ("a".to_string(), true));
        let left = list(&db, "a").unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].current && left[0].title.is_empty());
        assert_eq!(db.get_agent("a").unwrap().unwrap().session_id, None);
        assert!(search(&db, "newer", 10).unwrap().is_empty(), "deleted messages leave the index");
    }

    #[test]
    fn searches_messages_with_snippets() {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("a")).unwrap();
        db.insert_message("a", "user", "How do store setters work in Solid?", "done").unwrap();
        let reply = db.insert_message("a", "assistant", "draft", "streaming").unwrap();
        db.update_message(reply.id, "Setters take a draft function, and writes batch.", "done").unwrap();
        db.insert_message("a", "tool", "$ grep setters", "done").unwrap();

        let hits = search(&db, "setter", 10).unwrap();
        assert_eq!(hits.len(), 2, "prefix match on the last word, tool rows not indexed: {hits:?}");
        assert!(hits.iter().all(|h| h.chat_title == "How do store setters work in Solid?"));
        assert!(hits.iter().any(|h| h.snippet.contains("\u{2}Setters\u{3}")));
        assert!(search(&db, "draft", 10).unwrap().iter().all(|h| h.role == "assistant"));
        assert_eq!(search(&db, "draft streaming", 10).unwrap().len(), 0, "edits replace the indexed text");
        assert_eq!(search(&db, "solid \"store", 10).unwrap().len(), 1, "quotes are escaped");
        assert!(search(&db, " -- ", 10).unwrap().is_empty());
    }

    #[test]
    fn builds_safe_fts_queries_and_titles() {
        assert_eq!(fts_query("  store set "), Some("\"store\" \"set\"*".into()));
        assert_eq!(fts_query("say \"hi\""), Some("\"say\" \"\"\"hi\"\"\"*".into()));
        assert_eq!(fts_query("* -"), None);
        assert_eq!(title_from("  Plan\n the launch "), "Plan the launch");
        let long = title_from(&"word ".repeat(40));
        assert_eq!(long.chars().count(), 80);
        assert!(long.ends_with('…'));
    }
}
