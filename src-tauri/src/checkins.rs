//! Check-ins: a familiar wakes on a schedule, looks over its ledger, open questions, recent rituals,
//! and today's drafts, and bothers the person only when something needs them. Off by default per
//! familiar. The ritual scheduler's timer runs them (`due`), so there is one clock for both.
//!
//! Each check-in is one turn in a hidden chat on a fresh session, with the familiar's model and
//! workspace-only access whatever access it has, so an unattended run can't run arbitrary shell
//! commands. Inside it only the Grimoire tools in `ALLOWED` work. A reply of exactly NOTHING, with no
//! question asked and no draft made, leaves no trace: the hidden chat is deleted and nothing is
//! notified. Anything else stays as an entry the person can open from the familiar's settings.
//!
//! Memory review is the same kind of hidden turn: when the person starts a new chat, the chat they
//! left gets one turn, on a copy of its session, asking the familiar to save what's worth keeping.
//!
//! `search_chats` also lives here: a familiar searches its own past chats and the rooms it was in.

use crate::db::{now_ms, Access, Agent, Db, Provider};
use crate::helpers::{self, Ending, Startup};
use crate::providers::LaneSession;
use crate::runtime::{Hidden, Runtime};
use crate::{side, AppState};
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDate, NaiveTime, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Notify;

pub const CHANGED: &str = "checkins_changed";
/// The shortest gap between check-ins, in minutes.
pub const MIN_EVERY: u32 = 15;
const MAX_EVERY: u32 = 24 * 60;
/// How long one check-in or memory review may run.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The whole reply of a check-in that found nothing for the person.
pub const NOTHING: &str = "NOTHING";
/// How many messages the person must have sent in a chat since its last review before it gets one.
pub const REVIEW_AFTER: i64 = 4;
pub const SEARCH_TOOL: &str = "search_chats";
const SEARCH_MAX: i64 = 20;
/// A room message for `search_chats`: id, room title, kind, author id and name, text, and time.
type RoomRow = (i64, String, String, Option<String>, Option<String>, String, i64);
/// How many of the latest room messages `search_chats` looks through.
const ROOM_SCAN: i64 = 2000;
const KIND_CHECKIN: &str = "checkin";
const KIND_REVIEW: &str = "review";
/// The source label on the first message of a check-in's transcript.
const CHECKIN_SOURCE: &str = "Check-in";
const REVIEW_SOURCE: &str = "Memory review";
/// The only Grimoire tools a check-in can call.
const ALLOWED: [&str; 9] =
    ["read_ledger", "update_ledger", "ask_person", "draft_message", "read_memory", "remember", "forget_memory", "search_chats", "list_familiars"];
/// The tools that put something in front of the person, so a run that used one is kept even when its
/// reply is NOTHING.
const SURFACING: [&str; 2] = ["ask_person", "draft_message"];
/// The only Grimoire tools a memory review can call.
const REVIEW_ALLOWED: [&str; 2] = ["read_memory", "remember"];

/// Schema version 30: check-in settings per familiar, the check-ins and reviews that ran, and how far
/// each chat has been reviewed.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE checkins (
           agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
           settings TEXT NOT NULL,
           next_run_at INTEGER,
           last_run_at INTEGER,
           last_note TEXT
         );
         CREATE TABLE checkin_runs (
           id TEXT PRIMARY KEY,
           agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
           chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
           kind TEXT NOT NULL,
           status TEXT NOT NULL,
           summary TEXT,
           started_at INTEGER NOT NULL,
           finished_at INTEGER
         );
         CREATE INDEX checkin_runs_by_agent ON checkin_runs(agent_id, started_at);
         CREATE TABLE chat_reviews (
           chat_id TEXT PRIMARY KEY REFERENCES chats(id) ON DELETE CASCADE,
           through_id INTEGER NOT NULL
         );
         PRAGMA user_version = 30;
         COMMIT;",
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub enabled: bool,
    pub every_minutes: u32,
    /// "HH:MM". When `end` is earlier than `start` the hours run past midnight; when they're equal, all day.
    pub start: String,
    pub end: String,
    /// 1 (Monday) to 7 (Sunday), for the day the hours start on. Empty means every day.
    pub days: Vec<u8>,
    /// Review the chat the person leaves for things to remember.
    #[serde(default)]
    pub review_chats: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: false, every_minutes: 60, start: "09:00".into(), end: "18:00".into(), days: vec![1, 2, 3, 4, 5], review_chats: false }
    }
}

fn parse_time(time: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(time, "%H:%M").map_err(|_| format!("Use a 24-hour time like 09:30, not {time}."))
}

impl Settings {
    /// Checks the settings and returns them tidied.
    pub fn checked(mut self) -> Result<Self, String> {
        if !(MIN_EVERY..=MAX_EVERY).contains(&self.every_minutes) {
            return Err(format!("Check in every {MIN_EVERY} minutes to 24 hours."));
        }
        parse_time(&self.start)?;
        parse_time(&self.end)?;
        if self.days.iter().any(|d| !(1..=7).contains(d)) {
            return Err("Days run from 1 (Monday) to 7 (Sunday).".into());
        }
        self.days.sort_unstable();
        self.days.dedup();
        Ok(self)
    }
}

/// The hours that start on `date`, as (open, close, whether close itself is inside).
fn window<Tz: TimeZone>(s: &Settings, date: NaiveDate, tz: &Tz) -> Option<(DateTime<Tz>, DateTime<Tz>, bool)> {
    if !s.days.is_empty() && !s.days.contains(&(date.weekday().number_from_monday() as u8)) {
        return None;
    }
    let (start, end) = (parse_time(&s.start).ok()?, parse_time(&s.end).ok()?);
    let open = tz.from_local_datetime(&date.and_time(start)).earliest()?;
    let close_date = if end > start { date } else { date + ChronoDuration::days(1) };
    let close = tz.from_local_datetime(&close_date.and_time(end)).earliest()?;
    // All day ends where the next day begins, so that moment isn't counted twice.
    Some((open, close, start != end))
}

/// The next check-in after `after`: the start of the hours, then every `every_minutes` until they end.
pub fn next_run<Tz: TimeZone>(s: &Settings, after: &DateTime<Tz>) -> Option<DateTime<Tz>> {
    let every = i64::from(s.every_minutes.max(MIN_EVERY)) * 60;
    let tz = after.timezone();
    // Yesterday's hours may run past midnight into today.
    (-1..=8).find_map(|offset| {
        let (open, close, inclusive) = window(s, after.date_naive() + ChronoDuration::days(offset), &tz)?;
        let at = if *after < open { open } else {
            let steps = (after.clone() - open.clone()).num_seconds() / every + 1;
            open + ChronoDuration::seconds(steps * every)
        };
        let inside = if inclusive { at <= close } else { at < close };
        inside.then_some(at)
    })
}

/// Whether `now` falls inside the check-in hours.
pub fn in_window<Tz: TimeZone>(s: &Settings, now: &DateTime<Tz>) -> bool {
    let tz = now.timezone();
    (-1..=0).any(|offset| {
        window(s, now.date_naive() + ChronoDuration::days(offset), &tz)
            .is_some_and(|(open, close, inclusive)| *now >= open && if inclusive { *now <= close } else { *now < close })
    })
}

