use super::{command, spawn_error, EventTx, IdleTimer, Lane, LaneSession, ModelOption, ProviderEvent, StderrTail};
use crate::db::{Access, Agent};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::oneshot;

const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

#[derive(Default)]
pub struct Route {
    tx: Option<EventTx>,
    turn_id: Option<String>,
    last_item: Option<String>,
    has_text: bool,
    /// The thread's token totals before this turn, from its first usage update.
    usage_base: Option<(u64, u64)>,
}

struct CodexProcess {
    stdin: tokio::sync::Mutex<ChildStdin>,
    child: Mutex<Option<Child>>,
    next_id: AtomicU64,
    pending: Pending,
    routes: Mutex<HashMap<String, Route>>,
    loaded: Mutex<HashSet<String>>,
}

impl CodexProcess {
    fn alive(&self) -> bool {
        self.child.lock().map(|c| c.is_some()).unwrap_or(false)
    }

    fn kill(&self) {
        if let Some(mut child) = self.child.lock().ok().and_then(|mut c| c.take()) {
            let _ = child.start_kill();
        }
    }

    async fn write(&self, message: &Value) -> Result<(), String> {
        let line = message.to_string() + "\n";
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().map_err(|_| "codex state poisoned")?.insert(id, tx);
        if let Err(e) = self.write(&json!({ "id": id, "method": method, "params": params })).await {
            self.pending.lock().ok().map(|mut p| p.remove(&id));
            return Err(format!("codex is not responding: {e}"));
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("codex stopped before it answered.".into()),
            Err(_) => {
                self.pending.lock().ok().map(|mut p| p.remove(&id));
                Err(format!("codex did not answer `{method}` in time."))
            }
        }
    }

    fn route_count(&self) -> usize {
        self.routes.lock().map(|r| r.len()).unwrap_or(0)
    }
}

#[derive(Default)]
pub struct Codex {
    process: tokio::sync::Mutex<Option<Arc<CodexProcess>>>,
    threads: Mutex<HashMap<String, String>>,
    idle: Arc<IdleTimer>,
}

/// "untrusted" makes Codex ask before commands it doesn't know are safe, so Grimoire's rules see them.
fn approval_policy() -> &'static str {
    if crate::permissions::active() { "untrusted" } else { "never" }
}

/// `tools_key` is the familiar's id, or its side conversation's lane key, for Grimoire's tool server.
fn thread_params(agent: &Agent, tools_key: &str) -> Value {
    let mut params = json!({
        "cwd": agent.workspace,
        "approvalPolicy": approval_policy(),
        // Approval requests come to Grimoire, not to Codex's own automatic reviewer.
        "approvalsReviewer": "user",
        "sandbox": match agent.access {
            Access::Workspace => "workspace-write",
            Access::Full => "danger-full-access",
        },
    });
    if let Some(model) = agent.model.as_deref().filter(|m| !m.is_empty()) {
        params["model"] = json!(model);
    }
    if let Some((command, args)) = crate::tools::server_command(tools_key) {
        // Codex refuses MCP calls that need approval under approvalPolicy "never", so pre-approve Grimoire's own tools.
        // Codex gives up on a tool call after 60 seconds by default; ask_familiar can wait minutes for a reply.
        let server = json!({
            "command": command,
            "args": args,
            "default_tools_approval_mode": "approve",
            "tool_timeout_sec": crate::tools::CALL_TIMEOUT_SECS,
        });
        params["config"] = json!({ "mcp_servers": { (crate::tools::SERVER_NAME): server } });
    }
    if let Some(memory) = crate::memory::session_prompt(agent) {
        params["developerInstructions"] = json!(memory);
    }
    params
}

/// A turn's `input`: each image as a `localImage` Codex reads from disk, then the text.
fn turn_input(text: &str, images: &[PathBuf]) -> Value {
    let mut input: Vec<Value> = images.iter().map(|path| json!({ "type": "localImage", "path": path })).collect();
    if !text.is_empty() || input.is_empty() {
        input.push(json!({ "type": "text", "text": text, "text_elements": [] }));
    }
    Value::Array(input)
}

impl Codex {
    pub async fn start_turn(self: &Arc<Self>, agent: &Agent, text: &str, images: &[PathBuf], tx: EventTx) -> Result<(), String> {
        self.start_turn_in(agent, None, text, images, tx).await
    }

