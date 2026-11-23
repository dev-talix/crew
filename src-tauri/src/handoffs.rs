//! The handoffs list: who handed what to whom with `ask_familiar` or `message_familiar`, and where
//! each one stands. A handoff is a side conversation with `chats.handoff` set to how it was sent;
//! helpers' side chats never get one.

use crate::db::Db;
use crate::AppState;
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::State;

/// Tells windows to reload the list when something changed it without a side message.
pub const CHANGED: &str = "handoffs_changed";
/// The most handoffs the list shows.
pub const LIMIT: usize = 100;
/// How much of the first message the list shows.
const WHAT_CHARS: usize = 280;

/// Schema version 23: side conversations remember whether they came from `ask_familiar` ("ask") or
/// `message_familiar` ("message"). Older ones are filled in from the link row in the sender's chat.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!("BEGIN; ALTER TABLE chats ADD COLUMN handoff TEXT; {BACKFILL}; PRAGMA user_version = 23; COMMIT;"))
}

const BACKFILL: &str = "UPDATE chats SET handoff = CASE WHEN (
      SELECT text FROM messages WHERE role = 'side' AND source = chats.id ORDER BY id DESC LIMIT 1
    ) LIKE 'Messaged %' THEN 'message' ELSE 'ask' END
  WHERE side_of IS NOT NULL AND id NOT IN (SELECT chat_id FROM helpers)";

/// Records how the latest exchange in a side conversation was sent: "ask" or "message".
pub fn mark(db: &Db, chat_id: &str, kind: &str) -> Result<(), String> {
    db.with(|c| c.execute("UPDATE chats SET handoff = ?2 WHERE id = ?1", params![chat_id, kind]).map(|_| ()))
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Sent, and the other familiar hasn't started on it.
    Waiting,
    /// The other familiar is working on it.
    Accepted,
    /// The other familiar replied.
    Done,
    /// An error, a denial, or a reply cut off.
    Failed,
}

/// Where a handoff stands, from the last row in its conversation (role and status) and whether a
/// turn runs in it right now. Follow-ups add rows, so this is always the latest exchange. A request
/// with nothing after it is waiting even while its turn starts: the other familiar hasn't written
/// or used a tool yet.
pub fn status(last: Option<(&str, &str)>, running: bool) -> Status {
    match last {
        Some(("error", _)) | Some(("assistant", "interrupted" | "stopped" | "cancelled")) => Status::Failed,
        None | Some(("user", _)) => Status::Waiting,
        Some(("assistant", "streaming")) => Status::Accepted,
        // A tool row or a reply segment with the turn still going.
        _ if running => Status::Accepted,
        // A reply, or a turn that ended after a tool without writing one.
        Some(_) => Status::Done,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Handoff {
    /// The side conversation's id.
    pub id: String,
    pub from_id: String,
    pub to_id: String,
    /// "ask" or "message".
    pub kind: String,
    /// The first message, trimmed.
    pub what: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub status: Status,
}

fn trimmed(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(WHAT_CHARS) {
        Some((at, _)) => format!("{}…", text[..at].trim_end()),
        None => text.to_string(),
    }
}

/// The latest `limit` handoffs, newest first. `running` says whether a turn holds a side lane.
pub fn list(db: &Db, limit: usize, running: impl Fn(&str) -> bool) -> Result<Vec<Handoff>, String> {
    type Row = (Handoff, Option<String>, Option<String>);
    let rows: Vec<Row> = db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT c.id, c.side_of, c.agent_id, c.handoff, c.created_at,
                    COALESCE((SELECT text FROM messages WHERE chat_id = c.id AND role = 'user' ORDER BY id LIMIT 1), c.title),
                    m.role, m.status, MAX(c.updated_at, COALESCE(m.updated_at, m.created_at, 0))
             FROM chats c LEFT JOIN messages m ON m.id = (SELECT MAX(id) FROM messages WHERE chat_id = c.id)
             WHERE c.handoff IS NOT NULL AND c.side_of IS NOT NULL AND c.id NOT IN (SELECT chat_id FROM helpers)
             ORDER BY c.created_at DESC, c.rowid DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((
                Handoff {
                    id: r.get(0)?,
                    from_id: r.get(1)?,
                    to_id: r.get(2)?,
                    kind: r.get(3)?,
                    created_at: r.get(4)?,
                    what: trimmed(&r.get::<_, String>(5)?),
                    updated_at: r.get(8)?,
                    status: Status::Waiting,
                },
                r.get(6)?,
                r.get(7)?,
            ))
        })?;
        rows.collect()
    })?;
    Ok(rows
        .into_iter()
        .map(|(mut handoff, role, state)| {
            let last = role.as_deref().zip(state.as_deref());
            handoff.status = status(last, running(&crate::side::lane_key(&handoff.to_id, &handoff.id)));
            handoff
        })
        .collect())
}

