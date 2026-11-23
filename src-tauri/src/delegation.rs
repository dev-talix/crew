//! `ask_familiar`: one familiar sends another a message and waits for its reply. The message shows in
//! the other familiar's chat as "Asked by Researcher". Guard rails keep familiars from asking
//! themselves, waiting on each other in a loop, or chaining questions too deep.

use crate::approvals::{self, Decision};
use crate::db::{Agent, Db};
use crate::runtime::TurnOutcome;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

/// How long a familiar waits for a reply, counted from when its question is queued.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// The most questions in one chain: A asks B, B asks C, C asks D.
pub const MAX_DEPTH: usize = 3;
pub const SOURCE: &str = "Asked by ";
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
    /// Ask the person before each question goes out.
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
struct Question {
    familiar: String,
    message: String,
}

/// Puts `caller`'s question to another familiar and returns its reply. `approve` asks the person when
/// the settings say so, and `send` delivers the message (target id, text, source label).
pub async fn ask<Approve, Approved, Send, Sent>(
    db: &Db,
    chains: &Chains,
    caller: &str,
    arguments: Value,
    timeout: Duration,
    approve: Approve,
    send: Send,
) -> Result<String, String>
where
    Approve: FnOnce(approvals::Request) -> Approved,
    Approved: Future<Output = Decision>,
    Send: FnOnce(String, String, String) -> Sent,
    Sent: Future<Output = TurnOutcome>,
{
    let question: Question = serde_json::from_value(arguments).map_err(|e| format!("Invalid arguments: {e}"))?;
    let message = question.message.trim().to_string();
    if message.is_empty() {
        return Err("Write the message to send.".into());
    }
    let agents = db.list_agents()?;
    let asker = agents.iter().find(|a| a.id == caller).ok_or("Only a familiar can ask another familiar.")?;
    let target = crate::tools::resolve_agent(&agents, &question.familiar)?;
    if target.id == asker.id {
        return Err("You can't ask yourself. Use list_familiars to find another familiar.".into());
    }
    let settings = load(db);
    allowed(settings.scope, asker, target)?;
    let _wait = chains.begin(asker, target)?;
    if settings.approve {
        let request = approvals::Request {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: asker.id.clone(),
            kind: "familiar",
            title: format!("{} wants to ask {}", asker.name, target.name),
            detail: preview(&message),
            can_remember: false,
        };
        if approve(request).await == Decision::Deny {
            return Err(format!("The person didn't let you ask {}. Carry on without it.", target.name));
        }
    }
    let source = format!("{SOURCE}{}", asker.name);
    match tokio::time::timeout(timeout, send(target.id.clone(), message, source)).await {
        Ok(TurnOutcome::Done(reply)) if reply.trim().is_empty() => Ok(format!("{} finished without writing a reply.", target.name)),
        Ok(TurnOutcome::Done(reply)) => Ok(reply),
        Ok(TurnOutcome::Stopped) => Err(format!("{} was stopped before it replied.", target.name)),
        Ok(TurnOutcome::Failed(reason)) => Err(format!("{} couldn't reply: {reason}", target.name)),
        Err(_) => Err(format!(
            "{} didn't reply within {} minutes. It may still be working; its reply will show in its own chat.",
            target.name,
            timeout.as_secs() / 60
        )),
    }
}

fn preview(message: &str) -> String {
    const LIMIT: usize = 400;
    match message.char_indices().nth(LIMIT) {
        Some((at, _)) => format!("{}…", message[..at].trim_end()),
        None => message.to_string(),
    }
}

static CHAINS: LazyLock<Chains> = LazyLock::new(Chains::default);
static DB: OnceLock<Arc<Db>> = OnceLock::new();

/// Lets `tool_label` turn a familiar's id into its name.
pub fn install(db: Arc<Db>) {
    let _ = DB.set(db);
}

/// Runs `ask_familiar` for real: through the approval prompt and the runtime's queue.
pub async fn run(ctx: &crate::tools::Context, caller: &str, arguments: Value) -> Result<String, String> {
    let approve = |request| ctx.approvals.ask(&ctx.app, request);
    let send = |target: String, text: String, source: String| async move { ctx.runtime.send_and_wait(&target, text, Some(&source)).await };
    ask(&ctx.db, &CHAINS, caller, arguments, TIMEOUT, approve, send).await
}

