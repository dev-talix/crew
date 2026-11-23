//! Familiars talking to each other behind the scenes. `ask_familiar` sends another familiar a message
//! and waits for its reply; `message_familiar` sends one and carries on, and the reply comes back later
//! as a new message. Either way the exchange runs in a side conversation (see `side`), out of both main
//! chats. Guard rails keep familiars from asking themselves, waiting on each other in a loop, chaining
//! questions too deep, or flooding each other with messages.

use crate::approvals::{self, Decision};
use crate::db::{Agent, Db};
use crate::runtime::TurnOutcome;
use crate::side::{self, SideChat};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long a familiar waits for a reply to `ask_familiar`.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The most questions in one chain: A asks B, B asks C, C asks D.
pub const MAX_DEPTH: usize = 3;
/// The most `message_familiar` calls one familiar makes in `WINDOW`, so two can't ping-pong forever.
pub const MAX_MESSAGES: usize = 5;
pub const WINDOW: Duration = Duration::from_secs(10 * 60);
/// The source label on a reply that comes back to the familiar who sent a message.
pub const REPLY_SOURCE: &str = "Reply from ";
const KEY: &str = "delegation";

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    #[default]
    Anyone,
    /// Only familiars in the same coven.
    Coven,
    Nobody,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub scope: Scope,
    /// Ask the person before each question or message goes out.
    pub approve: bool,
}

pub fn load(db: &Db) -> Settings {
    db.setting(KEY).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn save(db: &Db, settings: Settings) -> Result<Settings, String> {
    db.set_setting(KEY, &serde_json::to_string(&settings).map_err(|e| e.to_string())?)?;
    Ok(settings)
}

/// Questions in flight, as (asker, target) pairs: the asker is waiting on the target.
#[derive(Default)]
pub struct Chains {
    waits: Mutex<Vec<(String, String)>>,
}

/// Holds one question in `Chains` until it is dropped.
pub struct Wait<'a> {
    chains: &'a Chains,
    pair: (String, String),
}

impl Drop for Wait<'_> {
    fn drop(&mut self) {
        let mut waits = self.chains.waits.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = waits.iter().position(|w| *w == self.pair) {
            waits.remove(at);
        }
    }
}

impl Chains {
    /// Records that `asker` waits on `target`, unless that closes a loop or makes the chain too long.
    pub fn begin(&self, asker: &Agent, target: &Agent) -> Result<Wait<'_>, String> {
        let mut waits = self.waits.lock().unwrap_or_else(|e| e.into_inner());
        if reaches(&waits, &target.id, &asker.id) {
            return Err(format!(
                "{} is waiting on you, directly or through other familiars, so it can't answer until you do. \
                 Asking it would leave you both waiting. Reply with what you have instead.",
                target.name
            ));
        }
        if depth_into(&waits, &asker.id, 0) + 1 > MAX_DEPTH {
            return Err(format!(
                "This would be more than {MAX_DEPTH} familiars asking each other in a row, which is Grimoire's limit. \
                 Reply with what you have instead."
            ));
        }
        let pair = (asker.id.clone(), target.id.clone());
        waits.push(pair.clone());
        Ok(Wait { chains: self, pair })
    }
}

/// Whether `from` waits on `to`, directly or through others.
fn reaches(waits: &[(String, String)], from: &str, to: &str) -> bool {
    let mut stack = vec![from];
    let mut seen = std::collections::HashSet::new();
    while let Some(at) = stack.pop() {
        if at == to {
            return true;
        }
        if seen.insert(at) {
            stack.extend(waits.iter().filter(|(a, _)| a == at).map(|(_, t)| t.as_str()));
        }
    }
    false
}

/// The longest chain of questions that ends at `agent`.
fn depth_into(waits: &[(String, String)], agent: &str, hops: usize) -> usize {
    if hops > MAX_DEPTH {
        return hops;
    }
    waits.iter().filter(|(_, t)| t == agent).map(|(a, _)| 1 + depth_into(waits, a, hops + 1)).max().unwrap_or(0)
}

