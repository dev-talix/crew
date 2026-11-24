use super::{launch, spawn_error, EventTx, IdleTimer, Lane, LaneSession, ProviderEvent, StderrTail};
use crate::db::{Access, Agent};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};

const IDLE_EXIT: Duration = Duration::from_secs(5 * 60);
const INTERRUPT_GRACE: Duration = Duration::from_secs(5);
const BASH_HOOK: &str = "grimoire_bash";

struct ClaudeProcess {
    config: String,
    stdin: tokio::sync::Mutex<ChildStdin>,
    child: Mutex<Option<Child>>,
    route: Mutex<Option<EventTx>>,
    interrupting: AtomicBool,
    /// Whether the Bash hook that routes commands through Grimoire's rules is registered.
    hooked: AtomicBool,
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
    format!("{:?}|{:?}|{:?}|{}|{:?}", agent.model, agent.effort, agent.access, agent.workspace, agent.host)
}

fn args(agent: &Agent, lane: Option<&Lane>) -> Vec<String> {
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
    // In a side conversation, Grimoire's tools know which one the familiar is calling from.
    // The tool socket is on this Mac, so a familiar on another Mac runs without the tools.
    if let Some(tools) = crate::tools::server_command(lane.map_or(&agent.id, |l| &l.key)).filter(|_| agent.host.is_none()) {
        // "timeout" (ms) caps each tool call, and also lifts Claude's idle cutoff for silent calls like ask_familiar.
        let server = json!({
            "command": tools.command,
            "args": tools.args,
            "env": { (crate::tools::TOKEN_ENV): tools.token },
            "timeout": crate::tools::CALL_TIMEOUT_SECS * 1000,
        });
        let key = lane.map_or(&agent.id, |l| &l.key);
        let mut servers = serde_json::Map::from_iter([(crate::tools::SERVER_NAME.to_string(), server)]);
        let mut allowed = crate::tools::claude_tool_names();
        // Each connector the familiar has on runs through its own relay; the person turned it on, so its tools are allowed.
        for (name, id) in crate::connectors::servers_for(agent) {
            if let Some(relay) = crate::tools::connector_command(key, &id) {
                let relay = json!({ "command": relay.command, "args": relay.args, "env": { (crate::tools::TOKEN_ENV): relay.token }, "timeout": crate::tools::CALL_TIMEOUT_SECS * 1000 });
                allowed.push_str(&format!(",mcp__{name}"));
                servers.insert(name, relay);
            }
        }
        let config = json!({ "mcpServers": servers });
        if let Some(path) = crate::tools::claude_config_file(key, &config) {
            args.extend(["--mcp-config".into(), path.to_string_lossy().into_owned(), "--allowedTools".into(), allowed]);
        }
    }
    if let Some(model) = agent.model.as_deref().filter(|m| !m.is_empty()) {
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(effort) = agent.effort.as_deref().filter(|e| !e.is_empty()) {
        args.extend(["--effort".into(), effort.into()]);
    }
    if let Some(memory) = crate::briefing::session_prompt(agent) {
        // Claude records the system prompt when a chat starts and reuses it on resume, so a changed
        // memory reaches the familiar in its next new chat.
        args.extend(["--append-system-prompt".into(), memory]);
    }
    match lane.map(|l| &l.session) {
        None => {
            if let Some(session) = agent.session_id.as_deref() {
                args.extend(["--resume".into(), session.into()]);
            }
        }
        Some(LaneSession::Resume(session)) => args.extend(["--resume".into(), session.clone()]),
        Some(LaneSession::Fork(session)) => args.extend(["--resume".into(), session.clone(), "--fork-session".into()]),
        Some(LaneSession::Fresh) => {}
    }
    args
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { BASE64[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// The user message's content: plain text, or each image as a base64 block followed by the text.
fn user_content(text: &str, images: &[(&'static str, Vec<u8>)]) -> Value {
    if images.is_empty() {
        return json!(text);
    }
    let mut blocks: Vec<Value> = images
        .iter()
        .map(|(media_type, bytes)| json!({ "type": "image", "source": { "type": "base64", "media_type": media_type, "data": base64(bytes) } }))
        .collect();
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    Value::Array(blocks)
}

fn read_image(path: &Path) -> Result<(&'static str, Vec<u8>), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read {name}: {e}"))?;
    let media_type = crate::attachments::image_type(&bytes).ok_or(format!("{name} is not a PNG, JPEG, GIF, or WebP image."))?;
    Ok((media_type, bytes))
}

impl Claude {
    pub async fn start_turn(&self, agent: &Agent, text: &str, images: &[PathBuf], tx: EventTx) -> Result<(), String> {
        self.start_turn_in(agent, None, text, images, tx).await
    }

    /// Starts a turn in the familiar's main chat, or in a side conversation when `lane` names one.
    pub async fn start_turn_in(&self, agent: &Agent, lane: Option<&Lane>, text: &str, images: &[PathBuf], tx: EventTx) -> Result<(), String> {
        let images = images.iter().map(|p| read_image(p)).collect::<Result<Vec<_>, _>>()?;
        let process = self.process_for(agent, lane)?;
        process.idle.cancel();
        process.interrupting.store(false, Ordering::SeqCst);
        if let Ok(mut route) = process.route.lock() {
            *route = Some(tx);
        }
        let message = json!({ "type": "user", "message": { "role": "user", "content": user_content(text, &images) } });
        let hook = json!({
            "type": "control_request",
            "request_id": uuid::Uuid::new_v4().to_string(),
            "request": { "subtype": "initialize", "hooks": { "PreToolUse": [{ "matcher": "Bash", "hookCallbackIds": [BASH_HOOK] }] } },
        });
        let sent = match process.hooked.swap(true, Ordering::SeqCst) {
            true => Ok(()),
            false => process.write_line(&hook).await,
        };
        if let Err(e) = match sent {
            Ok(()) => process.write_line(&message).await,
            Err(e) => Err(e),
        } {
            process.take_route();
            self.forget(lane.map_or(&agent.id, |l| &l.key));
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

    fn process_for(&self, agent: &Agent, lane: Option<&Lane>) -> Result<Arc<ClaudeProcess>, String> {
        let mut processes = self.processes.lock().map_err(|_| "claude state poisoned")?;
        let id = lane.map_or(&agent.id, |l| &l.key);
        let key = config_key(agent);
        if let Some(existing) = processes.get(id) {
            if existing.config == key && existing.child.lock().map(|c| c.is_some()).unwrap_or(false) {
                return Ok(existing.clone());
            }
            existing.kill();
        }
        let mut child = launch(agent.host.as_deref(), &agent.workspace, "claude", args(agent, lane))
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
            hooked: AtomicBool::new(false),
            idle: IdleTimer::default(),
        });
        processes.insert(id.clone(), process.clone());

        let agent_id = id.clone();
        let familiar = agent.id.clone();
        let registry = self.processes.clone();
        let reader = process.clone();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut parser = ClaudeParser::default();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                if value["type"] == "control_request" && value["request"]["callback_id"] == BASH_HOOK {
                    let (agent, process) = (familiar.clone(), reader.clone());
                    tauri::async_runtime::spawn(async move {
                        let command = value["request"]["input"]["tool_input"]["command"].as_str().unwrap_or_default();
                        let allowed = crate::permissions::check_command(&agent, command, uuid::Uuid::new_v4().to_string()).await;
                        let output = json!({ "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": if allowed { "allow" } else { "deny" },
                            "permissionDecisionReason": if allowed { "Allowed by Grimoire." } else { "The user didn't allow this command in Grimoire." },
                        } });
                        let reply = json!({ "type": "control_response", "response": { "subtype": "success", "request_id": value["request_id"], "response": output } });
                        let _ = process.write_line(&reply).await;
                    });
                    continue;
                }
                for mut event in parser.parse(&value) {
                    let finished = matches!(event, ProviderEvent::Done | ProviderEvent::Failed(_));
                    if finished && reader.interrupting.swap(false, Ordering::SeqCst) {
                        event = ProviderEvent::Stopped;
                    }
                    if !finished {
                        if let Some(tx) = reader.route.lock().ok().and_then(|r| r.clone()) {
                            let _ = tx.send(event);
                        }
                        continue;
                    }
                    // Settle this turn before saying it's over: the next turn can start the moment it
                    // hears, and it must not lose its route or inherit this turn's idle timer.
                    let route = reader.take_route();
                    {
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
                    if let Some(tx) = route {
                        let _ = tx.send(event);
                    }
                }
            }
            if let Some(tx) = reader.take_route() {
                let detail = stderr.final_text().await;
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
        // Grimoire adds its own row that opens the side conversation.
        "mcp__grimoire__ask_familiar" | "mcp__grimoire__message_familiar" | "mcp__grimoire__start_helper" => Some(String::new()),
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
            "rate_limit_event" => vec![ProviderEvent::Limits(value["rate_limit_info"].clone())],
            "result" => {
                self.has_text = false;
                let mut events = Vec::new();
                if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
                    let n = |key: &str| usage[key].as_u64().unwrap_or(0);
                    let input = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
                    events.push(ProviderEvent::Usage { input, output: n("output_tokens") });
                }
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
                    events.push(ProviderEvent::Failed(reason));
                } else {
                    events.push(ProviderEvent::Done);
                }
                events
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        for (raw, encoded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(raw.as_bytes()), encoded);
        }
        assert_eq!(base64(&[0xFB, 0xFF]), "+/8=");
    }

    #[test]
    fn sends_images_as_base64_blocks_before_the_text() {
        assert_eq!(user_content("hi", &[]), json!("hi"));
        let content = user_content("What's this?", &[("image/png", b"png".to_vec())]);
        assert_eq!(
            content,
            json!([
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "cG5n" } },
                { "type": "text", "text": "What's this?" }
            ])
        );
        assert_eq!(user_content("", &[("image/gif", vec![])]).as_array().unwrap().len(), 1);

        let dir = std::env::temp_dir().join(format!("grimoire-claude-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.gif"), b"GIF89a").unwrap();
        std::fs::write(dir.join("b.png"), b"not an image").unwrap();
        assert_eq!(read_image(&dir.join("a.gif")).unwrap().0, "image/gif");
        assert!(read_image(&dir.join("b.png")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

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
                {"type":"tool_use","name":"Bash","input":{"command":"ls -la"}},
                {"type":"tool_use","name":"mcp__grimoire__ask_familiar","input":{"familiar":"Scribe","message":"hi"}}
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
                ProviderEvent::Tool(String::new()),
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
        let events = parse_all(&[
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"seven_day"}}),
            json!({"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":4,"cache_creation_input_tokens":100,"cache_read_input_tokens":900,"output_tokens":88}}),
        ]);
        assert_eq!(events[0], ProviderEvent::Limits(json!({"status":"allowed","rateLimitType":"seven_day"})));
        assert_eq!(events[1], ProviderEvent::Usage { input: 1004, output: 88 });
        assert_eq!(events[2], ProviderEvent::Done);

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
            coven_id: None,
            host: None,
            created_at: 0,
        };
        let joined = args(&agent, None).join(" ");
        assert!(joined.contains("--permission-mode bypassPermissions"));
        assert!(joined.contains("--model sonnet"));
        assert!(!joined.contains("--effort"));
        assert!(joined.ends_with("--resume s1"));
        assert!(!joined.contains("--append-system-prompt"), "no memory yet");

        // A side conversation forks the main session first, then resumes its own.
        let lane = |session| Lane { key: "a~c1".into(), session };
        assert!(args(&agent, Some(&lane(LaneSession::Fork("s1".into())))).join(" ").ends_with("--resume s1 --fork-session"));
        assert!(args(&agent, Some(&lane(LaneSession::Resume("s2".into())))).join(" ").ends_with("--resume s2"));
        assert!(!args(&agent, Some(&lane(LaneSession::Fresh))).contains(&"--resume".to_string()));
    }

    #[test]
    fn appends_the_familiar_memory_to_the_system_prompt() {
        let agent = Agent {
            id: "memory-claude".into(),
            name: "Scribe".into(),
            provider: crate::db::Provider::Claude,
            model: None,
            workspace: "/tmp".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: crate::db::Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: 0,
        };
        let dir = crate::memory::test_dir();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("memory-claude.md"), "The user's cat is Miso.").unwrap();
        let args = args(&agent, None);
        let at = args.iter().position(|a| a == "--append-system-prompt").unwrap();
        assert!(args[at + 1].contains("<memory of=\"you\">\nFacts:\n- ") && args[at + 1].contains(": The user's cat is Miso.\n</memory>"));
    }
}
