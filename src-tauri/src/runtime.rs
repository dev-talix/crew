use crate::db::{Agent, Db, Message, Provider};
use crate::providers::{claude::Claude, codex::Codex, ProviderEvent};
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
    reply: Option<Reply>,
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
}

pub struct Runtime {
    db: Arc<Db>,
    app: AppHandle,
    codex: Arc<Codex>,
    claude: Claude,
    agents: Mutex<HashMap<String, AgentState>>,
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
    }

    fn delta(&self, agent_id: &str, message_id: i64, chunk: &str) {
        let _ = self.emit("delta", DeltaPayload { agent_id, message_id, chunk });
    }

    fn model(&self, agent_id: &str, model: &str) {
        let _ = self.emit("agent_model", ModelPayload { agent_id, model });
    }
}

impl Runtime {
    pub fn new(db: Arc<Db>, app: AppHandle) -> Arc<Self> {
        Arc::new(Runtime {
            db,
            app,
            codex: Arc::new(Codex::default()),
            claude: Claude::default(),
            agents: Mutex::new(HashMap::new()),
        })
    }

    pub fn busy_agents(&self) -> Vec<String> {
        self.agents
            .lock()
            .map(|a| a.iter().filter(|(_, s)| s.busy).map(|(id, _)| id.clone()).collect())
            .unwrap_or_default()
    }

    fn set_busy(&self, agent_id: &str, busy: bool) {
        let _ = self.app.emit("agent_status", StatusPayload { agent_id, busy });
    }

    pub fn send(self: &Arc<Self>, agent_id: &str, text: String, source: Option<&str>) -> Result<Message, String> {
        self.enqueue(agent_id, text, source, None)
    }

    pub async fn send_and_wait(self: &Arc<Self>, agent_id: &str, text: String, source: Option<&str>) -> TurnOutcome {
        let (tx, rx) = oneshot::channel();
        if let Err(e) = self.enqueue(agent_id, text, source, Some(tx)) {
            return TurnOutcome::Failed(e);
        }
        rx.await.unwrap_or(TurnOutcome::Stopped)
    }

    fn enqueue(self: &Arc<Self>, agent_id: &str, text: String, source: Option<&str>, reply: Option<Reply>) -> Result<Message, String> {
        if self.db.get_agent(agent_id)?.is_none() {
            return Err("That familiar no longer exists.".into());
        }
        let mut agents = self.agents.lock().map_err(|_| "runtime state poisoned")?;
        let state = agents.entry(agent_id.to_string()).or_default();
        let status = if state.busy { "queued" } else { "done" };
        let message = self.db.insert_message_from(agent_id, "user", &text, status, source)?;
        self.app.message(&message);
        let job = Job { message_id: message.id, text, source: source.map(String::from), reply };
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
            agents.get_mut(agent_id).map(|s| s.queue.drain(..).collect()).unwrap_or_default()
        };
        for job in cancelled {
            self.update_status(job.message_id, agent_id, "cancelled");
            if let Some(reply) = job.reply {
                let _ = reply.send(TurnOutcome::Stopped);
            }
        }
        match self.db.get_agent(agent_id)?.map(|a| a.provider) {
            Some(Provider::Codex) => self.codex.stop(agent_id).await,
            Some(Provider::Claude) => {
                self.claude.stop(agent_id).await;
                Ok(())
            }
            None => Ok(()),
        }
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
        self.codex.reload_thread(agent_id).await;
    }

    pub fn claude_billing_changed(&self) {
        self.claude.forget_idle();
    }

    pub fn reset_session(&self, agent_id: &str) {
        self.codex.forget(agent_id);
        self.claude.forget(agent_id);
    }

    pub async fn remove_agent(self: &Arc<Self>, agent_id: &str) -> Result<(), String> {
        self.stop(agent_id).await?;
        self.reset_session(agent_id);
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
            let outcome = runtime.run_turn(&agent_id, &job.text, job.source.as_deref()).await;
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

    async fn run_turn(self: &Arc<Self>, agent_id: &str, text: &str, source: Option<&str>) -> TurnOutcome {
        let agent = match self.db.get_agent(agent_id) {
            Ok(Some(agent)) => agent,
            _ => return TurnOutcome::Failed("That familiar no longer exists.".into()),
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let started = match agent.provider {
            Provider::Codex => self.codex.start_turn(&agent, text, tx).await,
            Provider::Claude => self.claude.start_turn(&agent, text, tx).await,
        };
        let mut writer = TurnWriter::new(&self.db, &self.app, &agent);
        writer.origin = origin_of(source);
        match started {
            Ok(()) => stream_turn(&mut writer, rx).await,
            Err(e) => writer.fail(&e),
        }
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
            Some(ProviderEvent::Failed(reason)) => return writer.fail(&reason),
            None => return writer.fail("The familiar stopped without finishing its reply."),
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
    provider: Provider,
    /// "you", "ritual", or "spell".
    pub origin: &'static str,
    usage: Option<(u64, u64)>,
}

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
            provider: agent.provider.clone(),
            origin: "you",
            usage: None,
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
                let Ok(message) = self.db.insert_message(&self.agent_id, "assistant", chunk, "streaming") else {
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
        }
    }

    fn tool(&mut self, label: &str) {
        self.end_segment("done");
        if let Ok(message) = self.db.insert_message(&self.agent_id, "tool", label, "done") {
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
            let _ = self.db.set_session(&self.agent_id, Some(id));
            self.session = Some(id.to_string());
        }
    }

    fn close(&mut self, status: &str) -> TurnOutcome {
        self.end_segment(status);
        self.record_usage();
        if status == "stopped" {
            TurnOutcome::Stopped
        } else {
            TurnOutcome::Done(std::mem::take(&mut self.last_text))
        }
    }

    fn fail(&mut self, reason: &str) -> TurnOutcome {
        self.end_segment("done");
        self.record_usage();
        if let Ok(message) = self.db.insert_message(&self.agent_id, "error", reason.trim(), "done") {
            self.sink.message(&message);
        }
        TurnOutcome::Failed(reason.trim().to_string())
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
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        let sink = Recorder::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let codex = Arc::new(Codex::default());
        let claude = Claude::default();
        match provider {
            Provider::Codex => codex.start_turn(&agent, prompt, tx).await.unwrap(),
            Provider::Claude => claude.start_turn(&agent, prompt, tx).await.unwrap(),
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
}
