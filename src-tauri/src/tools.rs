//! Crew tools that agents call through MCP. The `crew mcp` process the CLI starts relays each call
//! here over a private Unix socket, so agents work on the live app state.

use crate::db::{Agent, Db, WorkflowStep};
use crate::workflows;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub const SERVER_NAME: &str = "crew";
const TOOL_NAMES: [&str; 3] = ["list_agents", "list_workflows", "create_workflow"];

/// How a CLI starts the tool server: this executable and the app's socket.
static LAUNCH: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

pub fn definitions() -> Value {
    json!([
        {
            "name": "list_agents",
            "description": "List the agents in Crew with their ids, names, providers, and models. Use an id or name in create_workflow.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_workflows",
            "description": "List the workflows saved in Crew and their steps.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_workflow",
            "description": "Save a new Crew workflow. Each step sends its prompt to one agent and waits for the reply before the next step. In a prompt, {{input}} is the text the workflow is started with and {{previous}} is the previous step's reply; the previous reply is appended automatically when a step does not use {{previous}}. The user runs workflows from Crew's Workflows page.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short name for the workflow." },
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "agent": { "type": "string", "description": "Agent id or exact name, from list_agents." },
                                "prompt": { "type": "string", "description": "Instructions sent to that agent." }
                            },
                            "required": ["agent", "prompt"]
                        }
                    }
                },
                "required": ["name", "steps"]
            }
        }
    ])
}

/// Tool names as Claude's `--allowedTools` expects them.
pub fn claude_tool_names() -> String {
    TOOL_NAMES.map(|name| format!("mcp__{SERVER_NAME}__{name}")).join(",")
}

/// The command and arguments a CLI runs to give `agent_id` the Crew tools, once the socket is up.
pub fn server_command(agent_id: &str) -> Option<(String, Vec<String>)> {
    let (exe, socket) = LAUNCH.get()?;
    Some((
        exe.to_string_lossy().into_owned(),
        vec!["mcp".into(), socket.to_string_lossy().into_owned(), agent_id.into()],
    ))
}

#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub text: String,
    pub workflows_changed: bool,
}

#[derive(Deserialize)]
struct NewWorkflow {
    name: String,
    steps: Vec<NewStep>,
}

#[derive(Deserialize)]
struct NewStep {
    agent: String,
    prompt: String,
}

