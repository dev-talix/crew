//! Familiars talking to each other behind the scenes. `ask_familiar` sends another familiar a message
//! and waits for its reply; `message_familiar` sends one and carries on, and the reply comes back later
//! as a new message. Either way the exchange runs in a side conversation (see `side`), out of both main
//! chats. Guard rails keep familiars from asking themselves, waiting on each other in a loop, chaining
//! questions too deep, or flooding each other with messages.

use crate::approvals::{self, Decision};
use crate::db::{Agent, Db};
use crate::link::{BoxFuture, Link, Linked};
use crate::runtime::TurnOutcome;
use crate::side::{self, SideChat};
use crate::tools::Found;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::Emitter;

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

/// Questions in flight, as (asker, target) pairs: the asker is waiting on the target. A familiar on a
/// linked Mac goes by its `peer:` id, so a chain that crosses Macs counts here too.
#[derive(Default)]
pub struct Chains {
    waits: Mutex<Vec<(String, String)>>,
}

/// Holds one question in `Chains`, with the chain that led to it, until it is dropped.
pub struct Wait<'a> {
    chains: &'a Chains,
    pairs: Vec<(String, String)>,
}

impl Drop for Wait<'_> {
    fn drop(&mut self) {
        let mut waits = self.chains.waits.lock().unwrap_or_else(|e| e.into_inner());
        for pair in &self.pairs {
            if let Some(at) = waits.iter().position(|w| w == pair) {
                waits.remove(at);
            }
        }
    }
}

impl Chains {
    /// Records that `asker` waits on `target`, unless that closes a loop or makes the chain too long.
    pub fn begin(&self, asker: &Agent, target: &Agent) -> Result<Wait<'_>, String> {
        self.begin_chain(vec![asker.id.clone()], &target.id, &target.name)
    }

    /// Records a question to `target` at the end of `chain`, the familiars waiting on each other in
    /// order with the asker last, unless that closes a loop or makes the chain too long. A chain from
    /// a linked Mac arrives whole, since this Mac never saw its first questions.
    pub fn begin_chain(&self, chain: Vec<String>, target_id: &str, target_name: &str) -> Result<Wait<'_>, String> {
        let mut waits = self.waits.lock().unwrap_or_else(|e| e.into_inner());
        let Some(first) = chain.first() else { return Err("Nobody is asking.".into()) };
        if chain.iter().any(|asker| asker == target_id || reaches(&waits, target_id, asker)) {
            return Err(format!(
                "{target_name} is waiting on you, directly or through other familiars, so it can't answer until you do. \
                 Asking it would leave you both waiting. Reply with what you have instead."
            ));
        }
        if depth_into(&waits, first, 0) + chain.len() > MAX_DEPTH {
            return Err(format!(
                "This would be more than {MAX_DEPTH} familiars asking each other in a row, which is Grimoire's limit. \
                 Reply with what you have instead."
            ));
        }
        let mut pairs: Vec<(String, String)> = chain.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect();
        pairs.push((chain[chain.len() - 1].clone(), target_id.to_string()));
        waits.extend(pairs.iter().cloned());
        Ok(Wait { chains: self, pairs })
    }

    /// The longest chain of familiars waiting on each other that ends at `agent`, first asker first,
    /// `agent` last.
    fn chain_to(&self, agent: &str) -> Vec<String> {
        let waits = self.waits.lock().unwrap_or_else(|e| e.into_inner());
        let mut chain = path_into(&waits, agent, 0);
        chain.push(agent.to_string());
        chain
    }
}