fn next_run_ms(s: &Settings, after_ms: i64) -> Option<i64> {
    let after = Local.timestamp_millis_opt(after_ms).single()?;
    next_run(s, &after).map(|t| t.timestamp_millis())
}

/// A familiar's check-in settings, with when the next one runs and how the last one went.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    #[serde(flatten)]
    pub settings: Settings,
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    /// "Nothing needed you.", "Skipped: …", and the like.
    pub last_note: Option<String>,
}

/// A `checkins` row: the settings as JSON, the next and last run, and the last note.
type Row = (String, Option<i64>, Option<i64>, Option<String>);

pub fn get(db: &Db, agent_id: &str) -> Result<Status, String> {
    let row: Option<Row> = db.with(|c| {
        c.query_row("SELECT settings, next_run_at, last_run_at, last_note FROM checkins WHERE agent_id = ?1", params![agent_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()
    })?;
    Ok(match row {
        Some((settings, next_run_at, last_run_at, last_note)) => {
            Status { settings: serde_json::from_str(&settings).unwrap_or_default(), next_run_at, last_run_at, last_note }
        }
        None => Status { settings: Settings::default(), next_run_at: None, last_run_at: None, last_note: None },
    })
}

/// Saves a familiar's settings and works out its next check-in. Saving never runs one.
pub fn save(db: &Db, agent_id: &str, settings: Settings, now: i64) -> Result<Status, String> {
    db.get_agent(agent_id)?.ok_or("That familiar no longer exists.")?;
    let settings = settings.checked()?;
    let next = if settings.enabled { next_run_ms(&settings, now) } else { None };
    let text = serde_json::to_string(&settings).map_err(|e| e.to_string())?;
    db.with(|c| {
        c.execute(
            "INSERT INTO checkins (agent_id, settings, next_run_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(agent_id) DO UPDATE SET settings = ?2, next_run_at = ?3",
            params![agent_id, text, next],
        )
    })?;
    get(db, agent_id)
}

fn note(db: &Db, agent_id: &str, text: &str) {
    let _ = db.with(|c| c.execute("UPDATE checkins SET last_note = ?2 WHERE agent_id = ?1", params![agent_id, text]));
}

/// How late the timer may wake for a check-in and still run it, even after the hours end: the last
/// check-in is due right at the end, and the timer always wakes a little after.
const LATE_MS: i64 = 5 * 60_000;

/// Moves every due check-in on and returns the familiars whose check-in runs now, and when the next
/// one is due. A check-in missed while Grimoire was closed runs once, if it's still within the hours.
pub fn due(db: &Db, now: i64) -> Result<(Vec<String>, Option<i64>), String> {
    let rows: Vec<(String, String, Option<i64>)> = db.with(|c| {
        let mut stmt = c.prepare_cached("SELECT agent_id, settings, next_run_at FROM checkins")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    })?;
    let local = Local.timestamp_millis_opt(now).single().ok_or("The clock is out of range.")?;
    let (mut run, mut soonest) = (Vec::new(), None::<i64>);
    for (agent_id, settings, next) in rows {
        let Ok(settings) = serde_json::from_str::<Settings>(&settings) else { continue };
        if !settings.enabled {
            continue;
        }
        let next = match next {
            Some(at) if at > now => at,
            due => {
                if due.is_some_and(|at| now - at <= LATE_MS || in_window(&settings, &local)) {
                    run.push(agent_id.clone());
                }
                let next = next_run_ms(&settings, now);
                db.with(|c| c.execute("UPDATE checkins SET next_run_at = ?2 WHERE agent_id = ?1", params![agent_id, next]))?;
                match next {
                    Some(at) => at,
                    None => continue,
                }
            }
        };
        soonest = Some(soonest.map_or(next, |s| s.min(next)));
    }
    Ok((run, soonest))
}

/// The familiar with workspace-only access, for a turn nobody watches.
pub fn clamped(agent: Agent) -> Agent {
    Agent { access: Access::Workspace, ..agent }
}

/// Whether a check-in's reply says nothing needs the person.
pub fn is_nothing(reply: &str) -> bool {
    reply.trim_matches(|c: char| c.is_whitespace() || "*_`.\"'!".contains(c)).eq_ignore_ascii_case(NOTHING)
}

/// Familiars with a check-in running, so a slow one never overlaps the next.
static RUNNING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
/// Hidden chats where a check-in or review is running.
static LANES: LazyLock<Mutex<HashMap<String, Watch>>> = LazyLock::new(Default::default);

/// What a running check-in or review may do, and the surfacing tools it has used.
#[derive(Default)]
struct Watch {
    review: bool,
    used: Vec<String>,
}

fn lanes() -> std::sync::MutexGuard<'static, HashMap<String, Watch>> {
    LANES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the lane key or tool caller `key` is a running check-in or memory review, which the
/// providers run with no shell approvals and no MCP servers but Grimoire's.
pub fn unattended(key: &str) -> bool {
    side::split(key).1.is_some_and(|chat| lanes().contains_key(chat))
}

/// Held while a familiar's check-in runs.
pub struct Claim(String);

impl Drop for Claim {
    fn drop(&mut self) {
        RUNNING.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

/// Marks a familiar's check-in as running, or None when one already is.
pub fn claim(agent_id: &str) -> Option<Claim> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).insert(agent_id.to_string()).then(|| Claim(agent_id.to_string()))
}

/// Refuses a tool a check-in or memory review isn't allowed, when the call comes from its hidden chat
/// (`lane`), and notes the tools that put something in front of the person.
pub fn check_lane(lane: Option<&str>, tool: &str) -> Result<(), String> {
    let Some(chat) = lane else { return Ok(()) };
    let mut lanes = lanes();
    let Some(watch) = lanes.get_mut(chat) else { return Ok(()) };
    if watch.review && !REVIEW_ALLOWED.contains(&tool) {
        return Err(format!("A memory review can't use {tool}. Save what's worth keeping with remember, then reply DONE."));
    }
    if !ALLOWED.contains(&tool) {
        return Err(format!(
            "A check-in can't use {tool}. Ask the person with ask_person, draft with draft_message, or keep a note with update_ledger or remember."
        ));
    }
    if SURFACING.contains(&tool) {
        watch.used.push(tool.to_string());
    }
    Ok(())
}

/// Whether the familiar is replying in a room right now.
fn in_room_turn(db: &Db, agent_id: &str) -> bool {
    db.with(|c| c.query_row("SELECT EXISTS(SELECT 1 FROM room_members WHERE agent_id = ?1 AND speaking = 1)", params![agent_id], |r| r.get(0)))
        .unwrap_or(false)
}

/// Why a familiar's check-in skips this run, if it does. `codex` is a fresh Codex plan reading.
pub fn skip_reason(db: &Db, agent: &Agent, busy: bool, tools_on: bool, codex: Option<&Value>, now: i64) -> Option<String> {
    if agent.host.is_some() {
        return Some(format!("{} runs on another Mac, where Grimoire's tools don't reach yet.", agent.name));
    }
    if !tools_on {
        return Some("Grimoire's tools are off, so a check-in couldn't act.".into());
    }
    if busy {
        return Some(format!("{} was in the middle of a turn.", agent.name));
    }
    if let Err(reason) = crate::budgets::check_turn(db, &agent.id) {
        return Some(reason);
    }
    crate::budgets::provider_pause(db, &agent.provider, codex, now).map(|reason| format!("Check-ins are paused. {reason}"))
}

fn short(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", flat[..at].trim_end()),
        None => flat,
    }
}

fn clock(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map(|t| t.format("%a %-d %b %H:%M").to_string()).unwrap_or_default()
}

/// What a check-in tells the familiar: the time, its ledger, its open questions, the rituals that ran
/// since `since`, and today's drafts, then what to do.
pub fn prompt(db: &Db, agent: &Agent, now: DateTime<Local>, since: i64) -> String {
    let mut text = format!(
        "(Grimoire: this is a scheduled check-in. Nobody is watching it. It's {}.)\n\nLook over what you're keeping track of for the person and decide whether anything needs them now.",
        now.format("%A %-d %B %Y, %H:%M")
    );
    let ledger = crate::ledger::call_tool(db, &agent.id, "read_ledger", &json!({})).unwrap_or_default();
    text.push_str(&format!("\n\nYour ledger:\n{ledger}"));

    let questions: Vec<String> = crate::inbox::open_questions(db)
        .unwrap_or_default()
        .into_iter()
        .filter(|q| q.agent_id == agent.id)
        .map(|q| format!("- {} (asked {})", q.title, q.created_at.map(clock).unwrap_or_default()))
        .collect();
    text.push_str("\n\nYour open questions in the person's inbox:\n");
    text.push_str(&if questions.is_empty() { "(none)".to_string() } else { questions.join("\n") });

    let rituals: Vec<String> = db
        .with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT m.source, m.text, m.created_at FROM messages m JOIN chats ON chats.id = m.chat_id
                 WHERE chats.agent_id = ?1 AND chats.side_of IS NULL AND m.role = 'user' AND m.source LIKE 'Ritual: %' AND m.created_at > ?2
                 ORDER BY m.id DESC LIMIT 5",
            )?;
            let rows = stmt.query_map(params![agent.id, since], |r| {
                let (source, body, at): (String, String, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
                Ok(format!("- {source}, {}: {}", clock(at), short(&body, 400)))
            })?;
            rows.collect()
        })
        .unwrap_or_default();
    text.push_str("\n\nRituals that messaged you since your last check-in, with what set them off:\n");
    text.push_str(&if rituals.is_empty() { "(none)".to_string() } else { rituals.join("\n") });

    let midnight = crate::budgets::window_start(crate::budgets::Period::Day, &now).timestamp_millis();
    let drafts: Vec<String> = db
        .with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT channel, recipients, subject, state FROM drafts WHERE agent_id = ?1 AND created_at >= ?2 ORDER BY created_at",
            )?;
            let rows = stmt.query_map(params![agent.id, midnight], |r| {
                let (channel, to, subject, state): (String, String, Option<String>, String) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
                let to: Vec<String> = serde_json::from_str(&to).unwrap_or_default();
                let subject = subject.map(|s| format!(", \"{s}\"")).unwrap_or_default();
                Ok(format!("- {channel} to {}{subject} ({})", to.join(", "), if state == "draft" { "waiting" } else { state.as_str() }))
            })?;
            rows.collect()
        })
        .unwrap_or_default();
    text.push_str("\n\nYour drafts today:\n");
    text.push_str(&if drafts.is_empty() { "(none)".to_string() } else { drafts.join("\n") });

    text.push_str(
        "\n\nIf nothing needs the person, reply with exactly the word NOTHING and nothing else. Otherwise act only through \
         Grimoire's tools: ask_person to put a question or decision in their inbox, draft_message to draft something for \
         them to send, update_ledger to keep your ledger current, and remember to save something worth keeping. \
         search_chats looks back through your past chats. Don't ask again what an open question already asks. You can't \
         run shell commands, message other familiars, or start helpers here. When you've acted, end with one short \
         sentence saying what you did.",
    );
    text
}

