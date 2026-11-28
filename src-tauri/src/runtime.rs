use crate::attachments::{Attachment, Staged};
use crate::db::{Agent, Db, Message, Provider};
use crate::providers::{claude::Claude, codex::Codex, Lane, LaneSession, ProviderEvent};
use crate::side::SideSink;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

const UI_FLUSH: Duration = Duration::from_millis(50);
const DB_FLUSH: Duration = Duration::from_millis(750);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DeltaPayload<'a> {
    agent_id: &'a str,
    message_id: i64,
    chunk: &'a str,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StatusPayload<'a> {
    agent_id: &'a str,
    busy: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    Done(String),
    Stopped,
    Failed(String),
}

type Reply = oneshot::Sender<TurnOutcome>;

struct Job {
    message_id: i64,
    text: String,
    source: Option<String>,
    attachments: Vec<Attachment>,
    reply: Option<Reply>,
}

/// Saves `text` as a message from `source` in `chat`, or in the familiar's current chat when `chat` is
/// none or was deleted, for the person to see. It also queues the text for the familiar's next turn in
/// that chat, since a stored message never reaches the CLI on its own. For results that come back while
/// the familiar can't take a turn there.
pub fn hold(db: &Db, agent_id: &str, chat: Option<&str>, text: &str, source: &str) -> Result<Message, String> {
    let home = chat.and_then(|chat| db.insert_message_in(chat, agent_id, "user", text, "done", Some(source)).ok().map(|row| (row, chat.to_string())));
    let (row, chat) = match home {
        Some((row, chat)) => (row, Some(chat)),
        None => (db.insert_message_from(agent_id, "user", text, "done", Some(source))?, crate::chats::current(db, agent_id)?),
    };
    db.with(|c| {
        c.execute(
            "INSERT INTO pending_notes (agent_id, chat_id, source, text, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![agent_id, chat, source, text, crate::db::now_ms()],
        )
    })?;
    Ok(row)
}

/// What goes before the familiar's next turn in its current chat: the person's ledger edits, then the
/// held messages for that chat it hasn't seen. Also returns their ids, to clear once the turn starts.
fn turn_notes(db: &Db, agent: &Agent) -> (Vec<String>, Vec<i64>) {
    // A resumed chat keeps its old system prompt, so pass on the person's ledger edits here.
    let mut notes: Vec<String> = crate::ledger::turn_note(db, agent).into_iter().collect();
    let chat = crate::chats::current(db, &agent.id).ok().flatten();
    let held: Vec<(i64, String, String)> = db
        .with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, source, text FROM pending_notes WHERE agent_id = ?1 AND (chat_id IS ?2 OR chat_id IS NULL) ORDER BY id",
            )?;
            let rows = stmt.query_map(rusqlite::params![agent.id, chat], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect()
        })
        .unwrap_or_default();
    let ids = held.iter().map(|(id, ..)| *id).collect();
    notes.extend(held.into_iter().map(|(_, source, text)| {
        format!("(Grimoire: this message, {source}, came in while you couldn't take a turn.)\n{text}")
    }));
    (notes, ids)
}

fn clear_notes(db: &Db, ids: &[i64]) {
    let _ = db.with(|c| {
        let mut stmt = c.prepare_cached("DELETE FROM pending_notes WHERE id = ?1")?;
        ids.iter().try_for_each(|id| stmt.execute(rusqlite::params![id]).map(|_| ()))
    });
}

pub const RITUAL_SOURCE: &str = "Ritual: ";
pub const SPELL_SOURCE: &str = "Spell: ";

/// Who started a turn, for usage: the person, a ritual, or a spell, from the message's source label.
pub fn origin_of(source: Option<&str>) -> &'static str {
    match source {
        Some(s) if s.starts_with(RITUAL_SOURCE) => "ritual",
        Some(s) if s.starts_with(SPELL_SOURCE) => "spell",
        _ => "you",
    }
}

#[derive(Default)]
struct AgentState {
    busy: bool,
    queue: VecDeque<Job>,
    /// Set by a stop. A turn still checking its Mac sees it before it starts the CLI.
    stop_requested: bool,
}

pub struct Runtime {
    db: Arc<Db>,
    app: AppHandle,
    codex: Arc<Codex>,
    /// Codex app servers on other Macs, one per host and workspace, since each runs in its workspace.
    remote_codex: Mutex<HashMap<(String, String), Arc<Codex>>>,
    claude: Claude,
    agents: Mutex<HashMap<String, AgentState>>,
    /// One turn at a time in each side conversation, by lane key.
    sides: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

pub trait Sink: Send + Sync {
    fn message(&self, message: &Message);
    fn delta(&self, agent_id: &str, message_id: i64, chunk: &str);
    fn model(&self, agent_id: &str, model: &str);
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelPayload<'a> {
    agent_id: &'a str,
    model: &'a str,
}

impl Sink for AppHandle {
    fn message(&self, message: &Message) {
        let _ = self.emit("message", message);
        crate::link::record("message", || crate::link::message_value(message));
    }

    fn delta(&self, agent_id: &str, message_id: i64, chunk: &str) {
        let _ = self.emit("delta", DeltaPayload { agent_id, message_id, chunk });
        crate::link::record("delta", || serde_json::json!(DeltaPayload { agent_id, message_id, chunk }));
    }

    fn model(&self, agent_id: &str, model: &str) {
        let _ = self.emit("agent_model", ModelPayload { agent_id, model });
        crate::link::record("model", || serde_json::json!(ModelPayload { agent_id, model }));
    }
}

impl Runtime {
    pub fn new(db: Arc<Db>, app: AppHandle) -> Arc<Self> {
        Arc::new(Runtime {
            db,
            app,
            codex: Arc::new(Codex::default()),
            remote_codex: Mutex::new(HashMap::new()),
            claude: Claude::default(),
            agents: Mutex::new(HashMap::new()),
            sides: Mutex::new(HashMap::new()),
        })
    }

    /// The Codex app server a familiar's turns go to: the local one, or one on its host.
    fn codex_for(&self, agent: &Agent) -> Arc<Codex> {
        let Some(host) = &agent.host else { return self.codex.clone() };
        let Ok(mut remote) = self.remote_codex.lock() else { return Arc::new(Codex::on(host)) };
        remote.entry((host.clone(), agent.workspace.clone())).or_insert_with(|| Arc::new(Codex::on(host))).clone()
    }