fn path_into(waits: &[(String, String)], agent: &str, hops: usize) -> Vec<String> {
    if hops > MAX_DEPTH {
        return Vec::new();
    }
    let paths = waits.iter().filter(|(_, t)| t == agent).map(|(a, _)| {
        let mut path = path_into(waits, a, hops + 1);
        path.push(a.clone());
        path
    });
    paths.max_by_key(Vec::len).unwrap_or_default()
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

/// A message to a familiar on a linked Mac that passed this Mac's checks.
#[derive(Debug, Clone)]
pub struct AwayOut {
    pub asker: Agent,
    pub target: Linked,
    pub message: String,
    /// The home Mac's conversation it follows up on, or None to start a new one.
    pub thread: Option<String>,
}

#[derive(Debug)]
pub enum Prepared {
    Here(Box<Outgoing>),
    Away(Box<AwayOut>),
}

/// Checks a request from `caller` to a familiar here or on a linked Mac (`linked`). `this_mac` is
/// this Mac's name, for "Name on Mac".
pub fn prepare(db: &Db, linked: &[Linked], this_mac: Option<&str>, caller: &str, arguments: Value) -> Result<Prepared, String> {
    let request: Request = serde_json::from_value(arguments).map_err(|e| format!("Invalid arguments: {e}"))?;
    let message = request.message.trim().to_string();
    if message.is_empty() {
        return Err("Write the message to send.".into());
    }
    let agents = db.list_agents()?;
    let asker = agents.iter().find(|a| a.id == caller).ok_or("Only a familiar can ask another familiar.")?;
    let thread = request.thread.as_deref().map(str::trim).filter(|t| !t.is_empty());
    match crate::tools::resolve_familiar(&agents, linked, this_mac, &request.familiar)? {
        Found::Here(target) => {
            if target.id == asker.id {
                return Err("You can't ask yourself. Use list_familiars to find another familiar.".into());
            }
            allowed(load(db).scope, asker, target)?;
            let thread = match thread {
                Some(thread) => Some(side::follow_up(db, thread, asker, target)?),
                None => None,
            };
            Ok(Prepared::Here(Box::new(Outgoing { asker: asker.clone(), target: target.clone(), message, thread })))
        }
        Found::Away(target) => {
            allowed_away(load(db).scope, target)?;
            if let Some(thread) = thread {
                let started = crate::handoffs::away_thread(db, thread)?;
                if started != Some((target.peer.clone(), asker.id.clone(), target.agent.clone())) {
                    return Err(format!(
                        "That conversation isn't one you started with {}. Leave thread out to start a new one.",
                        target.label()
                    ));
                }
            }
            Ok(Prepared::Away(Box::new(AwayOut { asker: asker.clone(), target: target.clone(), message, thread: thread.map(String::from) })))
        }
    }
}

/// Whether `scope` lets a familiar here ask one on a linked Mac. A coven never spans Macs.
fn allowed_away(scope: Scope, target: &Linked) -> Result<(), String> {
    match scope {
        Scope::Anyone => Ok(()),
        Scope::Nobody => Err("Familiars can't ask each other right now: the person turned it off in Settings, Permissions.".into()),
        Scope::Coven => Err(format!("You can only ask familiars in your own coven, and {} isn't in yours.", target.label())),
    }
}

/// Only for the checks that resolve a familiar on this Mac.
#[cfg(test)]
fn here(prepared: Prepared) -> Result<Outgoing, String> {
    match prepared {
        Prepared::Here(out) => Ok(*out),
        Prepared::Away(out) => Err(format!("{} lives on another Mac.", out.target.label())),
    }
}

/// Asks the person first when the settings say so. `deny` records the request a denial stops.
async fn approved<Approve, Approved>(
    db: &Db,
    asker: &Agent,
    target: &str,
    message: &str,
    verb: &str,
    approve: Approve,
    deny: impl FnOnce() -> Result<(), String>,
) -> Result<(), String>
where
    Approve: FnOnce(approvals::Request) -> Approved,
    Approved: Future<Output = Decision>,
{
    if !load(db).approve {
        return Ok(());
    }
    let request = approvals::Request {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: asker.id.clone(),
        kind: "familiar",
        title: format!("{} wants to {verb} {target}", asker.name),
        detail: preview(message),
        can_remember: false,
    };
    match approve(request).await {
        Decision::Deny => {
            deny()?;
            Err(format!("The person didn't let you {verb} {target}. Carry on without it."))
        }
        _ => Ok(()),
    }
}

/// Keeps a denied question or message in the handoffs list: its conversation, a new one or the one
/// it followed up on, gets the request and an error row saying the person said no.
fn record_denial(db: &Db, out: &Outgoing, kind: &str) -> Result<(), String> {
    let chat = match &out.thread {
        Some(chat) => chat.id.clone(),
        None => side::create(db, &out.asker.id, &out.target.id, &out.message)?.id,
    };
    crate::handoffs::record(db, &chat, kind, &out.message, crate::handoffs::Status::Failed)?;
    db.insert_message_in(&chat, &out.target.id, "user", &out.message, "done", Some(&format!("From {}", out.asker.name)))?;
    db.insert_message_in(&chat, &out.target.id, "error", "The person didn't allow this.", "done", None)?;
    Ok(())
}

/// How a familiar finds its way back to a conversation.
fn follow_up_note(chat: &str, name: &str) -> String {
    format!("(Conversation {chat} with {name}. To follow up, call ask_familiar or message_familiar with thread set to {chat}.)")
}

/// Puts `caller`'s question to another familiar in a side conversation and returns its reply.
/// `approve` asks the person when the settings say so, and `converse` opens the conversation and
/// runs the exchange, returning the conversation's id and how the turn ended.
#[cfg(test)]
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
    let out = here(prepare(db, &[], None, caller, arguments)?)?;
    ask_here(db, chains, out, timeout, approve, converse).await
}

async fn ask_here<Approve, Approved, Converse, Conversed>(
    db: &Db,
    chains: &Chains,
    out: Outgoing,
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
    let _wait = chains.begin(&out.asker, &out.target)?;
    approved(db, &out.asker, &out.target.name, &out.message, "ask", approve, || record_denial(db, &out, "ask")).await?;
    let name = out.target.name.clone();
    match tokio::time::timeout(timeout, converse(out)).await {
        Ok(Ok((chat, outcome))) => answer(&name, &chat, outcome),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(late(&name, timeout)),
    }
}

/// What `ask_familiar` returns once the other familiar's turn ends.
fn answer(name: &str, chat: &str, outcome: TurnOutcome) -> Result<String, String> {
    match outcome {
        TurnOutcome::Done(reply) => {
            let reply = if reply.trim().is_empty() { format!("{name} finished without writing a reply.") } else { reply };
            Ok(format!("{reply}\n\n{}", follow_up_note(chat, name)))
        }
        TurnOutcome::Stopped => Err(format!("{name} was stopped before it replied.")),
        TurnOutcome::Failed(reason) => Err(format!("{name} couldn't reply: {reason}")),
    }
}

fn late(name: &str, timeout: Duration) -> String {
    format!(
        "{name} didn't reply within {} minutes. It's still working, and its reply will come to you as a new message when it's done.",
        timeout.as_secs() / 60
    )
}

fn sent_note(name: &str, chat: &str) -> String {
    format!("Sent to {name}. Its reply will come to you as a new message when it's done, so carry on with your work now.\n\n{}", follow_up_note(chat, name))
}

/// Sends `caller`'s message to another familiar and returns right away. `start` opens the
/// conversation, sets the exchange going in the background, and returns the conversation's id.
#[cfg(test)]
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
    let out = here(prepare(db, &[], None, caller, arguments)?)?;
    message_here(db, limiter, out, now, approve, start).await
}