/// One check-in or memory review, for the person to open later.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub agent_id: String,
    /// Its hidden chat, which holds the transcript.
    pub chat_id: String,
    /// "running", "done", "failed", "timeout", "stopped", or "interrupted".
    pub status: String,
    pub summary: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

/// A familiar's recent check-ins, newest first. Memory reviews leave none.
pub fn list(db: &Db, agent_id: &str, limit: i64) -> Result<Vec<Run>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT id, agent_id, chat_id, status, summary, started_at, finished_at FROM checkin_runs
             WHERE agent_id = ?1 AND kind = ?2 ORDER BY started_at DESC, rowid DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![agent_id, KIND_CHECKIN, limit], |r| {
            Ok(Run {
                id: r.get(0)?,
                agent_id: r.get(1)?,
                chat_id: r.get(2)?,
                status: r.get(3)?,
                summary: r.get(4)?,
                started_at: r.get(5)?,
                finished_at: r.get(6)?,
            })
        })?;
        rows.collect()
    })
}

/// Saves a run and its hidden chat, and starts watching the chat's tool calls.
pub(crate) fn begin(db: &Db, agent_id: &str, kind: &str, title: &str) -> Result<(String, String), String> {
    let chat = side::create(db, agent_id, agent_id, title)?;
    let id = uuid::Uuid::new_v4().to_string();
    db.with(|c| {
        c.execute(
            "INSERT INTO checkin_runs (id, agent_id, chat_id, kind, status, started_at) VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
            params![id, agent_id, chat.id, kind, now_ms()],
        )
    })?;
    lanes().insert(chat.id.clone(), Watch { review: kind == KIND_REVIEW, used: Vec::new() });
    Ok((id, chat.id))
}

/// Stops watching a hidden chat and returns the surfacing tools it used.
pub(crate) fn release(chat_id: &str) -> Vec<String> {
    lanes().remove(chat_id).map(|w| w.used).unwrap_or_default()
}

fn delete_chat(db: &Db, chat_id: &str) -> Result<(), String> {
    db.with(|c| c.execute("DELETE FROM chats WHERE id = ?1", params![chat_id]).map(|_| ()))
}

/// Records how a check-in ended and returns the note for its familiar's settings. A NOTHING reply that
/// put nothing in front of the person deletes the hidden chat, and with it the run.
pub fn settle(db: &Db, run_id: &str, chat_id: &str, ending: &Ending, used: &[String]) -> Result<String, String> {
    let (status, summary) = match ending {
        Ending::Done(reply) if is_nothing(reply) && used.is_empty() => {
            delete_chat(db, chat_id)?;
            return Ok("Nothing needed you.".into());
        }
        Ending::Done(reply) => {
            let mut did: Vec<&str> = Vec::new();
            if used.iter().any(|t| t == "ask_person") {
                did.push("asked you in the inbox");
            }
            if used.iter().any(|t| t == "draft_message") {
                did.push("drafted a message");
            }
            let said = if is_nothing(reply) || reply.trim().is_empty() { String::new() } else { short(reply, 300) };
            let summary = match (did.is_empty(), said.is_empty()) {
                (true, true) => "Finished without a reply.".to_string(),
                (true, false) => said,
                (false, true) => format!("It {}.", did.join(" and ")),
                (false, false) => format!("It {}. {said}", did.join(" and ")),
            };
            ("done", summary)
        }
        Ending::Failed(reason) => ("failed", short(reason, 300)),
        Ending::Stopped => ("stopped", "Stopped before it finished.".to_string()),
        Ending::TimedOut => ("timeout", format!("Ran out of time after {} minutes.", TIMEOUT.as_secs() / 60)),
    };
    db.with(|c| {
        c.execute(
            "UPDATE checkin_runs SET status = ?2, summary = ?3, finished_at = ?4 WHERE id = ?1",
            params![run_id, status, summary, now_ms()],
        )
    })?;
    Ok(if status == "done" { format!("Checked in: {summary}") } else { format!("The last check-in didn't finish: {summary}") })
}

