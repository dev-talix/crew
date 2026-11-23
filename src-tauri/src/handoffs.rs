//! The handoffs list: who handed what to whom with `ask_familiar` or `message_familiar`, and where
//! each one stands. Every request gets its own row, so a follow-up in the same side conversation
//! never changes how an earlier request went. Helpers' side chats never get one.

use crate::db::{now_ms, Db};
use crate::runtime::TurnOutcome;
use crate::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use serde::Serialize;
use tauri::State;

/// Tells windows to reload the list.
pub const CHANGED: &str = "handoffs_changed";
/// The most handoffs the list shows.
pub const LIMIT: usize = 100;
/// How much of the request the list shows.
const WHAT_CHARS: usize = 280;

/// Schema version 23: side conversations remember whether they came from `ask_familiar` ("ask") or
/// `message_familiar` ("message"). Older ones are filled in from the link row in the sender's chat.
/// Version 24 replaces this with the `handoffs` table and stops writing the column.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!("BEGIN; ALTER TABLE chats ADD COLUMN handoff TEXT; {BACKFILL}; PRAGMA user_version = 23; COMMIT;"))
}

const BACKFILL: &str = "UPDATE chats SET handoff = CASE WHEN (
      SELECT text FROM messages WHERE role = 'side' AND source = chats.id ORDER BY id DESC LIMIT 1
    ) LIKE 'Messaged %' THEN 'message' ELSE 'ask' END
  WHERE side_of IS NOT NULL AND id NOT IN (SELECT chat_id FROM helpers)";

/// Schema version 24: one row per request, with its status saved as it changes. Requests already
/// in handoff conversations get a row each, their status read from the rows up to the next request.
pub(crate) fn migrate_requests(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "BEGIN;
         CREATE TABLE handoffs (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
           kind TEXT NOT NULL,
           text TEXT NOT NULL,
           status TEXT NOT NULL,
           created_at INTEGER NOT NULL
         );
         CREATE INDEX handoffs_by_chat ON handoffs(chat_id);
         {BACKFILL_REQUESTS};
         PRAGMA user_version = 24;
         COMMIT;"
    ))
}

const BACKFILL_REQUESTS: &str = "INSERT INTO handoffs (chat_id, kind, text, status, created_at)
  SELECT m.chat_id, c.handoff, m.text,
    CASE WHEN EXISTS (SELECT 1 FROM messages r WHERE r.chat_id = m.chat_id AND r.id > m.id AND r.id < m.next
                      AND (r.role = 'error' OR (r.role = 'assistant' AND r.status IN ('interrupted', 'stopped', 'cancelled', 'streaming'))))
           THEN 'failed'
         WHEN EXISTS (SELECT 1 FROM messages r WHERE r.chat_id = m.chat_id AND r.id > m.id AND r.id < m.next) THEN 'done'
         ELSE 'failed' END,
    m.created_at
  FROM (SELECT id, chat_id, text, created_at,
               COALESCE((SELECT MIN(n.id) FROM messages n WHERE n.chat_id = u.chat_id AND n.role = 'user' AND n.id > u.id), 9223372036854775807) AS next
        FROM messages u WHERE role = 'user') m
  JOIN chats c ON c.id = m.chat_id
  WHERE c.handoff IS NOT NULL AND c.side_of IS NOT NULL AND c.id NOT IN (SELECT chat_id FROM helpers)
  ORDER BY m.id";

/// Schema version 26: handoffs between Macs. On the home Mac, a side conversation asked by a familiar
/// on a linked Mac keeps that Mac (`remote_peer`) and the asker's id and name there. Its `side_of`
/// holds the asked familiar itself, as a helper's chat does, so every side-chat filter still finds it.
/// On the asking Mac, a handoff to a linked Mac's familiar has no chat here: `chat_id` is NULL and the
/// row keeps the home Mac's thread id, the Mac, the asker, and the familiar asked.
pub(crate) fn migrate_linked(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         ALTER TABLE chats ADD COLUMN remote_peer TEXT;
         ALTER TABLE chats ADD COLUMN remote_asker TEXT;
         ALTER TABLE chats ADD COLUMN remote_name TEXT;
         CREATE TABLE handoffs_new (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           chat_id TEXT REFERENCES chats(id) ON DELETE CASCADE,
           kind TEXT NOT NULL,
           text TEXT NOT NULL,
           status TEXT NOT NULL,
           created_at INTEGER NOT NULL,
           thread_id TEXT,
           peer_id TEXT,
           asker_id TEXT REFERENCES agents(id) ON DELETE CASCADE,
           target_id TEXT,
           target_name TEXT
         );
         INSERT INTO handoffs_new (id, chat_id, kind, text, status, created_at)
           SELECT id, chat_id, kind, text, status, created_at FROM handoffs;
         DROP TABLE handoffs;
         ALTER TABLE handoffs_new RENAME TO handoffs;
         CREATE INDEX handoffs_by_chat ON handoffs(chat_id);
         CREATE INDEX handoffs_by_thread ON handoffs(thread_id);
         PRAGMA user_version = 26;
         COMMIT;",
    )
}