async fn message_here<Approve, Approved, Start>(db: &Db, limiter: &Limiter, out: Outgoing, now: Instant, approve: Approve, start: Start) -> Result<String, String>
where
    Approve: FnOnce(approvals::Request) -> Approved,
    Approved: Future<Output = Decision>,
    Start: FnOnce(Outgoing) -> Result<String, String>,
{
    limiter.allow(&out.asker.id, now)?;
    approved(db, &out.asker, &out.target.name, &out.message, "message", approve, || record_denial(db, &out, "message")).await?;
    let name = out.target.name.clone();
    let chat = start(out)?;
    Ok(sent_note(&name, &chat))
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

pub(crate) static CHAINS: LazyLock<Chains> = LazyLock::new(Chains::default);
static LIMITER: LazyLock<Limiter> = LazyLock::new(Limiter::default);
pub(crate) static WAITERS: LazyLock<Waiters> = LazyLock::new(Waiters::default);

/// Opens the conversation, a new one or the one being followed up, and links it from the asker's
/// chat, or from the side conversation the asker is answering in (`lane`).
/// `kind` is "ask" or "message". Returns the conversation and the request's row in the handoffs list.
fn open(ctx: &crate::tools::Context, lane: Option<&str>, out: &Outgoing, verb: &str, kind: &str) -> Result<(String, i64), String> {
    let chat = match &out.thread {
        Some(chat) => chat.id.clone(),
        None => side::create(&ctx.db, &out.asker.id, &out.target.id, &out.message)?.id,
    };
    side::link(&ctx.app, &ctx.db, lane, &out.asker.id, &format!("{verb} {}", out.target.name), &chat)?;
    let handoff = crate::handoffs::record(&ctx.db, &chat, kind, &out.message, crate::handoffs::Status::Waiting)?;
    // An ask can wait a while behind the other familiar's current turn, so show it as Waiting now.
    let _ = ctx.app.emit(crate::handoffs::CHANGED, ());
    Ok((chat, handoff))
}

/// Brings a reply back to the familiar who sent it, in the chat it sent from (`home`); see `Runtime::deliver`.
fn deliver(runtime: &Arc<crate::runtime::Runtime>, asker_id: &str, home: Option<String>, name: &str, chat: &str, outcome: TurnOutcome) {
    runtime.deliver(asker_id, home, reply_text(name, chat, outcome), &format!("{REPLY_SOURCE}{name}"));
}

/// Checks a request against this Mac's familiars and every linked Mac's.
fn prepare_here_or_away(ctx: &crate::tools::Context, caller: &str, arguments: Value) -> Result<(Prepared, Option<Arc<Link>>), String> {
    let link = crate::link::current();
    let linked = link.as_ref().map(|l| l.linked()).unwrap_or_default();
    let prepared = prepare(&ctx.db, &linked, link.as_ref().map(|l| l.name()), caller, arguments)?;
    Ok((prepared, link))
}

fn reach<'a>(ctx: &'a crate::tools::Context, link: &'a Link, familiars: &'a AppFamiliars) -> Reach<'a> {
    Reach { db: &ctx.db, chains: &CHAINS, limiter: &LIMITER, waiters: &WAITERS, link, familiars }
}

/// Runs `ask_familiar` for real: through the approval prompt and a side conversation, here or on the
/// familiar's home Mac.
pub async fn run(ctx: &crate::tools::Context, caller: &str, lane: Option<&str>, arguments: Value) -> Result<String, String> {
    let approve = |request| ctx.approvals.ask(&ctx.app, request);
    let converse = |out: Outgoing| async move {
        let home = crate::chats::current(&ctx.db, &out.asker.id)?;
        let (chat, handoff) = open(ctx, lane, &out, "Asked", "ask")?;
        // The exchange runs as its own task, so a reply that outlasts the wait still finishes and is
        // saved. When nobody is waiting for it any more, it comes back like a message_familiar reply.
        let (runtime, chat_id) = (ctx.runtime.clone(), chat.clone());
        let (tx, rx) = tokio::sync::oneshot::channel();
        tauri::async_runtime::spawn(async move {
            let outcome = runtime.side_turn(&chat_id, handoff, &out.asker.name, &out.message).await;
            if let Err(outcome) = tx.send(outcome) {
                deliver(&runtime, &out.asker.id, home, &out.target.name, &chat_id, outcome);
            }
        });
        let outcome = rx.await.unwrap_or_else(|_| TurnOutcome::Failed("The conversation ended unexpectedly.".into()));
        Ok((chat, outcome))
    };
    let result = match prepare_here_or_away(ctx, caller, arguments) {
        Ok((Prepared::Here(out), _)) => ask_here(&ctx.db, &CHAINS, *out, TIMEOUT, approve, converse).await,
        Ok((Prepared::Away(out), Some(link))) => {
            let familiars = AppFamiliars { app: ctx.app.clone(), db: ctx.db.clone(), runtime: ctx.runtime.clone() };
            let home = crate::chats::current(&ctx.db, &out.asker.id)?;
            away(&reach(ctx, &link, &familiars), *out, Kind::Ask, lane, home, TIMEOUT, Instant::now(), approve).await
        }
        Ok((Prepared::Away(_), None)) => Err("Linked Macs aren't running.".into()),
        Err(e) => Err(e),
    };
    // A new request or a denial changes the handoffs list.
    let _ = ctx.app.emit(crate::handoffs::CHANGED, ());
    result
}

