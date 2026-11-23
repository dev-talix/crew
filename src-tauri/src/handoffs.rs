//! The handoffs list: who handed what to whom with `ask_familiar` or `message_familiar`, and where
//! each one stands. Every request gets its own row, so a follow-up in the same side conversation
//! never changes how an earlier request went. Helpers' side chats never get one.

use crate::db::{now_ms, Db};
use crate::runtime::TurnOutcome;
use crate::AppState;
use rusqlite::{params, Connection};
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
    /// The side conversation it was sent in.
    pub chat_id: String,
    pub from_id: String,
    pub to_id: String,
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

/// The latest `limit` requests, newest first.
pub fn list(db: &Db, limit: usize) -> Result<Vec<Handoff>, String> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT h.id, h.chat_id, c.side_of, c.agent_id, h.kind, h.text, h.created_at, h.status
             FROM handoffs h JOIN chats c ON c.id = h.chat_id
             ORDER BY h.created_at DESC, h.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(Handoff {
                id: r.get(0)?,
                chat_id: r.get(1)?,
                from_id: r.get(2)?,
                to_id: r.get(3)?,
                kind: r.get(4)?,
                what: trimmed(&r.get::<_, String>(5)?),
                created_at: r.get(6)?,
                status: Status::parse(&r.get::<_, String>(7)?),
            })
        })?;
        rows.collect()
    })
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
