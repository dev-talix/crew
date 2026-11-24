//! Rooms: one thread where the person and several familiars talk. A message goes to the familiars it
//! mentions as `@Name`, or to whoever spoke last. Familiars take the floor one at a time, and a reply
//! that mentions another member hands the floor to it, with a row in the room saying so. Each member
//! answers on its own CLI session for the room, and sees what was said since its last turn.
//!
//! Members and mentions are always read from the database: the person picks familiars only when
//! the room is made, and every id is checked then.

use crate::db::{now_ms, Agent, Db};
use crate::providers::{EventTx, Lane, LaneSession, ProviderEvent};
use crate::runtime::{Runtime, TurnOutcome};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

/// Marks a room's lane key, after the familiar's id and `side::lane_key`'s separator.
const LANE: &str = "room:";
/// The fewest and most familiars in a room.
const MIN_MEMBERS: usize = 2;
const MAX_MEMBERS: usize = 8;
/// Replies in a row, with no message from the person, before the room pauses.
pub const MAX_RUN: usize = 6;
const MAX_TITLE: usize = 80;
const MAX_TEXT: usize = 20_000;
/// The most of the room a turn's prompt carries; older messages are left out.
const MAX_PROMPT: usize = 24_000;
/// How many recent messages a room opens with.
const HISTORY: i64 = 500;
const UI_FLUSH: Duration = Duration::from_millis(50);
const DB_FLUSH: Duration = Duration::from_millis(750);

pub const CHANGED: &str = "rooms_changed";
const MESSAGE: &str = "room_message";
const DELTA: &str = "room_delta";
const FLOOR: &str = "room_floor";

/// Schema version 29: rooms, their members, and their messages.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE rooms (
           id TEXT PRIMARY KEY,
           title TEXT NOT NULL,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );
         CREATE TABLE room_members (
           room_id TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
           agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
           position INTEGER NOT NULL,
           session_id TEXT,
           seen INTEGER NOT NULL DEFAULT 0,
           speaking INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY (room_id, agent_id)
         );
         CREATE TABLE room_messages (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           room_id TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
           agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
           name TEXT,
           kind TEXT NOT NULL,
           text TEXT NOT NULL,
           status TEXT NOT NULL,
           target_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
           target_name TEXT,
           prompt TEXT,
           created_at INTEGER NOT NULL
         );
         CREATE INDEX room_messages_by_room ON room_messages(room_id, id);
         PRAGMA user_version = 29;
         COMMIT;",
    )
}

/// Closes turns cut off when Grimoire last quit: a reply mid-stream shows as interrupted, and a
/// turn cut off anywhere else, like while a tool ran, gets a note saying so. Returns how many turns.
pub fn recover(db: &Db) -> Result<usize, String> {
    // A turn cut off mid-reply already says so on the reply, so only the others get a note.
    let cut: Vec<(String, String, String)> = db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT m.room_id, m.agent_id, a.name FROM room_members m JOIN agents a ON a.id = m.agent_id WHERE m.speaking = 1
             AND NOT EXISTS (SELECT 1 FROM room_messages r WHERE r.room_id = m.room_id AND r.agent_id = m.agent_id AND r.status = 'streaming')",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    })?;
    for (room_id, agent_id, name) in &cut {
        let member = Member { agent_id: agent_id.clone(), name: name.clone() };
        let text = format!("{name}'s turn was cut off when Grimoire closed.");
        insert(db, room_id, Draft { agent: Some(&member), kind: kind::NOTE, text: &text, status: "done", ..Draft::default() })?;
    }
    db.with(|c| c.execute("UPDATE room_members SET speaking = 0 WHERE speaking = 1", []))?;
    let replies = db.with(|c| c.execute("UPDATE room_messages SET status = 'interrupted' WHERE status = 'streaming'", []))?;
    Ok(cut.len() + replies)
}

fn set_speaking(db: &Db, room_id: &str, agent_id: &str, speaking: bool) -> Result<(), String> {
    db.with(|c| {
        c.execute("UPDATE room_members SET speaking = ?3 WHERE room_id = ?1 AND agent_id = ?2", params![room_id, agent_id, speaking])
    })
    .map(|_| ())
}

/// What a row in a room is.
pub mod kind {
    /// From the person.
    pub const USER: &str = "user";
    /// A familiar's reply.
    pub const REPLY: &str = "reply";
    /// A tool a familiar used while replying.
    pub const TOOL: &str = "tool";
    /// A familiar handing the floor to another one.
    pub const PASS: &str = "pass";
    /// A note from Grimoire, like a pause.
    pub const NOTE: &str = "note";
    pub const ERROR: &str = "error";
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub agent_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Room {
    pub id: String,
    pub title: String,
    pub members: Vec<Member>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoomMessage {
    pub id: i64,
    pub room_id: String,
    /// The familiar who wrote it; none for the person, or once that familiar is deleted.
    pub agent_id: Option<String>,
    /// The familiar's name when it wrote, so history reads the same after a rename or delete.
    pub name: Option<String>,
    pub kind: String,
    pub text: String,
    pub status: String,
    /// Who a pass handed the floor to.
    pub target_id: Option<String>,
    pub target_name: Option<String>,
    /// Whether `room_prompt` can show what the familiar was sent for this reply.
    pub has_prompt: bool,
    pub created_at: i64,
}

const MESSAGE_COLUMNS: &str =
    "id, room_id, agent_id, name, kind, text, status, target_id, target_name, prompt IS NOT NULL, created_at";

fn row_to_message(r: &rusqlite::Row) -> rusqlite::Result<RoomMessage> {
    Ok(RoomMessage {
        id: r.get(0)?,
        room_id: r.get(1)?,
        agent_id: r.get(2)?,
        name: r.get(3)?,
        kind: r.get(4)?,
        text: r.get(5)?,
        status: r.get(6)?,
        target_id: r.get(7)?,
        target_name: r.get(8)?,
        has_prompt: r.get(9)?,
        created_at: r.get(10)?,
    })
}

/// A new row, before it's saved.
#[derive(Default)]
struct Draft<'a> {
    agent: Option<&'a Member>,
    kind: &'a str,
    text: &'a str,
    status: &'a str,
    target: Option<&'a Member>,
    prompt: Option<&'a str>,
}

fn insert(db: &Db, room_id: &str, d: Draft) -> Result<RoomMessage, String> {
    let now = now_ms();
    let id = db.with(|c| {
        c.execute(
            "INSERT INTO room_messages (room_id, agent_id, name, kind, text, status, target_id, target_name, prompt, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                room_id,
                d.agent.map(|m| &m.agent_id),
                d.agent.map(|m| &m.name),
                d.kind,
                d.text,
                d.status,
                d.target.map(|m| &m.agent_id),
                d.target.map(|m| &m.name),
                d.prompt,
                now
            ],
        )?;
        c.execute("UPDATE rooms SET updated_at = ?2 WHERE id = ?1", params![room_id, now])?;
        Ok(c.last_insert_rowid())
    })?;
    message(db, id)?.ok_or_else(|| "The message wasn't saved.".into())
}

fn message(db: &Db, id: i64) -> Result<Option<RoomMessage>, String> {
    db.with(|c| c.query_row(&format!("SELECT {MESSAGE_COLUMNS} FROM room_messages WHERE id = ?1"), params![id], row_to_message).optional())
}

