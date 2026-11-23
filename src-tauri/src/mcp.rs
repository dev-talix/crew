//! `grimoire mcp <socket> <agent-id>`: a stdio MCP server that Codex or Claude starts for an agent.
//! It answers the protocol itself and relays each tool call to the running Grimoire app.

use crate::tools;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

pub fn main(args: &[String]) -> i32 {
    let [socket, agent] = args else {
        eprintln!("usage: grimoire mcp <socket> <agent-id>");
        return 2;
    };
    let write = |reply: Value| {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{reply}").and_then(|_| out.flush()).is_ok()
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        // A tool call can take minutes (ask_familiar waits for a reply), so it runs on its own thread
        // and pings or other calls still get answered meanwhile.
        if message["method"] == "tools/call" {
            let (socket, agent) = (socket.clone(), agent.clone());
            std::thread::spawn(move || {
                if let Some(reply) = respond(&message, |tool, arguments| relay(&socket, &agent, tool, arguments)) {
                    write(reply);
                }
            });
        } else if let Some(reply) = respond(&message, |tool, arguments| relay(socket, agent, tool, arguments)) {
            if !write(reply) {
                break;
            }
        }
    }
    0
}

/// Answers one JSON-RPC message. Notifications get no reply.
pub fn respond(message: &Value, call: impl FnOnce(&str, Value) -> Result<String, String>) -> Option<Value> {
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
        "tools/list" => json!({ "tools": tools::definitions() }),
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

fn relay(socket: &str, agent: &str, tool: &str, arguments: Value) -> Result<String, String> {
    let closed = |e: std::io::Error| format!("Grimoire is not reachable ({e}). Is the app still open?");
    let mut stream = UnixStream::connect(socket).map_err(closed)?;
    let request = json!({ "agent": agent, "tool": tool, "arguments": arguments });
    writeln!(stream, "{request}").map_err(closed)?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(closed)?;
    let reply: Value = serde_json::from_str(&line).map_err(|_| "Grimoire sent an unreadable reply.".to_string())?;
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
        let init = respond(&request("initialize", json!({ "protocolVersion": "2025-03-26" })), |_, _| unreachable!()).unwrap();
        assert_eq!(init["id"], 7);
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(init["result"]["capabilities"]["tools"].is_object());

        let list = respond(&request("tools/list", json!({})), |_, _| unreachable!()).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["list_familiars", "list_spells", "create_spell", "list_themes", "create_theme", "vault_search", "vault_use", "ask_familiar", "message_familiar", "set_theme", "read_memory", "remember", "rewrite_memory"]);
    }

    #[test]
    fn relays_tool_calls_and_marks_failures() {
        let ok = respond(&request("tools/call", json!({ "name": "list_familiars", "arguments": { "x": 1 } })), |tool, args| {
            assert_eq!((tool, args), ("list_familiars", json!({ "x": 1 })));
            Ok("[]".into())
        })
        .unwrap();
        assert_eq!(ok["result"]["content"][0]["text"], "[]");
        assert!(ok["result"].get("isError").is_none());

        let failed = respond(&request("tools/call", json!({ "name": "create_spell" })), |_, args| {
            assert_eq!(args, json!({}));
            Err("Add at least one step.".into())
        })
        .unwrap();
        assert_eq!(failed["result"]["isError"], true);
        assert_eq!(failed["result"]["content"][0]["text"], "Add at least one step.");
    }

    #[test]
    fn ignores_notifications_and_rejects_unknown_methods() {
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(respond(&note, |_, _| unreachable!()).is_none());
        let unknown = respond(&request("resources/list", json!({})), |_, _| unreachable!()).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }
}