    fn all_codex(&self) -> Vec<Arc<Codex>> {
        let mut all = vec![self.codex.clone()];
        if let Ok(remote) = self.remote_codex.lock() {
            all.extend(remote.values().cloned());
        }
        all
    }

    pub fn busy_agents(&self) -> Vec<String> {
        self.agents
            .lock()
            .map(|a| a.iter().filter(|(_, s)| s.busy).map(|(id, _)| id.clone()).collect())
            .unwrap_or_default()
    }

    fn set_busy(&self, agent_id: &str, busy: bool) {
        let _ = self.app.emit("agent_status", StatusPayload { agent_id, busy });
        crate::link::record("status", || serde_json::json!(StatusPayload { agent_id, busy }));
        crate::tray::agent_status(&self.app, agent_id, busy);
    }

    pub fn send(self: &Arc<Self>, agent_id: &str, text: String, source: Option<&str>) -> Result<Message, String> {
        self.enqueue(agent_id, text, source, Staged::default(), None)
    }

    /// Sends a message from the composer, with the files it staged.
    pub fn send_with(self: &Arc<Self>, agent_id: &str, text: String, staged: Staged) -> Result<Message, String> {
        self.enqueue(agent_id, text, None, staged, None)
    }

    pub async fn send_and_wait(self: &Arc<Self>, agent_id: &str, text: String, source: Option<&str>) -> TurnOutcome {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.enqueue(agent_id, text, source, Staged::default(), Some(tx)) {
            return TurnOutcome::Failed(e);
        }
        rx.await.unwrap_or(TurnOutcome::Stopped)
    }

    fn enqueue(
        self: &Arc<Self>,
        agent_id: &str,
        text: String,
        source: Option<&str>,
        staged: Staged,
        reply: Option<Reply>,
    ) -> Result<Message, String> {
        let agent = self.db.get_agent(agent_id)?.ok_or("That familiar no longer exists.")?;
        if agent.host.is_some() && !staged.is_empty() {
            return Err("Attachments can't reach a familiar on another Mac yet. Send the message without them.".into());
        }
        if let Err(reason) = crate::budgets::check_turn(&self.db, agent_id) {
            // Spells and rituals have no composer to show the refusal, so it goes in the chat.
            if source.is_some() {
                if let Ok(message) = self.db.insert_message(agent_id, "error", &reason, "done") {
                    self.app.message(&message);
                }
            }
            return Err(reason);
        }
        let mut agents = self.agents.lock().map_err(|_| "runtime state poisoned")?;
        let state = agents.entry(agent_id.to_string()).or_default();
        let status = if state.busy { "queued" } else { "done" };
        let mut message = self.db.insert_message_from(agent_id, "user", &text, status, source)?;
        message.attachments = staged.store(&self.db, message.id).inspect_err(|_| crate::attachments::drop_message(&self.db, message.id))?;
        self.app.message(&message);
        let attachments = message.attachments.clone();
        let job = Job { message_id: message.id, text, source: source.map(String::from), attachments, reply };
        if state.busy {
            state.queue.push_back(job);
        } else {
            state.busy = true;
            drop(agents);
            self.set_busy(agent_id, true);
            self.spawn_turn(agent_id.to_string(), job);
        }
        Ok(message)
    }