fn update(db: &Db, id: i64, text: &str, status: &str) -> Result<(), String> {
    db.with(|c| c.execute("UPDATE room_messages SET text = ?2, status = ?3 WHERE id = ?1", params![id, text, status])).map(|_| ())
}

/// The room's members, in the order they were picked, with their current names.
pub fn members(db: &Db, room_id: &str) -> Result<Vec<Member>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT a.id, a.name FROM room_members m JOIN agents a ON a.id = m.agent_id WHERE m.room_id = ?1 ORDER BY m.position",
        )?;
        let rows = stmt.query_map(params![room_id], |r| Ok(Member { agent_id: r.get(0)?, name: r.get(1)? }))?;
        rows.collect()
    })
}

pub fn get(db: &Db, room_id: &str) -> Result<Option<Room>, String> {
    let room = db.with(|c| {
        c.query_row("SELECT id, title, created_at, updated_at FROM rooms WHERE id = ?1", params![room_id], |r| {
            Ok(Room { id: r.get(0)?, title: r.get(1)?, members: Vec::new(), created_at: r.get(2)?, updated_at: r.get(3)? })
        })
        .optional()
    })?;
    match room {
        Some(room) => Ok(Some(Room { members: members(db, &room.id)?, ..room })),
        None => Ok(None),
    }
}

/// Every room, the most recently active first.
pub fn list(db: &Db) -> Result<Vec<Room>, String> {
    let ids: Vec<String> = db.with(|c| {
        let mut stmt = c.prepare("SELECT id FROM rooms ORDER BY updated_at DESC, id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect()
    })?;
    ids.iter().filter_map(|id| get(db, id).transpose()).collect()
}

/// Makes a room with familiars picked by the person. Each id has to name a familiar on this Mac.
pub fn create(db: &Db, title: &str, agent_ids: &[String]) -> Result<Room, String> {
    let mut picked: Vec<Agent> = Vec::new();
    for id in agent_ids {
        let agent = db.get_agent(id)?.ok_or("One of those familiars no longer exists.")?;
        if !picked.iter().any(|a| a.id == agent.id) {
            picked.push(agent);
        }
    }
    if picked.len() < MIN_MEMBERS {
        return Err(format!("Pick at least {MIN_MEMBERS} familiars for a room."));
    }
    if picked.len() > MAX_MEMBERS {
        return Err(format!("A room holds at most {MAX_MEMBERS} familiars."));
    }
    for (i, a) in picked.iter().enumerate() {
        if picked[..i].iter().any(|b| b.name.to_lowercase() == a.name.to_lowercase()) {
            return Err(format!("Two of those familiars are called {}. Rename one so a mention can tell them apart.", a.name));
        }
    }
    let title = title.trim();
    let title = if title.is_empty() {
        format!("{} and {}", picked[..picked.len() - 1].iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "), picked[picked.len() - 1].name)
    } else {
        title.to_string()
    };
    if title.chars().count() > MAX_TITLE {
        return Err(format!("Keep the room's name under {MAX_TITLE} characters."));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    // One transaction, so a familiar deleted meanwhile leaves no half-made room.
    db.with(|c| {
        let tx = c.unchecked_transaction()?;
        tx.execute("INSERT INTO rooms (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)", params![id, title, now])?;
        for (position, agent) in picked.iter().enumerate() {
            tx.execute(
                "INSERT INTO room_members (room_id, agent_id, position) VALUES (?1, ?2, ?3)",
                params![id, agent.id, position as i64],
            )?;
        }
        tx.commit()
    })
    .map_err(|_| "One of those familiars no longer exists.".to_string())?;
    get(db, &id)?.ok_or_else(|| "The room wasn't saved.".into())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub messages: Vec<RoomMessage>,
    /// Whether earlier messages are left to load.
    pub has_older: bool,
}

/// A page of the room's messages before `before`, or its latest, oldest first.
pub fn history(db: &Db, room_id: &str, before: Option<i64>) -> Result<Page, String> {
    let mut messages: Vec<RoomMessage> = db.with(|c| {
        let mut stmt = c.prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM room_messages WHERE room_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![room_id, before.unwrap_or(i64::MAX), HISTORY + 1], row_to_message)?;
        rows.collect()
    })?;
    let has_older = messages.len() as i64 > HISTORY;
    messages.truncate(HISTORY as usize);
    messages.reverse();
    Ok(Page { messages, has_older })
}

/// The lane key a member's turns in a room run under. Tools see the room as the familiar's lane.
pub fn lane_key(agent_id: &str, room_id: &str) -> String {
    crate::side::lane_key(agent_id, &format!("{LANE}{room_id}"))
}

/// Refuses tools that would start another conversation from inside a room.
pub fn check_lane(lane: Option<&str>, tool: &str) -> Result<(), String> {
    match lane {
        Some(lane) if lane.starts_with(LANE) && ["ask_familiar", "message_familiar", "start_helper"].contains(&tool) => {
            Err("You're in a room. To bring in another familiar in the room, mention them as @Name in your reply.".into())
        }
        _ => Ok(()),
    }
}

/// The members `text` mentions as `@Name`, in the order they first appear. Names match whole and
/// ignore case; the longest name wins, so "@Ink Well" isn't read as "@Ink". An `@` inside a word,
/// like an email address, isn't a mention.
pub fn mentions(text: &str, members: &[Member]) -> Vec<String> {
    let mut by_length: Vec<&Member> = members.iter().collect();
    by_length.sort_by_key(|m| std::cmp::Reverse(m.name.chars().count()));
    let mut found: Vec<String> = Vec::new();
    let mut previous: Option<char> = None;
    // Where the last mention's name ends; nothing inside a matched name starts another mention.
    let mut skip_to = 0;
    for (at, ch) in text.char_indices() {
        if at >= skip_to && ch == '@' && !previous.is_some_and(is_word) {
            let rest = &text[at + 1..];
            if let Some(member) = by_length.iter().find(|m| names_at(rest, &m.name)) {
                skip_to = at + 1 + rest.chars().take(member.name.chars().count()).map(char::len_utf8).sum::<usize>();
                if !found.contains(&member.agent_id) {
                    found.push(member.agent_id.clone());
                }
            }
        }
        previous = Some(ch);
    }
    found
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Whether `rest` starts with `name`, ignoring case, followed by something that ends a word.
fn names_at(rest: &str, name: &str) -> bool {
    let mut chars = rest.chars();
    for want in name.chars() {
        match chars.next() {
            Some(got) if got.to_lowercase().eq(want.to_lowercase()) => {}
            _ => return false,
        }
    }
    !name.is_empty() && !chars.next().is_some_and(is_word)
}

/// What a member is sent for its turn: who's in the room and how it works, then what was said
/// since it last spoke, or the whole room on its first turn. The introduction comes every time, so
/// a familiar whose room session was lost still knows where it is. Returns the prompt and the last
/// message id it covers.
pub fn prompt_for(db: &Db, room: &Room, member: &Member, first: bool) -> Result<(String, i64), String> {
    let seen: i64 = db.with(|c| {
        c.query_row(
            "SELECT seen FROM room_members WHERE room_id = ?1 AND agent_id = ?2",
            params![room.id, member.agent_id],
            |r| r.get(0),
        )
    })?;
    let rows: Vec<RoomMessage> = db.with(|c| {
        let mut stmt = c.prepare_cached(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM room_messages WHERE room_id = ?1 AND id > ?2 AND kind IN ('user', 'reply', 'pass')
             AND status != 'streaming' ORDER BY id"
        ))?;
        let rows = stmt.query_map(params![room.id, seen], row_to_message)?;
        rows.collect()
    })?;
    let last = rows.last().map_or(seen, |r| r.id);
    let mut lines: Vec<String> = rows
        .iter()
        .filter(|r| r.agent_id.as_deref() != Some(&member.agent_id) || r.kind == kind::PASS)
        .filter_map(|r| {
            let who = r.name.as_deref().unwrap_or("A familiar who left");
            match r.kind.as_str() {
                kind::USER => Some(format!("The person: {}", r.text)),
                kind::PASS => Some(format!("({who} handed off to {}.)", r.target_name.as_deref().unwrap_or("a familiar who left"))),
                _ if r.text.trim().is_empty() => None,
                _ => Some(format!("{who}: {}", r.text)),
            }
        })
        .collect();
    let mut size: usize = lines.iter().map(|l| l.len() + 2).sum();
    let mut trimmed = false;
    while size > MAX_PROMPT && lines.len() > 1 {
        size -= lines.remove(0).len() + 2;
        trimmed = true;
    }
    let mut prompt = String::new();
    {
        let others: Vec<&str> = room.members.iter().filter(|m| m.agent_id != member.agent_id).map(|m| m.name.as_str()).collect();
        prompt.push_str(&format!(
            "You're {name}, one of the person's familiars in Grimoire. You're in a group room called \"{title}\" with the \
             person and these familiars: {others}. Everyone in the room reads every message. Speak only for yourself and keep \
             replies short. One familiar has the floor at a time. To hand the floor to another familiar in the room, mention \
             them as @Name in your reply and they answer after you. Write @ before a name only when you want that familiar \
             to answer next.\n\n",
            name = member.name,
            title = room.title,
            others = others.join(", "),
        ));
    }
    prompt.push_str(if first { "The room so far:\n\n" } else { "New in the room since your last turn:\n\n" });
    if trimmed {
        prompt.push_str("(Earlier messages are left out.)\n\n");
    }
    prompt.push_str(&lines.join("\n\n"));
    prompt.push_str("\n\nIt's your turn.");
    Ok((prompt, last))
}

fn set_seen(db: &Db, room_id: &str, agent_id: &str, seen: i64) -> Result<(), String> {
    db.with(|c| c.execute("UPDATE room_members SET seen = ?3 WHERE room_id = ?1 AND agent_id = ?2", params![room_id, agent_id, seen]))
        .map(|_| ())
}

fn set_session(db: &Db, room_id: &str, agent_id: &str, session: &str) -> Result<(), String> {
    db.with(|c| {
        c.execute("UPDATE room_members SET session_id = ?3 WHERE room_id = ?1 AND agent_id = ?2", params![room_id, agent_id, session])
    })
    .map(|_| ())
}

fn session_of(db: &Db, room_id: &str, agent_id: &str) -> Result<Option<String>, String> {
    db.with(|c| {
        c.query_row("SELECT session_id FROM room_members WHERE room_id = ?1 AND agent_id = ?2", params![room_id, agent_id], |r| r.get(0))
            .optional()
    })
    .map(Option::flatten)
}

/// The prompt a reply was written for.
pub fn prompt_of(db: &Db, message_id: i64) -> Result<Option<String>, String> {
    db.with(|c| c.query_row("SELECT prompt FROM room_messages WHERE id = ?1", params![message_id], |r| r.get(0)).optional())
        .map(Option::flatten)
}

/// Runs familiars' turns. The runtime in the app, a fake in tests.
pub trait Speaker: Clone + Send + Sync + 'static {
    fn start(&self, agent: Agent, lane: Lane, prompt: String, tx: EventTx) -> impl Future<Output = Result<(), String>> + Send + 'static;
    fn stop(&self, key: String) -> impl Future<Output = ()> + Send + 'static;
    fn forget(&self, key: &str);
}

impl Speaker for Arc<Runtime> {
    fn start(&self, agent: Agent, lane: Lane, prompt: String, tx: EventTx) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let runtime = self.clone();
        async move { runtime.start_lane(&agent, &lane, &prompt, tx).await }
    }

    fn stop(&self, key: String) -> impl Future<Output = ()> + Send + 'static {
        let runtime = self.clone();
        async move { runtime.stop_lane(&key).await }
    }

    fn forget(&self, key: &str) {
        self.forget_lane(key);
    }
}