/// Counts each familiar's recent `message_familiar` calls.
#[derive(Default)]
pub struct Limiter {
    sent: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl Limiter {
    /// Records a message from `asker` at `now`, unless it already sent `MAX_MESSAGES` in `WINDOW`.
    pub fn allow(&self, asker: &str, now: Instant) -> Result<(), String> {
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        let recent = sent.entry(asker.to_string()).or_default();
        while recent.front().is_some_and(|at| now.duration_since(*at) >= WINDOW) {
            recent.pop_front();
        }
        if recent.len() >= MAX_MESSAGES {
            return Err(format!(
                "You've sent {MAX_MESSAGES} messages to other familiars in the last {} minutes, which is Grimoire's limit. \
                 Wait for their replies before sending more.",
                WINDOW.as_secs() / 60
            ));
        }
        recent.push_back(now);
        Ok(())
    }
}

/// Whether `scope` lets `asker` ask `target`, with the reason when it doesn't.
pub fn allowed(scope: Scope, asker: &Agent, target: &Agent) -> Result<(), String> {
    match scope {
        Scope::Anyone => Ok(()),
        Scope::Nobody => Err("Familiars can't ask each other right now: the person turned it off in Settings, Permissions.".into()),
        Scope::Coven => match (&asker.coven_id, &target.coven_id) {
            (Some(a), Some(t)) if a == t => Ok(()),
            (None, _) => Err("You can only ask familiars in your own coven, and you aren't in one.".into()),
            _ => Err(format!("You can only ask familiars in your own coven, and {} isn't in yours.", target.name)),
        },
    }
}

#[derive(Deserialize)]
struct Request {
    familiar: String,
    message: String,
    #[serde(default)]
    thread: Option<String>,
}

/// A message that passed every check, ready to go into a side conversation.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub asker: Agent,
    pub target: Agent,
    pub message: String,
    /// The conversation it follows up on, or None to start a new one.
    pub thread: Option<SideChat>,
}

fn prepare(db: &Db, caller: &str, arguments: Value) -> Result<Outgoing, String> {
    let request: Request = serde_json::from_value(arguments).map_err(|e| format!("Invalid arguments: {e}"))?;
    let message = request.message.trim().to_string();
    if message.is_empty() {
        return Err("Write the message to send.".into());
    }
    let agents = db.list_agents()?;
    let asker = agents.iter().find(|a| a.id == caller).ok_or("Only a familiar can ask another familiar.")?;
    let target = crate::tools::resolve_agent(&agents, &request.familiar)?;
    if target.id == asker.id {
        return Err("You can't ask yourself. Use list_familiars to find another familiar.".into());
    }
    allowed(load(db).scope, asker, target)?;
    let thread = match request.thread.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(thread) => Some(side::follow_up(db, thread, asker, target)?),
        None => None,
    };
    Ok(Outgoing { asker: asker.clone(), target: target.clone(), message, thread })
}

/// Asks the person first when the settings say so.
async fn approved<Approve, Approved>(db: &Db, out: &Outgoing, verb: &str, approve: Approve) -> Result<(), String>
where
    Approve: FnOnce(approvals::Request) -> Approved,
    Approved: Future<Output = Decision>,
{
    if !load(db).approve {
        return Ok(());
    }
    let request = approvals::Request {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: out.asker.id.clone(),
        kind: "familiar",
        title: format!("{} wants to {verb} {}", out.asker.name, out.target.name),
        detail: preview(&out.message),
        can_remember: false,
    };
    match approve(request).await {
        Decision::Deny => Err(format!("The person didn't let you {verb} {}. Carry on without it.", out.target.name)),
        _ => Ok(()),
    }
}

/// How a familiar finds its way back to a conversation.
fn follow_up_note(chat: &str, name: &str) -> String {
    format!("(Conversation {chat} with {name}. To follow up, call ask_familiar or message_familiar with thread set to {chat}.)")
}

/// Puts `caller`'s question to another familiar in a side conversation and returns its reply.
/// `approve` asks the person when the settings say so, and `converse` opens the conversation and
/// runs the exchange, returning the conversation's id and how the turn ended.
pub async fn ask<Approve, Approved, Converse, Conversed>(
    db: &Db,
    chains: &Chains,
    caller: &str,
    arguments: Value,
    timeout: Duration,
    approve: Approve,
    converse: Converse,
) -> Result<String, String>
where
    Approve: FnOnce(approvals::Request) -> Approved,
    Approved: Future<Output = Decision>,
    Converse: FnOnce(Outgoing) -> Conversed,
    Conversed: Future<Output = Result<(String, TurnOutcome), String>>,
{
    let out = prepare(db, caller, arguments)?;
    let _wait = chains.begin(&out.asker, &out.target)?;
    approved(db, &out, "ask", approve).await?;
    let name = out.target.name.clone();
    match tokio::time::timeout(timeout, converse(out)).await {
        Ok(Ok((chat, TurnOutcome::Done(reply)))) => {
            let reply = if reply.trim().is_empty() { format!("{name} finished without writing a reply.") } else { reply };
            Ok(format!("{reply}\n\n{}", follow_up_note(&chat, &name)))
        }
        Ok(Ok((_, TurnOutcome::Stopped))) => Err(format!("{name} was stopped before it replied.")),
        Ok(Ok((_, TurnOutcome::Failed(reason)))) => Err(format!("{name} couldn't reply: {reason}")),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!(
            "{name} didn't reply within {} minutes. It's still working, and its reply will come to you as a new message when it's done.",
            timeout.as_secs() / 60
        )),
    }
}