    /// Starts a turn in the familiar's main chat, or in a side conversation when `lane` names one.
    pub async fn start_turn_in(self: &Arc<Self>, agent: &Agent, lane: Option<&Lane>, text: &str, images: &[PathBuf], tx: EventTx) -> Result<(), String> {
        let process = self.ensure_process(&agent.workspace).await?;
        self.idle.cancel();
        let key = lane.map_or(&agent.id, |l| &l.key);
        let known = match lane.map(|l| &l.session) {
            None => agent.session_id.as_deref(),
            Some(LaneSession::Resume(id)) => Some(id.as_str()),
            Some(_) => None,
        };
        let thread_id = match self.open_thread(&process, agent, key, lane).await {
            Ok((id, model)) => {
                if let Some(model) = model {
                    let _ = tx.send(ProviderEvent::Model(model));
                }
                id
            }
            Err(e) => {
                self.arm_idle_if_unused(&process);
                return Err(e);
            }
        };
        if known != Some(thread_id.as_str()) {
            let _ = tx.send(ProviderEvent::Session(thread_id.clone()));
        }
        if let Ok(mut threads) = self.threads.lock() {
            threads.insert(key.clone(), thread_id.clone());
        }
        if let Ok(mut routes) = process.routes.lock() {
            routes.insert(thread_id.clone(), Route { tx: Some(tx), ..Route::default() });
        }
        let mut params = json!({ "threadId": thread_id, "input": turn_input(text, images), "approvalPolicy": approval_policy(), "approvalsReviewer": "user" });
        if let Some(model) = agent.model.as_deref().filter(|m| !m.is_empty()) {
            params["model"] = json!(model);
        }
        if let Some(effort) = agent.effort.as_deref().filter(|e| !e.is_empty()) {
            params["effort"] = json!(effort);
        }
        match process.request("turn/start", params).await {
            Ok(result) => {
                if let (Some(turn_id), Ok(mut routes)) = (result["turn"]["id"].as_str(), process.routes.lock()) {
                    if let Some(route) = routes.get_mut(&thread_id) {
                        route.turn_id = Some(turn_id.to_string());
                    }
                }
                Ok(())
            }
            Err(e) => {
                process.routes.lock().ok().map(|mut r| r.remove(&thread_id));
                self.arm_idle_if_unused(&process);
                Err(e)
            }
        }
    }

    pub async fn stop(&self, agent_id: &str) -> Result<(), String> {
        let Some(thread_id) = self.threads.lock().ok().and_then(|t| t.get(agent_id).cloned()) else {
            return Ok(());
        };
        let Some(process) = self.process.lock().await.clone() else { return Ok(()) };
        let turn_id = process
            .routes
            .lock()
            .ok()
            .and_then(|r| r.get(&thread_id).and_then(|route| route.turn_id.clone()));
        match turn_id {
            Some(turn_id) => process
                .request("turn/interrupt", json!({ "threadId": thread_id, "turnId": turn_id }))
                .await
                .map(|_| ()),
            None => Ok(()),
        }
    }

    /// The account's plan limits, from `account/rateLimits/read`.
    pub async fn rate_limits(self: &Arc<Self>, cwd: &str) -> Result<Value, String> {
        let process = self.ensure_process(cwd).await?;
        let result = process.request("account/rateLimits/read", json!({ "excludeResetCreditDetails": true })).await;
        self.arm_idle_if_unused(&process);
        result
    }

    /// Makes the familiar's next turns resume with its current settings: its main thread and every
    /// side conversation's, so a lowered access level reaches them all.
    pub async fn reload_thread(&self, agent_id: &str) {
        let threads: Vec<String> = self
            .threads
            .lock()
            .map(|t| t.iter().filter(|(key, _)| crate::side::split(key).0 == agent_id).map(|(_, thread)| thread.clone()).collect())
            .unwrap_or_default();
        let process = self.process.lock().await.clone();
        if let Some(process) = process {
            if let Ok(mut loaded) = process.loaded.lock() {
                loaded.retain(|thread| !threads.contains(thread));
            }
        }
    }

    pub fn forget(&self, agent_id: &str) {
        if let Ok(mut threads) = self.threads.lock() {
            threads.remove(agent_id);
        }
    }