/// Sends an event to the windows. An `AppHandle` in the app, a recorder in tests.
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// Who has the floor in a room and who's waiting for it.
#[derive(Default)]
struct Floor {
    /// Whether a task is giving out turns, so only one ever does.
    running: bool,
    speaking: Option<String>,
    queue: VecDeque<String>,
    /// Familiar replies since the person last wrote.
    run: usize,
    /// Set by a stop during the current turn, so a turn still starting stops too and hands off to nobody.
    stopped: bool,
}

pub struct Rooms<S: Speaker> {
    db: Arc<Db>,
    speaker: S,
    emit: Emit,
    floors: Mutex<HashMap<String, Floor>>,
}

pub type AppRooms = Rooms<Arc<Runtime>>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub room: Room,
    pub messages: Vec<RoomMessage>,
    pub has_older: bool,
    pub speaking: Option<String>,
    pub queue: Vec<String>,
}

impl<S: Speaker> Rooms<S> {
    pub fn new(db: Arc<Db>, speaker: S, emit: Emit) -> Arc<Self> {
        Arc::new(Rooms { db, speaker, emit, floors: Mutex::new(HashMap::new()) })
    }

    fn floors(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Floor>>, String> {
        self.floors.lock().map_err(|_| "room state poisoned".to_string())
    }

    fn show(&self, message: &RoomMessage) {
        (self.emit)(MESSAGE, json!({ "roomId": message.room_id, "message": message }));
    }

    fn show_floor(&self, room_id: &str) {
        let (speaking, queue) = self.floor_of(room_id);
        (self.emit)(FLOOR, json!({ "roomId": room_id, "speaking": speaking, "queue": queue }));
    }

    fn floor_of(&self, room_id: &str) -> (Option<String>, Vec<String>) {
        self.floors()
            .ok()
            .and_then(|f| f.get(room_id).map(|f| (f.speaking.clone(), f.queue.iter().cloned().collect())))
            .unwrap_or_default()
    }

    pub fn thread(&self, room_id: &str) -> Result<Thread, String> {
        let room = get(&self.db, room_id)?.ok_or("This room was deleted.")?;
        let Page { messages, has_older } = history(&self.db, room_id, None)?;
        let (speaking, queue) = self.floor_of(room_id);
        Ok(Thread { room, messages, has_older, speaking, queue })
    }

    /// Posts the person's message and gives the floor to the familiars it mentions, or to whoever
    /// spoke last when it mentions nobody.
    pub fn send(self: &Arc<Self>, room_id: &str, text: &str) -> Result<RoomMessage, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("The message is empty.".into());
        }
        if text.chars().count() > MAX_TEXT {
            return Err(format!("Keep a room message under {MAX_TEXT} characters."));
        }
        let room = get(&self.db, room_id)?.ok_or("This room was deleted.")?;
        if room.members.is_empty() {
            return Err("Every familiar in this room was deleted.".into());
        }
        let message = insert(&self.db, room_id, Draft { kind: kind::USER, text, status: "done", ..Draft::default() })?;
        self.show(&message);
        let mut next = mentions(text, &room.members);
        if next.is_empty() {
            next.push(self.last_speaker(&room)?);
        }
        let start = {
            let mut floors = self.floors()?;
            let floor = floors.entry(room_id.to_string()).or_default();
            floor.run = 0;
            for id in next {
                if !floor.queue.contains(&id) {
                    floor.queue.push_back(id);
                }
            }
            !std::mem::replace(&mut floor.running, true)
        };
        if start {
            let rooms = self.clone();
            let room_id = room_id.to_string();
            tauri::async_runtime::spawn(async move { rooms.take_turns(&room_id).await });
        } else {
            self.show_floor(room_id);
        }
        (self.emit)(CHANGED, Value::Null);
        Ok(message)
    }

    /// The member who replied last, or the first member when nobody has.
    fn last_speaker(&self, room: &Room) -> Result<String, String> {
        let last: Option<String> = self.db.with(|c| {
            c.query_row(
                "SELECT m.agent_id FROM room_messages m JOIN room_members r ON r.room_id = m.room_id AND r.agent_id = m.agent_id
                 WHERE m.room_id = ?1 AND m.kind = 'reply' ORDER BY m.id DESC LIMIT 1",
                params![room.id],
                |r| r.get(0),
            )
            .optional()
        })?;
        Ok(last.unwrap_or_else(|| room.members[0].agent_id.clone()))
    }

    /// Gives the floor to each waiting familiar in turn until nobody's waiting.
    async fn take_turns(self: Arc<Self>, room_id: &str) {
        loop {
            let next = match self.floors() {
                Ok(mut floors) => {
                    let floor = floors.entry(room_id.to_string()).or_default();
                    floor.speaking = floor.queue.pop_front();
                    floor.running = floor.speaking.is_some();
                    floor.stopped = false;
                    floor.speaking.clone()
                }
                Err(_) => None,
            };
            self.show_floor(room_id);
            let Some(agent_id) = next else { break };
            self.turn(room_id, &agent_id).await;
        }
    }

    /// One member's turn: its reply, then a pass for each member it mentions.
    async fn turn(&self, room_id: &str, agent_id: &str) {
        let Ok(Some(room)) = get(&self.db, room_id) else { return };
        let Some(member) = room.members.iter().find(|m| m.agent_id == agent_id).cloned() else { return };
        let outcome = self.speak(&room, &member).await;
        let TurnOutcome::Done(reply) = outcome else { return };
        let passes: Vec<&Member> = mentions(&reply, &room.members)
            .into_iter()
            .filter(|id| id != agent_id)
            .filter_map(|id| room.members.iter().find(|m| m.agent_id == id))
            .collect();
        let paused = {
            let Ok(mut floors) = self.floors() else { return };
            let floor = floors.entry(room_id.to_string()).or_default();
            if floor.stopped {
                return;
            }
            floor.run += 1;
            // The familiars a reply hands off to answer next, ahead of anyone already waiting.
            for target in passes.iter().rev() {
                floor.queue.retain(|id| *id != target.agent_id);
                floor.queue.push_front(target.agent_id.clone());
            }
            let paused = floor.run >= MAX_RUN && !floor.queue.is_empty();
            if paused {
                floor.queue.clear();
            }
            paused
        };
        // A paused room hands off to nobody, so it records no handoff either.
        for target in passes.into_iter().filter(|_| !paused) {
            let text = format!("{} handed off to {}", member.name, target.name);
            let draft = Draft { agent: Some(&member), kind: kind::PASS, text: &text, status: "done", target: Some(target), ..Draft::default() };
            if let Ok(row) = insert(&self.db, room_id, draft) {
                self.show(&row);
            }
        }
        if paused {
            let text = format!("Paused after {MAX_RUN} replies in a row. Mention a familiar to keep going.");
            if let Ok(row) = insert(&self.db, room_id, Draft { kind: kind::NOTE, text: &text, status: "done", ..Draft::default() }) {
                self.show(&row);
            }
        }
    }

    /// Runs the member's reply on its room session and streams it into the room.
    async fn speak(&self, room: &Room, member: &Member) -> TurnOutcome {
        let fail = |reason: String| {
            let draft = Draft { agent: Some(member), kind: kind::ERROR, text: &reason, status: "done", ..Draft::default() };
            if let Ok(row) = insert(&self.db, &room.id, draft) {
                self.show(&row);
            }
            TurnOutcome::Failed(reason)
        };
        let agent = match self.db.get_agent(&member.agent_id) {
            Ok(Some(agent)) => agent,
            _ => return fail(format!("{} no longer exists.", member.name)),
        };
        if let Err(reason) = crate::budgets::check_turn(&self.db, &agent.id) {
            return fail(reason);
        }
        let session = match session_of(&self.db, &room.id, &agent.id) {
            Ok(session) => session,
            Err(e) => return fail(e),
        };
        let (prompt, covered) = match prompt_for(&self.db, room, member, session.is_none()) {
            Ok(built) => built,
            Err(e) => return fail(e),
        };
        let lane = Lane {
            key: lane_key(&agent.id, &room.id),
            session: match (&session, &agent.session_id) {
                (Some(own), _) => LaneSession::Resume(own.clone()),
                (None, Some(main)) => LaneSession::Fork(main.clone()),
                (None, None) => LaneSession::Fresh,
            },
        };
        let mut writer = Writer::new(self, room, member, &agent, &prompt);
        let (tx, rx) = mpsc::unbounded_channel();
        let key = lane.key.clone();
        let _ = set_speaking(&self.db, &room.id, &agent.id, true);
        let outcome = match self.speaker.start(agent.clone(), lane, prompt.clone(), tx).await {
            Ok(()) => {
                if self.floors().is_ok_and(|f| f.get(&room.id).is_some_and(|f| f.stopped)) {
                    self.speaker.stop(key.clone()).await;
                }
                writer.stream(rx).await
            }
            Err(e) => writer.close(Err(e)),
        };
        // Only a turn that ran its course read the prompt; a failed one gets the same messages next time.
        if matches!(outcome, TurnOutcome::Done(_) | TurnOutcome::Stopped) {
            let _ = set_seen(&self.db, &room.id, &agent.id, covered);
        }
        let _ = set_speaking(&self.db, &room.id, &agent.id, false);
        // The room or the familiar was deleted during this turn, and left its lane for the turn to let go of.
        let gone = get(&self.db, &room.id).is_ok_and(|r| r.is_none()) || self.db.get_agent(&agent.id).is_ok_and(|a| a.is_none());
        if gone {
            self.speaker.forget(&key);
        }
        outcome
    }

    /// Stops the familiar speaking and clears who's waiting.
    pub async fn stop(&self, room_id: &str) -> Result<(), String> {
        let speaking = {
            let mut floors = self.floors()?;
            let floor = floors.entry(room_id.to_string()).or_default();
            floor.queue.clear();
            floor.stopped = floor.speaking.is_some();
            floor.speaking.clone()
        };
        if let Some(agent_id) = speaking {
            self.speaker.stop(lane_key(&agent_id, room_id)).await;
        }
        self.show_floor(room_id);
        Ok(())
    }

    pub async fn delete(&self, room_id: &str) -> Result<(), String> {
        let members = members(&self.db, room_id)?;
        self.stop(room_id).await?;
        // The familiar speaking keeps its lane until its turn ends, so a stop can still reach it;
        // the turn lets go of it then.
        let speaking = self.floor_of(room_id).0;
        for m in members.iter().filter(|m| speaking.as_deref() != Some(m.agent_id.as_str())) {
            self.speaker.forget(&lane_key(&m.agent_id, room_id));
        }
        // The floor stays, stop flag and all, so a turn still starting stops once it can.
        self.db.with(|c| c.execute("DELETE FROM rooms WHERE id = ?1", params![room_id]))?;
        (self.emit)(CHANGED, Value::Null);
        Ok(())
    }

    /// Stops a familiar's room turns and lets go of its room sessions before it's deleted.
    pub async fn forget_familiar(&self, agent_id: &str) {
        let speaking: Vec<String> = self
            .floors()
            .map(|f| f.iter().filter(|(_, f)| f.speaking.as_deref() == Some(agent_id)).map(|(id, _)| id.clone()).collect())
            .unwrap_or_default();
        for room in &speaking {
            let _ = self.stop(room).await;
        }
        let rooms: Vec<String> = self
            .db
            .with(|c| {
                let mut stmt = c.prepare("SELECT room_id FROM room_members WHERE agent_id = ?1")?;
                let rows = stmt.query_map(params![agent_id], |r| r.get(0))?;
                rows.collect()
            })
            .unwrap_or_default();
        // A turn still running keeps its lane until it ends, so the stop can reach it.
        for room in rooms.iter().filter(|r| !speaking.contains(r)) {
            self.speaker.forget(&lane_key(agent_id, room));
        }
    }
}