/// The chat row for an `ask_familiar` call, like "Asked Scribe".
pub fn tool_label(arguments: &Value) -> String {
    let wanted = arguments.get("familiar").and_then(Value::as_str).unwrap_or_default().trim();
    let agents = DB.get().and_then(|db| db.list_agents().ok()).unwrap_or_default();
    let name = crate::tools::resolve_agent(&agents, wanted).map_or_else(|_| wanted.to_string(), |a| a.name.clone());
    if name.is_empty() { "Asked a familiar".into() } else { format!("Asked {name}") }
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

    fn never_sent(_: String, _: String, _: String) -> std::future::Ready<TurnOutcome> {
        panic!("should not send")
    }

    #[tokio::test]
    async fn sends_with_a_source_label_and_returns_the_reply() {
        let (db, ..) = setup();
        let chains = Chains::default();
        let sent = Mutex::new(None);
        let reply = ask(&db, &chains, "a", json!({ "familiar": "scribe", "message": " Summarize the notes " }), TIMEOUT, never_asked, |id, text, source| {
            *sent.lock().unwrap() = Some((id, text, source));
            async { TurnOutcome::Done("Here's the summary.".into()) }
        })
        .await
        .unwrap();
        assert_eq!(reply, "Here's the summary.");
        assert_eq!(sent.into_inner().unwrap(), Some(("b".into(), "Summarize the notes".into(), "Asked by Researcher".into())));
        assert!(chains.waits.lock().unwrap().is_empty(), "the wait ends with the reply");
    }

    #[tokio::test]
    async fn refuses_yourself_and_unknown_familiars() {
        let (db, ..) = setup();
        let chains = Chains::default();
        let send = never_sent;
        let err = ask(&db, &chains, "a", json!({ "familiar": "Researcher", "message": "hi" }), TIMEOUT, never_asked, send).await.unwrap_err();
        assert!(err.contains("can't ask yourself"), "{err}");
        let err = ask(&db, &chains, "a", json!({ "familiar": "Nobody", "message": "hi" }), TIMEOUT, never_asked, send).await.unwrap_err();
        assert!(err.contains("No familiar is called"), "{err}");
        let err = ask(&db, &chains, "a", json!({ "familiar": "b", "message": "  " }), TIMEOUT, never_asked, send).await.unwrap_err();
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
        let send = |_, _, _| async { TurnOutcome::Done("ok".into()) };
        assert!(ask(&db, &chains, "a", json!({ "familiar": "c", "message": "hi" }), TIMEOUT, never_asked, send).await.is_err());
        assert_eq!(ask(&db, &chains, "a", json!({ "familiar": "b", "message": "hi" }), TIMEOUT, never_asked, send).await.unwrap(), "ok");
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
        let send = |_, _, _| async { TurnOutcome::Done("ok".into()) };
        let args = || json!({ "familiar": "Scribe", "message": "Check the draft" });
        let err = ask(&db, &chains, "a", args(), TIMEOUT, approve(Decision::Deny), never_sent).await.unwrap_err();
        assert!(err.contains("didn't let you ask Scribe"), "{err}");
        assert_eq!(ask(&db, &chains, "a", args(), TIMEOUT, approve(Decision::Once), send).await.unwrap(), "ok");
        assert_eq!(asked.lock().unwrap()[0], ("Researcher wants to ask Scribe".into(), "Check the draft".into(), "familiar", false));
        assert!(chains.waits.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_after_the_timeout_and_reports_failures() {
        let (db, ..) = setup();
        let chains = Chains::default();
        let args = || json!({ "familiar": "b", "message": "hi" });
        let slow = |_, _, _| async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            TurnOutcome::Done("late".into())
        };
        let err = ask(&db, &chains, "a", args(), Duration::from_secs(120), never_asked, slow).await.unwrap_err();
        assert!(err.contains("Scribe didn't reply within 2 minutes"), "{err}");
        assert!(chains.waits.lock().unwrap().is_empty(), "a timed out wait leaves the chain");

        let failed = |_, _, _| async { TurnOutcome::Failed("no network".into()) };
        assert_eq!(ask(&db, &chains, "a", args(), TIMEOUT, never_asked, failed).await.unwrap_err(), "Scribe couldn't reply: no network");
        let stopped = |_, _, _| async { TurnOutcome::Stopped };
        assert!(ask(&db, &chains, "a", args(), TIMEOUT, never_asked, stopped).await.unwrap_err().contains("stopped"));
    }

    #[test]
    fn labels_the_tool_row_and_trims_long_previews() {
        assert_eq!(tool_label(&json!({ "familiar": "Scribe", "message": "x" })), "Asked Scribe");
        assert_eq!(tool_label(&json!({})), "Asked a familiar");
        let long = "word ".repeat(200);
        assert!(preview(&long).ends_with('…'));
        assert!(preview(&long).chars().count() <= 401);
    }
}
