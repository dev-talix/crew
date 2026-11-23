use super::{command, spawn_error, EventTx, IdleTimer, ProviderEvent, StderrTail};
use crate::db::{Access, Agent};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};

const IDLE_EXIT: Duration = Duration::from_secs(5 * 60);
const INTERRUPT_GRACE: Duration = Duration::from_secs(5);

struct ClaudeProcess {
    config: String,
    stdin: tokio::sync::Mutex<ChildStdin>,
    child: Mutex<Option<Child>>,
    route: Mutex<Option<EventTx>>,
    interrupting: AtomicBool,
    idle: IdleTimer,
}

impl ClaudeProcess {
    fn take_route(&self) -> Option<EventTx> {
        self.route.lock().ok().and_then(|mut r| r.take())
    }

    async fn write_line(&self, value: &Value) -> std::io::Result<()> {
        let line = value.to_string() + "\n";
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await
    }

    fn kill(&self) {
        if let Some(mut child) = self.child.lock().ok().and_then(|mut c| c.take()) {
            let _ = child.start_kill();
        }
    }
}

pub fn models() -> Vec<super::ModelOption> {
    let efforts: Vec<String> = ["low", "medium", "high", "xhigh", "max"].map(String::from).to_vec();
    [
        ("opus", "Opus", "The most capable model for hard, long tasks."),
        ("fable", "Fable", "Anthropic's newest model."),
        ("sonnet", "Sonnet", "Fast and strong for everyday coding."),
        ("haiku", "Haiku", "The fastest model, for quick small tasks."),
    ]
    .into_iter()
    .map(|(id, name, description)| super::ModelOption {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        efforts: efforts.clone(),
        default_effort: None,
        is_default: false,
    })
    .collect()
}

#[derive(Default)]
pub struct Claude {
    processes: Arc<Mutex<HashMap<String, Arc<ClaudeProcess>>>>,
}

fn config_key(agent: &Agent) -> String {
    format!("{:?}|{:?}|{:?}|{}", agent.model, agent.effort, agent.access, agent.workspace)
}

fn args(agent: &Agent) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(match agent.access {
        Access::Workspace => "acceptEdits".into(),
        Access::Full => "bypassPermissions".into(),
    });
    if let Some(model) = agent.model.as_deref().filter(|m| !m.is_empty()) {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(effort) = agent.effort.as_deref().filter(|e| !e.is_empty()) {
        args.extend(["--effort".into(), effort.into()]);
    }
    if let Some(session) = agent.session_id.as_deref() {
        args.extend(["--resume".into(), session.into()]);
    }
    args
}

impl Claude {
    pub async fn start_turn(&self, agent: &Agent, text: &str, tx: EventTx) -> Result<(), String> {
        let process = self.process_for(agent)?;
        process.idle.cancel();
        process.interrupting.store(false, Ordering::SeqCst);
        if let Ok(mut route) = process.route.lock() {
            *route = Some(tx);
        }
        let message = json!({ "type": "user", "message": { "role": "user", "content": text } });
        if let Err(e) = process.write_line(&message).await {
            process.take_route();
            self.forget(&agent.id);
            return Err(format!("Could not send the message to claude: {e}"));
        }
        Ok(())
    }

    pub async fn stop(&self, agent_id: &str) {
        let Some(process) = self.processes.lock().ok().and_then(|map| map.get(agent_id).cloned()) else {
            return;
        };
        let Some(turn) = process.route.lock().ok().and_then(|r| r.clone()) else {
            return;
        };
        process.interrupting.store(true, Ordering::SeqCst);
        let request = json!({ "type": "control_request", "request_id": uuid::Uuid::new_v4().to_string(), "request": { "subtype": "interrupt" } });
        let interrupted = process.write_line(&request).await.is_ok();
        let registry = self.processes.clone();
        let agent_id = agent_id.to_string();
        tauri::async_runtime::spawn(async move {
            if interrupted {
                tokio::time::sleep(INTERRUPT_GRACE).await;
            }
            let still_this_turn = process
                .route
                .lock()
                .map(|r| r.as_ref().is_some_and(|tx| tx.same_channel(&turn)))
                .unwrap_or(false);
            if !still_this_turn {
                return;
            }
            if let Some(tx) = process.take_route() {
                let _ = tx.send(ProviderEvent::Stopped);
            }
            if let Ok(mut map) = registry.lock() {
                if map.get(&agent_id).is_some_and(|p| Arc::ptr_eq(p, &process)) {
                    map.remove(&agent_id);
                }
            }
            process.idle.cancel();
            process.kill();
        });
    }