/// Writes one turn's rows into the room as the provider streams it: a reply, split by each tool the
/// familiar uses. The first reply row keeps the prompt.
struct Writer<'a, S: Speaker> {
    rooms: &'a Rooms<S>,
    room_id: String,
    member: &'a Member,
    provider: crate::db::Provider,
    prompt: Option<&'a str>,
    current: Option<RoomMessage>,
    unsent: String,
    sent_at: Instant,
    saved_at: Instant,
    /// Everything the familiar said this turn, for finding the members it hands off to.
    said: String,
    usage: Option<(u64, u64)>,
}

impl<'a, S: Speaker> Writer<'a, S> {
    fn new(rooms: &'a Rooms<S>, room: &Room, member: &'a Member, agent: &Agent, prompt: &'a str) -> Self {
        Writer {
            rooms,
            room_id: room.id.clone(),
            member,
            provider: agent.provider.clone(),
            prompt: Some(prompt),
            current: None,
            unsent: String::new(),
            sent_at: Instant::now(),
            saved_at: Instant::now(),
            said: String::new(),
            usage: None,
        }
    }

    fn add(&mut self, kind: &str, text: &str, status: &str) -> Option<RoomMessage> {
        let prompt = if kind == kind::REPLY { self.prompt.take() } else { None };
        let draft = Draft { agent: Some(self.member), kind, text, status, prompt, ..Draft::default() };
        let row = insert(&self.rooms.db, &self.room_id, draft).ok()?;
        self.rooms.show(&row);
        Some(row)
    }