/// Sends `caller`'s message to another familiar and returns right away. `start` opens the
/// conversation, sets the exchange going in the background, and returns the conversation's id.
pub async fn message<Approve, Approved, Start>(
    db: &Db,
    limiter: &Limiter,
    caller: &str,
    arguments: Value,
    now: Instant,
    approve: Approve,
    start: Start,
) -> Result<String, String>
where
    Approve: FnOnce(approvals::Request) -> Approved,
    Approved: Future<Output = Decision>,
    Start: FnOnce(Outgoing) -> Result<String, String>,
{
    let out = prepare(db, caller, arguments)?;
    limiter.allow(&out.asker.id, now)?;
    approved(db, &out, "message", approve).await?;
    let name = out.target.name.clone();
    let chat = start(out)?;
    Ok(format!(
        "Sent to {name}. Its reply will come to you as a new message when it's done, so carry on with your work now.\n\n{}",
        follow_up_note(&chat, &name)
    ))
}

/// The message that brings a reply back to the familiar who sent `message_familiar`.
pub fn reply_text(name: &str, chat: &str, outcome: TurnOutcome) -> String {
    let body = match outcome {
        TurnOutcome::Done(reply) if reply.trim().is_empty() => format!("{name} finished without writing a reply."),
        TurnOutcome::Done(reply) => format!("{name} replied to your message:\n\n{reply}"),
        TurnOutcome::Stopped => format!("{name} was stopped before it replied to your message."),
        TurnOutcome::Failed(reason) => format!("{name} couldn't reply to your message: {reason}"),
    };
    format!("{body}\n\n{}", follow_up_note(chat, name))
}

fn preview(message: &str) -> String {
    const LIMIT: usize = 400;
    match message.char_indices().nth(LIMIT) {
        Some((at, _)) => format!("{}…", message[..at].trim_end()),
        None => message.to_string(),
    }
}

static CHAINS: LazyLock<Chains> = LazyLock::new(Chains::default);
static LIMITER: LazyLock<Limiter> = LazyLock::new(Limiter::default);

/// Opens the conversation, a new one or the one being followed up, and links it from the asker's
/// chat, or from the side conversation the asker is answering in (`lane`).
fn open(ctx: &crate::tools::Context, lane: Option<&str>, out: &Outgoing, verb: &str) -> Result<String, String> {
    let chat = match &out.thread {
        Some(chat) => chat.id.clone(),
        None => side::create(&ctx.db, &out.asker.id, &out.target.id, &out.message)?.id,
    };
    side::link(&ctx.app, &ctx.db, lane, &out.asker.id, &format!("{verb} {}", out.target.name), &chat)?;
    Ok(chat)
}

/// Brings a reply back to the familiar who sent it, as a new message that starts its next turn in the
/// chat it sent from (`home`). If the person has since switched chats, or the turn can't start, the
/// reply waits in `home` instead of being lost.
fn deliver(runtime: &std::sync::Arc<crate::runtime::Runtime>, db: &Db, asker_id: &str, home: Option<String>, name: &str, chat: &str, outcome: TurnOutcome) {
    let (text, source) = (reply_text(name, chat, outcome), format!("{REPLY_SOURCE}{name}"));
    let still_home = crate::chats::current(db, asker_id).is_ok_and(|now| now == home);
    if still_home && runtime.send(asker_id, text.clone(), Some(&source)).is_ok() {
        return;
    }
    if let Some(home) = home {
        let _ = db.insert_message_in(&home, asker_id, "user", &text, "done", Some(&source));
    }
}