    pub fn forget_idle(&self) {
        let idle: Vec<String> = self
            .processes
            .lock()
            .map(|map| map.iter().filter(|(_, p)| p.route.lock().map(|r| r.is_none()).unwrap_or(false)).map(|(id, _)| id.clone()).collect())
            .unwrap_or_default();
        for id in idle {
            self.forget(&id);
        }
    }

    pub fn forget(&self, agent_id: &str) {
        let process = self.processes.lock().ok().and_then(|mut map| map.remove(agent_id));
        if let Some(process) = process {
            process.idle.cancel();
            process.kill();
        }
    }

    fn process_for(&self, agent: &Agent) -> Result<Arc<ClaudeProcess>, String> {
        let mut processes = self.processes.lock().map_err(|_| "claude state poisoned")?;
        let key = config_key(agent);
        if let Some(existing) = processes.get(&agent.id) {
            if existing.config == key && existing.child.lock().map(|c| c.is_some()).unwrap_or(false) {
                return Ok(existing.clone());
            }
            existing.kill();
        }
        let mut child = command("claude", &agent.workspace)
            .args(args(agent))
            .spawn()
            .map_err(|e| spawn_error("claude", e))?;
        let stdin = child.stdin.take().ok_or("claude has no stdin")?;
        let stdout = child.stdout.take().ok_or("claude has no stdout")?;
        let stderr = StderrTail::default();
        if let Some(err) = child.stderr.take() {
            stderr.collect(err);
        }
        let process = Arc::new(ClaudeProcess {
            config: key,
            stdin: tokio::sync::Mutex::new(stdin),
            child: Mutex::new(Some(child)),
            route: Mutex::new(None),
            interrupting: AtomicBool::new(false),
            idle: IdleTimer::default(),
        });
        processes.insert(agent.id.clone(), process.clone());

        let agent_id = agent.id.clone();
        let registry = self.processes.clone();
        let reader = process.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut parser = ClaudeParser::default();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                for mut event in parser.parse(&value) {
                    let finished = matches!(event, ProviderEvent::Done | ProviderEvent::Failed(_));
                    if finished && reader.interrupting.swap(false, Ordering::SeqCst) {
                        event = ProviderEvent::Stopped;
                    }
                    let route = reader.route.lock().ok().and_then(|r| r.clone());
                    if let Some(tx) = route {
                        let _ = tx.send(event);
                    }
                    if finished {
                        reader.take_route();
                        let registry = registry.clone();
                        let id = agent_id.clone();
                        let idle = reader.clone();
                        reader.idle.arm(IDLE_EXIT, move || {
                            let mut map = match registry.lock() {
                                Ok(map) => map,
                                Err(_) => return,
                            };
                            if map.get(&id).is_some_and(|p| Arc::ptr_eq(p, &idle)) {
                                map.remove(&id);
                            }
                            idle.kill();
                        });
                    }
                }
            }
            if let Some(tx) = reader.take_route() {
                let detail = stderr.text();
                let message = if detail.is_empty() { "claude stopped unexpectedly.".to_string() } else { detail };
                let _ = tx.send(ProviderEvent::Failed(message));
            }
            reader.idle.cancel();
            reader.kill();
            if let Ok(mut map) = registry.lock() {
                if map.get(&agent_id).is_some_and(|p| Arc::ptr_eq(p, &reader)) {
                    map.remove(&agent_id);
                }
            }
        });
        Ok(process)
    }
}

#[derive(Default)]
pub struct ClaudeParser {
    has_text: bool,
}

fn tool_label(name: &str, input: &Value) -> String {
    let field = |key: &str| input.get(key).and_then(Value::as_str);
    match name {
        "Bash" => field("command").map(|c| format!("$ {c}")),
        "Read" | "Edit" | "Write" | "NotebookEdit" => field("file_path").map(|p| format!("{name} {p}")),
        "Grep" | "Glob" => field("pattern").map(|p| format!("{name} {p}")),
        "WebFetch" => field("url").map(|u| format!("Fetch {u}")),
        "WebSearch" => field("query").map(|q| format!("Search {q}")),
        _ => None,
    }
    .unwrap_or_else(|| name.to_string())
}