    async fn open_thread(&self, process: &Arc<CodexProcess>, agent: &Agent, key: &str, lane: Option<&Lane>) -> Result<(String, Option<String>), String> {
        let model_of = |result: &Value| result["model"].as_str().map(str::to_string);
        let (saved, fork) = match lane.map(|l| &l.session) {
            None => (agent.session_id.as_deref(), None),
            Some(LaneSession::Resume(id)) => (Some(id.as_str()), None),
            Some(LaneSession::Fork(id)) => (None, Some(id.as_str())),
            Some(LaneSession::Fresh) => (None, None),
        };
        if let Some(saved) = saved {
            if process.loaded.lock().map(|l| l.contains(saved)).unwrap_or(false) {
                return Ok((saved.to_string(), None));
            }
            let mut params = thread_params(agent, key);
            params["threadId"] = json!(saved);
            if let Ok(result) = process.request("thread/resume", params).await {
                process.loaded.lock().ok().map(|mut l| l.insert(saved.to_string()));
                return Ok((saved.to_string(), model_of(&result)));
            }
        }
        if let Some(main) = fork {
            let mut params = thread_params(agent, key);
            params["threadId"] = json!(main);
            params["excludeTurns"] = json!(true);
            // A main thread Codex can't copy, say one it lost, starts the side conversation fresh.
            if let Ok(result) = process.request("thread/fork", params).await {
                if let Some(id) = result["thread"]["id"].as_str() {
                    process.loaded.lock().ok().map(|mut l| l.insert(id.to_string()));
                    return Ok((id.to_string(), model_of(&result)));
                }
            }
        }
        let result = process.request("thread/start", thread_params(agent, key)).await?;
        let id = result["thread"]["id"].as_str().ok_or("codex returned no thread id")?.to_string();
        process.loaded.lock().ok().map(|mut l| l.insert(id.clone()));
        Ok((id, model_of(&result)))
    }

    pub async fn list_models(self: &Arc<Self>, cwd: &str) -> Result<Vec<ModelOption>, String> {
        let process = self.ensure_process(cwd).await?;
        let result = process.request("model/list", json!({})).await;
        self.arm_idle_if_unused(&process);
        Ok(parse_models(&result?))
    }

    fn arm_idle_if_unused(self: &Arc<Self>, process: &Arc<CodexProcess>) {
        if process.route_count() > 0 {
            return;
        }
        let codex = self.clone();
        let target = process.clone();
        self.idle.arm(IDLE_EXIT, move || {
            tauri::async_runtime::spawn(async move {
                let mut slot = codex.process.lock().await;
                if slot.as_ref().is_some_and(|p| Arc::ptr_eq(p, &target)) && target.route_count() == 0 {
                    *slot = None;
                    target.kill();
                }
            });
        });
    }

