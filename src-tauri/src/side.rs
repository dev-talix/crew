//! Side conversations: familiars talking to each other out of sight. Each one is a hidden chat of the
//! familiar being asked, marked with the familiar who asked (`chats.side_of`), so neither main chat
//! fills up. Its turns run on their own CLI session, forked from the asked familiar's current chat:
//! the answer draws on what that familiar knows without changing its main conversation.

use crate::db::{now_ms, Agent, Db, Message};
use crate::runtime::Sink;
use crate::AppState;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Joins a familiar's id and a side chat's id into the key its side turns run under. The ids are
/// UUIDs, which never contain it.
const MARK: char = '~';

/// The role of the row in the asking familiar's chat that links to a side conversation. Its source
/// holds the side chat's id.
pub const LINK_ROLE: &str = "side";

/// Schema version 14: side conversations are chats with `side_of` set to the familiar who asked.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         ALTER TABLE chats ADD COLUMN side_of TEXT REFERENCES agents(id) ON DELETE CASCADE;
         PRAGMA user_version = 14;
         COMMIT;",
    )
}

/// The key a side conversation's turns run under in the runtime and the providers.
pub fn lane_key(agent_id: &str, chat_id: &str) -> String {
    format!("{agent_id}{MARK}{chat_id}")
}

/// Splits a lane key, or a tool caller id, into the familiar and the side chat it's working in.
pub fn split(key: &str) -> (&str, Option<&str>) {
    match key.split_once(MARK) {
        Some((agent, chat)) if !chat.is_empty() => (agent, Some(chat)),
        Some((agent, _)) => (agent, None),
        None => (key, None),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SideChat {
    pub id: String,
    /// The familiar who started it.
    pub asker_id: String,
    /// The familiar who answers, whose hidden chat this is.
    pub target_id: String,
    pub title: String,
    pub session_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn row_to_side(r: &rusqlite::Row) -> rusqlite::Result<SideChat> {
    Ok(SideChat {
        id: r.get(0)?,
        target_id: r.get(1)?,
        asker_id: r.get(2)?,
        title: r.get(3)?,
        session_id: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

pub fn get(db: &Db, chat_id: &str) -> Result<Option<SideChat>, String> {
    db.with(|c| {
        c.query_row(
            "SELECT id, agent_id, side_of, title, session_id, created_at, updated_at FROM chats WHERE id = ?1 AND side_of IS NOT NULL",
            params![chat_id],
            row_to_side,
        )
        .optional()
    })
}

/// Starts a side conversation from `asker` to `target`, titled from its first message.
pub fn create(db: &Db, asker_id: &str, target_id: &str, first: &str) -> Result<SideChat, String> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "INSERT INTO chats (id, agent_id, title, session_id, created_at, updated_at, side_of) VALUES (?1, ?2, ?3, NULL, ?4, ?4, ?5)",
            params![id, target_id, crate::chats::title_from(first), now, asker_id],
        )
    })?;
    get(db, &id)?.ok_or_else(|| "The side conversation wasn't saved.".into())
}

/// The conversation `thread` names, when it runs from `asker` to `target`.
pub fn follow_up(db: &Db, thread: &str, asker: &Agent, target: &Agent) -> Result<SideChat, String> {
    let chat = get(db, thread.trim())?.ok_or("No conversation has that id. Leave thread out to start a new one.")?;
    if chat.asker_id != asker.id || chat.target_id != target.id {
        return Err(format!(
            "That conversation isn't one you started with {}. Leave thread out to start a new one.",
            target.name
        ));
    }
    Ok(chat)
}

/// The lane keys of every side conversation `agent_id` asks in or answers in.
pub fn lanes_of(db: &Db, agent_id: &str) -> Result<Vec<String>, String> {
    db.with(|c| {
        let mut stmt = c.prepare("SELECT agent_id, id FROM chats WHERE side_of IS NOT NULL AND (agent_id = ?1 OR side_of = ?1)")?;
        let rows = stmt.query_map(params![agent_id], |r| Ok(lane_key(&r.get::<_, String>(0)?, &r.get::<_, String>(1)?)))?;
        rows.collect()
    })
}

/// Every message in a side conversation, oldest first.
pub fn messages(db: &Db, chat_id: &str) -> Result<Vec<Message>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT id, agent_id, role, text, status, created_at, source, updated_at FROM messages WHERE chat_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![chat_id], |r| {
            Ok(Message {
                id: r.get(0)?,
                agent_id: r.get(1)?,
                role: r.get(2)?,
                text: r.get(3)?,
                status: r.get(4)?,
                created_at: r.get(5)?,
                source: r.get(6)?,
                updated_at: r.get(7)?,
                attachments: Vec::new(),
            })
        })?;
        rows.collect()
    })
}