/// Runs one tool for the agent `caller`.
pub fn call(db: &Db, caller: &str, tool: &str, arguments: Value) -> Result<Outcome, String> {
    let agents = db.list_agents()?;
    let name_of = |id: &str| agents.iter().find(|a| a.id == id).map_or("a deleted agent", |a| a.name.as_str()).to_string();
    let text = match tool {
        "list_agents" => {
            let list: Vec<Value> = agents
                .iter()
                .map(|a| {
                    json!({
                        "id": a.id,
                        "name": a.name,
                        "provider": a.provider,
                        "model": a.active_model.as_deref().or(a.model.as_deref()).unwrap_or("default"),
                        "you": a.id == caller,
                    })
                })
                .collect();
            pretty(&list)
        }
        "list_workflows" => {
            let list: Vec<Value> = db
                .list_workflows()?
                .iter()
                .map(|w| {
                    json!({
                        "id": w.id,
                        "name": w.name,
                        "createdBy": w.created_by.as_deref().map(&name_of),
                        "steps": w.steps.iter().map(|s| json!({ "agent": name_of(&s.agent_id), "prompt": s.prompt })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            pretty(&list)
        }
        "create_workflow" => {
            let input: NewWorkflow = serde_json::from_value(arguments).map_err(|e| format!("Invalid arguments: {e}"))?;
            let steps = input
                .steps
                .into_iter()
                .enumerate()
                .map(|(index, step)| {
                    let agent = resolve_agent(&agents, &step.agent).map_err(|e| format!("Step {}: {e}", index + 1))?;
                    Ok(WorkflowStep { agent_id: agent.id.clone(), prompt: step.prompt })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let creator = agents.iter().any(|a| a.id == caller).then(|| caller.to_string());
            let saved = workflows::save(db, None, &input.name, steps, creator)?;
            return Ok(Outcome {
                text: format!(
                    "Saved the workflow \"{}\" with {} step{}. The user can run it from Crew's Workflows page.",
                    saved.name,
                    saved.steps.len(),
                    if saved.steps.len() == 1 { "" } else { "s" }
                ),
                workflows_changed: true,
            });
        }
        _ => return Err(format!("Crew has no tool named {tool}.")),
    };
    Ok(Outcome { text, workflows_changed: false })
}

fn resolve_agent<'a>(agents: &'a [Agent], wanted: &str) -> Result<&'a Agent, String> {
    let wanted = wanted.trim();
    if let Some(agent) = agents.iter().find(|a| a.id == wanted) {
        return Ok(agent);
    }
    let named: Vec<&Agent> = agents.iter().filter(|a| a.name.eq_ignore_ascii_case(wanted)).collect();
    match named.as_slice() {
        [agent] => Ok(agent),
        [] => Err(format!("No agent is called \"{wanted}\". Use list_agents to see who exists.")),
        _ => Err(format!("More than one agent is called \"{wanted}\". Use its id instead.")),
    }
}

fn pretty(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Listens on `socket` for relayed tool calls, one JSON object per line in each direction.
/// Agents only get the tools once this succeeds.
pub fn serve(app: AppHandle, db: Arc<Db>, socket: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(socket);
    // Bind with std here: setup runs outside the async runtime, so tokio takes the socket over in the task.
    let listener = std::os::unix::net::UnixListener::bind(socket)?;
    listener.set_nonblocking(true)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    }
    let _ = LAUNCH.set((std::env::current_exe()?, socket.to_path_buf()));
    tauri::async_runtime::spawn(async move {
        let listener = match UnixListener::from_std(listener) {
            Ok(listener) => listener,
            Err(e) => return eprintln!("Crew tools are off: {e}"),
        };
        while let Ok((stream, _)) = listener.accept().await {
            tauri::async_runtime::spawn(handle_connection(stream, app.clone(), db.clone()));
        }
    });
    Ok(())
}

#[derive(Deserialize)]
struct Request {
    agent: String,
    tool: String,
    #[serde(default)]
    arguments: Value,
}

async fn handle_connection(stream: UnixStream, app: AppHandle, db: Arc<Db>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let db = db.clone();
                let result = tokio::task::spawn_blocking(move || call(&db, &request.agent, &request.tool, request.arguments))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                match result {
                    Ok(outcome) => {
                        if outcome.workflows_changed {
                            let _ = app.emit("workflows_changed", ());
                        }
                        json!({ "ok": true, "text": outcome.text })
                    }
                    Err(error) => json!({ "ok": false, "error": error }),
                }
            }
            Err(e) => json!({ "ok": false, "error": format!("Bad request: {e}") }),
        };
        if write.write_all(format!("{reply}\n").as_bytes()).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{now_ms, Access, Avatar, Provider};

    fn agent(id: &str, name: &str) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            provider: Provider::Codex,
            model: None,
            workspace: format!("/tmp/{id}"),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            created_at: now_ms(),
        }
    }

    fn setup() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("r1", "Researcher")).unwrap();
        db.insert_agent(&agent("w1", "Writer")).unwrap();
        db
    }

    #[test]
    fn creates_a_workflow_credited_to_the_caller() {
        let db = setup();
        let args = json!({ "name": " Brief ", "steps": [
            { "agent": "researcher", "prompt": " Research {{input}} " },
            { "agent": "w1", "prompt": "Write it up" }
        ]});
        let outcome = call(&db, "r1", "create_workflow", args).unwrap();
        assert!(outcome.workflows_changed);
        assert!(outcome.text.contains("\"Brief\" with 2 steps"));
        let saved = &db.list_workflows().unwrap()[0];
        assert_eq!(saved.name, "Brief");
        assert_eq!(saved.created_by.as_deref(), Some("r1"));
        assert_eq!(saved.steps[0], WorkflowStep { agent_id: "r1".into(), prompt: "Research {{input}}".into() });
        assert_eq!(saved.steps[1].agent_id, "w1");

        // Editing it from the Workflows page keeps the creator.
        workflows::save(&db, Some(&saved.id), "Brief v2", saved.steps.clone(), None).unwrap();
        assert_eq!(db.list_workflows().unwrap()[0].created_by.as_deref(), Some("r1"));
    }

    #[test]
    fn refuses_unknown_or_ambiguous_agents_and_empty_workflows() {
        let db = setup();
        db.insert_agent(&agent("w2", "writer")).unwrap();
        let step = |agent: &str| json!({ "name": "X", "steps": [{ "agent": agent, "prompt": "Go" }] });
        assert!(call(&db, "r1", "create_workflow", step("Editor")).unwrap_err().contains("No agent is called \"Editor\""));
        assert!(call(&db, "r1", "create_workflow", step("Writer")).unwrap_err().contains("More than one agent"));
        let empty = json!({ "name": "X", "steps": [] });
        assert_eq!(call(&db, "r1", "create_workflow", empty).unwrap_err(), "Add at least one step.");
        assert!(call(&db, "r1", "create_workflow", json!({ "steps": [] })).unwrap_err().starts_with("Invalid arguments"));
        assert!(db.list_workflows().unwrap().is_empty());
    }

    #[test]
    fn lists_agents_and_workflows() {
        let db = setup();
        let agents: Vec<Value> = serde_json::from_str(&call(&db, "w1", "list_agents", json!({})).unwrap().text).unwrap();
        let by_id = |id: &str| agents.iter().find(|a| a["id"] == id).unwrap().clone();
        assert_eq!(by_id("w1")["name"], "Writer");
        assert_eq!(by_id("w1")["you"], true);
        assert_eq!(by_id("r1")["you"], false);

        let args = json!({ "name": "Brief", "steps": [{ "agent": "Writer", "prompt": "Write" }] });
        call(&db, "unknown", "create_workflow", args).unwrap();
        let list: Value = serde_json::from_str(&call(&db, "w1", "list_workflows", json!({})).unwrap().text).unwrap();
        assert_eq!(list[0]["steps"][0]["agent"], "Writer");
        assert_eq!(list[0]["createdBy"], Value::Null);
        assert!(call(&db, "w1", "delete_everything", json!({})).is_err());
    }
}