/// Schema version 27: on the home Mac, a request from a familiar on a linked Mac keeps the asking
/// Mac's id for it, so a restart that cuts it off can still send that Mac a reply.
pub(crate) fn migrate_remote_request(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         ALTER TABLE handoffs ADD COLUMN remote_request INTEGER;
         PRAGMA user_version = 27;
         COMMIT;",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Sent, and the other familiar hasn't started on it.
    Waiting,
    /// The other familiar is working on it.
    Accepted,
    /// The other familiar finished its turn.
    Done,
    /// An error, a denial, a stop, or a reply cut off.
    Failed,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Waiting => "waiting",
            Status::Accepted => "accepted",
            Status::Done => "done",
            Status::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Status {
        match text {
            "waiting" => Status::Waiting,
            "accepted" => Status::Accepted,
            "done" => Status::Done,
            _ => Status::Failed,
        }
    }

    /// How a request ended, from its turn's outcome.
    pub fn of(outcome: &TurnOutcome) -> Status {
        match outcome {
            TurnOutcome::Done(_) => Status::Done,
            TurnOutcome::Stopped | TurnOutcome::Failed(_) => Status::Failed,
        }
    }
}

/// Records a request sent in side conversation `chat_id`, by `kind` ("ask" or "message").
pub fn record(db: &Db, chat_id: &str, kind: &str, text: &str, status: Status) -> Result<i64, String> {
    db.with(|c| {
        c.execute(
            "INSERT INTO handoffs (chat_id, kind, text, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![chat_id, kind, text, status.as_str(), now_ms()],
        )?;
        Ok(c.last_insert_rowid())
    })
}

/// Saves the asking Mac's id for a request a familiar on that Mac sent here.
pub fn set_remote_request(db: &Db, id: i64, request: i64) -> Result<(), String> {
    db.with(|c| c.execute("UPDATE handoffs SET remote_request = ?2 WHERE id = ?1", params![id, request]).map(|_| ()))
}

/// A request from a familiar on a linked Mac that a restart here cut off, which that Mac still waits on.
#[derive(Debug, Clone, PartialEq)]
pub struct CutOff {
    pub peer: String,
    /// The side conversation here.
    pub thread: String,
    /// The asking Mac's id for it.
    pub request: i64,
}

/// Requests from familiars on linked Macs still unfinished. Run at startup before `fail_unfinished`.
pub fn cut_off_remote(db: &Db) -> Result<Vec<CutOff>, String> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT c.remote_peer, h.chat_id, h.remote_request FROM handoffs h JOIN chats c ON c.id = h.chat_id
             WHERE h.status IN ('waiting', 'accepted') AND c.remote_peer IS NOT NULL AND h.remote_request IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok(CutOff { peer: r.get(0)?, thread: r.get(1)?, request: r.get(2)? }))?;
        rows.collect()
    })
}

/// A familiar on a linked Mac that a familiar here handed something to.
pub struct Away<'a> {
    /// The home Mac's thread, once it has answered, or the one being followed up.
    pub thread: Option<&'a str>,
    pub peer: &'a str,
    pub asker_id: &'a str,
    /// The familiar's id on its home Mac.
    pub target_id: &'a str,
    pub target_name: &'a str,
}

/// Records a request sent to a familiar on a linked Mac.
pub fn record_away(db: &Db, away: &Away, kind: &str, text: &str, status: Status) -> Result<i64, String> {
    db.with(|c| {
        c.execute(
            "INSERT INTO handoffs (chat_id, kind, text, status, created_at, thread_id, peer_id, asker_id, target_id, target_name)
             VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![kind, text, status.as_str(), now_ms(), away.thread, away.peer, away.asker_id, away.target_id, away.target_name],
        )?;
        Ok(c.last_insert_rowid())
    })
}