    fn append(&mut self, chunk: &str) {
        if self.current.is_none() {
            let chunk = chunk.trim_start();
            if chunk.is_empty() {
                return;
            }
            self.said.push_str(chunk);
            self.current = self.add(kind::REPLY, chunk, "streaming");
            self.sent_at = Instant::now();
            self.saved_at = Instant::now();
            return;
        }
        self.said.push_str(chunk);
        let Some(row) = self.current.as_mut() else { return };
        row.text.push_str(chunk);
        self.unsent.push_str(chunk);
        if self.sent_at.elapsed() >= UI_FLUSH {
            (self.rooms.emit)(DELTA, json!({ "roomId": self.room_id, "messageId": row.id, "chunk": self.unsent }));
            self.unsent.clear();
            self.sent_at = Instant::now();
        }
        if self.saved_at.elapsed() >= DB_FLUSH {
            let _ = update(&self.rooms.db, row.id, &row.text, "streaming");
            self.saved_at = Instant::now();
        }
    }

    fn end_reply(&mut self, status: &str) {
        if let Some(mut row) = self.current.take() {
            row.text.truncate(row.text.trim_end().len());
            row.status = status.to_string();
            let _ = update(&self.rooms.db, row.id, &row.text, status);
            self.rooms.show(&row);
            self.unsent.clear();
        }
    }

    async fn stream(&mut self, mut rx: mpsc::UnboundedReceiver<ProviderEvent>) -> TurnOutcome {
        loop {
            match rx.recv().await {
                Some(ProviderEvent::Delta(chunk)) => self.append(&chunk),
                Some(ProviderEvent::Tool(label)) => {
                    self.end_reply("done");
                    // Keeps "@Scribe" before a tool apart from the words after it.
                    self.said.push('\n');
                    if !label.is_empty() {
                        self.add(kind::TOOL, &label, "done");
                    }
                }
                Some(ProviderEvent::Session(id)) => {
                    let _ = set_session(&self.rooms.db, &self.room_id, &self.member.agent_id, &id);
                }
                Some(ProviderEvent::Usage { input, output }) => self.usage = Some((input, output)),
                Some(ProviderEvent::Limits(info)) => {
                    let snapshot = json!({ "info": info, "at": now_ms() });
                    let _ = self.rooms.db.set_setting(crate::runtime::CLAUDE_LIMITS, &snapshot.to_string());
                }
                Some(ProviderEvent::Model(_)) => {}
                Some(ProviderEvent::Done) => return self.close(Ok("done")),
                Some(ProviderEvent::Stopped) => return self.close(Ok("stopped")),
                Some(ProviderEvent::Failed(reason)) => return self.close(Err(reason)),
                None => return self.close(Err("The familiar stopped without finishing its reply.".into())),
            }
        }
    }

    fn close(&mut self, end: Result<&str, String>) -> TurnOutcome {
        let status = match &end {
            Ok(status) => *status,
            Err(_) => "done",
        };
        self.end_reply(status);
        if let Some((input, output)) = self.usage.take() {
            let _ = self.rooms.db.insert_usage(&self.member.agent_id, &self.provider, "you", input, output);
        }
        match end {
            Ok("stopped") => TurnOutcome::Stopped,
            Ok(_) => {
                // A turn that only used tools still shows as answered, and keeps its prompt.
                if self.said.trim().is_empty() {
                    self.add(kind::REPLY, "(no reply)", "done");
                }
                TurnOutcome::Done(std::mem::take(&mut self.said))
            }
            Err(reason) => {
                let reason = reason.trim().to_string();
                let draft = Draft { agent: Some(self.member), kind: kind::ERROR, text: &reason, status: "done", prompt: self.prompt.take(), ..Draft::default() };
                if let Ok(row) = insert(&self.rooms.db, &self.room_id, draft) {
                    self.rooms.show(&row);
                }
                TurnOutcome::Failed(reason)
            }
        }
    }
}

/// An `Emit` that sends to every window.
pub fn emitter(app: AppHandle) -> Emit {
    Arc::new(move |name, payload| {
        let _ = app.emit(name, payload);
    })
}

#[tauri::command]
pub fn list_rooms(rooms: State<'_, Arc<AppRooms>>) -> Result<Vec<Room>, String> {
    list(&rooms.db)
}

#[tauri::command]
pub fn create_room(rooms: State<'_, Arc<AppRooms>>, title: String, agent_ids: Vec<String>) -> Result<Room, String> {
    let room = create(&rooms.db, &title, &agent_ids)?;
    (rooms.emit)(CHANGED, Value::Null);
    Ok(room)
}