impl ClaudeParser {
    pub fn parse(&mut self, value: &Value) -> Vec<ProviderEvent> {
        let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
        let nested = value.get("parent_tool_use_id").is_some_and(|p| !p.is_null());
        match kind {
            "system" if value.get("subtype").and_then(Value::as_str) == Some("init") => {
                let mut events = Vec::new();
                if let Some(id) = value.get("session_id").and_then(Value::as_str) {
                    events.push(ProviderEvent::Session(id.to_string()));
                }
                if let Some(model) = value.get("model").and_then(Value::as_str) {
                    events.push(ProviderEvent::Model(model.to_string()));
                }
                events
            }
            "stream_event" if !nested => {
                let event = &value["event"];
                match event.get("type").and_then(Value::as_str) {
                    Some("content_block_start") if event["content_block"]["type"] == "text" && self.has_text => {
                        vec![ProviderEvent::Delta("\n\n".into())]
                    }
                    Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                        let text = event["delta"]["text"].as_str().unwrap_or_default();
                        if text.is_empty() {
                            return vec![];
                        }
                        self.has_text = true;
                        vec![ProviderEvent::Delta(text.to_string())]
                    }
                    _ => vec![],
                }
            }
            "assistant" if !nested => value["message"]["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b["type"] == "tool_use")
                        .map(|b| ProviderEvent::Tool(tool_label(b["name"].as_str().unwrap_or("tool"), &b["input"])))
                        .collect()
                })
                .unwrap_or_default(),
            "result" => {
                self.has_text = false;
                let failed = value["is_error"].as_bool().unwrap_or(false)
                    || value.get("subtype").and_then(Value::as_str) != Some("success");
                if failed {
                    let reason = value["result"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| value["errors"].as_array().map(|e| {
                            e.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")
                        }))
                        .filter(|r| !r.is_empty())
                        .unwrap_or_else(|| format!("claude ended the turn: {}", value["subtype"].as_str().unwrap_or("error")));
                    vec![ProviderEvent::Failed(reason)]
                } else {
                    vec![ProviderEvent::Done]
                }
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(lines: &[Value]) -> Vec<ProviderEvent> {
        let mut parser = ClaudeParser::default();
        lines.iter().flat_map(|l| parser.parse(l)).collect()
    }

    fn delta(text: &str) -> Value {
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}})
    }

    fn text_start() -> Value {
        json!({"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}})
    }

    #[test]
    fn streams_a_turn() {
        let events = parse_all(&[
            json!({"type":"system","subtype":"init","session_id":"s1","model":"claude-opus-5-5"}),
            text_start(),
            delta("po"),
            delta("ng"),
            json!({"type":"assistant","parent_tool_use_id":null,"message":{"content":[
                {"type":"text","text":"pong"},
                {"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}
            ]}}),
            text_start(),
            delta("done"),
            json!({"type":"result","subtype":"success","is_error":false,"result":"pong"}),
        ]);
        assert_eq!(
            events,
            vec![
                ProviderEvent::Session("s1".into()),
                ProviderEvent::Model("claude-opus-5-5".into()),
                ProviderEvent::Delta("po".into()),
                ProviderEvent::Delta("ng".into()),
                ProviderEvent::Tool("$ ls -la".into()),
                ProviderEvent::Delta("\n\n".into()),
                ProviderEvent::Delta("done".into()),
                ProviderEvent::Done,
            ]
        );
    }

    #[test]
    fn ignores_subagent_output() {
        let mut nested = delta("hidden");
        nested["parent_tool_use_id"] = json!("toolu_1");
        assert!(parse_all(&[nested]).is_empty());
    }

    #[test]
    fn reports_failed_results() {
        let events = parse_all(&[json!({"type":"result","subtype":"error_max_turns","is_error":true})]);
        assert_eq!(events, vec![ProviderEvent::Failed("claude ended the turn: error_max_turns".into())]);
        let events = parse_all(&[json!({"type":"result","subtype":"success","is_error":true,"result":"Credit balance is too low"})]);
        assert_eq!(events, vec![ProviderEvent::Failed("Credit balance is too low".into())]);
    }

    #[test]
    fn separator_resets_between_turns() {
        let events = parse_all(&[
            delta("a"),
            json!({"type":"result","subtype":"success","is_error":false}),
            text_start(),
            delta("b"),
        ]);
        assert_eq!(
            events,
            vec![ProviderEvent::Delta("a".into()), ProviderEvent::Done, ProviderEvent::Delta("b".into())]
        );
    }

    #[test]
    fn resumes_saved_session_with_chosen_access() {
        let agent = Agent {
            id: "a".into(),
            name: "Ada".into(),
            provider: crate::db::Provider::Claude,
            model: Some("sonnet".into()),
            workspace: "/tmp".into(),
            access: Access::Full,
            session_id: Some("s1".into()),
            avatar: crate::db::Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            created_at: 0,
        };
        let joined = args(&agent).join(" ");
        assert!(joined.contains("--permission-mode bypassPermissions"));
        assert!(joined.contains("--model sonnet"));
        assert!(!joined.contains("--effort"));
        assert!(joined.ends_with("--resume s1"));
    }
}