#[tauri::command]
pub fn list_handoffs(state: State<AppState>) -> Result<Vec<Handoff>, String> {
    list(&state.db, LIMIT, |key| state.runtime.side_running(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{now_ms, Access, Agent, Avatar, Provider};

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

    #[test]
    fn derives_status_from_the_last_row_and_the_lane() {
        assert_eq!(status(None, false), Status::Waiting);
        assert_eq!(status(Some(("user", "done")), false), Status::Waiting);
        assert_eq!(status(Some(("user", "done")), true), Status::Waiting, "the turn hasn't produced anything yet");
        assert_eq!(status(Some(("assistant", "streaming")), false), Status::Accepted);
        assert_eq!(status(Some(("tool", "done")), true), Status::Accepted);
        assert_eq!(status(Some(("assistant", "done")), true), Status::Accepted, "a segment ends before a tool call");
        assert_eq!(status(Some(("assistant", "done")), false), Status::Done);
        assert_eq!(status(Some(("error", "done")), true), Status::Failed);
        assert_eq!(status(Some(("assistant", "interrupted")), false), Status::Failed);
        assert_eq!(status(Some(("assistant", "stopped")), false), Status::Failed);
    }

    #[test]
    fn lists_handoffs_newest_first_without_helpers() {
        let db = Db::open_in_memory().unwrap();
        let (a, b) = (familiar(&db, "a"), familiar(&db, "b"));
        let handoff = |kind: &str, first: &str, rows: &[(&str, &str)]| {
            let chat = crate::side::create(&db, &a.id, &b.id, first).unwrap();
            mark(&db, &chat.id, kind).unwrap();
            for (role, state) in rows {
                db.insert_message_in(&chat.id, &b.id, role, first, state, Some("From A")).unwrap();
            }
            chat.id
        };
        let waiting = handoff("ask", "  Check the draft  ", &[("user", "done")]);
        let done = handoff("message", "Summarize", &[("user", "done"), ("assistant", "done")]);
        let failed = handoff("ask", "Deploy", &[("user", "done"), ("error", "done")]);
        let busy = handoff("message", "Research otters", &[("user", "done"), ("tool", "done")]);
        // A follow-up that failed after an earlier reply shows the latest exchange.
        let followed = handoff("ask", "First question", &[("user", "done"), ("assistant", "done"), ("user", "done"), ("assistant", "interrupted")]);
        // A helper's chat is a side chat too, but nobody handed it anything.
        let (_, helper) = crate::helpers::create(&db, &a.id, None, &serde_json::json!({ "task": "x" })).unwrap();
        db.insert_message_in(&helper.chat_id, &a.id, "assistant", "x", "done", None).unwrap();

        let busy_lane = crate::side::lane_key(&b.id, &busy);
        let list = list(&db, LIMIT, |key| key == busy_lane).unwrap();
        let got: Vec<(&str, &str, Status)> = list.iter().map(|h| (h.id.as_str(), h.kind.as_str(), h.status)).collect();
        assert_eq!(
            got,
            [
                (followed.as_str(), "ask", Status::Failed),
                (busy.as_str(), "message", Status::Accepted),
                (failed.as_str(), "ask", Status::Failed),
                (done.as_str(), "message", Status::Done),
                (waiting.as_str(), "ask", Status::Waiting),
            ]
        );
        let first = list.last().unwrap();
        assert_eq!((first.from_id.as_str(), first.to_id.as_str(), first.what.as_str()), ("a", "b", "Check the draft"));
        assert!(list.iter().all(|h| h.updated_at >= h.created_at));
        assert_eq!(super::list(&db, 2, |_| false).unwrap().len(), 2);
    }

    #[test]
    fn fills_in_the_kind_of_older_handoffs() {
        let db = Db::open_in_memory().unwrap();
        let (a, b) = (familiar(&db, "a"), familiar(&db, "b"));
        let asked = crate::side::create(&db, &a.id, &b.id, "q").unwrap();
        let messaged = crate::side::create(&db, &a.id, &b.id, "m").unwrap();
        db.insert_message_from(&a.id, crate::side::LINK_ROLE, "Asked B", "done", Some(&asked.id)).unwrap();
        db.insert_message_from(&a.id, crate::side::LINK_ROLE, "Messaged B", "done", Some(&messaged.id)).unwrap();
        let (_, helper) = crate::helpers::create(&db, &a.id, None, &serde_json::json!({ "task": "x" })).unwrap();
        let kinds = || -> Vec<(String, Option<String>)> {
            db.with(|c| c.prepare("SELECT id, handoff FROM chats WHERE side_of IS NOT NULL")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect())
                .unwrap()
        };
        assert!(kinds().iter().all(|(_, kind)| kind.is_none()));
        db.with(|c| c.execute_batch(BACKFILL)).unwrap();
        let kind = |id: &str| kinds().into_iter().find(|(chat, _)| chat == id).unwrap().1;
        assert_eq!(kind(&asked.id).as_deref(), Some("ask"));
        assert_eq!(kind(&messaged.id).as_deref(), Some("message"));
        assert_eq!(kind(&helper.chat_id), None, "helpers' chats aren't handoffs");
    }

    #[test]
    fn trims_long_requests() {
        let long = "word ".repeat(100);
        assert!(trimmed(&long).ends_with('…'));
        assert!(trimmed(&long).chars().count() <= WHAT_CHARS + 1);
    }
}