/// Runs `message_familiar` for real. The reply goes to the sender's main chat as a new message,
/// which starts its next turn.
pub async fn run_message(ctx: &crate::tools::Context, caller: &str, lane: Option<&str>, arguments: Value) -> Result<String, String> {
    let approve = |request| ctx.approvals.ask(&ctx.app, request);
    let start = |out: Outgoing| {
        let home = crate::chats::current(&ctx.db, &out.asker.id)?;
        let (chat, handoff) = open(ctx, lane, &out, "Messaged", "message")?;
        let (runtime, chat_id) = (ctx.runtime.clone(), chat.clone());
        tauri::async_runtime::spawn(async move {
            let outcome = runtime.side_turn(&chat_id, handoff, &out.asker.name, &out.message).await;
            deliver(&runtime, &out.asker.id, home, &out.target.name, &chat_id, outcome);
        });
        Ok(chat)
    };
    let result = match prepare_here_or_away(ctx, caller, arguments) {
        Ok((Prepared::Here(out), _)) => message_here(&ctx.db, &LIMITER, *out, Instant::now(), approve, start).await,
        Ok((Prepared::Away(out), Some(link))) => {
            let familiars = AppFamiliars { app: ctx.app.clone(), db: ctx.db.clone(), runtime: ctx.runtime.clone() };
            let home = crate::chats::current(&ctx.db, &out.asker.id)?;
            away(&reach(ctx, &link, &familiars), *out, Kind::Message, lane, home, TIMEOUT, Instant::now(), approve).await
        }
        Ok((Prepared::Away(_), None)) => Err("Linked Macs aren't running.".into()),
        Err(e) => Err(e),
    };
    let _ = ctx.app.emit(crate::handoffs::CHANGED, ());
    result
}

// ---------- Familiars on linked Macs ----------
//
// A familiar here asks one on a linked Mac: this Mac runs its own checks (the limiter, the who-can-ask
// setting, the approval prompt, loops and chain depth), then posts the request to the familiar's home
// Mac. That Mac runs the side conversation and posts the reply back. Ids that cross Macs name the Mac
// too, as `<install id>:<familiar id>`.

/// The most bytes in a message from a linked Mac.
const MAX_INCOMING: usize = 200_000;

/// What a handoff between Macs needs from the app: the runtime and the windows, or a fake in tests.
pub trait Familiars: Send + Sync {
    /// Runs one exchange in a side conversation; see `Runtime::side_turn`.
    fn side_turn(&self, chat_id: &str, handoff: i64, asker: &str, message: &str) -> BoxFuture<'static, TurnOutcome>;
    /// Brings a reply back as a new message; see `Runtime::deliver`.
    fn deliver(&self, asker_id: &str, home: Option<String>, text: String, source: &str);
    /// Adds the row that links a familiar's chat to a conversation; see `side::link`.
    fn link_row(&self, lane: Option<&str>, agent_id: &str, label: &str, source: &str) -> Result<(), String>;
    /// Tells windows the handoffs list changed.
    fn changed(&self);
}

pub struct AppFamiliars {
    pub app: tauri::AppHandle,
    pub db: Arc<Db>,
    pub runtime: Arc<crate::runtime::Runtime>,
}

impl Familiars for AppFamiliars {
    fn side_turn(&self, chat_id: &str, handoff: i64, asker: &str, message: &str) -> BoxFuture<'static, TurnOutcome> {
        let (runtime, chat, asker, message) = (self.runtime.clone(), chat_id.to_string(), asker.to_string(), message.to_string());
        Box::pin(async move { runtime.side_turn(&chat, handoff, &asker, &message).await })
    }

    fn deliver(&self, asker_id: &str, home: Option<String>, text: String, source: &str) {
        self.runtime.deliver(asker_id, home, text, source);
    }

    fn link_row(&self, lane: Option<&str>, agent_id: &str, label: &str, source: &str) -> Result<(), String> {
        side::link(&self.app, &self.db, lane, agent_id, label, source)
    }

    fn changed(&self) {
        let _ = self.app.emit(crate::handoffs::CHANGED, ());
    }
}

/// How long a request to a linked Mac waits for its reply before it counts as failed: a little past
/// the 25 minutes the home Mac spends trying to bring a reply back (see `link::send_reply`).
pub const REPLY_HORIZON: Duration = Duration::from_secs(30 * 60);

/// Someone waiting for a reply from a linked Mac.
struct Waiter {
    /// An `ask_familiar` call still waiting. None for `message_familiar`.
    reply: Option<tokio::sync::oneshot::Sender<TurnOutcome>>,
    /// The chat the asker sent from; see `Runtime::deliver`.
    home: Option<String>,
    since: Instant,
}

/// Requests sent to linked Macs and still waiting for a reply, by their row in the handoffs list.
#[derive(Default)]
pub struct Waiters(Mutex<HashMap<i64, Waiter>>);

impl Waiters {
    fn add(&self, id: i64, waiter: Waiter) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(id, waiter);
    }

    fn take(&self, id: i64) -> Option<Waiter> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id)
    }

    /// Takes the waiters that have waited REPLY_HORIZON by `now`, with the chats they sent from.
    fn expired(&self, now: Instant) -> Vec<(i64, Option<String>)> {
        let mut waiters = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<i64> = waiters.iter().filter(|(_, w)| now.saturating_duration_since(w.since) >= REPLY_HORIZON).map(|(id, _)| *id).collect();
        ids.into_iter().filter_map(|id| waiters.remove(&id).map(|w| (id, w.home))).collect()
    }
}

/// Gives up on requests to linked Macs whose reply hasn't come within REPLY_HORIZON: each shows as
/// failed, and the familiar that sent it hears so. Returns how many failed.
pub fn expire(db: &Db, waiters: &Waiters, familiars: &dyn Familiars, now: Instant) -> Result<usize, String> {
    let mut failed = 0;
    for (id, home) in waiters.expired(now) {
        let Some(sent) = crate::handoffs::sent(db, id)? else { continue };
        if !matches!(sent.status, crate::handoffs::Status::Waiting | crate::handoffs::Status::Accepted) {
            continue;
        }
        crate::handoffs::set(db, id, crate::handoffs::Status::Failed)?;
        failed += 1;
        let Some(thread) = sent.thread else { continue };
        let mac = sent.mac.as_deref().unwrap_or("a linked Mac");
        let label = format!("{} on {mac}", sent.target_name);
        let reason = format!("its reply never came back from {mac}.");
        familiars.deliver(&sent.asker_id, home, reply_text(&label, &thread, TurnOutcome::Failed(reason)), &format!("{REPLY_SOURCE}{label}"));
    }
    if failed > 0 {
        familiars.changed();
    }
    Ok(failed)
}