/// What the asked familiar's CLI sees: who wrote, and that the reply goes back to them.
pub fn framed(asker: &str, message: &str) -> String {
    format!(
        "{asker} is another of the person's familiars in Grimoire, working for the same person you do. It sent you this \
         in a side conversation. Help it as you would help the person: your reply goes back to {asker}, and the person \
         can read the conversation.\n\n{message}"
    )
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SideMessage<'a> {
    chat_id: &'a str,
    message: &'a Message,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SideDelta<'a> {
    chat_id: &'a str,
    message_id: i64,
    chunk: &'a str,
}

/// Streams a side conversation's turn to windows that have it open, apart from the main chats.
pub struct SideSink {
    pub app: AppHandle,
    pub chat_id: String,
}

impl Sink for SideSink {
    fn message(&self, message: &Message) {
        let _ = self.app.emit("side_message", SideMessage { chat_id: &self.chat_id, message });
    }

    fn delta(&self, _agent_id: &str, message_id: i64, chunk: &str) {
        let _ = self.app.emit("side_delta", SideDelta { chat_id: &self.chat_id, message_id, chunk });
    }

    fn model(&self, agent_id: &str, model: &str) {
        self.app.model(agent_id, model);
    }
}

/// Adds the row that links `agent`'s chat to a side conversation: in its main chat, or in the side
/// conversation it's answering from when `lane` names one.
pub fn link(app: &AppHandle, db: &Db, lane: Option<&str>, agent_id: &str, label: &str, chat_id: &str) -> Result<(), String> {
    match lane {
        Some(lane) => {
            let message = db.insert_message_in(lane, agent_id, LINK_ROLE, label, "done", Some(chat_id))?;
            SideSink { app: app.clone(), chat_id: lane.to_string() }.message(&message);
        }
        None => {
            let message = db.insert_message_from(agent_id, LINK_ROLE, label, "done", Some(chat_id))?;
            app.message(&message);
        }
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideThread {
    pub chat: SideChat,
    pub messages: Vec<Message>,
}

#[tauri::command]
pub fn side_thread(state: State<AppState>, chat_id: String) -> Result<SideThread, String> {
    let chat = get(&state.db, &chat_id)?.ok_or("This conversation was deleted.")?;
    let messages = messages(&state.db, &chat_id)?;
    Ok(SideThread { chat, messages })
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
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        agent
    }

    #[test]
    fn splits_lane_keys_and_plain_ids() {
        assert_eq!(split(&lane_key("a1", "c9")), ("a1", Some("c9")));
        assert_eq!(split("a1"), ("a1", None));
        assert_eq!(split("a1~"), ("a1", None));
    }

    #[test]
    fn side_chats_stay_out_of_the_main_chats() {
        let db = Db::open_in_memory().unwrap();
        let (a, b) = (familiar(&db, "a", "Researcher"), familiar(&db, "b", "Scribe"));
        db.insert_message("b", "user", "Main question", "done").unwrap();
        let side = create(&db, &a.id, &b.id, "What do you know about otters?").unwrap();
        assert_eq!((side.asker_id.as_str(), side.target_id.as_str()), ("a", "b"));
        db.insert_message_in(&side.id, "b", "user", "What do you know about otters?", "done", Some("From Researcher")).unwrap();
        db.insert_message_in(&side.id, "b", "assistant", "They hold hands asleep.", "done", None).unwrap();
        db.set_chat_session(&side.id, "s-side").unwrap();

        // The main chat, the chat list, search and the sidebar preview never show it.
        let main: Vec<String> = db.list_messages("b", None, 10).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(main, ["Main question"]);
        assert_eq!(crate::chats::list(&db, "b").unwrap().len(), 1);
        assert!(crate::chats::search(&db, "otters", 10).unwrap().is_empty());
        assert_eq!(db.last_messages().unwrap()[0].text, "Main question");
        assert_eq!(db.get_agent("b").unwrap().unwrap().session_id, None, "the side session stays on its chat");

        let texts: Vec<String> = messages(&db, &side.id).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(texts, ["What do you know about otters?", "They hold hands asleep."]);
        assert_eq!(get(&db, &side.id).unwrap().unwrap().session_id.as_deref(), Some("s-side"));
        // It can't be opened as a main chat, and both familiars' side lanes find it.
        assert!(crate::chats::open(&db, "b", &side.id).is_err());
        let lane = lane_key("b", &side.id);
        assert_eq!(lanes_of(&db, "a").unwrap(), lanes_of(&db, "b").unwrap());
        assert_eq!(lanes_of(&db, "a").unwrap(), [lane]);
        // Deleting the main chat picks another main chat, never the side one.
        let current = crate::chats::list(&db, "b").unwrap().remove(0);
        crate::chats::delete(&db, &current.id).unwrap();
        let left = crate::chats::list(&db, "b").unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].current && left[0].id != side.id);
    }

    #[test]
    fn follow_ups_must_be_between_the_same_two() {
        let db = Db::open_in_memory().unwrap();
        let (a, b, c) = (familiar(&db, "a", "Researcher"), familiar(&db, "b", "Scribe"), familiar(&db, "c", "Critic"));
        let side = create(&db, &a.id, &b.id, "hi").unwrap();
        assert_eq!(follow_up(&db, &side.id, &a, &b).unwrap().id, side.id);
        assert!(follow_up(&db, &side.id, &a, &c).unwrap_err().contains("isn't one you started with Critic"));
        assert!(follow_up(&db, &side.id, &b, &a).is_err(), "the other direction is a different conversation");
        assert!(follow_up(&db, "nope", &a, &b).unwrap_err().contains("No conversation"));
    }

    #[test]
    fn frames_the_message_with_who_sent_it() {
        let text = framed("Researcher", "Summarize the notes");
        assert!(text.starts_with("Researcher is another of the person's familiars"));
        assert!(text.ends_with("\n\nSummarize the notes"));
    }
}
