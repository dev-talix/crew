//! `grimoire mcp <socket> <agent-id>`: a stdio MCP server that Codex or Claude starts for an agent.
//! It answers the protocol itself and relays each tool call to the running Grimoire app, with the
//! token from `GRIMOIRE_TOOLS_TOKEN` that proves which agent it speaks for.
//!
//! `grimoire mcp <socket> <agent-id> <connector-id>` relays every message, unread, to a connector's
//! hosted server through the app, which adds the person's sign-in. The token never reaches this process.

use crate::tools;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

pub fn main(args: &[String]) -> i32 {
    let (socket, agent, connector) = match args {
        [socket, agent] => (socket, agent, None),
        [socket, agent, connector] => (socket, agent, Some(connector.clone())),
        _ => {
            eprintln!("usage: grimoire mcp <socket> <agent-id> [connector-id]");
            return 2;
        }
    };
    let Ok(token) = std::env::var(tools::TOKEN_ENV) else {
        eprintln!("grimoire mcp needs {} from the Grimoire app.", tools::TOKEN_ENV);
        return 2;
    };
    let caller = Caller { socket: socket.clone(), agent: agent.clone(), token };
    let write = |reply: Value| {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{reply}").and_then(|_| out.flush()).is_ok()
    };
    if let Some(connector) = connector {
        return bridge(&caller, &connector, write);
    }
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        // A tool call can take minutes (ask_familiar waits for a reply), so it runs on its own thread
        // and pings or other calls still get answered meanwhile.
        if message["method"] == "tools/call" {
            let caller = caller.clone();
            std::thread::spawn(move || {
                if let Some(reply) = respond(&message, || tool_list(&caller), |tool, arguments| relay(&caller, tool, arguments)) {
                    write(reply);
                }
            });
        } else if let Some(reply) = respond(&message, || tool_list(&caller), |tool, arguments| relay(&caller, tool, arguments)) {
            if !write(reply) {
                break;
            }
        }
    }
    0
}

/// Relays each message to a connector through the app and writes back whatever the server sent.
/// Requests run on their own threads so a slow tool call doesn't hold up pings. Notifications and
/// replies go in order, so `notifications/initialized` reaches the server before the next request.
fn bridge(caller: &Caller, connector: &str, write: impl Fn(Value) -> bool + Copy + Send + 'static) -> i32 {
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let (caller, connector) = (caller.clone(), connector.to_string());
        let relay = move || {
            for reply in connector_replies(&message, exchange(&caller, json!({ "connector": connector, "message": message }))) {
                write(reply);
            }
        };
        if is_request(&line) {
            std::thread::spawn(relay);
        } else {
            relay();
        }
    }
    0
}

/// Whether a JSON-RPC line is a request, which expects an answer, rather than a notification or a reply.
fn is_request(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|m| m.get("id").is_some() && m.get("method").is_some())
}

/// What to write back for one relayed message: the server's messages, or a JSON-RPC error when the
/// relay failed and the message expects an answer.
fn connector_replies(message: &Value, reply: Result<Value, String>) -> Vec<Value> {
    let failure = match reply {
        Ok(reply) if reply["ok"] == true => return reply["messages"].as_array().cloned().unwrap_or_default(),
        Ok(reply) => reply["error"].as_str().unwrap_or("Grimoire could not reach the connector.").to_string(),
        Err(error) => error,
    };
    match message.get("id").filter(|_| message.get("method").is_some()) {
        Some(id) => vec![json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": failure } })],
        None => vec![],
    }
}

/// Answers one JSON-RPC message. Notifications get no reply. `tools` lists the familiar's tools.
pub fn respond(message: &Value, tools: impl FnOnce() -> Value, call: impl FnOnce(&str, Value) -> Result<String, String>) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message.get("method")?.as_str()?;
    let params = &message["params"];
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": tools::SERVER_NAME, "title": "Grimoire", "version": env!("CARGO_PKG_VERSION") },
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call(name, arguments) {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                Err(error) => json!({ "content": [{ "type": "text", "text": error }], "isError": true }),
            }
        }
        _ => return Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Unknown method {method}") } })),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

#[derive(Clone)]
struct Caller {
    socket: String,
    agent: String,
    token: String,
}