/// What a request to a linked Mac goes through.
pub struct Reach<'a> {
    pub db: &'a Db,
    pub chains: &'a Chains,
    pub limiter: &'a Limiter,
    pub waiters: &'a Waiters,
    pub link: &'a Link,
    pub familiars: &'a dyn Familiars,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Ask,
    Message,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Ask => "ask",
            Kind::Message => "message",
        }
    }
}

/// A familiar id as another Mac understands it: `<install id>:<familiar id>`. `here` is this Mac's.
fn global_id(here: &str, id: &str) -> String {
    match crate::link::parse_remote(id) {
        Some((peer, agent)) => format!("{peer}:{agent}"),
        None => format!("{here}:{id}"),
    }
}

/// A familiar id from another Mac as this Mac names it, or None when it's malformed.
fn local_id(here: &str, id: &str) -> Option<String> {
    let (mac, agent) = id.split_once(':')?;
    if !crate::link::valid_id(mac) || !crate::link::valid_id(agent) {
        return None;
    }
    Some(if mac == here { agent.to_string() } else { crate::link::remote_id(mac, agent) })
}

/// Sends `out` to its familiar's home Mac, after this Mac's checks. An ask waits up to `timeout` for
/// the reply; a message returns once the home Mac has it. `home` is the chat the asker sends from.
#[allow(clippy::too_many_arguments)]
pub async fn away<Approve, Approved>(
    reach: &Reach<'_>,
    out: AwayOut,
    kind: Kind,
    lane: Option<&str>,
    home: Option<String>,
    timeout: Duration,
    now: Instant,
    approve: Approve,
) -> Result<String, String>
where
    Approve: FnOnce(approvals::Request) -> Approved,
    Approved: Future<Output = Decision>,
{
    let (db, target) = (reach.db, &out.target);
    let label = target.label();
    if !target.online {
        return Err(format!("{} is offline, so {} can't be reached right now.", target.mac, target.name));
    }
    let _wait = match kind {
        Kind::Ask => Some(reach.chains.begin_chain(vec![out.asker.id.clone()], &target.id, &label)?),
        Kind::Message => {
            reach.limiter.allow(&out.asker.id, now)?;
            None
        }
    };
    let row = crate::handoffs::Away {
        thread: out.thread.as_deref(),
        peer: &target.peer,
        asker_id: &out.asker.id,
        target_id: &target.agent,
        target_name: &target.name,
    };
    let deny = || crate::handoffs::record_away(db, &row, kind.as_str(), &out.message, crate::handoffs::Status::Failed).map(|_| ());
    approved(db, &out.asker, &label, &out.message, kind.as_str(), approve, deny).await?;
    let here = reach.link.install_id();
    let chain: Vec<String> = reach.chains.chain_to(&out.asker.id).iter().map(|id| global_id(here, id)).collect();
    let id = crate::handoffs::record_away(db, &row, kind.as_str(), &out.message, crate::handoffs::Status::Waiting)?;
    reach.familiars.changed();
    let (reply, wait) = match kind {
        Kind::Ask => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            (Some(tx), Some(rx))
        }
        Kind::Message => (None, None),
    };
    // Waiting before the request goes, since the reply may come back before the request returns.
    reach.waiters.add(id, Waiter { reply, home, since: Instant::now() });
    let body = json!({
        "targetId": target.agent,
        "asker": { "id": out.asker.id, "name": out.asker.name, "mac": reach.link.name() },
        "message": out.message,
        "kind": kind.as_str(),
        "thread": out.thread,
        "depth": chain.len(),
        "chainIds": chain,
        "requestId": id,
    });
    let thread = match reach.link.hand_off(target, &body).await {
        Ok(thread) => thread,
        Err(e) => {
            reach.waiters.take(id);
            crate::handoffs::set(db, id, crate::handoffs::Status::Failed)?;
            reach.familiars.changed();
            return Err(e);
        }
    };
    // The request is out, so its reply still comes back if saving here fails.
    let _ = crate::handoffs::set_thread(db, id, &thread);
    let verb = if kind == Kind::Ask { "Asked" } else { "Messaged" };
    // The row points at a thread on the other Mac, so windows here show it without opening it.
    let _ = reach.familiars.link_row(lane, &out.asker.id, &format!("{verb} {label}"), &crate::link::remote_id(&target.peer, &thread));
    reach.familiars.changed();
    let Some(wait) = wait else { return Ok(sent_note(&label, &thread)) };
    match tokio::time::timeout(timeout, wait).await {
        Ok(Ok(outcome)) => answer(&label, &thread, outcome),
        Ok(Err(_)) => Err("The conversation ended unexpectedly.".into()),
        Err(_) => Err(late(&label, timeout)),
    }
}