#[tauri::command]
pub fn room_thread(rooms: State<'_, Arc<AppRooms>>, room_id: String) -> Result<Thread, String> {
    rooms.thread(&room_id)
}

#[tauri::command]
pub fn room_history(rooms: State<'_, Arc<AppRooms>>, room_id: String, before_id: i64) -> Result<Page, String> {
    history(&rooms.db, &room_id, Some(before_id))
}

#[tauri::command]
pub fn room_send(rooms: State<'_, Arc<AppRooms>>, room_id: String, text: String) -> Result<RoomMessage, String> {
    rooms.inner().send(&room_id, &text)
}

#[tauri::command]
pub async fn room_stop(rooms: State<'_, Arc<AppRooms>>, room_id: String) -> Result<(), String> {
    rooms.stop(&room_id).await
}

#[tauri::command]
pub async fn delete_room(rooms: State<'_, Arc<AppRooms>>, room_id: String) -> Result<(), String> {
    rooms.delete(&room_id).await
}

#[tauri::command]
pub fn room_prompt(rooms: State<'_, Arc<AppRooms>>, message_id: i64) -> Result<String, String> {
    prompt_of(&rooms.db, message_id)?.ok_or_else(|| "Grimoire didn't keep what this familiar was sent.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Avatar, Provider};

    fn familiar(db: &Db, id: &str, name: &str) -> Member {
        db.insert_agent(&Agent {
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
        })
        .unwrap();
        Member { agent_id: id.into(), name: name.into() }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn finds_mentions_by_whole_name_in_order() {
        let members = vec![
            Member { agent_id: "a".into(), name: "Ink".into() },
            Member { agent_id: "b".into(), name: "Ink Well".into() },
            Member { agent_id: "c".into(), name: "Scribe".into() },
        ];
        assert_eq!(mentions("@scribe then @Ink, and @Scribe again", &members), ids(&["c", "a"]));
        assert_eq!(mentions("@Ink Well, look", &members), ids(&["b"]), "the longest name wins");
        assert_eq!(mentions("@Inkling and me@Scribe.com", &members), Vec::<String>::new(), "not part of a word");
        assert_eq!(mentions("(@Scribe) @Scribe's turn", &members), ids(&["c"]));
        assert!(mentions("no mentions @ all", &members).is_empty());
        let odd = vec![Member { agent_id: "x".into(), name: "@Scribe".into() }, Member { agent_id: "c".into(), name: "Scribe".into() }];
        assert_eq!(mentions("@@Scribe", &odd), ids(&["x"]), "a name never holds a second mention");
    }

    #[test]
    fn makes_rooms_only_from_real_familiars() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", "Researcher");
        familiar(&db, "b", "Scribe");
        familiar(&db, "c", "Critic");
        let room = create(&db, "  ", &ids(&["a", "b", "a", "c"])).unwrap();
        assert_eq!(room.title, "Researcher, Scribe and Critic");
        let names: Vec<&str> = room.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["Researcher", "Scribe", "Critic"], "picked order, repeats dropped");
        assert!(create(&db, "x", &ids(&["a", "peer:mac:z"])).unwrap_err().contains("no longer exists"));
        assert!(create(&db, "x", &ids(&["a", "a"])).unwrap_err().contains("at least 2"));
        assert!(create(&db, &"x".repeat(81), &ids(&["a", "b"])).is_err());
        assert_eq!(list(&db).unwrap().len(), 1);
    }

    #[test]
    fn keeps_room_tools_from_starting_other_conversations() {
        assert!(check_lane(Some("room:r1"), "ask_familiar").is_err());
        assert!(check_lane(Some("room:r1"), "start_helper").is_err());
        assert!(check_lane(Some("room:r1"), "update_ledger").is_ok());
        assert!(check_lane(Some("chat-1"), "ask_familiar").is_ok());
        assert!(check_lane(None, "ask_familiar").is_ok());
        assert_eq!(crate::side::split(&lane_key("a", "r1")), ("a", Some("room:r1")));
    }

    #[test]
    fn prompts_carry_what_was_said_since_the_last_turn() {
        let db = Db::open_in_memory().unwrap();
        let a = familiar(&db, "a", "Researcher");
        let b = familiar(&db, "b", "Scribe");
        let room = create(&db, "Otters", &ids(&["a", "b"])).unwrap();
        let say = |who: Option<&Member>, kind: &str, text: &str, target: Option<&Member>| {
            insert(&db, &room.id, Draft { agent: who, kind, text, status: "done", target, ..Draft::default() }).unwrap()
        };
        say(None, kind::USER, "What do otters eat?", None);
        say(Some(&a), kind::REPLY, "Fish, mostly. @Scribe write it up.", None);
        say(Some(&a), kind::TOOL, "Read notes.md", None);
        say(Some(&a), kind::PASS, "Researcher handed off to Scribe", Some(&b));

        let (first, covered) = prompt_for(&db, &room, &b, true).unwrap();
        assert!(first.starts_with("You're Scribe, one of the person's familiars"));
        assert!(first.contains("called \"Otters\"") && first.contains("these familiars: Researcher."));
        assert!(first.contains("The person: What do otters eat?\n\nResearcher: Fish, mostly."));
        assert!(first.contains("(Researcher handed off to Scribe.)") && !first.contains("notes.md"));
        assert!(first.ends_with("It's your turn."));
        set_seen(&db, &room.id, "b", covered).unwrap();

        say(Some(&b), kind::REPLY, "Otters eat fish.", None);
        say(None, kind::USER, "Thanks!", None);
        let (next, _) = prompt_for(&db, &room, &b, false).unwrap();
        assert!(next.starts_with("You're Scribe"), "the room is introduced every turn");
        assert!(next.ends_with("New in the room since your last turn:\n\nThe person: Thanks!\n\nIt's your turn."), "its own reply is left out");

        let (for_a, _) = prompt_for(&db, &room, &a, false).unwrap();
        assert!(!for_a.contains("Fish, mostly"), "a familiar never reads its own reply back");
        assert!(for_a.contains("Scribe: Otters eat fish."));
    }

    #[test]
    fn long_rooms_leave_out_the_oldest_messages() {
        let db = Db::open_in_memory().unwrap();
        let b = familiar(&db, "b", "Scribe");
        familiar(&db, "a", "Researcher");
        let room = create(&db, "Long", &ids(&["a", "b"])).unwrap();
        for i in 0..40 {
            insert(&db, &room.id, Draft { kind: kind::USER, text: &format!("{i} {}", "x".repeat(1000)), status: "done", ..Draft::default() }).unwrap();
        }
        let (prompt, _) = prompt_for(&db, &room, &b, false).unwrap();
        assert!(prompt.len() < MAX_PROMPT + 200);
        assert!(prompt.contains("(Earlier messages are left out.)") && prompt.contains("The person: 39 "));
        assert!(!prompt.contains("The person: 0 "));
    }

    /// Replies with a script per familiar, one chunk at a time, and records each turn.
    #[derive(Clone, Default)]
    struct Fake {
        replies: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
        turns: Arc<Mutex<Vec<(String, LaneSession, String)>>>,
        active: Arc<Mutex<(usize, usize)>>,
        stopped: Arc<Mutex<Vec<String>>>,
        /// How long each start takes, like a CLI launching.
        startup: Arc<Mutex<Duration>>,
        forgotten: Arc<Mutex<Vec<(String, usize)>>>,
    }

    impl Fake {
        fn script(&self, agent: &str, replies: &[&str]) {
            self.replies.lock().unwrap().insert(agent.into(), replies.iter().map(|r| r.to_string()).collect());
        }
    }

    impl Speaker for Fake {
        fn start(&self, agent: Agent, lane: Lane, prompt: String, tx: EventTx) -> impl Future<Output = Result<(), String>> + Send + 'static {
            let fake = self.clone();
            async move {
                let reply = fake.replies.lock().unwrap().get_mut(&agent.id).and_then(|q| q.pop_front()).unwrap_or_else(|| "ok".into());
                fake.turns.lock().unwrap().push((agent.id.clone(), lane.session, prompt));
                let startup = *fake.startup.lock().unwrap();
                tokio::time::sleep(startup).await;
                if reply == "!FAIL" {
                    return Err("The CLI didn't start.".into());
                }
                {
                    let mut active = fake.active.lock().unwrap();
                    active.0 += 1;
                    active.1 = active.1.max(active.0);
                }
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    let _ = tx.send(ProviderEvent::Session(format!("s-{}", agent.id)));
                    // "|TOOL|" in a script stands for a tool the familiar uses mid-reply.
                    for (i, part) in reply.split("|TOOL|").enumerate() {
                        if i > 0 {
                            let _ = tx.send(ProviderEvent::Tool("Read notes.md".into()));
                        }
                        let (head, tail) = part.split_at(part.len() / 2);
                        let _ = tx.send(ProviderEvent::Delta(head.into()));
                        let _ = tx.send(ProviderEvent::Delta(tail.into()));
                    }
                    fake.active.lock().unwrap().0 -= 1;
                    let _ = tx.send(ProviderEvent::Done);
                });
                Ok(())
            }
        }

        fn stop(&self, key: String) -> impl Future<Output = ()> + Send + 'static {
            let stopped = self.stopped.clone();
            async move { stopped.lock().unwrap().push(key) }
        }

        /// Records each lane let go of, with how many stops came before it.
        fn forget(&self, key: &str) {
            let stops = self.stopped.lock().unwrap().len();
            self.forgotten.lock().unwrap().push((key.to_string(), stops));
        }
    }

    struct Setup {
        rooms: Arc<Rooms<Fake>>,
        fake: Fake,
        room: Room,
        events: Arc<Mutex<Vec<String>>>,
    }

    fn setup() -> Setup {
        let db = Arc::new(Db::open_in_memory().unwrap());
        familiar(&db, "a", "Researcher");
        familiar(&db, "b", "Scribe");
        familiar(&db, "c", "Critic");
        let room = create(&db, "Desk", &ids(&["a", "b", "c"])).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let emit: Emit = Arc::new(move |name, _| seen.lock().unwrap().push(name.to_string()));
        let fake = Fake::default();
        Setup { rooms: Rooms::new(db, fake.clone(), emit), fake, room, events }
    }

    async fn settle(rooms: &Rooms<Fake>, room_id: &str) {
        for _ in 0..400 {
            if rooms.floors().unwrap().get(room_id).is_none_or(|f| !f.running) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the room never went quiet");
    }

    fn rows(rooms: &Rooms<Fake>, room_id: &str) -> Vec<String> {
        history(&rooms.db, room_id, None)
            .unwrap()
            .messages
            .into_iter()
            .map(|m| format!("{}:{}:{}", m.kind, m.name.unwrap_or_default(), m.text))
            .collect()
    }

    #[tokio::test]
    async fn a_mention_gives_the_floor_to_that_familiar_only() {
        let s = setup();
        s.fake.script("b", &["Here's the draft."]);
        s.rooms.send(&s.room.id, "@Scribe draft it please").unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert_eq!(rows(&s.rooms, &s.room.id), ["user::@Scribe draft it please", "reply:Scribe:Here's the draft."]);
        let turns = s.fake.turns.lock().unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].1, LaneSession::Fresh);
        let reply = history(&s.rooms.db, &s.room.id, None).unwrap().messages.pop().unwrap();
        assert!(reply.has_prompt);
        assert_eq!(prompt_of(&s.rooms.db, reply.id).unwrap().as_deref(), Some(turns[0].2.as_str()));
        assert!(s.events.lock().unwrap().contains(&DELTA.to_string()) || reply.text == "Here's the draft.");
    }

    #[tokio::test]
    async fn no_mention_goes_to_whoever_spoke_last_on_its_room_session() {
        let s = setup();
        s.rooms.send(&s.room.id, "Hello room").unwrap();
        settle(&s.rooms, &s.room.id).await;
        s.rooms.send(&s.room.id, "@Critic thoughts?").unwrap();
        settle(&s.rooms, &s.room.id).await;
        s.rooms.send(&s.room.id, "And then?").unwrap();
        settle(&s.rooms, &s.room.id).await;
        s.rooms.send(&s.room.id, "@Critic one more").unwrap();
        settle(&s.rooms, &s.room.id).await;
        let turns = s.fake.turns.lock().unwrap();
        let who: Vec<&str> = turns.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(who, ["a", "c", "c", "c"], "the first member, then whoever spoke last");
        assert_eq!(turns[2].1, LaneSession::Resume("s-c".into()));
        assert!(turns[3].2.contains("New in the room since your last turn:\n\nThe person: @Critic one more"));
    }

    #[tokio::test]
    async fn a_reply_that_mentions_a_member_hands_off_one_at_a_time() {
        let s = setup();
        s.fake.script("a", &["Found three sources. @Scribe and @Critic, over to you. @Researcher"]);
        s.fake.script("b", &["Draft done."]);
        s.fake.script("c", &["Looks right."]);
        s.rooms.send(&s.room.id, "@Researcher dig in").unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert_eq!(
            rows(&s.rooms, &s.room.id),
            [
                "user::@Researcher dig in",
                "reply:Researcher:Found three sources. @Scribe and @Critic, over to you. @Researcher",
                "pass:Researcher:Researcher handed off to Scribe",
                "pass:Researcher:Researcher handed off to Critic",
                "reply:Scribe:Draft done.",
                "reply:Critic:Looks right.",
            ],
            "a familiar mentioning itself isn't a handoff"
        );
        assert_eq!(s.fake.active.lock().unwrap().1, 1, "never two familiars at once");
        assert!(s.fake.turns.lock().unwrap()[2].2.contains("Scribe: Draft done."), "the next speaker reads the one before");
    }

    #[tokio::test]
    async fn familiars_handing_off_forever_pause_after_a_run() {
        let s = setup();
        s.fake.script("a", &["@Scribe"; 10]);
        s.fake.script("b", &["@Researcher"; 10]);
        s.rooms.send(&s.room.id, "@Researcher start").unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert_eq!(s.fake.turns.lock().unwrap().len(), MAX_RUN);
        let last = history(&s.rooms.db, &s.room.id, None).unwrap().messages.pop().unwrap();
        assert_eq!((last.kind.as_str(), last.text.as_str()), (kind::NOTE, "Paused after 6 replies in a row. Mention a familiar to keep going."));
        let passes = rows(&s.rooms, &s.room.id).iter().filter(|r| r.starts_with("pass:")).count();
        assert_eq!(passes, MAX_RUN - 1, "the handoff the pause dropped isn't shown");
        s.rooms.send(&s.room.id, "@Critic wrap up").unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert_eq!(s.fake.turns.lock().unwrap().last().unwrap().0, "c", "the person's message starts a new run");
    }

    #[tokio::test]
    async fn stopping_clears_the_queue_and_stops_the_speaker() {
        let s = setup();
        s.rooms.send(&s.room.id, "@Researcher @Scribe @Critic all of you").unwrap();
        s.rooms.stop(&s.room.id).await.unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert!(s.fake.turns.lock().unwrap().len() <= 1);
        assert!(s.fake.stopped.lock().unwrap().iter().all(|k| k.ends_with(&format!("~room:{}", s.room.id))));
        assert!(s.rooms.send(&s.room.id, "  ").unwrap_err().contains("empty"));
        assert!(s.rooms.send("nope", "hi").unwrap_err().contains("deleted"));
    }

    #[tokio::test]
    async fn deleting_rooms_and_familiars_keeps_history_readable() {
        let s = setup();
        s.fake.script("c", &["Noted."]);
        s.rooms.send(&s.room.id, "@Critic hi").unwrap();
        settle(&s.rooms, &s.room.id).await;
        s.rooms.db.delete_agent("c").unwrap();
        let thread = s.rooms.thread(&s.room.id).unwrap();
        assert_eq!(thread.room.members.len(), 2);
        let reply = thread.messages.last().unwrap();
        assert_eq!((reply.agent_id.as_deref(), reply.name.as_deref()), (None, Some("Critic")));
        s.rooms.delete(&s.room.id).await.unwrap();
        assert!(get(&s.rooms.db, &s.room.id).unwrap().is_none());
        assert!(history(&s.rooms.db, &s.room.id, None).unwrap().messages.is_empty());
    }

    #[tokio::test]
    async fn a_handoff_goes_ahead_of_familiars_already_waiting() {
        let s = setup();
        s.fake.script("a", &["Over to @Scribe|TOOL|One more thing."]);
        s.rooms.send(&s.room.id, "@Researcher @Critic both of you").unwrap();
        settle(&s.rooms, &s.room.id).await;
        let who: Vec<String> = s.fake.turns.lock().unwrap().iter().map(|t| t.0.clone()).collect();
        assert_eq!(who, ["a", "b", "c"], "Scribe answers the handoff before Critic's turn");
        assert!(rows(&s.rooms, &s.room.id).contains(&"pass:Researcher:Researcher handed off to Scribe".to_string()), "a tool between the mention and more text keeps the handoff");
    }

    #[tokio::test]
    async fn stopping_while_a_familiar_starts_stops_it_and_its_handoffs() {
        let s = setup();
        *s.fake.startup.lock().unwrap() = Duration::from_millis(60);
        s.fake.script("a", &["@Scribe take it"]);
        s.rooms.send(&s.room.id, "@Researcher go").unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.rooms.stop(&s.room.id).await.unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert_eq!(s.fake.turns.lock().unwrap().len(), 1, "Scribe never gets the floor");
        assert_eq!(s.fake.stopped.lock().unwrap().len(), 2, "stopped at once, and again once the CLI started");
        assert!(!rows(&s.rooms, &s.room.id).iter().any(|r| r.starts_with("pass:")));
    }

    #[tokio::test]
    async fn a_familiar_that_never_started_still_gets_the_message_next_time() {
        let s = setup();
        s.fake.script("b", &["!FAIL", "Got it."]);
        s.rooms.send(&s.room.id, "@Scribe the first question").unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert!(rows(&s.rooms, &s.room.id).last().unwrap().starts_with("error:Scribe:The CLI didn't start."));
        s.rooms.send(&s.room.id, "@Scribe again").unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert!(s.fake.turns.lock().unwrap()[1].2.contains("The person: @Scribe the first question"));
    }

    #[tokio::test]
    async fn deleting_a_room_while_a_familiar_starts_stops_it() {
        let s = setup();
        *s.fake.startup.lock().unwrap() = Duration::from_millis(60);
        s.rooms.send(&s.room.id, "@Researcher go").unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.rooms.delete(&s.room.id).await.unwrap();
        settle(&s.rooms, &s.room.id).await;
        assert_eq!(s.fake.stopped.lock().unwrap().len(), 2, "stopped again once its CLI started");
        let key = lane_key("a", &s.room.id);
        let forgotten = s.fake.forgotten.lock().unwrap();
        let speaker: Vec<usize> = forgotten.iter().filter(|(k, _)| *k == key).map(|(_, stops)| *stops).collect();
        assert_eq!(speaker, [2], "the speaker's lane goes once, only after the second stop reached it");
        assert_eq!(forgotten.len(), 3);
    }

    #[test]
    fn refuses_familiars_a_mention_cant_tell_apart() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", "Scribe");
        familiar(&db, "b", "SCRIBE");
        assert!(create(&db, "", &ids(&["a", "b"])).unwrap_err().contains("Two of those familiars are called SCRIBE"));
    }

    #[test]
    fn long_histories_load_a_page_at_a_time() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", "Researcher");
        familiar(&db, "b", "Scribe");
        let room = create(&db, "R", &ids(&["a", "b"])).unwrap();
        for i in 0..(HISTORY + 3) {
            insert(&db, &room.id, Draft { kind: kind::USER, text: &i.to_string(), status: "done", ..Draft::default() }).unwrap();
        }
        let latest = history(&db, &room.id, None).unwrap();
        assert!(latest.has_older);
        assert_eq!((latest.messages.len() as i64, latest.messages[0].text.as_str()), (HISTORY, "3"));
        let older = history(&db, &room.id, Some(latest.messages[0].id)).unwrap();
        assert!(!older.has_older);
        let texts: Vec<&str> = older.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["0", "1", "2"]);
    }

    #[test]
    fn notes_turns_cut_off_outside_a_reply() {
        let db = Db::open_in_memory().unwrap();
        let a = familiar(&db, "a", "Researcher");
        familiar(&db, "b", "Scribe");
        let room = create(&db, "R", &ids(&["a", "b"])).unwrap();
        insert(&db, &room.id, Draft { agent: Some(&a), kind: kind::TOOL, text: "Read notes.md", status: "done", ..Draft::default() }).unwrap();
        set_speaking(&db, &room.id, "a", true).unwrap();
        assert_eq!(recover(&db).unwrap(), 1);
        let last = history(&db, &room.id, None).unwrap().messages.pop().unwrap();
        assert_eq!((last.kind.as_str(), last.text.as_str()), (kind::NOTE, "Researcher's turn was cut off when Grimoire closed."));
        assert_eq!(recover(&db).unwrap(), 0, "only once");

        let reply = insert(&db, &room.id, Draft { agent: Some(&a), kind: kind::REPLY, text: "Half", status: "streaming", ..Draft::default() }).unwrap();
        set_speaking(&db, &room.id, "a", true).unwrap();
        assert_eq!(recover(&db).unwrap(), 1, "a reply cut off says so itself, with no second note");
        assert_eq!(history(&db, &room.id, None).unwrap().messages.last().unwrap().id, reply.id);
    }

    #[test]
    fn marks_replies_cut_off_at_quit_as_interrupted() {
        let db = Db::open_in_memory().unwrap();
        let a = familiar(&db, "a", "Researcher");
        familiar(&db, "b", "Scribe");
        let room = create(&db, "R", &ids(&["a", "b"])).unwrap();
        let row = insert(&db, &room.id, Draft { agent: Some(&a), kind: kind::REPLY, text: "Half", status: "streaming", ..Draft::default() }).unwrap();
        recover(&db).unwrap();
        assert_eq!(message(&db, row.id).unwrap().unwrap().status, "interrupted");
    }
}