/// Runs one hidden turn on its own lane, supervised like a helper's, and lets the lane go after.
async fn hidden(runtime: &Arc<Runtime>, agent_id: &str, chat_id: &str, prompt: &str, source: &str, session: LaneSession) -> Ending {
    let (startup, stop) = (Startup::default(), Notify::new());
    let key = side::lane_key(agent_id, chat_id);
    let turn = Hidden { chat_id, shown: prompt, source: Some(source), prompt, session, workspace_only: true, origin: "ritual" };
    let ending = helpers::supervise(runtime.hidden_turn(turn, &startup), &stop, TIMEOUT, &startup, || runtime.stop_lane(&key)).await;
    runtime.forget_lane(&key);
    // Nothing runs in the lane again, so a tool server left over from it can't call in.
    crate::tools::revoke(&key);
    ending
}

/// Runs a familiar's check-in in the background, unless something says to skip it.
pub fn spawn(db: Arc<Db>, runtime: Arc<Runtime>, app: AppHandle, agent_id: String) {
    tauri::async_runtime::spawn(async move {
        run(&db, &runtime, &agent_id).await;
        let _ = app.emit(CHANGED, ());
    });
}

async fn run(db: &Db, runtime: &Arc<Runtime>, agent_id: &str) {
    let Ok(Some(agent)) = db.get_agent(agent_id) else { return };
    let Some(_running) = claim(agent_id) else {
        return note(db, agent_id, "Skipped: the last check-in was still running.");
    };
    // Read Codex's limits only when they could matter, since that starts Codex.
    let needs_codex = agent.provider == Provider::Codex
        && crate::budgets::plan_guard(db).enabled
        && !runtime.is_busy(agent_id)
        && crate::budgets::check_turn(db, agent_id).is_ok();
    let codex = if needs_codex { runtime.codex_rate_limits().await.ok() } else { None };
    if let Some(limits) = &codex {
        crate::budgets::remember_codex(db, limits);
    }
    let now = now_ms();
    // Looked at after reading the limits, which can take a while, so a turn that started meanwhile counts.
    let busy = runtime.is_busy(agent_id) || in_room_turn(db, agent_id);
    if let Some(reason) = skip_reason(db, &agent, busy, crate::tools::LAUNCH.get().is_some(), codex.as_ref(), now) {
        eprintln!("Skipped {}'s check-in: {reason}", agent.name);
        return note(db, agent_id, &format!("Skipped: {reason}"));
    }
    let last = get(db, agent_id).ok().and_then(|s| s.last_run_at);
    let since = last.unwrap_or(0).max(now - 86_400_000);
    let text = prompt(db, &agent, Local::now(), since);
    let (run_id, chat_id) = match begin(db, agent_id, KIND_CHECKIN, CHECKIN_SOURCE) {
        Ok(ids) => ids,
        Err(e) => return note(db, agent_id, &format!("The last check-in didn't start: {e}")),
    };
    let ending = hidden(runtime, agent_id, &chat_id, &text, CHECKIN_SOURCE, LaneSession::Fresh).await;
    let used = release(&chat_id);
    let summary = settle(db, &run_id, &chat_id, &ending, &used).unwrap_or_else(|e| format!("The last check-in didn't save: {e}"));
    let _ = db.with(|c| c.execute("UPDATE checkins SET last_run_at = ?2, last_note = ?3 WHERE agent_id = ?1", params![agent_id, now, summary]));
}

/// The newest message in `chat_id` to mark as reviewed, when the person has sent at least
/// `REVIEW_AFTER` messages there since its last review.
pub fn review_due(db: &Db, chat_id: &str) -> Result<Option<i64>, String> {
    db.with(|c| {
        let through: i64 = c.query_row("SELECT through_id FROM chat_reviews WHERE chat_id = ?1", params![chat_id], |r| r.get(0)).optional()?.unwrap_or(0);
        let sent: i64 = c.query_row(
            "SELECT COUNT(*) FROM messages WHERE chat_id = ?1 AND id > ?2 AND role = 'user' AND source IS NULL",
            params![chat_id, through],
            |r| r.get(0),
        )?;
        if sent < REVIEW_AFTER {
            return Ok(None);
        }
        c.query_row("SELECT MAX(id) FROM messages WHERE chat_id = ?1", params![chat_id], |r| r.get(0))
    })
}

fn mark_reviewed(db: &Db, chat_id: &str, through: i64) -> Result<(), String> {
    db.with(|c| {
        c.execute(
            "INSERT INTO chat_reviews (chat_id, through_id) VALUES (?1, ?2) ON CONFLICT(chat_id) DO UPDATE SET through_id = ?2",
            params![chat_id, through],
        )
        .map(|_| ())
    })
}

const REVIEW_PROMPT: &str = "(Grimoire: the person has started a new chat, and this one is closing. Nobody is watching this turn.)\n\n\
    Look back over this chat and save anything worth keeping for future chats with remember: a correction, or how they want \
    something done from now on, as a rule in their words; lasting facts about them or their world as facts; details of a \
    named project as project notes. Skip what your memory already has and what only this chat needed. Don't do anything \
    else. Then reply with exactly DONE.";

/// When the person starts a new chat with a familiar that reviews its chats, gives the chat they
/// left one hidden turn, on a copy of its session, to save what's worth remembering.
pub fn after_new_chat(db: &Arc<Db>, runtime: &Arc<Runtime>, agent_id: &str, left: &str) {
    let Ok(Some(agent)) = db.get_agent(agent_id) else { return };
    if !get(db, agent_id).is_ok_and(|s| s.settings.review_chats) || agent.host.is_some() || crate::tools::LAUNCH.get().is_none() {
        return;
    }
    let session: Option<String> = db.with(|c| c.query_row("SELECT session_id FROM chats WHERE id = ?1", params![left], |r| r.get(0))).ok().flatten();
    let (Some(session), Ok(Some(through))) = (session, review_due(db, left)) else { return };
    if crate::budgets::check_turn(db, agent_id).is_err() || !reviewing().insert(left.to_string()) {
        return;
    }
    let (db, runtime, agent_id, left) = (db.clone(), runtime.clone(), agent_id.to_string(), left.to_string());
    tauri::async_runtime::spawn(async move {
        if let Ok((_, chat_id)) = begin(&db, &agent_id, KIND_REVIEW, REVIEW_SOURCE) {
            let ending = hidden(&runtime, &agent_id, &chat_id, REVIEW_PROMPT, REVIEW_SOURCE, LaneSession::Fork(session)).await;
            release(&chat_id);
            reviewed(&db, &left, through, &ending);
            // What it saved shows in the familiar's memory, credited to it, so the transcript goes.
            let _ = delete_chat(&db, &chat_id);
        }
        reviewing().remove(&left);
    });
}

/// Chats with a memory review running, so starting new chats quickly never reviews one twice.
static REVIEWING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