/// `peer` brings back the reply to request `id` from here, in its conversation `thread`. It goes to
/// the `ask_familiar` call still waiting for it, or else to the asker as a new message. Only the Mac
/// the request went to can answer it, once.
pub fn away_reply(db: &Db, waiters: &Waiters, familiars: &dyn Familiars, peer: &str, thread: &str, id: i64, outcome: TurnOutcome) -> Result<(), String> {
    let sent = crate::handoffs::sent(db, id)?.filter(|s| s.peer == peer && s.thread.as_deref().is_none_or(|t| t == thread));
    let sent = sent.ok_or("That reply isn't for a request this Mac sent you.")?;
    if !matches!(sent.status, crate::handoffs::Status::Waiting | crate::handoffs::Status::Accepted) {
        return Err("That request already has its reply.".into());
    }
    crate::handoffs::set(db, id, crate::handoffs::Status::of(&outcome))?;
    if sent.thread.is_none() {
        crate::handoffs::set_thread(db, id, thread)?;
    }
    familiars.changed();
    let label = format!("{} on {}", sent.target_name, sent.mac.as_deref().unwrap_or("a linked Mac"));
    let (home, outcome) = match waiters.take(id) {
        Some(Waiter { reply: Some(tx), home, .. }) => match tx.send(outcome) {
            Ok(()) => return Ok(()),
            Err(outcome) => (home, outcome),
        },
        Some(Waiter { reply: None, home, .. }) => (home, outcome),
        None => (None, outcome),
    };
    familiars.deliver(&sent.asker_id, home, reply_text(&label, thread, outcome), &format!("{REPLY_SOURCE}{label}"));
    Ok(())
}

/// A request from a familiar on a linked Mac, as `POST /v1/handoffs` carries it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Incoming {
    /// The familiar here it's for.
    pub target_id: String,
    pub asker: RemoteAsker,
    pub message: String,
    /// "ask" or "message".
    pub kind: String,
    /// A conversation here the same asker started with the same familiar, to follow up in.
    #[serde(default)]
    pub thread: Option<String>,
    /// How many familiars ask in a row with this one, which is `chain_ids`' length.
    pub depth: usize,
    /// The familiars waiting on each other that led here, as `<install id>:<familiar id>`, the asker last.
    pub chain_ids: Vec<String>,
    /// The request's row in the asking Mac's handoffs list, which the reply names.
    pub request_id: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteAsker {
    /// Its id on the asking Mac.
    pub id: String,
    pub name: String,
    /// The asking Mac's name for itself. Checked, but this Mac names it from its own link record.
    pub mac: String,
}

/// Starts the side conversation for a request from a familiar on `peer`, called `mac` here. `here` is
/// this Mac's install id. It goes through what a request from a familiar here would, apart from the
/// approval prompt, which the asking Mac shows. The asker stays a familiar on `peer`: it gets no tools,
/// vault, or anything else here, and the side chat records it as remote. Returns the conversation's
/// id and its exchange, which holds the chain in `chains` while it runs.
pub fn incoming(
    db: &Db,
    chains: &'static Chains,
    familiars: Arc<dyn Familiars>,
    here: &str,
    peer: &str,
    mac: &str,
    request: &Incoming,
) -> Result<(String, BoxFuture<'static, TurnOutcome>), String> {
    let message = request.message.trim().to_string();
    let asker_name = crate::link::clean_mac_name(&request.asker.name);
    let ids = crate::link::valid_id(&request.asker.id) && crate::link::valid_id(&request.target_id);
    let (Some(asker_name), true) = (asker_name, ids && crate::link::clean_mac_name(&request.asker.mac).is_some()) else {
        return Err("Bad handoff.".into());
    };
    if message.is_empty() || message.len() > MAX_INCOMING {
        return Err(format!("A message to another familiar must have text and be under {} KB.", MAX_INCOMING / 1000));
    }
    let kind = match request.kind.as_str() {
        "ask" => Kind::Ask,
        "message" => Kind::Message,
        _ => return Err("Bad handoff.".into()),
    };
    // The asker is on the Mac that sent this, so it can't speak for a familiar anywhere else.
    let chain: Option<Vec<String>> = request.chain_ids.iter().take(MAX_DEPTH + 2).map(|id| local_id(here, id)).collect();
    let asker_id = crate::link::remote_id(peer, &request.asker.id);
    let chain = chain.filter(|c| c.last() == Some(&asker_id) && c.len() == request.chain_ids.len() && request.depth == c.len());
    let chain = chain.ok_or("Bad handoff.")?;
    let target = db.get_agent(&request.target_id)?.ok_or("That familiar no longer exists.")?;
    let asker = format!("{asker_name} on {mac}");
    match load(db).scope {
        Scope::Anyone => {}
        Scope::Nobody => return Err(format!("{} can't be asked right now: the person turned it off on its Mac, in Settings, Permissions.", target.name)),
        Scope::Coven => return Err(format!("{} only takes requests from familiars in its own coven, and you aren't in it.", target.name)),
    }
    let thread = match request.thread.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(thread) => {
            let chat = side::get(db, thread)?.filter(|c| c.asker_id == asker_id && c.target_id == target.id);
            Some(chat.ok_or_else(|| format!("That conversation isn't one you started with {}. Leave thread out to start a new one.", target.name))?)
        }
        None => None,
    };
    let hold = match kind {
        Kind::Ask => Some(chains.begin_chain(chain, &target.id, &target.name)?),
        Kind::Message => None,
    };
    let chat = match thread {
        Some(chat) => chat.id,
        None => side::create_remote(db, peer, &request.asker.id, &asker_name, &target.id, &message)?.id,
    };
    let handoff = crate::handoffs::record(db, &chat, kind.as_str(), &message, crate::handoffs::Status::Waiting)?;
    crate::handoffs::set_remote_request(db, handoff, request.request_id)?;
    familiars.changed();
    let chat_id = chat.clone();
    let turn = Box::pin(async move {
        let _hold = hold;
        familiars.side_turn(&chat_id, handoff, &asker, &message).await
    });
    Ok((chat, turn))
}