/// Runs `ask_familiar` for real: through the approval prompt and a side conversation.
pub async fn run(ctx: &crate::tools::Context, caller: &str, lane: Option<&str>, arguments: Value) -> Result<String, String> {
    let approve = |request| ctx.approvals.ask(&ctx.app, request);
    let converse = |out: Outgoing| async move {
        let chat = open(ctx, lane, &out, "Asked")?;
        let home = crate::chats::current(&ctx.db, &out.asker.id)?;
        // The exchange runs as its own task, so a reply that outlasts the wait still finishes and is
        // saved. When nobody is waiting for it any more, it comes back like a message_familiar reply.
        let (runtime, db, chat_id) = (ctx.runtime.clone(), ctx.db.clone(), chat.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        tauri::async_runtime::spawn(async move {
            let outcome = runtime.side_turn(&chat_id, &out.asker.name, &out.message).await;
            if let Err(outcome) = tx.send(outcome) {
                deliver(&runtime, &db, &out.asker.id, home, &out.target.name, &chat_id, outcome);
            }
        });
        let outcome = rx.await.unwrap_or_else(|_| TurnOutcome::Failed("The conversation ended unexpectedly.".into()));
        Ok((chat, outcome))
    };
    ask(&ctx.db, &CHAINS, caller, arguments, TIMEOUT, approve, converse).await
}

/// Runs `message_familiar` for real. The reply goes to the sender's main chat as a new message,
/// which starts its next turn.
pub async fn run_message(ctx: &crate::tools::Context, caller: &str, lane: Option<&str>, arguments: Value) -> Result<String, String> {
    let approve = |request| ctx.approvals.ask(&ctx.app, request);
    let start = |out: Outgoing| {
        let chat = open(ctx, lane, &out, "Messaged")?;
        let home = crate::chats::current(&ctx.db, &out.asker.id)?;
        let (runtime, db, chat_id) = (ctx.runtime.clone(), ctx.db.clone(), chat.clone());
        tauri::async_runtime::spawn(async move {
            let outcome = runtime.side_turn(&chat_id, &out.asker.name, &out.message).await;
            deliver(&runtime, &db, &out.asker.id, home, &out.target.name, &chat_id, outcome);
        });
        Ok(chat)
    };
    message(&ctx.db, &LIMITER, caller, arguments, Instant::now(), approve, start).await
}

/// Closes side conversations that Grimoire quit in the middle of: each gets an error row, so it no
/// longer shows as working, and the familiar who started it hears that no reply is coming. The note
/// waits in its current chat rather than starting a turn as the app opens. Run once at startup.
pub fn recover(db: &Db) -> Result<usize, String> {
    let unfinished = side::unfinished(db)?;
    for chat in &unfinished {
        let name = db.get_agent(&chat.target_id)?.map_or_else(|| "The other familiar".to_string(), |a| a.name);
        db.insert_message_in(&chat.id, &chat.target_id, "error", "Grimoire closed before this reply finished.", "done", None)?;
        let text = reply_text(&name, &chat.id, TurnOutcome::Failed("Grimoire closed before it finished. Ask again if you still need it.".into()));
        db.insert_message_from(&chat.asker_id, "user", &text, "done", Some(&format!("{REPLY_SOURCE}{name}")))?;
    }
    Ok(unfinished.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{now_ms, Access, Avatar, Provider};
    use serde_json::json;

    fn familiar(db: &Db, id: &str, name: &str, coven: Option<&str>) -> Agent {
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
            coven_id: coven.map(String::from),
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        agent
    }

    fn setup() -> (Db, Agent, Agent, Agent, Agent) {
        let db = Db::open_in_memory().unwrap();
        let ops = crate::covens::save(&db, None, "Ops", &[]).unwrap();
        let a = familiar(&db, "a", "Researcher", Some(&ops.id));
        let b = familiar(&db, "b", "Scribe", Some(&ops.id));
        let c = familiar(&db, "c", "Critic", None);
        let d = familiar(&db, "d", "Editor", None);
        (db, a, b, c, d)
    }

    fn never_asked(_: approvals::Request) -> std::future::Ready<Decision> {
        panic!("should not ask the person")
    }

    fn never_conversed(_: Outgoing) -> std::future::Ready<Result<(String, TurnOutcome), String>> {
        panic!("should not converse")
    }

    fn replies(text: &str) -> impl FnOnce(Outgoing) -> std::future::Ready<Result<(String, TurnOutcome), String>> {
        let text = text.to_string();
        move |_| std::future::ready(Ok(("c1".into(), TurnOutcome::Done(text))))
    }

    #[tokio::test]
    async fn asks_in_a_side_conversation_and_returns_the_reply() {
        let (db, ..) = setup();
        let chains = Chains::default();
        let sent = Mutex::new(None);
        let reply = ask(&db, &chains, "a", json!({ "familiar": "scribe", "message": " Summarize the notes " }), TIMEOUT, never_asked, |out: Outgoing| {
            *sent.lock().unwrap() = Some((out.asker.id, out.target.id, out.message, out.thread));
            std::future::ready(Ok(("c1".into(), TurnOutcome::Done("Here's the summary.".into()))))
        })
        .await
        .unwrap();
        assert!(reply.starts_with("Here's the summary.\n\n(Conversation c1 with Scribe."), "{reply}");
        assert_eq!(sent.into_inner().unwrap(), Some(("a".into(), "b".into(), "Summarize the notes".into(), None)));
        assert!(chains.waits.lock().unwrap().is_empty(), "the wait ends with the reply");
    }

    #[tokio::test]
    async fn follows_up_only_on_its_own_conversations() {
        let (db, a, b, c, _) = setup();
        let mine = side::create(&db, &a.id, &b.id, "first").unwrap();
        let theirs = side::create(&db, &c.id, &b.id, "other").unwrap();
        let chains = Chains::default();
        let picked = Mutex::new(None);
        ask(&db, &chains, "a", json!({ "familiar": "b", "message": "and then?", "thread": mine.id }), TIMEOUT, never_asked, |out: Outgoing| {
            *picked.lock().unwrap() = out.thread.map(|t| t.id);
            std::future::ready(Ok((mine.id.clone(), TurnOutcome::Done("ok".into()))))
        })
        .await
        .unwrap();
        assert_eq!(picked.into_inner().unwrap(), Some(mine.id.clone()));
        let err = ask(&db, &chains, "a", json!({ "familiar": "b", "message": "x", "thread": theirs.id }), TIMEOUT, never_asked, never_conversed).await.unwrap_err();
        assert!(err.contains("isn't one you started with Scribe"), "{err}");
    }

    #[tokio::test]
    async fn refuses_yourself_and_unknown_familiars() {
        let (db, ..) = setup();
        let chains = Chains::default();
        let err = ask(&db, &chains, "a", json!({ "familiar": "Researcher", "message": "hi" }), TIMEOUT, never_asked, never_conversed).await.unwrap_err();
        assert!(err.contains("can't ask yourself"), "{err}");
        let err = ask(&db, &chains, "a", json!({ "familiar": "Nobody", "message": "hi" }), TIMEOUT, never_asked, never_conversed).await.unwrap_err();
        assert!(err.contains("No familiar is called"), "{err}");
        let err = ask(&db, &chains, "a", json!({ "familiar": "b", "message": "  " }), TIMEOUT, never_asked, never_conversed).await.unwrap_err();
        assert_eq!(err, "Write the message to send.");
    }

    #[test]
    fn refuses_loops_and_chains_deeper_than_three() {
        let (_, a, b, c, d) = setup();
        let chains = Chains::default();
        let ab = chains.begin(&a, &b).unwrap();
        // B can't ask A back while A waits on B.
        assert!(chains.begin(&b, &a).err().unwrap().contains("Researcher is waiting on you"));
        let bc = chains.begin(&b, &c).unwrap();
        // Nor can C ask A through B.
        assert!(chains.begin(&c, &a).err().unwrap().contains("waiting on you"));
        let cd = chains.begin(&c, &d).unwrap();
        // A asked B, B asked C, C asked D: D asking anyone makes a fourth.
        let e = Agent { id: "e".into(), name: "Extra".into(), ..d.clone() };
        assert!(chains.begin(&d, &e).err().unwrap().contains("more than 3"));
        // A fresh chain elsewhere is fine, and so is the same pair asking twice in parallel.
        drop(chains.begin(&e, &d).unwrap());
        drop(chains.begin(&a, &b).unwrap());
        drop((ab, bc, cd));
        assert!(chains.waits.lock().unwrap().is_empty());
        drop(chains.begin(&d, &e).unwrap());
        drop(chains.begin(&b, &a).unwrap());
    }

    #[tokio::test]
    async fn follows_the_who_can_ask_setting() {
        let (db, a, b, c, _) = setup();
        assert_eq!(load(&db), Settings { scope: Scope::Anyone, approve: false });
        assert!(allowed(Scope::Anyone, &a, &c).is_ok());
        assert!(allowed(Scope::Coven, &a, &b).is_ok());
        assert!(allowed(Scope::Coven, &a, &c).unwrap_err().contains("Critic isn't in yours"));
        assert!(allowed(Scope::Coven, &c, &a).unwrap_err().contains("you aren't in one"));
        assert!(allowed(Scope::Nobody, &a, &b).unwrap_err().contains("turned it off"));

        save(&db, Settings { scope: Scope::Coven, approve: false }).unwrap();
        let chains = Chains::default();
        assert!(ask(&db, &chains, "a", json!({ "familiar": "c", "message": "hi" }), TIMEOUT, never_asked, never_conversed).await.is_err());
        assert!(ask(&db, &chains, "a", json!({ "familiar": "b", "message": "hi" }), TIMEOUT, never_asked, replies("ok")).await.unwrap().starts_with("ok"));
        let err = message(&db, &Limiter::default(), "a", json!({ "familiar": "c", "message": "hi" }), Instant::now(), never_asked, |_| panic!("sent")).await;
        assert!(err.unwrap_err().contains("own coven"));
    }

    #[tokio::test]
    async fn asks_the_person_first_when_set() {
        let (db, ..) = setup();
        save(&db, Settings { scope: Scope::Anyone, approve: true }).unwrap();
        let chains = Chains::default();
        let asked = Mutex::new(Vec::new());
        let approve = |decision: Decision| {
            let asked = &asked;
            move |request: approvals::Request| {
                asked.lock().unwrap().push((request.title, request.detail, request.kind, request.can_remember));
                std::future::ready(decision)
            }
        };
        let args = || json!({ "familiar": "Scribe", "message": "Check the draft" });
        let err = ask(&db, &chains, "a", args(), TIMEOUT, approve(Decision::Deny), never_conversed).await.unwrap_err();
        assert!(err.contains("didn't let you ask Scribe"), "{err}");
        assert!(ask(&db, &chains, "a", args(), TIMEOUT, approve(Decision::Once), replies("ok")).await.is_ok());
        assert_eq!(asked.lock().unwrap()[0], ("Researcher wants to ask Scribe".into(), "Check the draft".into(), "familiar", false));
        assert!(chains.waits.lock().unwrap().is_empty());

        let err = message(&db, &Limiter::default(), "a", args(), Instant::now(), approve(Decision::Deny), |_| panic!("sent")).await.unwrap_err();
        assert!(err.contains("didn't let you message Scribe"), "{err}");
        assert_eq!(asked.lock().unwrap()[2].0, "Researcher wants to message Scribe");
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_the_timeout_and_reports_failures() {
        let (db, ..) = setup();
        let chains = Chains::default();
        let args = || json!({ "familiar": "b", "message": "hi" });
        let slow = |_| async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(("c1".to_string(), TurnOutcome::Done("late".into())))
        };
        let err = ask(&db, &chains, "a", args(), Duration::from_secs(120), never_asked, slow).await.unwrap_err();
        assert!(err.contains("Scribe didn't reply within 2 minutes") && err.contains("come to you as a new message"), "{err}");
        assert!(chains.waits.lock().unwrap().is_empty(), "a timed out wait leaves the chain");

        let failed = |_| std::future::ready(Ok(("c1".to_string(), TurnOutcome::Failed("no network".into()))));
        assert_eq!(ask(&db, &chains, "a", args(), TIMEOUT, never_asked, failed).await.unwrap_err(), "Scribe couldn't reply: no network");
        let stopped = |_| std::future::ready(Ok(("c1".to_string(), TurnOutcome::Stopped)));
        assert!(ask(&db, &chains, "a", args(), TIMEOUT, never_asked, stopped).await.unwrap_err().contains("stopped"));
        let empty = replies("  ");
        assert!(ask(&db, &chains, "a", args(), TIMEOUT, never_asked, empty).await.unwrap().starts_with("Scribe finished without writing a reply."));
    }

    #[tokio::test]
    async fn messages_return_at_once_and_are_rate_limited() {
        let (db, ..) = setup();
        let limiter = Limiter::default();
        let start = Instant::now();
        let sent = Mutex::new(Vec::new());
        for i in 0..MAX_MESSAGES {
            let note = message(&db, &limiter, "a", json!({ "familiar": "Scribe", "message": format!("note {i}") }), start, never_asked, |out: Outgoing| {
                sent.lock().unwrap().push(out.message);
                Ok("c9".into())
            })
            .await
            .unwrap();
            assert!(note.starts_with("Sent to Scribe.") && note.contains("thread set to c9"), "{note}");
        }
        assert_eq!(sent.lock().unwrap().len(), MAX_MESSAGES);
        let args = || json!({ "familiar": "Scribe", "message": "one more" });
        let err = message(&db, &limiter, "a", args(), start + Duration::from_secs(60), never_asked, |_| panic!("sent")).await.unwrap_err();
        assert!(err.contains("Grimoire's limit"), "{err}");
        // Others aren't held back, and the window moves on.
        assert!(message(&db, &limiter, "b", json!({ "familiar": "a", "message": "hi" }), start, never_asked, |_| Ok("c2".into())).await.is_ok());
        assert!(message(&db, &limiter, "a", args(), start + WINDOW, never_asked, |_| Ok("c9".into())).await.is_ok());
    }

    #[test]
    fn words_replies_for_the_sender() {
        let done = reply_text("Scribe", "c1", TurnOutcome::Done("All set.".into()));
        assert!(done.starts_with("Scribe replied to your message:\n\nAll set.\n\n(Conversation c1"), "{done}");
        assert!(reply_text("Scribe", "c1", TurnOutcome::Failed("offline".into())).starts_with("Scribe couldn't reply to your message: offline"));
        assert!(reply_text("Scribe", "c1", TurnOutcome::Stopped).starts_with("Scribe was stopped"));
    }

    #[test]
    fn closes_conversations_cut_off_by_a_restart() {
        let (db, a, b, ..) = setup();
        let cut = side::create(&db, &a.id, &b.id, "draft it").unwrap();
        db.insert_message_in(&cut.id, &b.id, "user", "draft it", "done", Some("From Researcher")).unwrap();
        db.insert_message_in(&cut.id, &b.id, "assistant", "Half a dra", "interrupted", None).unwrap();
        let finished = side::create(&db, &a.id, &b.id, "hi").unwrap();
        db.insert_message_in(&finished.id, &b.id, "assistant", "hello", "done", None).unwrap();

        assert_eq!(recover(&db).unwrap(), 1);
        let last = side::messages(&db, &cut.id).unwrap().pop().unwrap();
        assert_eq!((last.role.as_str(), last.text.as_str()), ("error", "Grimoire closed before this reply finished."));
        // The sender hears about it in its own chat, and the reply isn't still pending.
        let note = db.list_messages("a", None, 10).unwrap().pop().unwrap();
        assert_eq!(note.source.as_deref(), Some("Reply from Scribe"));
        assert!(note.text.starts_with("Scribe couldn't reply to your message: Grimoire closed"), "{}", note.text);
        assert!(note.text.contains(&format!("thread set to {}", cut.id)));
        assert_eq!(side::messages(&db, &finished.id).unwrap().len(), 1);
        assert_eq!(recover(&db).unwrap(), 0, "each conversation closes once");
    }

    #[test]
    fn trims_long_previews() {
        let long = "word ".repeat(200);
        assert!(preview(&long).ends_with('…'));
        assert!(preview(&long).chars().count() <= 401);
    }

    struct Quiet;

    impl crate::runtime::Sink for Quiet {
        fn message(&self, _: &crate::db::Message) {}
        fn delta(&self, _: &str, _: i64, _: &str) {}
        fn model(&self, _: &str, _: &str) {}
    }

    /// Two cheap familiars talk through the real CLIs and the dev build's `grimoire mcp` relay. Each
    /// first learns a code word in its main chat. Then each asks the other for its word: the answer
    /// comes from a side conversation forked from the main chat, so it knows the word, while the main
    /// chats stay as they were. A stand-in for the app serves the socket. Run `cargo build` first.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real codex and claude CLIs"]
    async fn live_familiars_talk_behind_the_scenes() {
        use crate::providers::{claude::Claude, codex::Codex, Lane, LaneSession};
        use crate::runtime::{stream_turn, TurnWriter};
        use std::sync::Arc;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/grimoire");
        assert!(exe.exists(), "run cargo build first");
        let id = &uuid::Uuid::new_v4().to_string()[..8];
        let socket = std::path::PathBuf::from(format!("/tmp/grim-side-{id}.sock"));
        let workspace = std::env::temp_dir().join(format!("grimoire-side-{id}"));
        std::fs::create_dir_all(&workspace).unwrap();
        let db = Arc::new(Db::open_in_memory().unwrap());
        let live = |id: &str, name: &str, provider: Provider, model: &str, effort: Option<&str>| {
            let agent = Agent {
                id: id.into(),
                name: name.into(),
                provider,
                model: Some(model.into()),
                workspace: workspace.to_string_lossy().into_owned(),
                access: Access::Workspace,
                session_id: None,
                avatar: Avatar::for_seed(id),
                effort: effort.map(String::from),
                active_model: None,
                coven_id: None,
                created_at: now_ms(),
            };
            db.insert_agent(&agent).unwrap();
            agent
        };
        live("r", "Researcher", Provider::Claude, "claude-haiku-5-5", None);
        live("s", "Scribe", Provider::Codex, "gpt-6.2-sol", Some("low"));
        crate::tools::LAUNCH.set((exe, socket.clone())).unwrap();

        let codex = Arc::new(Codex::default());
        let claude = Arc::new(Claude::default());
        let chains = Arc::new(Chains::default());
        // One turn for `agent`, in its main chat or in a side conversation.
        let turn = {
            let (db, codex, claude) = (db.clone(), codex.clone(), claude.clone());
            move |agent_id: String, prompt: String, side: Option<String>| {
                let (db, codex, claude) = (db.clone(), codex.clone(), claude.clone());
                async move {
                    let agent = db.get_agent(&agent_id).unwrap().unwrap();
                    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                    let lane = side.as_ref().map(|chat| {
                        let session = side::get(&db, chat).unwrap().unwrap().session_id;
                        let session = match (session, agent.session_id.clone()) {
                            (Some(s), _) => LaneSession::Resume(s),
                            (None, Some(m)) => LaneSession::Fork(m),
                            (None, None) => LaneSession::Fresh,
                        };
                        Lane { key: side::lane_key(&agent.id, chat), session }
                    });
                    match agent.provider {
                        Provider::Codex => codex.start_turn_in(&agent, lane.as_ref(), &prompt, &[], tx).await.unwrap(),
                        Provider::Claude => claude.start_turn_in(&agent, lane.as_ref(), &prompt, &[], tx).await.unwrap(),
                    }
                    match &side {
                        Some(chat) => {
                            let session = side::get(&db, chat).unwrap().unwrap().session_id;
                            stream_turn(&mut TurnWriter::for_side(&db, &Quiet, &agent, chat, session), rx).await
                        }
                        None => stream_turn(&mut TurnWriter::new(&db, &Quiet, &agent), rx).await,
                    }
                }
            }
        };
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = {
            let (db, chains, turn) = (db.clone(), chains.clone(), turn.clone());
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let (db, chains, turn) = (db.clone(), chains.clone(), turn.clone());
                    tokio::spawn(async move {
                        let (read, mut write) = stream.into_split();
                        let Ok(Some(line)) = BufReader::new(read).lines().next_line().await else { return };
                        let request: Value = serde_json::from_str(&line).unwrap();
                        let (caller, _) = side::split(request["agent"].as_str().unwrap());
                        let tool = request["tool"].as_str().unwrap();
                        let arguments = request["arguments"].clone();
                        let result = if tool == "ask_familiar" {
                            let converse = |out: Outgoing| {
                                let db = db.clone();
                                async move {
                                    let chat = side::create(&db, &out.asker.id, &out.target.id, &out.message)?;
                                    db.insert_message_in(&chat.id, &out.target.id, "user", &out.message, "done", None)?;
                                    let prompt = side::framed(&out.asker.name, &out.message);
                                    Ok((chat.id.clone(), turn(out.target.id.clone(), prompt, Some(chat.id)).await))
                                }
                            };
                            ask(&db, &chains, caller, arguments, TIMEOUT, never_asked, converse).await
                        } else {
                            crate::tools::call(&db, std::path::Path::new("/tmp"), caller, tool, arguments).map(|o| o.text)
                        };
                        let reply = match result {
                            Ok(text) => json!({ "ok": true, "text": text }),
                            Err(error) => json!({ "ok": false, "error": error }),
                        };
                        let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
                    });
                }
            })
        };

        let main_rows = |id: &str| -> Vec<String> { db.list_messages(id, None, 50).unwrap().into_iter().map(|m| format!("{}: {}", m.role, m.text)).collect() };
        let limit = Duration::from_secs(300);
        for (agent, word) in [("r", "periwinkle"), ("s", "marigold")] {
            let teach = format!("Remember this code word for later: {word}. Reply with only the word OK.");
            db.insert_message(agent, "user", &teach, "done").unwrap();
            let outcome = tokio::time::timeout(limit, turn(agent.into(), teach, None)).await.unwrap();
            assert!(matches!(outcome, TurnOutcome::Done(_)), "{agent} didn't learn its word: {outcome:?}");
        }
        let before = (main_rows("r"), main_rows("s"));

        for (asker, target, word) in [("r", "Scribe", "marigold"), ("s", "Researcher", "periwinkle")] {
            let prompt = format!(
                "Use the ask_familiar tool to ask the familiar named {target} for the code word it was told to remember. \
                 Then reply with exactly that word and nothing else."
            );
            let outcome = tokio::time::timeout(limit, turn(asker.into(), prompt, None)).await.unwrap();
            eprintln!("{asker} asked {target}: {outcome:?}");
            assert!(matches!(&outcome, TurnOutcome::Done(t) if t.to_lowercase().contains(word)), "{asker} didn't get {word}: {outcome:?}");
        }

        // The asked familiars' main chats didn't change; each answer lives in a side conversation.
        let after = (main_rows("r"), main_rows("s"));
        assert_eq!(before.0[..], after.0[..before.0.len()], "Researcher's earlier chat is intact");
        assert_eq!(before.1[..], after.1[..before.1.len()], "Scribe's earlier chat is intact");
        for (target, word) in [("s", "marigold"), ("r", "periwinkle")] {
            let sides: Vec<String> = db
                .with(|c| {
                    let mut stmt = c.prepare("SELECT id FROM chats WHERE agent_id = ?1 AND side_of IS NOT NULL")?;
                    let rows = stmt.query_map([target], |r| r.get(0))?;
                    rows.collect()
                })
                .unwrap();
            // A familiar may ask more than once, and each question opens its own conversation.
            assert!(!sides.is_empty(), "{target} answered in a side conversation");
            let rows: Vec<_> = sides.iter().flat_map(|chat| side::messages(&db, chat).unwrap()).collect();
            eprintln!("{target}'s side conversations: {:#?}", rows.iter().map(|m| (&m.role, &m.text)).collect::<Vec<_>>());
            assert!(rows.iter().any(|m| m.role == "assistant" && m.text.to_lowercase().contains(word)));
            assert!(!main_rows(target).iter().any(|r| r.contains("code word it was told")), "the question stayed out of {target}'s main chat");
        }

        server.abort();
        claude.forget_idle();
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