/// Saves the home Mac's thread on a request sent to a linked Mac.
pub fn set_thread(db: &Db, id: i64, thread: &str) -> Result<(), String> {
    db.with(|c| c.execute("UPDATE handoffs SET thread_id = ?2 WHERE id = ?1 AND chat_id IS NULL", params![id, thread]).map(|_| ()))
}

/// The Mac, asker, and familiar of a thread on a linked Mac that a familiar here started.
pub fn away_thread(db: &Db, thread: &str) -> Result<Option<(String, String, String)>, String> {
    db.with(|c| {
        c.query_row(
            "SELECT peer_id, asker_id, target_id FROM handoffs WHERE thread_id = ?1 AND chat_id IS NULL ORDER BY id LIMIT 1",
            params![thread],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
    })
}

/// A request a familiar here sent to a linked Mac, as its reply finds it.
#[derive(Debug)]
pub struct Sent {
    pub peer: String,
    pub thread: Option<String>,
    pub asker_id: String,
    pub target_name: String,
    /// The linked Mac's name, while it stays linked.
    pub mac: Option<String>,
    pub status: Status,
}

pub fn sent(db: &Db, id: i64) -> Result<Option<Sent>, String> {
    db.with(|c| {
        c.query_row(
            "SELECT h.peer_id, h.thread_id, h.asker_id, h.target_name, p.name, h.status
             FROM handoffs h LEFT JOIN link_peers p ON p.id = h.peer_id
             WHERE h.id = ?1 AND h.chat_id IS NULL AND h.peer_id IS NOT NULL AND h.asker_id IS NOT NULL",
            params![id],
            |r| {
                Ok(Sent {
                    peer: r.get(0)?,
                    thread: r.get(1)?,
                    asker_id: r.get(2)?,
                    target_name: r.get::<_, Option<String>>(3)?.unwrap_or_else(|| "A familiar".into()),
                    mac: r.get(4)?,
                    status: Status::parse(&r.get::<_, String>(5)?),
                })
            },
        )
        .optional()
    })
}

/// Moves one request on to `status`.
pub fn set(db: &Db, id: i64, status: Status) -> Result<(), String> {
    db.with(|c| c.execute("UPDATE handoffs SET status = ?2 WHERE id = ?1", params![id, status.as_str()]).map(|_| ()))
}

/// Fails every request still waiting or accepted. No turn outlives the app, so run once at startup.
pub fn fail_unfinished(db: &Db) -> Result<usize, String> {
    db.with(|c| c.execute("UPDATE handoffs SET status = 'failed' WHERE status IN ('waiting', 'accepted')", []))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Handoff {
    pub id: i64,
    /// The side conversation it was sent in. For a familiar on a linked Mac, the thread there.
    pub chat_id: String,
    /// Ids as `list_agents` shows them: a familiar on a linked Mac has its `peer:` id.
    pub from_id: String,
    pub to_id: String,
    pub from_name: String,
    pub to_name: String,
    /// The linked Mac a familiar lives on, or None for this Mac.
    pub from_mac: Option<String>,
    pub to_mac: Option<String>,
    /// "ask" or "message".
    pub kind: String,
    /// The request, trimmed.
    pub what: String,
    pub created_at: i64,
    pub status: Status,
}

fn trimmed(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(WHAT_CHARS) {
        Some((at, _)) => format!("{}…", text[..at].trim_end()),
        None => text.to_string(),
    }
}

/// One row of the list as stored, before names are filled in.
struct Row {
    id: i64,
    chat_id: Option<String>,
    thread_id: Option<String>,
    kind: String,
    text: String,
    created_at: i64,
    status: String,
    side_of: Option<String>,
    agent_id: Option<String>,
    remote_peer: Option<String>,
    remote_asker: Option<String>,
    remote_name: Option<String>,
    asker_id: Option<String>,
    peer_id: Option<String>,
    target_id: Option<String>,
    target_name: Option<String>,
    mac: Option<String>,
}

/// The latest `limit` requests, newest first.
pub fn list(db: &Db, limit: usize) -> Result<Vec<Handoff>, String> {
    let names: HashMap<String, String> = db.list_agents()?.into_iter().map(|a| (a.id, a.name)).collect();
    let rows = db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT h.id, h.chat_id, h.thread_id, h.kind, h.text, h.created_at, h.status,
                    c.side_of, c.agent_id, c.remote_peer, c.remote_asker, c.remote_name,
                    h.asker_id, h.peer_id, h.target_id, h.target_name, p.name
             FROM handoffs h
             LEFT JOIN chats c ON c.id = h.chat_id
             LEFT JOIN link_peers p ON p.id = COALESCE(c.remote_peer, h.peer_id)
             ORDER BY h.created_at DESC, h.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(Row {
                id: r.get(0)?,
                chat_id: r.get(1)?,
                thread_id: r.get(2)?,
                kind: r.get(3)?,
                text: r.get(4)?,
                created_at: r.get(5)?,
                status: r.get(6)?,
                side_of: r.get(7)?,
                agent_id: r.get(8)?,
                remote_peer: r.get(9)?,
                remote_asker: r.get(10)?,
                remote_name: r.get(11)?,
                asker_id: r.get(12)?,
                peer_id: r.get(13)?,
                target_id: r.get(14)?,
                target_name: r.get(15)?,
                mac: r.get(16)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<Row>>>()
    })?;
    let name_of = |id: &str| names.get(id).cloned().unwrap_or_else(|| "A familiar".into());
    let local = |id: Option<String>| {
        let id = id.unwrap_or_default();
        (name_of(&id), id)
    };
    Ok(rows
        .into_iter()
        .map(|row| {
            let mac = Some(row.mac.clone().unwrap_or_else(|| "a linked Mac".into()));
            let ((from_name, from_id, from_mac), (to_name, to_id, to_mac), chat_id) = match (row.chat_id, row.remote_peer) {
                // Asked here by a familiar on a linked Mac.
                (Some(chat), Some(peer)) => {
                    let from = crate::link::remote_id(&peer, &row.remote_asker.unwrap_or_default());
                    let (to_name, to_id) = local(row.agent_id);
                    ((row.remote_name.unwrap_or_else(|| "A familiar".into()), from, mac), (to_name, to_id, None), chat)
                }
                (Some(chat), None) => {
                    let ((from_name, from_id), (to_name, to_id)) = (local(row.side_of), local(row.agent_id));
                    ((from_name, from_id, None), (to_name, to_id, None), chat)
                }
                // Sent from here to a familiar on a linked Mac.
                (None, _) => {
                    let (from_name, from_id) = local(row.asker_id);
                    let to = crate::link::remote_id(&row.peer_id.unwrap_or_default(), &row.target_id.unwrap_or_default());
                    let to_name = row.target_name.unwrap_or_else(|| "A familiar".into());
                    ((from_name, from_id, None), (to_name, to, mac), row.thread_id.unwrap_or_default())
                }
            };
            Handoff {
                id: row.id,
                chat_id,
                from_id,
                to_id,
                from_name,
                to_name,
                from_mac,
                to_mac,
                kind: row.kind,
                what: trimmed(&row.text),
                created_at: row.created_at,
                status: Status::parse(&row.status),
            }
        })
        .collect())
}

#[tauri::command]
pub fn list_handoffs(state: State<AppState>) -> Result<Vec<Handoff>, String> {
    list(&state.db, LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Agent, Avatar, Provider};

    fn familiar(db: &Db, id: &str) -> Agent {
        let agent = Agent {
            id: id.into(),
            name: id.to_uppercase(),
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

    fn statuses(db: &Db) -> Vec<(String, Status)> {
        list(db, LIMIT).unwrap().into_iter().map(|h| (h.what, h.status)).collect()
    }

    #[test]
    fn lists_one_row_per_request_newest_first() {
        let db = Db::open_in_memory().unwrap();
        let (a, b) = (familiar(&db, "a"), familiar(&db, "b"));
        let chat = crate::side::create(&db, &a.id, &b.id, "First").unwrap();
        let first = record(&db, &chat.id, "ask", "  First  ", Status::Waiting).unwrap();
        set(&db, first, Status::Done).unwrap();
        // A denied follow-up fails on its own row and leaves the earlier request done.
        record(&db, &chat.id, "message", "Second", Status::Failed).unwrap();
        let other = crate::side::create(&db, &a.id, &b.id, "Other").unwrap();
        record(&db, &other.id, "ask", "Other", Status::Waiting).unwrap();

        let list = list(&db, LIMIT).unwrap();
        let got: Vec<(&str, &str, &str, Status)> = list.iter().map(|h| (h.chat_id.as_str(), h.kind.as_str(), h.what.as_str(), h.status)).collect();
        assert_eq!(
            got,
            [
                (other.id.as_str(), "ask", "Other", Status::Waiting),
                (chat.id.as_str(), "message", "Second", Status::Failed),
                (chat.id.as_str(), "ask", "First", Status::Done),
            ]
        );
        assert_eq!((list[2].from_id.as_str(), list[2].to_id.as_str()), ("a", "b"));
        assert_eq!(super::list(&db, 2).unwrap().len(), 2);
    }

    #[test]
    fn a_late_reply_to_one_request_leaves_the_next_ones_failure() {
        let db = Db::open_in_memory().unwrap();
        let (a, b) = (familiar(&db, "a"), familiar(&db, "b"));
        let chat = crate::side::create(&db, &a.id, &b.id, "One").unwrap();
        let one = record(&db, &chat.id, "message", "One", Status::Accepted).unwrap();
        record(&db, &chat.id, "message", "Two", Status::Failed).unwrap();
        set(&db, one, Status::of(&TurnOutcome::Done("Reply to one".into()))).unwrap();
        assert_eq!(statuses(&db), [("Two".into(), Status::Failed), ("One".into(), Status::Done)]);
    }

    #[test]
    fn restarting_fails_requests_left_unfinished() {
        let db = Db::open_in_memory().unwrap();
        let (a, b) = (familiar(&db, "a"), familiar(&db, "b"));
        let chat = crate::side::create(&db, &a.id, &b.id, "x").unwrap();
        for (text, status) in [("w", Status::Waiting), ("a", Status::Accepted), ("d", Status::Done)] {
            record(&db, &chat.id, "ask", text, status).unwrap();
        }
        assert_eq!(fail_unfinished(&db).unwrap(), 2);
        assert_eq!(statuses(&db), [("d".into(), Status::Done), ("a".into(), Status::Failed), ("w".into(), Status::Failed)]);
    }

    #[test]
    fn fills_in_requests_from_older_conversations() {
        let db = Db::open_in_memory().unwrap();
        let (a, b) = (familiar(&db, "a"), familiar(&db, "b"));
        let chat = crate::side::create(&db, &a.id, &b.id, "q").unwrap();
        let rows = [("user", "q1", "done"), ("assistant", "a1", "done"), ("user", "q2", "done"), ("error", "no", "done"), ("user", "q3", "done"), ("assistant", "cut off", "streaming")];
        for (role, text, state) in rows {
            db.insert_message_in(&chat.id, &b.id, role, text, state, None).unwrap();
        }
        let messaged = crate::side::create(&db, &a.id, &b.id, "m").unwrap();
        db.insert_message_in(&messaged.id, &b.id, "user", "m1", "done", None).unwrap();
        db.insert_message_in(&messaged.id, &b.id, "tool", "$ ls", "done", None).unwrap();
        db.insert_message_from(&a.id, crate::side::LINK_ROLE, "Messaged B", "done", Some(&messaged.id)).unwrap();
        let (_, helper) = crate::helpers::create(&db, &a.id, None, &serde_json::json!({ "task": "x" })).unwrap();
        db.insert_message_in(&helper.chat_id, &a.id, "user", "x", "done", None).unwrap();
        db.with(|c| c.execute_batch(&format!("UPDATE chats SET handoff = NULL; DELETE FROM handoffs; {BACKFILL}; {BACKFILL_REQUESTS}"))).unwrap();

        let mut got: Vec<(String, String, Status)> = list(&db, LIMIT).unwrap().into_iter().map(|h| (h.what, h.kind, h.status)).collect();
        got.sort_by(|x, y| x.0.cmp(&y.0));
        let want = [("m1", "message", Status::Done), ("q1", "ask", Status::Done), ("q2", "ask", Status::Failed), ("q3", "ask", Status::Failed)];
        assert_eq!(got, want.map(|(what, kind, status)| (what.to_string(), kind.to_string(), status)), "helpers' chats aren't handoffs");
    }

    #[test]
    fn trims_long_requests() {
        let long = "word ".repeat(100);
        assert!(trimmed(&long).ends_with('…'));
        assert!(trimmed(&long).chars().count() <= WHAT_CHARS + 1);
    }
}