    async fn ensure_process(self: &Arc<Self>, cwd: &str) -> Result<Arc<CodexProcess>, String> {
        let mut slot = self.process.lock().await;
        if let Some(process) = slot.as_ref().filter(|p| p.alive()) {
            return Ok(process.clone());
        }
        let mut child = command("codex", cwd)
            .arg("app-server")
            .spawn()
            .map_err(|e| spawn_error("codex", e))?;
        let stdin = child.stdin.take().ok_or("codex has no stdin")?;
        let stdout = child.stdout.take().ok_or("codex has no stdout")?;
        let stderr = StderrTail::default();
        if let Some(err) = child.stderr.take() {
            stderr.collect(err);
        }
        let process = Arc::new(CodexProcess {
            stdin: tokio::sync::Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
            loaded: Mutex::new(HashSet::new()),
        });

        let reader = process.clone();
        let codex = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
                codex.handle_message(&reader, message).await;
            }
            reader.kill();
            let detail = stderr.text();
            let reason = if detail.is_empty() { "codex stopped unexpectedly.".to_string() } else { detail };
            if let Ok(mut pending) = reader.pending.lock() {
                for (_, waiter) in pending.drain() {
                    let _ = waiter.send(Err(reason.clone()));
                }
            }
            if let Ok(mut routes) = reader.routes.lock() {
                for (_, route) in routes.drain() {
                    if let Some(tx) = route.tx {
                        let _ = tx.send(ProviderEvent::Failed(reason.clone()));
                    }
                }
            }
        });

        let init = json!({ "clientInfo": { "name": "grimoire", "title": "Grimoire", "version": env!("CARGO_PKG_VERSION") }, "capabilities": null });
        process.request("initialize", init).await?;
        process.write(&json!({ "method": "initialized" })).await?;
        *slot = Some(process.clone());
        Ok(process)
    }

    async fn handle_message(self: &Arc<Self>, process: &Arc<CodexProcess>, message: Value) {
        let method = message.get("method").and_then(Value::as_str);
        match (message.get("id"), method) {
            (Some(id), Some(method)) => {
                let reply = match method {
                    "item/commandExecution/requestApproval" => {
                        // Grimoire's rules decide, which can mean waiting on the person, so answer off the reader.
                        let params = message.get("params").cloned().unwrap_or(Value::Null);
                        let thread = params["threadId"].as_str().unwrap_or_default().to_string();
                        let agent = self.threads.lock().ok().and_then(|t| t.iter().find(|(_, th)| **th == thread).map(|(a, _)| a.clone()));
                        let command = unwrap_shell(params["command"].as_str().unwrap_or_default()).to_string();
                        let (id, process) = (id.clone(), process.clone());
                        tauri::async_runtime::spawn(async move {
                            let allowed = match agent {
                                Some(key) => crate::permissions::check_command(crate::side::split(&key).0, &command, codex_request_key(&id)).await,
                                None => false,
                            };
                            let decision = if allowed { "accept" } else { "decline" };
                            let _ = process.write(&json!({ "id": id, "result": { "decision": decision } })).await;
                        });
                        return;
                    }
                    "item/fileChange/requestApproval" => json!({ "id": id, "result": { "decision": "decline" } }),
                    _ => json!({ "id": id, "error": { "code": -32601, "message": "Grimoire does not support this request." } }),
                };
                let _ = process.write(&reply).await;
            }
            (Some(id), None) => {
                let waiter = id.as_u64().and_then(|id| process.pending.lock().ok()?.remove(&id));
                if let Some(waiter) = waiter {
                    let result = match message.get("error") {
                        Some(error) => Err(error["message"].as_str().unwrap_or("codex request failed").to_string()),
                        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = waiter.send(result);
                }
            }
            (None, Some("serverRequest/resolved")) => {
                // Codex settled a request on its own; take its card down.
                crate::permissions::withdraw(&codex_request_key(&message["params"]["requestId"]));
            }
            (None, Some(method)) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else { return };
                let finished = {
                    let Ok(mut routes) = process.routes.lock() else { return };
                    let Some(route) = routes.get_mut(thread_id) else { return };
                    let (events, finished) = map_notification(method, &params, route);
                    if let Some(tx) = &route.tx {
                        for event in events {
                            let _ = tx.send(event);
                        }
                    }
                    if finished {
                        routes.remove(thread_id);
                    }
                    finished
                };
                if finished {
                    self.arm_idle_if_unused(process);
                }
            }
            (None, None) => {}
        }
    }
}

/// The approval id for a Codex server request, so a later `serverRequest/resolved` can find it.
fn codex_request_key(id: &Value) -> String {
    format!("codex-{id}")
}

fn unwrap_shell(command: &str) -> &str {
    let Some((shell, rest)) = command.split_once(" -lc ").or_else(|| command.split_once(" -c ")) else {
        return command;
    };
    if !matches!(shell.rsplit('/').next(), Some("zsh" | "bash" | "sh")) {
        return command;
    }
    let rest = rest.trim();
    for quote in ['\'', '"'] {
        if let Some(inner) = rest.strip_prefix(quote).and_then(|r| r.strip_suffix(quote)) {
            return inner;
        }
    }
    rest
}