fn reviewing() -> std::sync::MutexGuard<'static, HashSet<String>> {
    REVIEWING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Counts a chat as reviewed through `through` only when its review finished, so a failed one runs
/// again the next time.
fn reviewed(db: &Db, chat_id: &str, through: i64, ending: &Ending) {
    match ending {
        Ending::Done(_) => {
            let _ = mark_reviewed(db, chat_id, through);
        }
        other => eprintln!("A memory review didn't finish: {other:?}"),
    }
}

/// Closes check-ins and reviews cut off when Grimoire last quit. A review's hidden chat goes, since
/// nobody opens one; a check-in stays listed as interrupted.
pub fn recover(db: &Db) -> Result<(), String> {
    db.with(|c| {
        c.execute(
            "DELETE FROM chats WHERE id IN (SELECT chat_id FROM checkin_runs WHERE kind = ?1 AND status = 'running')",
            params![KIND_REVIEW],
        )?;
        c.execute(
            "UPDATE checkin_runs SET status = 'interrupted', summary = 'Grimoire quit while it ran.', finished_at = ?1 WHERE status = 'running'",
            params![now_ms()],
        )
        .map(|_| ())
    })
}

/// One past message `search_chats` found.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    /// "chat" or "room".
    pub place: String,
    pub title: String,
    pub message_id: i64,
    /// "the person", "you", or another familiar's name in a room.
    pub from: String,
    pub snippet: String,
    pub at: i64,
}

/// The words of a search, at most 8, split at anything but letters and digits like the chat search index.
fn words(query: &str) -> Vec<String> {
    query.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).take(8).map(str::to_lowercase).collect()
}

/// The part of `text` around the first of `words`, about 200 characters.
fn excerpt(text: &str, words: &[String]) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // Lowercase it a character at a time, remembering where each lowercase byte came from, since
    // lowercasing can change a character's length.
    let (mut lower, mut origin) = (String::new(), Vec::new());
    for (i, c) in flat.char_indices() {
        let before = lower.len();
        lower.extend(c.to_lowercase());
        origin.extend(std::iter::repeat_n(i, lower.len() - before));
    }
    let at = words.iter().filter_map(|w| lower.find(w.as_str())).min().map_or(0, |p| origin[p]);
    let start = flat[..at].char_indices().rev().nth(80).map_or(0, |(i, _)| i);
    let lead = if start > 0 { "…" } else { "" };
    format!("{lead}{}", short(&flat[start..], 200))
}

/// Whether `text` has every word, the last one as the start of a word, like the chat search index.
fn has_words(text: &str, words: &[String]) -> bool {
    let tokens: Vec<String> = text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_lowercase).collect();
    words.iter().enumerate().all(|(i, w)| tokens.iter().any(|t| if i + 1 == words.len() { t.starts_with(w.as_str()) } else { t == w }))
}

/// Searches `agent_id`'s own chats, then the rooms it was in, best matches first. Never another
/// familiar's chats, and never hidden chats.
pub fn search(db: &Db, agent_id: &str, query: &str, limit: i64) -> Result<Vec<Found>, String> {
    let Some(fts) = crate::chats::fts_query(query) else { return Ok(Vec::new()) };
    let words = words(query);
    let limit = limit.clamp(1, SEARCH_MAX);
    let rows: Vec<(i64, String, String, String, i64)> = db.with(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT m.id, chats.title, m.role, m.text, m.created_at FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             JOIN chats ON chats.id = m.chat_id
             WHERE messages_fts MATCH ?1 AND chats.agent_id = ?2 AND chats.side_of IS NULL
             ORDER BY bm25(messages_fts), m.id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![fts, agent_id, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        rows.collect()
    })?;
    let mut found: Vec<Found> = rows
        .into_iter()
        .map(|(id, title, role, text, at)| Found {
            place: "chat".into(),
            title,
            message_id: id,
            from: if role == "user" { "the person".into() } else { "you".into() },
            snippet: excerpt(&text, &words),
            at,
        })
        .collect();
    if (found.len() as i64) < limit {
        // Rooms aren't in the search index, so read the latest of their messages and match them here.
        let rows: Vec<RoomRow> = db.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT r.id, rooms.title, r.kind, r.agent_id, r.name, r.text, r.created_at FROM room_messages r JOIN rooms ON rooms.id = r.room_id
                 WHERE r.room_id IN (SELECT room_id FROM room_members WHERE agent_id = ?1 UNION SELECT room_id FROM room_messages WHERE agent_id = ?1)
                 AND r.kind IN ('user', 'reply') ORDER BY r.id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![agent_id, ROOM_SCAN], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?;
            rows.collect()
        })?;
        let rooms = rows.into_iter().filter(|row| has_words(&row.5, &words)).take((limit - found.len() as i64) as usize);
        found.extend(rooms.map(|(id, title, kind, author, name, text, at)| Found {
            place: "room".into(),
            title,
            message_id: id,
            from: match (kind.as_str(), author) {
                ("user", _) => "the person".to_string(),
                (_, Some(author)) if author == agent_id => "you".to_string(),
                _ => name.unwrap_or_else(|| "another familiar".into()),
            },
            snippet: excerpt(&text, &words),
            at,
        }));
    }
    Ok(found)
}

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": SEARCH_TOOL,
        "description": "Search your own past chats with the person, and the rooms you were in, for words. Returns up to 20 matches, best first, each with the chat or room title, the date, who wrote it, a snippet, and the message id. Use it to recall what was said or decided before. You only ever see your own chats.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Words to look for. Every word must match; the last can be the start of a word." },
                "limit": { "type": "integer", "minimum": 1, "maximum": SEARCH_MAX, "description": "How many matches to return. Defaults to 10." }
            },
            "required": ["query"]
        }
    })]
}

/// Runs `search_chats` for the familiar `caller`, which the tool socket took from the connection.
pub fn search_tool(db: &Db, caller: &str, arguments: &Value) -> Result<String, String> {
    db.get_agent(caller)?.ok_or("Only a familiar can search its chats.")?;
    let query = arguments.get("query").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if words(query).is_empty() {
        return Err("Give some words to search for.".into());
    }
    let limit = arguments.get("limit").and_then(Value::as_i64).unwrap_or(10);
    let found = search(db, caller, query, limit)?;
    if found.is_empty() {
        return Ok(format!("Nothing in your past chats matches \"{query}\"."));
    }
    let list: Vec<Value> = found
        .iter()
        .map(|f| {
            let date = Local.timestamp_millis_opt(f.at).single().map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
            json!({ "in": f.place, "title": f.title, "date": date, "from": f.from, "messageId": f.message_id, "snippet": f.snippet })
        })
        .collect();
    Ok(serde_json::to_string_pretty(&list).unwrap_or_default())
}

#[tauri::command]
pub fn get_checkins(state: State<AppState>, agent_id: String) -> Result<Status, String> {
    crate::link::local_only(&agent_id)?;
    get(&state.db, &agent_id)
}

#[tauri::command]
pub fn save_checkins(app: AppHandle, state: State<AppState>, agent_id: String, settings: Settings) -> Result<Status, String> {
    crate::link::local_only(&agent_id)?;
    let status = save(&state.db, &agent_id, settings, now_ms())?;
    state.scheduler.changed();
    let _ = app.emit(CHANGED, ());
    Ok(status)
}