/// Sends one request to the app with this relay's agent and token, and reads the reply.
fn exchange(caller: &Caller, mut request: Value) -> Result<Value, String> {
    let closed = |e: std::io::Error| format!("Grimoire is not reachable ({e}). Is the app still open?");
    let mut stream = UnixStream::connect(&caller.socket).map_err(closed)?;
    request["agent"] = json!(caller.agent);
    request["token"] = json!(caller.token);
    writeln!(stream, "{request}").map_err(closed)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(closed)?;
    serde_json::from_str(&line).map_err(|_| "Grimoire sent an unreadable reply.".to_string())
}

/// The tools the app says this familiar has, which depend on its settings. The tools everyone has
/// when the app can't say.
fn tool_list(caller: &Caller) -> Value {
    match exchange(caller, json!({ "list": true })) {
        Ok(reply) if reply["ok"] == true && reply["tools"].is_array() => reply["tools"].clone(),
        _ => tools::definitions(),
    }
}

fn relay(caller: &Caller, tool: &str, arguments: Value) -> Result<String, String> {
    let reply = exchange(caller, json!({ "tool": tool, "arguments": arguments }))?;
    match reply["ok"].as_bool() {
        Some(true) => Ok(reply["text"].as_str().unwrap_or_default().to_string()),
        _ => Err(reply["error"].as_str().unwrap_or("Grimoire could not run that tool.").to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })
    }

    #[test]
    fn answers_initialize_and_lists_tools() {
        let init = respond(&request("initialize", json!({ "protocolVersion": "2025-03-26" })), || unreachable!(), |_, _| unreachable!()).unwrap();
        assert_eq!(init["id"], 7);
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(init["result"]["capabilities"]["tools"].is_object());

        let list = respond(&request("tools/list", json!({})), tools::definitions, |_, _| unreachable!()).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["list_familiars", "list_spells", "create_spell", "list_themes", "create_theme", "vault_search", "vault_use", "ask_familiar", "message_familiar", "set_theme", "read_memory", "remember", "forget_memory", "ask_person", "read_ledger", "update_ledger", "start_helper", "list_helpers", "stop_helper", "draft_message", "browser_open", "browser_read", "browser_click", "browser_type", "browser_close", "search_chats"]);
    }

    #[test]
    fn relays_tool_calls_and_marks_failures() {
        let ok = respond(&request("tools/call", json!({ "name": "list_familiars", "arguments": { "x": 1 } })), || unreachable!(), |tool, args| {
            assert_eq!((tool, args), ("list_familiars", json!({ "x": 1 })));
            Ok("[]".into())
        })
        .unwrap();
        assert_eq!(ok["result"]["content"][0]["text"], "[]");
        assert!(ok["result"].get("isError").is_none());

        let failed = respond(&request("tools/call", json!({ "name": "create_spell" })), || unreachable!(), |_, args| {
            assert_eq!(args, json!({}));
            Err("Add at least one step.".into())
        })
        .unwrap();
        assert_eq!(failed["result"]["isError"], true);
        assert_eq!(failed["result"]["content"][0]["text"], "Add at least one step.");
    }

    #[test]
    fn passes_connector_replies_through_and_answers_failures() {
        let call = request("tools/call", json!({ "name": "create_issue" }));
        let messages = json!({ "ok": true, "messages": [{ "jsonrpc": "2.0", "id": 7, "result": {} }] });
        assert_eq!(connector_replies(&call, Ok(messages)), [json!({ "jsonrpc": "2.0", "id": 7, "result": {} })]);
        let refused = connector_replies(&call, Ok(json!({ "ok": false, "error": "Turned off." })));
        assert_eq!(refused[0]["error"]["message"], "Turned off.");
        assert_eq!(refused[0]["id"], 7);
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(connector_replies(&note, Err("gone".into())).is_empty());
        let response = json!({ "jsonrpc": "2.0", "id": 3, "result": {} });
        assert!(connector_replies(&response, Err("gone".into())).is_empty(), "a reply to the server needs no answer");
    }

    #[test]
    fn only_requests_run_out_of_order() {
        assert!(is_request(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#));
        assert!(!is_request(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#));
        assert!(!is_request(r#"{"jsonrpc":"2.0","id":4,"result":{}}"#), "a reply to the server keeps its place");
    }

    #[test]
    fn ignores_notifications_and_rejects_unknown_methods() {
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(respond(&note, || unreachable!(), |_, _| unreachable!()).is_none());
        let unknown = respond(&request("resources/list", json!({})), || unreachable!(), |_, _| unreachable!()).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }
}