pub fn parse_models(result: &Value) -> Vec<ModelOption> {
    let str_of = |v: &Value| v.as_str().map(str::to_string);
    result["data"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter(|m| !m["hidden"].as_bool().unwrap_or(false))
                .filter_map(|m| {
                    Some(ModelOption {
                        id: str_of(&m["model"]).or_else(|| str_of(&m["id"]))?,
                        name: str_of(&m["displayName"]).unwrap_or_default(),
                        description: str_of(&m["description"]).unwrap_or_default(),
                        efforts: m["supportedReasoningEfforts"]
                            .as_array()
                            .map(|e| e.iter().filter_map(|o| str_of(&o["reasoningEffort"])).collect())
                            .unwrap_or_default(),
                        default_effort: str_of(&m["defaultReasoningEffort"]),
                        is_default: m["isDefault"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tool_label(item: &Value) -> Option<String> {
    let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    match item.get("type").and_then(Value::as_str)? {
        "commandExecution" => Some(format!("$ {}", unwrap_shell(&text("command")))),
        "fileChange" => {
            let paths: Vec<&str> = item["changes"]
                .as_array()
                .map(|c| c.iter().filter_map(|c| c["path"].as_str()).collect())
                .unwrap_or_default();
            Some(format!("Edit {}", paths.join(", ")))
        }
        // Grimoire adds its own row that opens the side conversation.
        "mcpToolCall" if text("server") == crate::tools::SERVER_NAME && matches!(text("tool").as_str(), "ask_familiar" | "message_familiar") => {
            Some(String::new())
        }
        "mcpToolCall" => Some(format!("{}.{}", text("server"), text("tool"))),
        "dynamicToolCall" => Some(text("tool")),
        "webSearch" => Some("Web search".into()),
        _ => None,
    }
}

pub fn map_notification(method: &str, params: &Value, route: &mut Route) -> (Vec<ProviderEvent>, bool) {
    match method {
        "item/agentMessage/delta" => {
            let delta = params["delta"].as_str().unwrap_or_default();
            let item = params["itemId"].as_str().map(str::to_string);
            let mut events = Vec::new();
            if route.has_text && route.last_item != item {
                events.push(ProviderEvent::Delta("\n\n".into()));
            }
            route.last_item = item;
            if !delta.is_empty() {
                route.has_text = true;
                events.push(ProviderEvent::Delta(delta.to_string()));
            }
            (events, false)
        }
        "item/started" => (tool_label(&params["item"]).map(ProviderEvent::Tool).into_iter().collect(), false),
        "thread/tokenUsage/updated" => {
            // `total` covers the whole thread and `last` the latest model call, so the first update of a
            // turn tells where the turn started.
            let tokens = |b: &Value| (b["inputTokens"].as_u64().unwrap_or(0), b["outputTokens"].as_u64().unwrap_or(0));
            let (total, last) = (tokens(&params["tokenUsage"]["total"]), tokens(&params["tokenUsage"]["last"]));
            let base = *route.usage_base.get_or_insert((total.0.saturating_sub(last.0), total.1.saturating_sub(last.1)));
            (vec![ProviderEvent::Usage { input: total.0.saturating_sub(base.0), output: total.1.saturating_sub(base.1) }], false)
        }
        "turn/completed" => {
            let turn = &params["turn"];
            let event = match turn["status"].as_str() {
                Some("interrupted") => ProviderEvent::Stopped,
                Some("failed") => ProviderEvent::Failed(
                    turn["error"]["message"].as_str().unwrap_or("codex could not finish the turn.").to_string(),
                ),
                _ => ProviderEvent::Done,
            };
            (vec![event], true)
        }
        _ => (vec![], false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_images_as_local_images_before_the_text() {
        let input = turn_input("What's this?", &[PathBuf::from("/data/attachments/4/shot.png")]);
        assert_eq!(
            input,
            json!([
                { "type": "localImage", "path": "/data/attachments/4/shot.png" },
                { "type": "text", "text": "What's this?", "text_elements": [] }
            ])
        );
        assert_eq!(turn_input("", &[PathBuf::from("/a.png")]), json!([{ "type": "localImage", "path": "/a.png" }]));
        assert_eq!(turn_input("hi", &[]), json!([{ "type": "text", "text": "hi", "text_elements": [] }]));
    }

    fn run(steps: &[(&str, Value)]) -> (Vec<ProviderEvent>, bool) {
        let mut route = Route::default();
        let mut events = Vec::new();
        let mut finished = false;
        for (method, params) in steps {
            let (mut e, done) = map_notification(method, params, &mut route);
            events.append(&mut e);
            finished = done;
        }
        (events, finished)
    }

    #[test]
    fn counts_only_this_turns_tokens() {
        let usage = |total: (u64, u64), last: (u64, u64)| {
            json!({ "threadId": "t", "turnId": "u", "tokenUsage": {
                "total": { "inputTokens": total.0, "outputTokens": total.1 },
                "last": { "inputTokens": last.0, "outputTokens": last.1 } } })
        };
        // The thread had 1000 in and 200 out before this turn; the turn makes two model calls.
        let (events, _) = run(&[("thread/tokenUsage/updated", usage((1300, 250), (300, 50))), ("thread/tokenUsage/updated", usage((1700, 300), (400, 50)))]);
        assert_eq!(events.last(), Some(&ProviderEvent::Usage { input: 700, output: 100 }));
    }

    #[test]
    fn streams_text_and_tools() {
        let (events, finished) = run(&[
            ("item/agentMessage/delta", json!({"threadId":"t","itemId":"m1","delta":"Looking"})),
            ("item/started", json!({"threadId":"t","item":{"type":"commandExecution","id":"c1","command":"ls"}})),
            ("item/started", json!({"threadId":"t","item":{"type":"fileChange","id":"f1","changes":[{"path":"a.rs"},{"path":"b.rs"}]}})),
            ("item/started", json!({"threadId":"t","item":{"type":"reasoning","id":"r1"}})),
            ("item/started", json!({"threadId":"t","item":{"type":"mcpToolCall","id":"m","server":"grimoire","tool":"ask_familiar","arguments":{"familiar":"Scribe"}}})),
            ("item/started", json!({"threadId":"t","item":{"type":"mcpToolCall","id":"n","server":"grimoire","tool":"list_spells","arguments":{}}})),
            ("item/agentMessage/delta", json!({"threadId":"t","itemId":"m2","delta":"Done"})),
            ("item/agentMessage/delta", json!({"threadId":"t","itemId":"m2","delta":"!"})),
            ("turn/completed", json!({"threadId":"t","turn":{"status":"completed"}})),
        ]);
        assert!(finished);
        assert_eq!(
            events,
            vec![
                ProviderEvent::Delta("Looking".into()),
                ProviderEvent::Tool("$ ls".into()),
                ProviderEvent::Tool("Edit a.rs, b.rs".into()),
                ProviderEvent::Tool(String::new()),
                ProviderEvent::Tool("grimoire.list_spells".into()),
                ProviderEvent::Delta("\n\n".into()),
                ProviderEvent::Delta("Done".into()),
                ProviderEvent::Delta("!".into()),
                ProviderEvent::Done,
            ]
        );
    }

    #[test]
    fn shows_commands_without_the_shell_wrapper() {
        assert_eq!(unwrap_shell("/bin/zsh -lc 'ls -la'"), "ls -la");
        assert_eq!(unwrap_shell("bash -lc \"pwd; git status\""), "pwd; git status");
        assert_eq!(unwrap_shell("/bin/sh -c echo hi"), "echo hi");
        assert_eq!(unwrap_shell("python -c 'print(1)'"), "python -c 'print(1)'");
        assert_eq!(unwrap_shell("cargo test"), "cargo test");
    }

    #[test]
    fn reads_the_model_catalog() {
        let result = json!({"data": [
            {"id": "gpt-6.2-sol", "model": "gpt-6.2-sol", "displayName": "GPT-6.2-Sol", "description": "Default",
             "hidden": false, "isDefault": true, "defaultReasoningEffort": "low",
             "supportedReasoningEfforts": [{"reasoningEffort": "low"}, {"reasoningEffort": "high"}]},
            {"id": "secret", "model": "secret", "displayName": "Hidden", "hidden": true}
        ]});
        let models = parse_models(&result);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "GPT-6.2-Sol");
        assert_eq!(models[0].efforts, ["low", "high"]);
        assert!(models[0].is_default);
    }

    #[test]
    fn maps_turn_end_states() {
        let failed = run(&[("turn/completed", json!({"turn":{"status":"failed","error":{"message":"quota"}}}))]);
        assert_eq!(failed.0, vec![ProviderEvent::Failed("quota".into())]);
        let stopped = run(&[("turn/completed", json!({"turn":{"status":"interrupted","error":null}}))]);
        assert_eq!(stopped.0, vec![ProviderEvent::Stopped]);
    }

    #[test]
    fn workspace_access_uses_the_sandbox() {
        let agent = Agent {
            id: "a".into(),
            name: "Ada".into(),
            provider: crate::db::Provider::Codex,
            model: None,
            workspace: "/tmp/ada".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: crate::db::Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: None,
            created_at: 0,
        };
        let params = thread_params(&agent, &agent.id);
        assert_eq!(params["sandbox"], "workspace-write");
        assert_eq!(params["approvalPolicy"], "never");
        assert!(params.get("model").is_none());
        assert!(params.get("developerInstructions").is_none(), "no memory yet");

        let agent = Agent { id: "memory-codex".into(), ..agent };
        let dir = crate::memory::test_dir();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("memory-codex.md"), "Deploys go out on Fridays.").unwrap();
        let instructions = thread_params(&agent, &agent.id)["developerInstructions"].as_str().unwrap().to_string();
        assert!(instructions.contains("Deploys go out on Fridays."));
    }
}