#[tauri::command]
pub fn list_checkins(state: State<AppState>, agent_id: String) -> Result<Vec<Run>, String> {
    crate::link::local_only(&agent_id)?;
    list(&state.db, &agent_id, 20)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Avatar, Provider};
    use chrono::{FixedOffset, Timelike};

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn hours(start: &str, end: &str, every: u32, days: Vec<u8>) -> Settings {
        Settings { enabled: true, every_minutes: every, start: start.into(), end: end.into(), days, review_chats: false }
    }

    fn familiar(db: &Db, id: &str, access: Access) -> Agent {
        let agent = Agent {
            id: id.into(),
            name: id.to_uppercase(),
            provider: Provider::Claude,
            model: None,
            workspace: format!("/tmp/{id}"),
            access,
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
    fn runs_from_the_start_of_the_hours_every_interval() {
        let s = hours("09:00", "17:00", 30, vec![]);
        assert_eq!(next_run(&s, &at("2026-11-23T07:12:00-05:00")).unwrap(), at("2026-11-23T09:00:00-05:00"));
        assert_eq!(next_run(&s, &at("2026-11-23T09:00:00-05:00")).unwrap(), at("2026-11-23T09:30:00-05:00"));
        assert_eq!(next_run(&s, &at("2026-11-23T10:10:30-05:00")).unwrap(), at("2026-11-23T10:30:00-05:00"));
        // The end is inside the hours, then the next run waits for tomorrow.
        assert_eq!(next_run(&s, &at("2026-11-23T16:45:00-05:00")).unwrap(), at("2026-11-23T17:00:00-05:00"));
        assert_eq!(next_run(&s, &at("2026-11-23T17:00:00-05:00")).unwrap(), at("2026-11-24T09:00:00-05:00"));
        // An interval that doesn't divide the hours stops before the end.
        let odd = hours("09:00", "10:00", 45, vec![]);
        assert_eq!(next_run(&odd, &at("2026-11-23T09:00:00-05:00")).unwrap(), at("2026-11-23T09:45:00-05:00"));
        assert_eq!(next_run(&odd, &at("2026-11-23T09:45:00-05:00")).unwrap(), at("2026-11-24T09:00:00-05:00"));
    }

    #[test]
    fn keeps_to_the_chosen_days() {
        let weekdays = hours("09:00", "17:00", 60, vec![1, 2, 3, 4, 5]);
        let friday_evening = at("2026-11-27T18:00:00-05:00");
        assert_eq!(friday_evening.weekday(), chrono::Weekday::Fri);
        assert_eq!(next_run(&weekdays, &friday_evening).unwrap(), at("2026-11-30T09:00:00-05:00"));
        assert!(!in_window(&weekdays, &at("2026-11-28T12:00:00-05:00")), "Saturday is off");
        assert!(in_window(&weekdays, &at("2026-11-27T12:00:00-05:00")));
    }

    #[test]
    fn hours_can_run_past_midnight() {
        // Friday night only: 22:00 to 02:00, which ends on Saturday.
        let night = hours("22:00", "02:00", 60, vec![5]);
        assert_eq!(next_run(&night, &at("2026-11-27T21:00:00-05:00")).unwrap(), at("2026-11-27T22:00:00-05:00"));
        assert_eq!(next_run(&night, &at("2026-11-27T23:30:00-05:00")).unwrap(), at("2026-11-28T00:00:00-05:00"));
        assert_eq!(next_run(&night, &at("2026-11-28T01:00:00-05:00")).unwrap(), at("2026-11-28T02:00:00-05:00"));
        assert_eq!(next_run(&night, &at("2026-11-28T02:00:00-05:00")).unwrap(), at("2026-12-04T22:00:00-05:00"));
        assert!(in_window(&night, &at("2026-11-28T01:30:00-05:00")), "Saturday 01:30 belongs to Friday night");
        assert!(!in_window(&night, &at("2026-11-28T22:30:00-05:00")), "Saturday night isn't chosen");
        // Equal start and end means all day, without running twice at midnight.
        let all_day = hours("00:00", "00:00", 360, vec![]);
        assert_eq!(next_run(&all_day, &at("2026-11-23T18:00:00-05:00")).unwrap(), at("2026-11-24T00:00:00-05:00"));
        assert!(in_window(&all_day, &at("2026-11-23T03:00:00-05:00")));
    }

    #[test]
    fn checks_settings() {
        assert!(hours("09:00", "17:00", 10, vec![]).checked().unwrap_err().contains("every 15 minutes"));
        assert!(hours("9am", "17:00", 15, vec![]).checked().is_err());
        assert!(hours("09:00", "17:00", 15, vec![0]).checked().is_err());
        assert_eq!(hours("09:00", "17:00", 15, vec![3, 1, 3]).checked().unwrap().days, vec![1, 3]);
        assert!(!Settings::default().enabled, "check-ins are off until the person turns them on");
    }

    #[test]
    fn a_missed_check_in_runs_once_within_the_hours() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        familiar(&db, "b", Access::Workspace);
        familiar(&db, "c", Access::Workspace);
        let now = now_ms();
        let hour = Local::now().hour();
        save(&db, "a", hours("00:00", "00:00", 15, vec![]), now).unwrap();
        // Hours that are over for today, so a missed run there just moves on.
        let later = format!("{:02}:00", (hour + 2) % 24);
        let after = format!("{:02}:00", (hour + 3) % 24);
        save(&db, "b", hours(&later, &after, 15, vec![]), now).unwrap();
        save(&db, "c", Settings { enabled: false, ..hours("00:00", "00:00", 15, vec![]) }, now).unwrap();
        let next_a = get(&db, "a").unwrap().next_run_at.unwrap();
        assert!(next_a > now && next_a <= now + 15 * 60_000);
        assert_eq!(get(&db, "c").unwrap().next_run_at, None);

        // Grimoire was closed through both familiars' runs.
        db.with(|c| c.execute("UPDATE checkins SET next_run_at = ?1 WHERE agent_id IN ('a', 'b')", params![now - 3 * 3_600_000])).unwrap();
        let (run, soonest) = due(&db, now).unwrap();
        assert_eq!(run, ["a"], "only the familiar still within its hours runs, once");
        assert!(get(&db, "a").unwrap().next_run_at.unwrap() > now);
        assert!(get(&db, "b").unwrap().next_run_at.unwrap() > now);
        assert!(soonest.unwrap() > now);
        assert!(due(&db, now).unwrap().0.is_empty(), "and not again");

        // The last check-in is due right as the hours end, and the timer wakes a moment after.
        db.with(|c| c.execute("UPDATE checkins SET next_run_at = ?1 WHERE agent_id = 'b'", params![now - 1])).unwrap();
        assert_eq!(due(&db, now).unwrap().0, ["b"], "a late wake still runs it");
    }

    #[test]
    fn saving_never_runs_one() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        save(&db, "a", hours("00:00", "00:00", 15, vec![]), now_ms()).unwrap();
        assert!(due(&db, now_ms()).unwrap().0.is_empty());
    }

    #[test]
    fn unattended_turns_run_workspace_only() {
        let db = Db::open_in_memory().unwrap();
        let full = familiar(&db, "a", Access::Full);
        let clamped = clamped(full.clone());
        assert_eq!(clamped.access, Access::Workspace);
        assert_eq!(Agent { access: Access::Full, ..clamped }.id, full.id, "nothing else changes");
        assert_eq!(db.get_agent("a").unwrap().unwrap().access, Access::Full, "the familiar keeps its own access");
    }

    #[test]
    fn hears_nothing_however_it_is_written() {
        for reply in ["NOTHING", " nothing.\n", "**NOTHING**", "`NOTHING`", "\"Nothing\""] {
            assert!(is_nothing(reply), "{reply}");
        }
        for reply in ["Nothing needs you, but I updated the ledger.", "NOTHING NEW", "", "I asked you about the launch."] {
            assert!(!is_nothing(reply), "{reply}");
        }
    }

    #[test]
    fn a_nothing_check_in_leaves_no_trace() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        db.insert_message("a", "user", "Main chat", "done").unwrap();
        let visible = |db: &Db| crate::chats::list(db, "a").unwrap().len();
        let (run, chat) = begin(&db, "a", KIND_CHECKIN, CHECKIN_SOURCE).unwrap();
        db.insert_message_in(&chat, "a", "user", "check-in prompt", "done", Some(CHECKIN_SOURCE)).unwrap();
        db.insert_message_in(&chat, "a", "assistant", "NOTHING", "done", None).unwrap();
        assert_eq!(visible(&db), 1, "the hidden chat is never in the chat list");
        let used = release(&chat);
        assert_eq!(settle(&db, &run, &chat, &Ending::Done("NOTHING".into()), &used).unwrap(), "Nothing needed you.");
        assert!(side::get(&db, &chat).unwrap().is_none(), "the hidden chat is gone");
        assert!(list(&db, "a", 20).unwrap().is_empty(), "and so is its entry");
        let main: Vec<String> = db.list_messages("a", None, 10).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(main, ["Main chat"]);
        assert!(crate::chats::search(&db, "prompt", 10).unwrap().is_empty());
    }

    #[test]
    fn a_check_in_that_asks_is_kept_for_the_person() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        let (run, chat) = begin(&db, "a", KIND_CHECKIN, CHECKIN_SOURCE).unwrap();
        check_lane(Some(&chat), "read_ledger").unwrap();
        check_lane(Some(&chat), "ask_person").unwrap();
        let used = release(&chat);
        assert_eq!(used, ["ask_person"]);
        // Even a NOTHING reply is kept once it put a question in the inbox.
        let note = settle(&db, &run, &chat, &Ending::Done("NOTHING".into()), &used).unwrap();
        assert_eq!(note, "Checked in: It asked you in the inbox.");
        let runs = list(&db, "a", 20).unwrap();
        assert_eq!((runs[0].status.as_str(), runs[0].chat_id.as_str()), ("done", chat.as_str()));
        assert!(side::get(&db, &chat).unwrap().is_some(), "its transcript stays");

        let (run, chat) = begin(&db, "a", KIND_CHECKIN, CHECKIN_SOURCE).unwrap();
        let note = settle(&db, &run, &chat, &Ending::TimedOut, &release(&chat)).unwrap();
        assert!(note.contains("Ran out of time"), "{note}");
        assert_eq!(list(&db, "a", 20).unwrap().len(), 2);
    }

    #[test]
    fn check_ins_only_get_their_tools() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Full);
        let (_, chat) = begin(&db, "a", KIND_CHECKIN, CHECKIN_SOURCE).unwrap();
        for tool in ["start_helper", "message_familiar", "ask_familiar", "vault_use", "browser_open", "create_spell", "set_theme"] {
            assert!(check_lane(Some(&chat), tool).unwrap_err().contains("A check-in can't use"), "{tool}");
        }
        for tool in ALLOWED {
            assert!(check_lane(Some(&chat), tool).is_ok(), "{tool}");
        }
        assert!(check_lane(None, "vault_use").is_ok(), "the main chat keeps every tool");
        release(&chat);
        assert!(check_lane(Some(&chat), "vault_use").is_ok(), "nothing is watched once it ends");
    }

    #[test]
    fn skips_over_budget_mid_turn_and_paused() {
        let db = Db::open_in_memory().unwrap();
        let agent = familiar(&db, "a", Access::Workspace);
        assert_eq!(skip_reason(&db, &agent, false, true, None, now_ms()), None);
        assert!(skip_reason(&db, &agent, true, true, None, now_ms()).unwrap().contains("middle of a turn"));
        assert!(skip_reason(&db, &agent, false, false, None, now_ms()).unwrap().contains("tools are off"));
        let remote = Agent { host: Some("studio.local".into()), ..agent.clone() };
        assert!(skip_reason(&db, &remote, false, true, None, now_ms()).unwrap().contains("another Mac"));

        crate::budgets::set(&db, "a", Some(&crate::budgets::Budget { tokens: 100, period: crate::budgets::Period::Day })).unwrap();
        db.insert_usage("a", &Provider::Claude, "ritual", 90, 20).unwrap();
        assert!(skip_reason(&db, &agent, false, true, None, now_ms()).unwrap().contains("over its token budget"));
        crate::budgets::set(&db, "a", None).unwrap();

        // Claude's last reading is past the plan limit guard.
        let limits = json!({ "info": { "rateLimitType": "five_hour", "utilization": 0.95 }, "at": now_ms() });
        db.set_setting(crate::runtime::CLAUDE_LIMITS, &limits.to_string()).unwrap();
        assert!(skip_reason(&db, &agent, false, true, None, now_ms()).unwrap().starts_with("Check-ins are paused."));
    }

    #[test]
    fn one_check_in_per_familiar_at_a_time() {
        let first = claim("overlap-a").expect("nothing running yet");
        assert!(claim("overlap-a").is_none(), "a second one skips");
        assert!(claim("overlap-b").is_some(), "other familiars aren't held up");
        drop(first);
        assert!(claim("overlap-a").is_some(), "the next run goes once the first ends");
    }

    #[test]
    fn prompt_has_the_ledger_questions_rituals_and_drafts() {
        let db = Db::open_in_memory().unwrap();
        let agent = familiar(&db, "a", Access::Workspace);
        familiar(&db, "b", Access::Workspace);
        crate::ledger::update(&db, "a", crate::ledger::Patch { goal: Some("Ship the launch post".into()), ..Default::default() }, "you").unwrap();
        crate::inbox::call_tool(&db, "a", "ask_person", &json!({ "question": "Post on Tuesday?", "options": ["Yes", "No"], "recommended": "Yes" })).unwrap();
        crate::inbox::call_tool(&db, "b", "ask_person", &json!({ "question": "Not mine", "options": ["Yes", "No"], "recommended": "Yes" })).unwrap();
        db.insert_message_from("a", "user", "Look at it\n\nTriggered by: PR #7 opened in o/r", "done", Some("Ritual: PR watch")).unwrap();
        crate::drafts::create(&db, "a", &json!({ "channel": "email", "to": ["sam@example.com"], "subject": "Launch", "body": "Hi" })).unwrap();
        let text = prompt(&db, &agent, Local::now(), now_ms() - 3_600_000);
        assert!(text.contains("goal: Ship the launch post"), "{text}");
        assert!(text.contains("- Post on Tuesday?") && !text.contains("Not mine"), "{text}");
        assert!(text.contains("Ritual: PR watch") && text.contains("Triggered by: PR #7"), "{text}");
        assert!(text.contains("email to sam@example.com, \"Launch\" (waiting)"), "{text}");
        assert!(text.contains("reply with exactly the word NOTHING"));
    }

    #[test]
    fn reviews_a_chat_after_enough_messages() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        let chat = crate::chats::current(&db, "a").unwrap();
        assert!(chat.is_none());
        for text in ["one", "two", "three"] {
            db.insert_message("a", "user", text, "done").unwrap();
            db.insert_message("a", "assistant", "ok", "done").unwrap();
        }
        // Rituals and inbox answers aren't the person writing.
        db.insert_message_from("a", "user", "ritual", "done", Some("Ritual: x")).unwrap();
        let chat = crate::chats::current(&db, "a").unwrap().unwrap();
        assert_eq!(review_due(&db, &chat).unwrap(), None);
        let fourth = db.insert_message("a", "user", "four", "done").unwrap();
        assert_eq!(review_due(&db, &chat).unwrap(), Some(fourth.id));
        mark_reviewed(&db, &chat, fourth.id).unwrap();
        assert_eq!(review_due(&db, &chat).unwrap(), None, "it counts again from the last review");
        for text in ["5", "6", "7", "8"] {
            db.insert_message("a", "user", text, "done").unwrap();
        }
        assert!(review_due(&db, &chat).unwrap().is_some());
    }

    #[test]
    fn search_finds_only_your_own_chats_and_rooms() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        familiar(&db, "b", Access::Workspace);
        db.insert_message("a", "user", "The marmalade recipe needs Seville oranges.", "done").unwrap();
        db.insert_message("b", "user", "My marmalade secret is whisky.", "done").unwrap();
        let side = side::create(&db, "a", "a", "hidden").unwrap();
        db.insert_message_in(&side.id, "a", "assistant", "marmalade in a hidden chat", "done", None).unwrap();
        let found = search(&db, "a", "marmalade", 10).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].place.as_str(), found[0].from.as_str()), ("chat", "the person"));
        assert!(found[0].snippet.contains("Seville"));
        assert!(search(&db, "b", "seville", 10).unwrap().is_empty(), "b can't see a's chat");

        // A room a and b were both in, and one only b was in.
        db.with(|c| {
            c.execute_batch(
                "INSERT INTO rooms (id, title, created_at, updated_at) VALUES ('r1', 'Kitchen', 1, 1), ('r2', 'Secret', 1, 1);
                 INSERT INTO room_members (room_id, agent_id, position) VALUES ('r1', 'a', 0), ('r1', 'b', 1), ('r2', 'b', 0);
                 INSERT INTO room_messages (room_id, agent_id, name, kind, text, status, created_at) VALUES
                   ('r1', NULL, NULL, 'user', 'Who makes the best marmalade?', 'done', 5),
                   ('r1', 'b', 'B', 'reply', 'I do, with 100% oranges.', 'done', 6),
                   ('r2', 'b', 'B', 'reply', 'marmalade plans nobody else sees', 'done', 7);",
            )
        })
        .unwrap();
        let found = search(&db, "a", "marmalade", 10).unwrap();
        let rooms: Vec<(&str, &str)> = found.iter().filter(|f| f.place == "room").map(|f| (f.title.as_str(), f.from.as_str())).collect();
        assert_eq!(rooms, [("Kitchen", "the person")]);
        let percent = search(&db, "a", "100%", 10).unwrap();
        assert_eq!(percent.iter().map(|f| f.from.as_str()).collect::<Vec<_>>(), ["B"], "a % in the query is a plain character");
        assert!(search(&db, "b", "marmalade", 10).unwrap().iter().any(|f| f.title == "Secret"));

        assert!(search_tool(&db, "a", &json!({ "query": " -- " })).unwrap_err().contains("Give some words"));
        assert!(search_tool(&db, "a", &json!({ "query": "zebra" })).unwrap().starts_with("Nothing in your past chats"));
        let listed: Vec<Value> = serde_json::from_str(&search_tool(&db, "a", &json!({ "query": "Seville" })).unwrap()).unwrap();
        assert_eq!(listed[0]["in"], "chat");
        assert!(listed[0]["messageId"].as_i64().is_some() && listed[0]["date"].as_str().is_some());
    }

    #[test]
    fn a_restart_closes_runs_left_running() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        let (_, checkin) = begin(&db, "a", KIND_CHECKIN, CHECKIN_SOURCE).unwrap();
        let (_, review) = begin(&db, "a", KIND_REVIEW, REVIEW_SOURCE).unwrap();
        release(&checkin);
        release(&review);
        // Neither looks like a side conversation waiting for a reply.
        assert!(side::unfinished(&db).unwrap().is_empty());
        recover(&db).unwrap();
        assert_eq!(list(&db, "a", 20).unwrap()[0].status, "interrupted");
        assert!(side::get(&db, &review).unwrap().is_none(), "a review's chat goes");
    }

    #[test]
    fn excerpts_survive_text_that_changes_length_when_lowercased() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        // İ grows and the Kelvin sign shrinks when lowercased, so byte offsets shift either way.
        db.insert_message("a", "user", "İ é İ\u{212A} and more", "done").unwrap();
        assert!(excerpt("İ é İ\u{212A}", &["é".into()]).contains('é'));
        assert_eq!(search(&db, "a", "é", 10).unwrap().len(), 1);
        assert!(db.list_agents().is_ok(), "the database is still usable");
    }

    #[test]
    fn rooms_match_whole_words_in_any_script() {
        let words = |q: &str| super::words(q);
        assert!(!has_words("A flaw appeared.", &words("law appeared")));
        assert!(has_words("CAFÉ funds arrived.", &words("café")));
        assert!(has_words("I do, with 100% oranges.", &words("100%")));
        assert!(has_words("Lisbon venue", &words("venue lis")), "the last word can be the start of one");
        assert!(!has_words("Lisbon venue", &words("ven lisbon")), "the others must be whole");
    }

    #[test]
    fn a_failed_review_runs_again_next_time() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        for text in ["1", "2", "3", "4"] {
            db.insert_message("a", "user", text, "done").unwrap();
        }
        let chat = crate::chats::current(&db, "a").unwrap().unwrap();
        let through = review_due(&db, &chat).unwrap().unwrap();
        reviewed(&db, &chat, through, &Ending::Failed("offline".into()));
        assert_eq!(review_due(&db, &chat).unwrap(), Some(through));
        reviewed(&db, &chat, through, &Ending::Done("DONE".into()));
        assert_eq!(review_due(&db, &chat).unwrap(), None);
    }

    #[test]
    fn reviews_only_get_memory_tools_and_lanes_say_they_are_unattended() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Access::Workspace);
        let (_, chat) = begin(&db, "a", KIND_REVIEW, REVIEW_SOURCE).unwrap();
        for tool in ["ask_person", "draft_message", "update_ledger", "search_chats"] {
            assert!(check_lane(Some(&chat), tool).unwrap_err().contains("A memory review can't use"), "{tool}");
        }
        assert!(check_lane(Some(&chat), "remember").is_ok());
        let key = side::lane_key("a", &chat);
        assert!(unattended(&key));
        assert!(!unattended("a"), "the main chat isn't");
        release(&chat);
        assert!(!unattended(&key));
    }

}