    pub async fn stop(self: &Arc<Self>, agent_id: &str) -> Result<(), String> {
        let cancelled: Vec<Job> = {
            let mut agents = self.agents.lock().map_err(|_| "runtime state poisoned")?;
            agents
                .get_mut(agent_id)
                .map(|s| {
                    s.stop_requested = s.busy;
                    s.queue.drain(..).collect()
                })
                .unwrap_or_default()
        };
        for job in cancelled {
            self.update_status(job.message_id, agent_id, "cancelled");
            crate::telegram::turn_ended(agent_id, job.message_id, &TurnOutcome::Stopped);
            if let Some(reply) = job.reply {
                let _ = reply.send(TurnOutcome::Stopped);
            }
        }
        match self.db.get_agent(agent_id)? {
            Some(agent) if agent.provider == Provider::Codex => self.codex_for(&agent).stop(agent_id).await,
            Some(_) => {
                self.claude.stop(agent_id).await;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Clears a pending stop for `agent_id`, returning whether there was one.
    fn take_stop(&self, agent_id: &str) -> bool {
        self.agents.lock().is_ok_and(|mut a| a.get_mut(agent_id).is_some_and(|s| std::mem::take(&mut s.stop_requested)))
    }

    pub fn is_busy(&self, agent_id: &str) -> bool {
        self.agents.lock().map(|a| a.get(agent_id).is_some_and(|s| s.busy)).unwrap_or(false)
    }

    /// Codex's plan limits. Starts the app server if it isn't running, which stops again when idle.
    pub async fn codex_rate_limits(&self) -> Result<serde_json::Value, String> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        self.codex.rate_limits(&home).await
    }

    pub async fn list_models(&self, provider: &Provider) -> Result<Vec<crate::providers::ModelOption>, String> {
        match provider {
            Provider::Claude => Ok(crate::providers::claude::models()),
            Provider::Codex => {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
                self.codex.list_models(&home).await
            }
        }
    }

    pub async fn settings_changed(&self, agent_id: &str) {
        for codex in self.all_codex() {
            codex.reload_thread(agent_id).await;
        }
    }

    pub fn claude_billing_changed(&self) {
        self.claude.forget_idle();
    }

    pub fn reset_session(&self, agent_id: &str) {
        self.all_codex().iter().for_each(|codex| codex.forget(agent_id));
        self.claude.forget(agent_id);
    }

    pub async fn remove_agent(self: &Arc<Self>, agent_id: &str) -> Result<(), String> {
        self.stop(agent_id).await?;
        self.reset_session(agent_id);
        // Its side conversations go with it, so stop any turn still running in one.
        for key in crate::side::lanes_of(&self.db, agent_id)? {
            for codex in self.all_codex() {
                let _ = codex.stop(&key).await;
                codex.forget(&key);
            }
            self.claude.forget(&key);
        }
        if let Ok(mut agents) = self.agents.lock() {
            agents.remove(agent_id);
        }
        Ok(())
    }

    fn update_status(&self, id: i64, agent_id: &str, status: &str) {
        if let Some(mut message) = self
            .db
            .list_messages(agent_id, Some(id + 1), 1)
            .ok()
            .and_then(|m| m.into_iter().find(|m| m.id == id))
        {
            message.status = status.to_string();
            let _ = self.db.update_message(id, &message.text, status);
            self.app.message(&message);
        }
    }

    fn spawn_turn(self: &Arc<Self>, agent_id: String, job: Job) {
        let runtime = self.clone();
        tauri::async_runtime::spawn(async move {
            let outcome = runtime.run_turn(&agent_id, &job.text, job.source.as_deref(), &job.attachments).await;
            crate::notify::turn_ended(&runtime.app, &agent_id, job.source.as_deref(), &outcome);
            crate::telegram::turn_ended(&agent_id, job.message_id, &outcome);
            if let Some(reply) = job.reply {
                let _ = reply.send(outcome);
            }
            runtime.finish_turn(&agent_id);
        });
    }

    fn finish_turn(self: &Arc<Self>, agent_id: &str) {
        let next = {
            let Ok(mut agents) = self.agents.lock() else { return };
            let Some(state) = agents.get_mut(agent_id) else { return };
            let next = state.queue.pop_front();
            state.busy = next.is_some();
            next
        };
        match next {
            Some(job) => {
                self.update_status(job.message_id, agent_id, "done");
                self.spawn_turn(agent_id.to_string(), job);
            }
            None => self.set_busy(agent_id, false),
        }
    }

    /// Brings a result back to a familiar as a message that starts its next turn, when the person is
    /// still in the chat it came from (`home`). Otherwise, or when the turn can't start, it waits in
    /// `home` and reaches the familiar with its next turn.
    pub fn deliver(self: &Arc<Self>, agent_id: &str, home: Option<String>, text: String, source: &str) {
        let still_home = crate::chats::current(&self.db, agent_id).is_ok_and(|now| now == home);
        if still_home && self.send(agent_id, text.clone(), Some(source)).is_ok() {
            return;
        }
        if let Ok(row) = hold(&self.db, agent_id, home.as_deref(), &text, source) {
            if still_home {
                self.app.message(&row);
            }
        }
    }

    /// Runs one exchange in a side conversation: `asker`'s message goes to the familiar whose hidden
    /// chat it is, on that chat's own session, alongside whatever it's doing in its main chat.
    /// `handoff` is the request's row in the handoffs list, moved on as the exchange goes.
    pub async fn side_turn(self: &Arc<Self>, chat_id: &str, handoff: i64, asker: &str, message: &str) -> TurnOutcome {
        let outcome = self.side_exchange(chat_id, handoff, asker, message).await;
        self.move_handoff(handoff, crate::handoffs::Status::of(&outcome));
        outcome
    }

    fn move_handoff(&self, id: i64, status: crate::handoffs::Status) {
        let _ = crate::handoffs::set(&self.db, id, status);
        let _ = self.app.emit(crate::handoffs::CHANGED, ());
    }

    async fn side_exchange(self: &Arc<Self>, chat_id: &str, handoff: i64, asker: &str, message: &str) -> TurnOutcome {
        let Ok(Some(chat)) = crate::side::get(&self.db, chat_id) else {
            return TurnOutcome::Failed("That conversation was deleted.".into());
        };
        let key = crate::side::lane_key(&chat.target_id, &chat.id);
        let lock = match self.sides.lock() {
            Ok(mut sides) => sides.entry(key.clone()).or_default().clone(),
            Err(_) => return TurnOutcome::Failed("runtime state poisoned".into()),
        };
        let _turn = lock.lock().await;
        let sink = SideSink { app: self.app.clone(), chat_id: chat.id.clone() };
        let source = format!("From {asker}");
        match self.db.insert_message_in(&chat.id, &chat.target_id, "user", message, "done", Some(&source)) {
            Ok(row) => sink.message(&row),
            Err(e) => return TurnOutcome::Failed(e),
        }
        let agent = match self.db.get_agent(&chat.target_id) {
            Ok(Some(agent)) => agent,
            _ => return TurnOutcome::Failed("That familiar no longer exists.".into()),
        };
        // Read again under the lock: the previous exchange may have saved the session.
        let session = crate::side::get(&self.db, &chat.id).ok().flatten().and_then(|c| c.session_id);
        let mut writer = TurnWriter::for_side(&self.db, &sink, &agent, &chat.id, session.clone());
        if let Err(reason) = crate::budgets::check_turn(&self.db, &agent.id) {
            return writer.fail(&reason);
        }
        if let Err(outcome) = gate(&mut writer, &self.db, &agent, crate::remote::quick_setup).await {
            return outcome;
        }
        let lane = Lane { key, session: lane_session(session, agent.session_id.clone()) };
        let prompt = crate::side::framed(asker, message);
        let (tx, rx) = mpsc::unbounded_channel();
        let started = match agent.provider {
            Provider::Codex => self.codex_for(&agent).start_turn_in(&agent, Some(&lane), &prompt, &[], tx).await,
            Provider::Claude => self.claude.start_turn_in(&agent, Some(&lane), &prompt, &[], tx).await,
        };
        match started {
            Ok(()) => {
                self.move_handoff(handoff, crate::handoffs::Status::Accepted);
                stream_turn(&mut writer, rx).await
            }
            Err(e) => writer.fail_explained(e).await,
        }
    }

    /// Runs a background helper's one turn in its hidden chat (see `helpers`): `task` is shown in the
    /// transcript and `prompt` goes to the CLI, on a fresh session in the familiar's own settings.
    pub async fn helper_turn(self: &Arc<Self>, chat_id: &str, task: &str, prompt: &str, startup: &crate::helpers::Startup) -> TurnOutcome {
        let turn = Hidden { chat_id, shown: task, source: None, prompt, session: LaneSession::Fresh, workspace_only: false, origin: "you" };
        self.hidden_turn(turn, startup).await
    }

    /// Runs one turn in a familiar's hidden chat for a caller that keeps its own record of it: a helper,
    /// a check-in, or a memory review (see `Hidden`).
    pub async fn hidden_turn(self: &Arc<Self>, turn: Hidden<'_>, startup: &crate::helpers::Startup) -> TurnOutcome {
        let Ok(Some(chat)) = crate::side::get(&self.db, turn.chat_id) else {
            return TurnOutcome::Failed("The hidden chat was deleted.".into());
        };
        let agent = match self.db.get_agent(&chat.target_id) {
            Ok(Some(agent)) if turn.workspace_only => crate::checkins::clamped(agent),
            Ok(Some(agent)) => agent,
            _ => return TurnOutcome::Failed("That familiar no longer exists.".into()),
        };
        let sink = SideSink { app: self.app.clone(), chat_id: chat.id.clone() };
        let source = turn.source.map_or_else(|| format!("From {}", agent.name), String::from);
        match self.db.insert_message_in(&chat.id, &agent.id, "user", turn.shown, "done", Some(&source)) {
            Ok(row) => sink.message(&row),
            Err(e) => return TurnOutcome::Failed(e),
        }
        let mut writer = TurnWriter::for_side(&self.db, &sink, &agent, &chat.id, None);
        writer.origin = turn.origin;
        if let Err(reason) = crate::budgets::check_turn(&self.db, &agent.id) {
            return writer.fail(&reason);
        }
        if let Err(outcome) = gate(&mut writer, &self.db, &agent, crate::remote::quick_setup).await {
            return outcome;
        }
        let prompt = turn.prompt;
        let lane = Lane { key: crate::side::lane_key(&agent.id, &chat.id), session: turn.session };
        if !startup.begin() {
            return TurnOutcome::Stopped;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let started = match agent.provider {
            Provider::Codex => self.codex.start_turn_in(&agent, Some(&lane), prompt, &[], tx).await,
            Provider::Claude => self.claude.start_turn_in(&agent, Some(&lane), prompt, &[], tx).await,
        };
        startup.started();
        match started {
            Ok(()) => stream_turn(&mut writer, rx).await,
            Err(e) => writer.fail_explained(e).await,
        }
    }

    /// Starts `agent`'s turn under `lane` for a caller that keeps its own rows, like a room. Checks the
    /// familiar's Mac first when it runs on another one.
    pub async fn start_lane(&self, agent: &Agent, lane: &Lane, prompt: &str, tx: mpsc::UnboundedSender<ProviderEvent>) -> Result<(), String> {
        crate::remote::ready(&self.db, agent, crate::remote::quick_setup, crate::remote::probe).await?;
        match agent.provider {
            Provider::Codex => self.codex_for(agent).start_turn_in(agent, Some(lane), prompt, &[], tx).await,
            Provider::Claude => self.claude.start_turn_in(agent, Some(lane), prompt, &[], tx).await,
        }
    }

    /// Interrupts whatever turn runs under a lane key, on either provider.
    /// Covers every Codex app server, since a familiar on another Mac runs its lanes on that Mac's.
    pub async fn stop_lane(&self, key: &str) {
        for codex in self.all_codex() {
            let _ = codex.stop(key).await;
        }
        self.claude.stop(key).await;
    }

    /// Lets go of a lane's CLI process or thread once nothing will run in it again.
    pub fn forget_lane(&self, key: &str) {
        for codex in self.all_codex() {
            codex.forget(key);
        }
        self.claude.forget(key);
    }

    async fn run_turn(self: &Arc<Self>, agent_id: &str, text: &str, source: Option<&str>, attachments: &[Attachment]) -> TurnOutcome {
        let agent = match self.db.get_agent(agent_id) {
            Ok(Some(agent)) => agent,
            _ => return TurnOutcome::Failed("That familiar no longer exists.".into()),
        };
        let mut writer = TurnWriter::new(&self.db, &self.app, &agent);
        writer.origin = origin_of(source);
        self.take_stop(agent_id);
        if let Err(outcome) = gate(&mut writer, &self.db, &agent, crate::remote::quick_setup).await {
            return outcome;
        }
        // A stop that came while the gate checked the other Mac found no CLI turn to interrupt.
        if self.take_stop(agent_id) {
            return TurnOutcome::Stopped;
        }
        let mut input = match crate::attachments::turn_input(std::path::Path::new(&agent.workspace), text, attachments) {
            Ok(input) => input,
            Err(e) => return writer.fail(&e),
        };
        let (notes, held) = turn_notes(&self.db, &agent);
        if !notes.is_empty() {
            input.text = format!("{}\n\n{}", notes.join("\n\n"), input.text);
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let started = match agent.provider {
            Provider::Codex => self.codex_for(&agent).start_turn(&agent, &input.text, &input.images, tx).await,
            Provider::Claude => self.claude.start_turn(&agent, &input.text, &input.images, tx).await,
        };
        match started {
            Ok(()) => {
                clear_notes(&self.db, &held);
                // A stop that came while the turn was starting found nothing to interrupt yet.
                if self.take_stop(agent_id) {
                    match agent.provider {
                        Provider::Codex => drop(self.codex_for(&agent).stop(agent_id).await),
                        Provider::Claude => self.claude.stop(agent_id).await,
                    }
                }
                stream_turn(&mut writer, rx).await
            }
            Err(e) => writer.fail_explained(e).await,
        }
    }
}

/// One turn in a familiar's hidden chat, on a session of its own.
pub struct Hidden<'a> {
    pub chat_id: &'a str,
    /// What the transcript shows as the first message, under `source`, or "From <familiar>" when None.
    pub shown: &'a str,
    pub source: Option<&'a str>,
    /// What goes to the CLI.
    pub prompt: &'a str,
    pub session: LaneSession,
    /// Runs with workspace-only access whatever the familiar has, for turns nobody watches.
    pub workspace_only: bool,
    /// Who the usage counts as: "you", "ritual", or "spell".
    pub origin: &'static str,
}

/// Stops a turn before anything spawns when the familiar's Mac can't be reached, with a plain error
/// row in the chat. A familiar saved while its Mac was offline finishes its setup here first.
async fn gate<F, Fut>(writer: &mut TurnWriter<'_>, db: &Db, agent: &Agent, setup: F) -> Result<(), TurnOutcome>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<(), crate::remote::SshError>>,
{
    crate::remote::ready(db, agent, setup, writer.probe).await.map_err(|reason| writer.fail(&reason))
}

/// A side conversation carries on its own session, or starts from a copy of the familiar's main one.
fn lane_session(side: Option<String>, main: Option<String>) -> LaneSession {
    match (side, main) {
        (Some(side), _) => LaneSession::Resume(side),
        (None, Some(main)) => LaneSession::Fork(main),
        (None, None) => LaneSession::Fresh,
    }
}

pub async fn stream_turn(writer: &mut TurnWriter<'_>, mut rx: mpsc::UnboundedReceiver<ProviderEvent>) -> TurnOutcome {
    let mut ui_due: Option<Instant> = None;
    loop {
        let event = tokio::select! {
            event = rx.recv() => event,
            _ = sleep_until(ui_due), if ui_due.is_some() => {
                writer.flush_ui();
                ui_due = None;
                continue;
            }
        };
        match event {
            Some(ProviderEvent::Delta(chunk)) => {
                writer.append(&chunk);
                ui_due.get_or_insert_with(|| Instant::now() + UI_FLUSH);
            }
            Some(ProviderEvent::Tool(label)) => writer.tool(&label),
            Some(ProviderEvent::Session(id)) => writer.session(&id),
            Some(ProviderEvent::Model(model)) => writer.model(&model),
            Some(ProviderEvent::Usage { input, output }) => writer.usage = Some((input, output)),
            Some(ProviderEvent::Limits(info)) => {
                let snapshot = serde_json::json!({ "info": info, "at": crate::db::now_ms() });
                let _ = writer.db.set_setting(CLAUDE_LIMITS, &snapshot.to_string());
            }
            Some(ProviderEvent::Done) => return writer.close("done"),
            Some(ProviderEvent::Stopped) => return writer.close("stopped"),
            Some(ProviderEvent::Failed(reason)) => return writer.fail_explained(reason).await,
            None => return writer.fail_explained("The familiar stopped without finishing its reply.".into()).await,
        }
    }
}

async fn sleep_until(due: Option<Instant>) {
    if let Some(due) = due {
        tokio::time::sleep_until(due).await;
    }
}

struct Segment {
    message: Message,
    unsent: String,
    saved_at: Instant,
}

pub struct TurnWriter<'a> {
    db: &'a Db,
    sink: &'a dyn Sink,
    agent_id: String,
    session: Option<String>,
    active_model: Option<String>,
    segment: Option<Segment>,
    last_text: String,
    /// Whether the latest row this turn wrote is a reply, not a tool.
    replied: bool,
    provider: Provider,
    /// "you", "ritual", or "spell".
    pub origin: &'static str,
    usage: Option<(u64, u64)>,
    /// The side conversation this turn writes to, instead of the familiar's current chat.
    chat_id: Option<String>,
    /// The familiar's name and host when it runs on another Mac, to explain an ssh failure plainly.
    remote: Option<(String, String)>,
    /// How a remote familiar's turn checks whether its Mac answers. `remote::probe` outside tests.
    probe: crate::remote::Probe,
}

/// The row a side conversation gets when a turn finishes without a reply after its last tool.
const NO_REPLY: &str = "(no reply)";

/// The settings key for Claude's latest plan limit snapshot.
pub const CLAUDE_LIMITS: &str = "claude_limits";

impl<'a> TurnWriter<'a> {
    pub fn new(db: &'a Db, sink: &'a dyn Sink, agent: &Agent) -> Self {
        TurnWriter {
            db,
            sink,
            agent_id: agent.id.clone(),
            session: agent.session_id.clone(),
            active_model: agent.active_model.clone(),
            segment: None,
            last_text: String::new(),
            replied: false,
            provider: agent.provider.clone(),
            origin: "you",
            usage: None,
            chat_id: None,
            remote: agent.host.clone().map(|host| (agent.name.clone(), host)),
            probe: crate::remote::probe,
        }
    }

    /// A writer for a side conversation's turn: its rows and its session stay on that chat.
    pub fn for_side(db: &'a Db, sink: &'a dyn Sink, agent: &Agent, chat_id: &str, session: Option<String>) -> Self {
        TurnWriter { session, chat_id: Some(chat_id.to_string()), ..TurnWriter::new(db, sink, agent) }
    }

    fn insert(&self, role: &str, text: &str, status: &str) -> Result<Message, String> {
        match &self.chat_id {
            Some(chat) => self.db.insert_message_in(chat, &self.agent_id, role, text, status, None),
            None => self.db.insert_message(&self.agent_id, role, text, status),
        }
    }

    fn record_usage(&mut self) {
        if let Some((input, output)) = self.usage.take() {
            let _ = self.db.insert_usage(&self.agent_id, &self.provider, self.origin, input, output);
        }
    }

    fn append(&mut self, chunk: &str) {
        let segment = match self.segment.as_mut() {
            Some(segment) => segment,
            None => {
                let chunk = chunk.trim_start();
                if chunk.is_empty() {
                    return;
                }
                let Ok(message) = self.insert("assistant", chunk, "streaming") else {
                    return;
                };
                self.sink.message(&message);
                self.segment = Some(Segment { message, unsent: String::new(), saved_at: Instant::now() });
                return;
            }
        };
        segment.message.text.push_str(chunk);
        segment.unsent.push_str(chunk);
        if segment.saved_at.elapsed() >= DB_FLUSH {
            let _ = self.db.update_message(segment.message.id, &segment.message.text, "streaming");
            segment.saved_at = Instant::now();
        }
    }

    fn flush_ui(&mut self) {
        if let Some(segment) = self.segment.as_mut().filter(|s| !s.unsent.is_empty()) {
            self.sink.delta(&self.agent_id, segment.message.id, &segment.unsent);
            segment.unsent.clear();
        }
    }

    fn end_segment(&mut self, status: &str) {
        if let Some(mut segment) = self.segment.take() {
            segment.message.text.truncate(segment.message.text.trim_end().len());
            segment.message.status = status.to_string();
            segment.message.updated_at = Some(crate::db::now_ms());
            let _ = self.db.update_message(segment.message.id, &segment.message.text, status);
            self.sink.message(&segment.message);
            self.last_text = segment.message.text;
            self.replied = true;
        }
    }

    fn tool(&mut self, label: &str) {
        self.end_segment("done");
        self.replied = false;
        // An empty label is a tool that adds its own row, like asking another familiar.
        if label.is_empty() {
            return;
        }
        if let Ok(message) = self.insert("tool", label, "done") {
            self.sink.message(&message);
        }
    }

    fn model(&mut self, model: &str) {
        if self.active_model.as_deref() != Some(model) {
            let _ = self.db.set_active_model(&self.agent_id, model);
            self.active_model = Some(model.to_string());
            self.sink.model(&self.agent_id, model);
        }
    }

    fn session(&mut self, id: &str) {
        if self.session.as_deref() != Some(id) {
            let _ = match &self.chat_id {
                Some(chat) => self.db.set_chat_session(chat, id),
                None => self.db.set_session(&self.agent_id, Some(id)),
            };
            self.session = Some(id.to_string());
        }
    }

    fn close(&mut self, status: &str) -> TurnOutcome {
        self.end_segment(status);
        // A side conversation shows a turn that ended after a tool, or with no text at all, as finished.
        if status == "done" && self.chat_id.is_some() && !self.replied {
            if let Ok(message) = self.insert("assistant", NO_REPLY, "done") {
                self.sink.message(&message);
            }
        }
        self.record_usage();
        if let Some((_, host)) = &self.remote {
            crate::remote::set_offline(host, false);
        }
        if status == "stopped" {
            TurnOutcome::Stopped
        } else {
            TurnOutcome::Done(std::mem::take(&mut self.last_text))
        }
    }

    /// `fail` for a provider's own failure. On another Mac, a lost connection reads as the plain
    /// offline message, not what ssh printed.
    async fn fail_explained(&mut self, reason: String) -> TurnOutcome {
        let reason = match &self.remote {
            Some((name, host)) => crate::remote::explain(name, host, reason, self.probe).await,
            None => reason,
        };
        self.fail(&reason)
    }

    fn fail(&mut self, reason: &str) -> TurnOutcome {
        self.end_segment("done");
        self.record_usage();
        let reason = reason.trim().to_string();
        if let Ok(message) = self.insert("error", &reason, "done") {
            self.sink.message(&message);
        }
        TurnOutcome::Failed(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{now_ms, Access};

    #[derive(Default)]
    struct Recorder {
        messages: Mutex<Vec<Message>>,
        deltas: Mutex<Vec<(i64, String)>>,
    }

    impl Sink for Recorder {
        fn message(&self, message: &Message) {
            self.messages.lock().unwrap().push(message.clone());
        }
        fn delta(&self, _agent_id: &str, message_id: i64, chunk: &str) {
            self.deltas.lock().unwrap().push((message_id, chunk.to_string()));
        }
        fn model(&self, _agent_id: &str, _model: &str) {}
    }

    #[test]
    fn tells_who_started_a_turn() {
        assert_eq!(origin_of(None), "you");
        assert_eq!(origin_of(Some(&format!("{RITUAL_SOURCE}Morning summary"))), "ritual");
        assert_eq!(origin_of(Some(&format!("{SPELL_SOURCE}Topic brief"))), "spell");
        assert_eq!(origin_of(Some("Something else")), "you");
    }

    #[tokio::test]
    async fn records_a_turns_usage_once_it_ends() {
        let (db, agent) = setup();
        let sink = Recorder::default();
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(ProviderEvent::Usage { input: 10, output: 2 }).unwrap();
        tx.send(ProviderEvent::Usage { input: 30, output: 5 }).unwrap();
        tx.send(ProviderEvent::Done).unwrap();
        let mut writer = TurnWriter::new(&db, &sink, &agent);
        writer.origin = "spell";
        stream_turn(&mut writer, rx).await;
        let rows = db.usage_since(0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].input, rows[0].output, rows[0].origin.as_str()), (30, 5, "spell"));
    }

    fn setup() -> (Db, Agent) {
        let db = Db::open_in_memory().unwrap();
        let agent = Agent {
            id: "a".into(),
            name: "Ada".into(),
            provider: Provider::Claude,
            model: None,
            workspace: "/tmp".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: crate::db::Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        (db, agent)
    }

    #[tokio::test(start_paused = true)]
    async fn splits_text_around_tools_and_saves_session() {
        let (db, agent) = setup();
        let sink = Recorder::default();
        let (tx, rx) = mpsc::unbounded_channel();
        for event in [
            ProviderEvent::Session("s1".into()),
            ProviderEvent::Model("gpt-6.2-sol".into()),
            ProviderEvent::Delta("Let me ".into()),
            ProviderEvent::Delta("check.".into()),
            ProviderEvent::Tool("$ ls".into()),
            ProviderEvent::Delta("\n\n".into()),
            ProviderEvent::Delta("Found it.\n".into()),
            ProviderEvent::Done,
        ] {
            tx.send(event).unwrap();
        }
        let outcome = stream_turn(&mut TurnWriter::new(&db, &sink, &agent), rx).await;
        assert_eq!(outcome, TurnOutcome::Done("Found it.".into()));

        let saved = db.list_messages("a", None, 10).unwrap();
        let rows: Vec<(&str, &str, &str)> =
            saved.iter().map(|m| (m.role.as_str(), m.text.as_str(), m.status.as_str())).collect();
        assert_eq!(
            rows,
            [("assistant", "Let me check.", "done"), ("tool", "$ ls", "done"), ("assistant", "Found it.", "done")]
        );
        assert_eq!(db.get_agent("a").unwrap().unwrap().session_id.as_deref(), Some("s1"));
        assert_eq!(db.get_agent("a").unwrap().unwrap().active_model.as_deref(), Some("gpt-6.2-sol"));
    }

    #[tokio::test(start_paused = true)]
    async fn side_turns_write_to_their_own_chat() {
        let (db, agent) = setup();
        db.insert_message("a", "user", "Main chat", "done").unwrap();
        db.set_session("a", Some("main-session")).unwrap();
        let other = Agent { id: "b".into(), name: "Bo".into(), ..agent.clone() };
        db.insert_agent(&other).unwrap();
        let side = crate::side::create(&db, "b", "a", "hello").unwrap();
        let sink = Recorder::default();
        let (tx, rx) = mpsc::unbounded_channel();
        for event in [
            ProviderEvent::Session("side-session".into()),
            ProviderEvent::Delta("Hi ".into()),
            ProviderEvent::Tool(String::new()),
            ProviderEvent::Tool("$ ls".into()),
            ProviderEvent::Delta("Bo.".into()),
            ProviderEvent::Done,
        ] {
            tx.send(event).unwrap();
        }
        let mut writer = TurnWriter::for_side(&db, &sink, &agent, &side.id, None);
        assert_eq!(stream_turn(&mut writer, rx).await, TurnOutcome::Done("Bo.".into()));

        let rows: Vec<(String, String)> = crate::side::messages(&db, &side.id).unwrap().into_iter().map(|m| (m.role, m.text)).collect();
        assert_eq!(rows, [("assistant".into(), "Hi".into()), ("tool".into(), "$ ls".into()), ("assistant".into(), "Bo.".into())]);
        let main: Vec<String> = db.list_messages("a", None, 10).unwrap().into_iter().map(|m| m.text).collect();
        assert_eq!(main, ["Main chat"], "the main chat is untouched");
        assert_eq!(db.get_agent("a").unwrap().unwrap().session_id.as_deref(), Some("main-session"));
        assert_eq!(crate::side::get(&db, &side.id).unwrap().unwrap().session_id.as_deref(), Some("side-session"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_side_turn_that_ends_without_a_reply_says_so() {
        let (db, agent) = setup();
        let side = crate::side::create(&db, "a", "a", "hello").unwrap();
        let sink = Recorder::default();
        let rows = |events: Vec<ProviderEvent>, chat: Option<&str>| {
            let (tx, rx) = mpsc::unbounded_channel();
            for event in events {
                tx.send(event).unwrap();
            }
            let mut writer = match chat {
                Some(chat) => TurnWriter::for_side(&db, &sink, &agent, chat, None),
                None => TurnWriter::new(&db, &sink, &agent),
            };
            async move { stream_turn(&mut writer, rx).await }
        };
        assert_eq!(rows(vec![ProviderEvent::Done], Some(&side.id)).await, TurnOutcome::Done(String::new()));
        assert_eq!(rows(vec![ProviderEvent::Tool("$ ls".into()), ProviderEvent::Done], Some(&side.id)).await, TurnOutcome::Done(String::new()));
        let got: Vec<(String, String, String)> = crate::side::messages(&db, &side.id).unwrap().into_iter().map(|m| (m.role, m.text, m.status)).collect();
        let row = |role: &str, text: &str| (role.to_string(), text.to_string(), "done".to_string());
        assert_eq!(got, [row("assistant", NO_REPLY), row("tool", "$ ls"), row("assistant", NO_REPLY)]);
        rows(vec![ProviderEvent::Done], None).await;
        assert!(db.list_messages("a", None, 10).unwrap().is_empty(), "the main chat gets no placeholder");
    }

    #[test]
    fn held_messages_reach_the_next_turn_once() {
        let (db, agent) = setup();
        assert_eq!(turn_notes(&db, &agent), (vec![], vec![]));
        let row = hold(&db, "a", None, "Scribe replied to your message:\n\nDone.", "Reply from Scribe").unwrap();
        assert_eq!((row.role.as_str(), row.source.as_deref()), ("user", Some("Reply from Scribe")), "the person sees it");
        hold(&db, "a", None, "Helper tests finished.", "Helper tests").unwrap();
        let (notes, held) = turn_notes(&db, &agent);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].contains("Reply from Scribe") && notes[0].ends_with("Done."), "{}", notes[0]);
        assert!(notes[1].ends_with("Helper tests finished."), "{}", notes[1]);
        // A turn that fails to start leaves them for the next one; one that starts clears them.
        assert_eq!(turn_notes(&db, &agent).0, notes);
        clear_notes(&db, &held);
        assert_eq!(turn_notes(&db, &agent), (vec![], vec![]));
    }

    #[test]
    fn held_messages_wait_for_their_own_chat() {
        let (db, agent) = setup();
        let first = crate::chats::start(&db, "a").unwrap().id;
        hold(&db, "a", Some(&first), "Reply for the first chat.", "Reply from Scribe").unwrap();
        let second = crate::chats::start(&db, "a").unwrap().id;
        assert!(turn_notes(&db, &agent).0.is_empty(), "a turn in another chat doesn't take it");
        crate::chats::open(&db, "a", &first).unwrap();
        assert_eq!(turn_notes(&db, &agent).0.len(), 1, "it waits for the first chat");
        // A deleted home chat sends it to the current chat instead of losing it.
        crate::chats::open(&db, "a", &second).unwrap();
        crate::chats::delete(&db, &first).unwrap();
        hold(&db, "a", Some(&first), "Reply after the chat was deleted.", "Reply from Scribe").unwrap();
        let notes = turn_notes(&db, &agent).0;
        assert!(notes.len() == 1 && notes[0].ends_with("Reply after the chat was deleted."), "{notes:?}");
    }

    #[test]
    fn side_conversations_fork_the_main_session_once() {
        assert_eq!(lane_session(None, Some("m".into())), LaneSession::Fork("m".into()));
        assert_eq!(lane_session(Some("s".into()), Some("m".into())), LaneSession::Resume("s".into()));
        assert_eq!(lane_session(None, None), LaneSession::Fresh);
    }

    #[tokio::test(start_paused = true)]
    async fn batches_deltas_for_the_ui() {
        let (db, agent) = setup();
        let sink = Recorder::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let feeder = tokio::spawn(async move {
            tx.send(ProviderEvent::Delta("a".into())).unwrap();
            for chunk in ["b", "c", "d"] {
                tx.send(ProviderEvent::Delta(chunk.into())).unwrap();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            tx.send(ProviderEvent::Delta("e".into())).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            tx.send(ProviderEvent::Done).unwrap();
        });
        stream_turn(&mut TurnWriter::new(&db, &sink, &agent), rx).await;
        feeder.await.unwrap();
        let chunks: Vec<String> = sink.deltas.lock().unwrap().iter().map(|(_, c)| c.clone()).collect();
        assert_eq!(chunks, ["bcd", "e"]);
        assert_eq!(db.list_messages("a", None, 1).unwrap()[0].text, "abcde");
    }

    async fn live_turn(provider: Provider, prompt: &str) -> (Db, Vec<Message>) {
        let workspace = std::env::temp_dir().join(format!("grimoire-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let db = Db::open_in_memory().unwrap();
        let agent = Agent {
            id: "live".into(),
            name: "Live".into(),
            provider: provider.clone(),
            model: None,
            workspace: workspace.to_string_lossy().into_owned(),
            access: Access::Workspace,
            session_id: None,
            avatar: crate::db::Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        let sink = Recorder::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let codex = Arc::new(Codex::default());
        let claude = Claude::default();
        match provider {
            Provider::Codex => codex.start_turn(&agent, prompt, &[], tx).await.unwrap(),
            Provider::Claude => claude.start_turn(&agent, prompt, &[], tx).await.unwrap(),
        }
        stream_turn(&mut TurnWriter::new(&db, &sink, &agent), rx).await;
        claude.forget("live");
        let messages = db.list_messages("live", None, 50).unwrap();
        assert!(workspace.join("notes.md").exists(), "agent did not write notes.md: {messages:#?}");
        std::fs::remove_dir_all(workspace).unwrap();
        (db, messages)
    }

    const LIVE_PROMPT: &str = "Create a file named notes.md containing the line `- [ ] test`. Then reply with the single word done.";

    fn assert_live(db: &Db, messages: &[Message]) {
        let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
        assert!(roles.contains(&"tool"), "no tool rows: {messages:#?}");
        assert!(!roles.contains(&"error"), "turn failed: {messages:#?}");
        let last = messages.iter().rev().find(|m| m.role == "assistant").expect("no reply");
        assert_eq!(last.status, "done");
        assert!(last.text.to_lowercase().contains("done"), "unexpected reply: {}", last.text);
        assert!(db.get_agent("live").unwrap().unwrap().session_id.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real codex CLI"]
    async fn live_codex_turn() {
        let (db, messages) = live_turn(Provider::Codex, LIVE_PROMPT).await;
        assert_live(&db, &messages);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real claude CLI"]
    async fn live_claude_turn() {
        let (db, messages) = live_turn(Provider::Claude, LIVE_PROMPT).await;
        assert_live(&db, &messages);
    }

    /// Sends a PNG with the word MARMALADE on it and returns the reply.
    async fn live_image_reply(provider: Provider, model: &str) -> String {
        let workspace = std::env::temp_dir().join(format!("grimoire-live-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let image = workspace.join("word.png");
        std::fs::write(&image, include_bytes!("testdata/word.png")).unwrap();
        let db = Db::open_in_memory().unwrap();
        let agent = Agent {
            id: "live".into(),
            name: "Live".into(),
            provider: provider.clone(),
            model: Some(model.into()),
            workspace: workspace.to_string_lossy().into_owned(),
            access: Access::Workspace,
            session_id: None,
            avatar: crate::db::Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        let (codex, claude) = (Arc::new(Codex::default()), Claude::default());
        let (tx, rx) = mpsc::unbounded_channel();
        let prompt = "Without using any tools, what word is written in the attached image? Reply with just the word.";
        match provider {
            Provider::Codex => codex.start_turn(&agent, prompt, &[image], tx).await.unwrap(),
            Provider::Claude => claude.start_turn(&agent, prompt, &[image], tx).await.unwrap(),
        }
        let outcome = stream_turn(&mut TurnWriter::new(&db, &Recorder::default(), &agent), rx).await;
        claude.forget("live");
        let tools: Vec<String> = db.list_messages("live", None, 50).unwrap().into_iter().filter(|m| m.role == "tool").map(|m| m.text).collect();
        assert!(tools.is_empty(), "the model used tools instead of the image block: {tools:?}");
        std::fs::remove_dir_all(workspace).unwrap();
        match outcome {
            TurnOutcome::Done(text) => {
                eprintln!("{provider:?} replied: {text}");
                text
            }
            other => panic!("turn did not finish: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real codex CLI"]
    async fn live_codex_sees_an_image() {
        let reply = live_image_reply(Provider::Codex, "gpt-6.2-sol").await;
        assert!(reply.to_uppercase().contains("MARMALADE"), "unexpected reply: {reply}");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real claude CLI"]
    async fn live_claude_sees_an_image() {
        let reply = live_image_reply(Provider::Claude, "haiku").await;
        assert!(reply.to_uppercase().contains("MARMALADE"), "unexpected reply: {reply}");
    }

    #[tokio::test(start_paused = true)]
    async fn records_failures_and_closed_channels() {
        let (db, agent) = setup();
        let sink = Recorder::default();
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(ProviderEvent::Delta("partial".into())).unwrap();
        drop(tx);
        stream_turn(&mut TurnWriter::new(&db, &sink, &agent), rx).await;
        let saved = db.list_messages("a", None, 10).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!((saved[0].text.as_str(), saved[0].status.as_str()), ("partial", "done"));
        assert_eq!(saved[1].role, "error");
    }

    #[tokio::test]
    async fn a_turn_for_an_offline_familiar_says_so_plainly() {
        let (db, local) = setup();
        let agent = Agent { id: "r".into(), host: Some("gate-test.local".into()), workspace: "~/w".into(), ..local };
        db.insert_agent(&agent).unwrap();
        db.set_setup_pending("r", true).unwrap();
        let sink = Recorder::default();
        let mut writer = TurnWriter::new(&db, &sink, &agent);
        let gated = gate(&mut writer, &db, &agent, |_, _| async { Err(crate::remote::SshError::Offline) }).await;
        let expected = "Ada runs on gate-test.local, and that Mac is offline. Grimoire will finish setting it up when it connects.";
        assert_eq!(gated, Err(TurnOutcome::Failed(expected.into())), "the turn stops before anything spawns");
        let rows: Vec<(String, String)> = db.list_messages("r", None, 10).unwrap().into_iter().map(|m| (m.role, m.text)).collect();
        assert_eq!(rows, [("error".to_string(), expected.to_string())]);
        assert_eq!(sink.messages.lock().unwrap().len(), 1, "the chat shows it");

        // ssh failing once the CLI starts is explained the same way, without the raw ssh line.
        crate::remote::set_offline("gate-test.local", false);
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(ProviderEvent::Failed("ssh: connect to host gate-test.local port 22: Operation timed out".into())).unwrap();
        let outcome = stream_turn(&mut TurnWriter::new(&db, &sink, &agent), rx).await;
        let later = "Ada runs on gate-test.local, and that Mac is offline. Try again when it's back.";
        assert_eq!(outcome, TurnOutcome::Failed(later.into()));
        assert!(crate::remote::is_offline("gate-test.local"));

        fn down(_: String) -> crate::remote::ProbeFuture {
            Box::pin(async { Err(crate::remote::SshError::Offline) })
        }
        // A familiar that's set up, on a Mac that went offline since its last turn, gets checked first.
        db.set_setup_pending("r", false).unwrap();
        let set_up = Agent { id: "s".into(), host: Some("first-turn-test.local".into()), ..agent.clone() };
        db.insert_agent(&set_up).unwrap();
        let mut writer = TurnWriter::new(&db, &sink, &set_up);
        writer.probe = down;
        let first = "Ada runs on first-turn-test.local, and that Mac is offline. Try again when it's back.";
        let no_setup = |_, _| async { panic!("only a pending familiar runs setup") };
        assert_eq!(gate(&mut writer, &db, &set_up, no_setup).await, Err(TurnOutcome::Failed(first.into())));

        // A CLI that ends without a word from ssh gets the Mac checked, and a lost Mac says so.
        crate::remote::set_offline("first-turn-test.local", false);
        for event in [Some(ProviderEvent::Failed("claude stopped unexpectedly.".into())), None] {
            let (tx, rx) = mpsc::unbounded_channel();
            if let Some(event) = event {
                tx.send(event).unwrap();
            }
            drop(tx);
            let mut writer = TurnWriter::new(&db, &sink, &set_up);
            writer.probe = down;
            assert_eq!(stream_turn(&mut writer, rx).await, TurnOutcome::Failed(first.into()));
        }

        // A local familiar's turn never waits on ssh.
        let mut writer = TurnWriter::new(&db, &sink, &setup().1);
        assert_eq!(gate(&mut writer, &db, &setup().1, |_, _| async { Err(crate::remote::SshError::Offline) }).await, Ok(()));
    }
}