/// Closes side conversations that Grimoire quit in the middle of: each gets an error row, so it no
/// longer shows as working, and the familiar who started it hears that no reply is coming. The note
/// waits in its current chat and comes with its next turn, rather than starting a turn as the app
/// opens. Their requests show as failed in the handoffs list. Helpers' chats are left to
/// `helpers::interrupt_leftovers`. A familiar on a linked Mac that asked hears through the link: see
/// `handoffs::cut_off_remote`, which must run first. Run once at startup.
pub fn recover(db: &Db) -> Result<usize, String> {
    crate::handoffs::fail_unfinished(db)?;
    let unfinished = side::unfinished(db)?;
    for chat in &unfinished {
        let name = db.get_agent(&chat.target_id)?.map_or_else(|| "The other familiar".to_string(), |a| a.name);
        db.insert_message_in(&chat.id, &chat.target_id, "error", "Grimoire closed before this reply finished.", "done", None)?;
        if crate::link::is_remote(&chat.asker_id) {
            continue;
        }
        let text = reply_text(&name, &chat.id, TurnOutcome::Failed("Grimoire closed before it finished. Ask again if you still need it.".into()));
        crate::runtime::hold(db, &chat.asker_id, None, &text, &format!("{REPLY_SOURCE}{name}"))?;
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
            host: None,
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
        // The denied question still shows in the handoffs list, as failed.
        let denied = crate::handoffs::list(&db, 10).unwrap();
        assert_eq!(denied.len(), 1);
        assert_eq!((denied[0].kind.as_str(), denied[0].what.as_str(), denied[0].status), ("ask", "Check the draft", crate::handoffs::Status::Failed));
        assert!(ask(&db, &chains, "a", args(), TIMEOUT, approve(Decision::Once), replies("ok")).await.is_ok());
        assert_eq!(asked.lock().unwrap()[0], ("Researcher wants to ask Scribe".into(), "Check the draft".into(), "familiar", false));
        assert!(chains.waits.lock().unwrap().is_empty());

        let err = message(&db, &Limiter::default(), "a", args(), Instant::now(), approve(Decision::Deny), |_| panic!("sent")).await.unwrap_err();
        assert!(err.contains("didn't let you message Scribe"), "{err}");
        assert_eq!(asked.lock().unwrap()[2].0, "Researcher wants to message Scribe");

        // A denied follow-up fails on its own row and leaves the earlier request in that thread done.
        let thread = side::create(&db, "a", "b", "First").unwrap();
        let first = crate::handoffs::record(&db, &thread.id, "ask", "First", crate::handoffs::Status::Done).unwrap();
        let follow = json!({ "familiar": "Scribe", "message": "Second", "thread": thread.id });
        assert!(message(&db, &Limiter::default(), "a", follow, Instant::now(), approve(Decision::Deny), |_| panic!("sent")).await.is_err());
        let rows: Vec<_> = crate::handoffs::list(&db, 10).unwrap().into_iter().filter(|h| h.chat_id == thread.id).collect();
        let got: Vec<_> = rows.iter().map(|h| (h.id, h.what.as_str(), h.status)).collect();
        assert_eq!(got[1], (first, "First", crate::handoffs::Status::Done));
        assert_eq!((got[0].1, got[0].2), ("Second", crate::handoffs::Status::Failed));
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
        // A helper cut off by the restart isn't a reply anyone waits for.
        let (_, helper) = crate::helpers::create(&db, &a.id, None, &json!({ "task": "x" })).unwrap();
        db.insert_message_in(&helper.chat_id, &a.id, "assistant", "Half", "interrupted", None).unwrap();

        assert_eq!(recover(&db).unwrap(), 1);
        let last = side::messages(&db, &cut.id).unwrap().pop().unwrap();
        assert_eq!((last.role.as_str(), last.text.as_str()), ("error", "Grimoire closed before this reply finished."));
        // The sender hears about it in its own chat, and the reply isn't still pending.
        let note = db.list_messages("a", None, 10).unwrap().pop().unwrap();
        assert_eq!(note.source.as_deref(), Some("Reply from Scribe"));
        assert!(note.text.starts_with("Scribe couldn't reply to your message: Grimoire closed"), "{}", note.text);
        assert!(note.text.contains(&format!("thread set to {}", cut.id)));
        assert_eq!(side::messages(&db, &finished.id).unwrap().len(), 1);
        // The sender's CLI hears it with its next turn too.
        let held: Vec<String> = db
            .with(|c| c.prepare("SELECT text FROM pending_notes WHERE agent_id = 'a'")?.query_map([], |r| r.get(0))?.collect())
            .unwrap();
        assert_eq!(held, vec![note.text]);
        assert_eq!(recover(&db).unwrap(), 0, "each conversation closes once");
    }

    #[test]
    fn a_conversation_cut_off_for_a_familiar_on_a_linked_mac_closes_without_a_note_here() {
        let (db, _, b, ..) = setup();
        let cut = side::create_remote(&db, "mac-b", "ada", "Ada", &b.id, "draft it").unwrap();
        assert_eq!(cut.asker_id, "peer:mac-b:ada", "the asker stays a familiar on its own Mac");
        assert_eq!(recover(&db).unwrap(), 1);
        assert_eq!(side::messages(&db, &cut.id).unwrap().pop().unwrap().role, "error");
        let notes: i64 = db.with(|c| c.query_row("SELECT COUNT(*) FROM pending_notes", [], |r| r.get(0))).unwrap();
        assert_eq!(notes, 0);
    }

    fn scribe_on(mac: &str) -> Linked {
        Linked {
            id: "peer:m1:s1".into(),
            peer: "m1".into(),
            agent: "s1".into(),
            name: "Scribe".into(),
            mac: mac.into(),
            online: true,
            provider: None,
            model: None,
        }
    }

    #[test]
    fn a_familiar_on_a_linked_mac_is_never_in_your_coven() {
        let (db, ..) = setup();
        // Researcher's coven-mate here is also called Scribe, so the one on NigelBot needs its Mac.
        let linked = [scribe_on("NigelBot")];
        let args = || json!({ "familiar": "Scribe on NigelBot", "message": "hi" });
        assert!(matches!(prepare(&db, &linked, None, "a", args()).unwrap(), Prepared::Away(_)));
        save(&db, Settings { scope: Scope::Coven, approve: false }).unwrap();
        let err = prepare(&db, &linked, None, "a", args()).unwrap_err();
        assert_eq!(err, "You can only ask familiars in your own coven, and Scribe on NigelBot isn't in yours.");
        assert!(matches!(prepare(&db, &linked, None, "a", json!({ "familiar": "b", "message": "hi" })).unwrap(), Prepared::Here(_)));
        save(&db, Settings { scope: Scope::Nobody, approve: false }).unwrap();
        assert!(prepare(&db, &linked, None, "a", args()).unwrap_err().contains("turned it off"));
    }

    #[test]
    fn follows_up_on_a_linked_mac_only_in_its_own_threads() {
        let (db, ..) = setup();
        let linked = [scribe_on("NigelBot")];
        let sent = |asker: &'static str, thread: &'static str| crate::handoffs::Away { thread: Some(thread), peer: "m1", asker_id: asker, target_id: "s1", target_name: "Scribe" };
        crate::handoffs::record_away(&db, &sent("a", "t1"), "ask", "x", crate::handoffs::Status::Done).unwrap();
        crate::handoffs::record_away(&db, &sent("c", "t2"), "ask", "x", crate::handoffs::Status::Done).unwrap();
        let follow = |thread: &str| prepare(&db, &linked, None, "a", json!({ "familiar": "peer:m1:s1", "message": "more", "thread": thread }));
        match follow("t1").unwrap() {
            Prepared::Away(out) => assert_eq!(out.thread.as_deref(), Some("t1")),
            other => panic!("{other:?}"),
        }
        assert!(follow("t2").unwrap_err().contains("isn't one you started with Scribe on NigelBot"));
        assert!(follow("nope").is_err());
    }

    #[test]
    fn chains_carry_across_macs_as_install_and_familiar_ids() {
        let chains = Chains::default();
        let held = chains.begin_chain(vec!["peer:mac-b:x".into(), "a".into()], "b", "Scribe").unwrap();
        assert_eq!(chains.chain_to("b"), ["peer:mac-b:x", "a", "b"]);
        let global: Vec<String> = chains.chain_to("b").iter().map(|id| global_id("mac-a", id)).collect();
        assert_eq!(global, ["mac-b:x", "mac-a:a", "mac-a:b"]);
        assert_eq!(global.iter().map(|id| local_id("mac-b", id).unwrap()).collect::<Vec<_>>(), ["x", "peer:mac-a:a", "peer:mac-a:b"]);
        assert_eq!(local_id("mac-b", "../x:y"), None);
        drop(held);
        assert_eq!(chains.chain_to("b"), ["b"]);
    }

    #[derive(Default)]
    struct Delivered(Mutex<Vec<(String, Option<String>, String)>>);

    impl Familiars for Delivered {
        fn side_turn(&self, _: &str, _: i64, _: &str, _: &str) -> BoxFuture<'static, TurnOutcome> {
            unreachable!()
        }
        fn deliver(&self, asker_id: &str, home: Option<String>, text: String, _: &str) {
            self.0.lock().unwrap().push((asker_id.into(), home, text));
        }
        fn link_row(&self, _: Option<&str>, _: &str, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn changed(&self) {}
    }

    #[test]
    fn requests_to_a_linked_mac_with_no_reply_fail_after_the_retry_horizon() {
        let (db, a, ..) = setup();
        let away = crate::handoffs::Away { thread: None, peer: "mac-b", asker_id: &a.id, target_id: "s1", target_name: "Scribe" };
        let (old, fresh) = (
            crate::handoffs::record_away(&db, &away, "message", "Draft it", crate::handoffs::Status::Waiting).unwrap(),
            crate::handoffs::record_away(&db, &away, "message", "And this", crate::handoffs::Status::Waiting).unwrap(),
        );
        crate::handoffs::set_thread(&db, old, "t1").unwrap();
        let (waiters, start) = (Waiters::default(), Instant::now());
        waiters.add(old, Waiter { reply: None, home: Some("main".into()), since: start });
        waiters.add(fresh, Waiter { reply: None, home: None, since: start + Duration::from_secs(600) });
        let familiars = Delivered::default();

        assert_eq!(expire(&db, &waiters, &familiars, start + REPLY_HORIZON - Duration::from_secs(1)).unwrap(), 0);
        assert_eq!(expire(&db, &waiters, &familiars, start + REPLY_HORIZON).unwrap(), 1);
        assert_eq!(crate::handoffs::sent(&db, old).unwrap().unwrap().status, crate::handoffs::Status::Failed);
        assert_eq!(crate::handoffs::sent(&db, fresh).unwrap().unwrap().status, crate::handoffs::Status::Waiting);
        let delivered = familiars.0.lock().unwrap().clone();
        assert_eq!(delivered.len(), 1);
        assert_eq!((delivered[0].0.as_str(), delivered[0].1.as_deref()), ("a", Some("main")));
        assert!(delivered[0].2.starts_with("Scribe on a linked Mac couldn't reply to your message: its reply never came back"), "{}", delivered[0].2);
        assert!(waiters.take(old).is_none() && waiters.take(fresh).is_some(), "only the expired waiter is gone");
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
                host: None,
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
