//! Grimoire tools that familiars call through MCP. The `grimoire mcp` process the CLI starts relays each
//! call here over a private Unix socket, so familiars work on the live app state.
//!
//! In code a familiar is an agent and a spell is a workflow; the tools use the names people see.

use crate::approvals::{self, Approvals, Decision};
use crate::db::{Agent, Db, WorkflowStep};
use crate::vault::{self, Kind, SecretStore};
use crate::{themes, workflows};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub const SERVER_NAME: &str = "grimoire";
const TOOL_NAMES: [&str; 8] =
    ["list_familiars", "list_spells", "create_spell", "list_themes", "create_theme", "set_theme", "vault_search", "vault_use"];

/// How a CLI starts the tool server: this executable and the app's socket.
static LAUNCH: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

pub fn definitions() -> Value {
    let colors = json!({
        "type": "object",
        "description": format!(
            "Hex colors by role. canvas (the window background) and accent (buttons, selection, focus) are required; \
             Grimoire derives the rest from them. Optional roles: {}.",
            themes::ROLES[2..].join(", ")
        ),
        "properties": { "canvas": { "type": "string" }, "accent": { "type": "string" } },
        "additionalProperties": { "type": "string" }
    });
    json!([
        {
            "name": "list_familiars",
            "description": "List the familiars in Grimoire with their ids, names, providers, and models. Use an id or name in create_spell.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_spells",
            "description": "List the spells saved in Grimoire and their steps. A spell is a chain of steps, each run by one familiar.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_spell",
            "description": "Save a new Grimoire spell. Each step sends its prompt to one familiar and waits for the reply before the next step. In a prompt, {{input}} is the text the spell is cast with and {{previous}} is the previous step's reply; the previous reply is appended automatically when a step does not use {{previous}}. The user casts spells from Grimoire's Spells page.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short name for the spell." },
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "familiar": { "type": "string", "description": "Familiar id or exact name, from list_familiars." },
                                "prompt": { "type": "string", "description": "Instructions sent to that familiar." }
                            },
                            "required": ["familiar", "prompt"]
                        }
                    }
                },
                "required": ["name", "steps"]
            }
        },
        {
            "name": "list_themes",
            "description": "List Grimoire's color themes, which one is active, and each theme's colors.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_theme",
            "description": "Save a new Grimoire color theme. Give colors for the dark appearance, the light one, or both; a theme with only one follows that appearance. Use calm, readable colors: text must stay legible on canvas. The theme is not applied until you call set_theme.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Short name for the theme." },
                    "dark": colors.clone(),
                    "light": colors,
                    "css": { "type": "string", "description": "Optional CSS applied after Grimoire's styles. Hooks: [data-slot=sidebar], [data-slot=chat], [data-slot=composer], [data-slot=message][data-role=user|assistant], [data-slot=page], and the variables --font-ui and --font-display." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "vault_search",
            "description": "Search the user's vault of logins, API keys, and cards. Returns names, sites, usernames, and the last digits of cards, never secrets. Use vault_use to use a secret.",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string", "description": "Part of a name, site, or username. Leave out to list everything." } }
            }
        },
        {
            "name": "vault_use",
            "description": "Use a secret from the vault without seeing it. Grimoire may ask the user first. You get back a path that holds the secret for one read; read it with $(cat PATH) inside the command that needs it, such as curl. Never print, echo, log, or save the secret. Needs shell access.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "item": { "type": "string", "description": "Item id, name, or site, from vault_search." },
                    "field": { "type": "string", "description": "password for a login, key for an API key, or number, expiry, or cvc for a card. Defaults to the first." }
                },
                "required": ["item"]
            }
        },
        {
            "name": "set_theme",
            "description": "Switch Grimoire to a theme, by id or name from list_themes.",
            "inputSchema": {
                "type": "object",
                "properties": { "theme": { "type": "string", "description": "Theme id or name." } },
                "required": ["theme"]
            }
        }
    ])
}

/// Tool names as Claude's `--allowedTools` expects them.
pub fn claude_tool_names() -> String {
    TOOL_NAMES.map(|name| format!("mcp__{SERVER_NAME}__{name}")).join(",")
}

/// The command and arguments a CLI runs to give `agent_id` the Grimoire tools, once the socket is up.
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
    /// The event to tell the app's windows about, when the tool changed something they show.
    pub changed: Option<&'static str>,
}

#[derive(Deserialize)]
struct NewSpell {
    name: String,
    steps: Vec<NewStep>,
}

#[derive(Deserialize)]
struct NewStep {
    familiar: String,
    prompt: String,
}

#[derive(Deserialize)]
struct NewTheme {
    name: String,
    dark: Option<Value>,
    light: Option<Value>,
    css: Option<String>,
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Runs one tool for the familiar `caller`.
pub fn call(db: &Db, themes_dir: &Path, caller: &str, tool: &str, arguments: Value) -> Result<Outcome, String> {
    let agents = db.list_agents()?;
    let name_of = |id: &str| agents.iter().find(|a| a.id == id).map_or("a deleted familiar", |a| a.name.as_str()).to_string();
    let creator = agents.iter().any(|a| a.id == caller).then(|| caller.to_string());
    let invalid = |e: serde_json::Error| format!("Invalid arguments: {e}");
    let (text, changed) = match tool {
        "list_familiars" => {
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
            (pretty(&list), None)
        }
        "list_spells" => {
            let list: Vec<Value> = db
                .list_workflows()?
                .iter()
                .map(|w| {
                    json!({
                        "id": w.id,
                        "name": w.name,
                        "createdBy": w.created_by.as_deref().map(&name_of),
                        "steps": w.steps.iter().map(|s| json!({ "familiar": name_of(&s.agent_id), "prompt": s.prompt })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            (pretty(&list), None)
        }
        "create_spell" => {
            let input: NewSpell = serde_json::from_value(arguments).map_err(invalid)?;
            let steps = input
                .steps
                .into_iter()
                .enumerate()
                .map(|(index, step)| {
                    let agent = resolve_agent(&agents, &step.familiar).map_err(|e| format!("Step {}: {e}", index + 1))?;
                    Ok(WorkflowStep { agent_id: agent.id.clone(), prompt: step.prompt })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let saved = workflows::save(db, None, &input.name, steps, creator)?;
            let text = format!(
                "Saved the spell \"{}\" with {}. The user can cast it from Grimoire's Spells page.",
                saved.name,
                plural(saved.steps.len(), "step")
            );
            (text, Some("workflows_changed"))
        }
        "list_themes" => {
            let list = themes::list(db, themes_dir)?;
            let entries: Vec<Value> = list
                .themes
                .iter()
                .map(|t| {
                    json!({
                        "id": t.id,
                        "name": t.theme["name"],
                        "active": t.id == list.active,
                        "builtIn": t.built_in,
                        "createdBy": t.theme.get("createdBy").and_then(Value::as_str).map(&name_of),
                        "colors": t.theme.get("colors"),
                        "variants": t.theme.get("variants"),
                    })
                })
                .collect();
            (pretty(&entries), None)
        }
        "create_theme" => {
            let input: NewTheme = serde_json::from_value(arguments).map_err(invalid)?;
            let mut variants = serde_json::Map::new();
            for (mode, colors) in [("dark", input.dark), ("light", input.light)] {
                if let Some(colors) = colors {
                    variants.insert(mode.into(), colors);
                }
            }
            let theme = json!({ "name": input.name, "variants": variants, "css": input.css });
            let saved = themes::save(themes_dir, None, &theme, creator)?;
            let text = format!("Saved the theme \"{}\" with id {}. Call set_theme to switch to it.", saved.theme["name"].as_str().unwrap_or_default(), saved.id);
            (text, Some("themes_changed"))
        }
        "set_theme" => {
            let wanted = arguments.get("theme").and_then(Value::as_str).map(str::trim).unwrap_or_default();
            let list = themes::list(db, themes_dir)?;
            let by_name: Vec<_> = list.themes.iter().filter(|t| t.theme["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(wanted))).collect();
            let theme = match list.themes.iter().find(|t| t.id == wanted) {
                Some(theme) => theme,
                None => match by_name.as_slice() {
                    [theme] => *theme,
                    [] => return Err(format!("No theme is called \"{wanted}\". Use list_themes to see them.")),
                    _ => return Err(format!("More than one theme is called \"{wanted}\". Use its id instead.")),
                },
            };
            themes::set_active(db, themes_dir, &theme.id)?;
            (format!("Grimoire now uses the theme \"{}\".", theme.theme["name"].as_str().unwrap_or_default()), Some("themes_changed"))
        }
        "vault_search" => {
            let query = arguments.get("query").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase();
            let matches = |text: &Option<String>| text.as_deref().is_some_and(|t| t.to_lowercase().contains(&query));
            let items: Vec<Value> = vault::list(db)?
                .into_iter()
                .filter(|i| query.is_empty() || i.name.to_lowercase().contains(&query) || matches(&i.site) || matches(&i.username))
                .map(|i| json!({ "id": i.id, "kind": i.kind, "name": i.name, "site": i.site, "username": i.username, "hint": i.hint, "fields": i.kind.fields() }))
                .collect();
            (pretty(&items), None)
        }
        _ => return Err(format!("Grimoire has no tool named {tool}.")),
    };
    Ok(Outcome { text, changed })
}

fn resolve_agent<'a>(agents: &'a [Agent], wanted: &str) -> Result<&'a Agent, String> {
    let wanted = wanted.trim();
    if let Some(agent) = agents.iter().find(|a| a.id == wanted) {
        return Ok(agent);
    }
    let named: Vec<&Agent> = agents.iter().filter(|a| a.name.eq_ignore_ascii_case(wanted)).collect();
    match named.as_slice() {
        [agent] => Ok(agent),
        [] => Err(format!("No familiar is called \"{wanted}\". Use list_familiars to see who exists.")),
        _ => Err(format!("More than one familiar is called \"{wanted}\". Use its id instead.")),
    }
}

fn pretty(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// What the tool server needs from the app.
pub struct Context {
    pub app: AppHandle,
    pub db: Arc<Db>,
    pub themes_dir: PathBuf,
    pub approvals: Arc<Approvals>,
    pub secrets: Arc<dyn SecretStore>,
}

/// Hands a vault secret to `caller` through a one-time pipe. `ask` puts the question to the user,
/// unless the familiar is always allowed to use that item.
async fn vault_use<Ask, Answer>(db: &Db, secrets: &dyn SecretStore, caller: &str, arguments: Value, ask: Ask) -> Result<String, String>
where
    Ask: FnOnce(approvals::Request) -> Answer,
    Answer: std::future::Future<Output = Decision>,
{
    let wanted = arguments.get("item").and_then(Value::as_str).ok_or("Name the vault item to use.")?;
    let item = vault::resolve(db, wanted)?;
    let fields = item.kind.fields();
    let field = match arguments.get("field").and_then(Value::as_str) {
        Some(field) => field.trim().to_lowercase(),
        None => fields[0].to_string(),
    };
    if !fields.contains(&field.as_str()) {
        return Err(format!("{} has no {field}. Its fields are {}.", item.name, fields.join(", ")));
    }
    let agent = db.get_agent(caller)?.ok_or("Only a familiar can use the vault.")?;
    let scope = vault::scope(&item.id);
    let label = item.site.clone().unwrap_or_else(|| item.name.clone());
    let allowed = db.has_grant(caller, &scope)? || {
        let request = approvals::Request {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: caller.to_string(),
            kind: "vault",
            title: format!("{} wants to use the {field} for {label}", agent.name),
            detail: "Grimoire hands it over through a one-time pipe, so the model never sees it.".into(),
            can_remember: true,
        };
        match ask(request).await {
            Decision::Always => {
                db.add_grant(caller, &scope)?;
                true
            }
            Decision::Once => true,
            Decision::Deny => false,
        }
    };
    vault::record_use(db, &item.id, Some(caller), &field, if allowed { "allowed" } else { "denied" })?;
    if !allowed {
        return Err(format!("The user didn't allow using the {field} for {label}. Don't ask for it again unless they tell you to."));
    }
    let value = vault::secret(secrets, &item.id)?
        .get(&field)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or(format!("{} has no {field} saved.", item.name))?;
    let path = vault::hand_off(value, vault::HAND_OFF_TTL)?;
    let path = path.display();
    let example = match item.kind {
        Kind::Login => format!("curl -u '{}:'\"$(cat {path})\" https://...", item.username.as_deref().unwrap_or("USER")),
        Kind::ApiKey => format!("curl -H \"Authorization: Bearer $(cat {path})\" https://..."),
        Kind::Card => format!("some-command --{field} \"$(cat {path})\""),
    };
    Ok(format!(
        "The {field} for {label} is waiting at {path}. Read it with $(cat {path}) inside the command that needs it, for example:\n{example}\n\
         It can be read once and expires in 2 minutes. Never print, echo, log, or save it, and don't cat it on its own."
    ))
}

/// Listens on `socket` for relayed tool calls, one JSON object per line in each direction.
/// Familiars only get the tools once this succeeds.
pub fn serve(ctx: Arc<Context>, socket: &Path) -> std::io::Result<()> {
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
            Err(e) => return eprintln!("Grimoire tools are off: {e}"),
        };
        while let Ok((stream, _)) = listener.accept().await {
            tauri::async_runtime::spawn(handle_connection(stream, ctx.clone()));
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

async fn handle_connection(stream: UnixStream, ctx: Arc<Context>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let result = if request.tool == "vault_use" {
                    let ask = |question| ctx.approvals.ask(&ctx.app, question);
                    let used = vault_use(&ctx.db, ctx.secrets.as_ref(), &request.agent, request.arguments, ask).await;
                    // Every attempt lands in the item's log, allowed or not.
                    let _ = ctx.app.emit("vault_changed", ());
                    used.map(|text| Outcome { text, changed: None })
                } else {
                    let ctx = ctx.clone();
                    tokio::task::spawn_blocking(move || call(&ctx.db, &ctx.themes_dir, &request.agent, &request.tool, request.arguments))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                };
                match result {
                    Ok(outcome) => {
                        if let Some(event) = outcome.changed {
                            let _ = ctx.app.emit(event, ());
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

    fn setup() -> (Db, PathBuf) {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("r1", "Researcher")).unwrap();
        db.insert_agent(&agent("w1", "Writer")).unwrap();
        (db, std::env::temp_dir().join(format!("grimoire-tools-{}", uuid::Uuid::new_v4())))
    }

    #[test]
    fn creates_a_spell_credited_to_the_caller() {
        let (db, dir) = setup();
        let args = json!({ "name": " Brief ", "steps": [
            { "familiar": "researcher", "prompt": " Research {{input}} " },
            { "familiar": "w1", "prompt": "Write it up" }
        ]});
        let outcome = call(&db, &dir, "r1", "create_spell", args).unwrap();
        assert_eq!(outcome.changed, Some("workflows_changed"));
        assert!(outcome.text.contains("\"Brief\" with 2 steps"));
        let saved = &db.list_workflows().unwrap()[0];
        assert_eq!(saved.name, "Brief");
        assert_eq!(saved.created_by.as_deref(), Some("r1"));
        assert_eq!(saved.steps[0], WorkflowStep { agent_id: "r1".into(), prompt: "Research {{input}}".into() });
        assert_eq!(saved.steps[1].agent_id, "w1");

        // Editing it from the Spells page keeps the creator.
        workflows::save(&db, Some(&saved.id), "Brief v2", saved.steps.clone(), None).unwrap();
        assert_eq!(db.list_workflows().unwrap()[0].created_by.as_deref(), Some("r1"));
    }

    #[test]
    fn refuses_unknown_or_ambiguous_familiars_and_empty_spells() {
        let (db, dir) = setup();
        db.insert_agent(&agent("w2", "writer")).unwrap();
        let step = |familiar: &str| json!({ "name": "X", "steps": [{ "familiar": familiar, "prompt": "Go" }] });
        assert!(call(&db, &dir, "r1", "create_spell", step("Editor")).unwrap_err().contains("No familiar is called \"Editor\""));
        assert!(call(&db, &dir, "r1", "create_spell", step("Writer")).unwrap_err().contains("More than one familiar"));
        let empty = json!({ "name": "X", "steps": [] });
        assert_eq!(call(&db, &dir, "r1", "create_spell", empty).unwrap_err(), "Add at least one step.");
        assert!(call(&db, &dir, "r1", "create_spell", json!({ "steps": [] })).unwrap_err().starts_with("Invalid arguments"));
        assert!(db.list_workflows().unwrap().is_empty());
    }

    #[test]
    fn lists_familiars_and_spells() {
        let (db, dir) = setup();
        let agents: Vec<Value> = serde_json::from_str(&call(&db, &dir, "w1", "list_familiars", json!({})).unwrap().text).unwrap();
        let by_id = |id: &str| agents.iter().find(|a| a["id"] == id).unwrap().clone();
        assert_eq!(by_id("w1")["name"], "Writer");
        assert_eq!(by_id("w1")["you"], true);
        assert_eq!(by_id("r1")["you"], false);

        let args = json!({ "name": "Brief", "steps": [{ "familiar": "Writer", "prompt": "Write" }] });
        call(&db, &dir, "unknown", "create_spell", args).unwrap();
        let list: Value = serde_json::from_str(&call(&db, &dir, "w1", "list_spells", json!({})).unwrap().text).unwrap();
        assert_eq!(list[0]["steps"][0]["familiar"], "Writer");
        assert_eq!(list[0]["createdBy"], Value::Null);
        assert!(call(&db, &dir, "w1", "delete_everything", json!({})).is_err());
    }

    #[test]
    fn vault_search_never_returns_secrets() {
        let (db, dir) = setup();
        let store = vault::MemoryStore::new();
        let input = vault::ItemInput {
            id: None,
            kind: Kind::Login,
            name: "GitHub".into(),
            site: Some("github.com".into()),
            username: Some("me".into()),
            secret: json!({ "password": "hunter2" }).as_object().unwrap().clone(),
        };
        vault::save(&db, &store, input).unwrap();
        let text = call(&db, &dir, "r1", "vault_search", json!({ "query": "GIT" })).unwrap().text;
        let found: Vec<Value> = serde_json::from_str(&text).unwrap();
        assert_eq!(found[0]["name"], "GitHub");
        assert_eq!(found[0]["fields"], json!(["password"]));
        assert!(!text.contains("hunter2"));
        assert_eq!(call(&db, &dir, "r1", "vault_search", json!({ "query": "bank" })).unwrap().text, "[]");
    }

    #[tokio::test]
    async fn vault_use_asks_hands_off_once_and_remembers_always() {
        let (db, _) = setup();
        let store = vault::MemoryStore::new();
        let input = vault::ItemInput {
            id: None,
            kind: Kind::Login,
            name: "GitHub".into(),
            site: Some("github.com".into()),
            username: Some("me".into()),
            secret: json!({ "password": "hunter2" }).as_object().unwrap().clone(),
        };
        let item = vault::save(&db, &store, input).unwrap();
        let args = || json!({ "item": "github.com" });

        let denied = vault_use(&db, &store, "r1", args(), |_| async { Decision::Deny }).await.unwrap_err();
        assert!(denied.contains("didn't allow"));

        let asked = std::sync::Mutex::new(Vec::new());
        let text = vault_use(&db, &store, "r1", args(), |q: approvals::Request| {
            asked.lock().unwrap().push(q.title.clone());
            async { Decision::Always }
        })
        .await
        .unwrap();
        assert_eq!(asked.lock().unwrap()[0], "Researcher wants to use the password for github.com");
        assert!(!text.contains("hunter2"));
        let path = text.split_whitespace().find(|w| w.contains("grimoire-secrets")).unwrap().trim_end_matches('.');
        assert_eq!(std::fs::read_to_string(path).unwrap(), "hunter2");

        // Always allowed now: no question, and the log has every attempt.
        let again = vault_use(&db, &store, "r1", args(), |_| async { panic!("should not ask") }).await.unwrap();
        let path = again.split_whitespace().find(|w| w.contains("grimoire-secrets")).unwrap().trim_end_matches('.');
        assert_eq!(std::fs::read_to_string(path).unwrap(), "hunter2");
        let outcomes: Vec<String> = vault::uses(&db, &item.id).unwrap().into_iter().map(|u| u.outcome).collect();
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.contains(&"denied".to_string()));

        assert!(vault_use(&db, &store, "w1", json!({ "item": "GitHub", "field": "pin" }), |_| async { Decision::Once }).await.unwrap_err().contains("fields are password"));
    }

    #[test]
    fn creates_and_switches_themes() {
        let (db, dir) = setup();
        let args = json!({ "name": "Rainy library", "dark": { "canvas": "#14181c", "accent": "#8fb3c9" } });
        let outcome = call(&db, &dir, "r1", "create_theme", args).unwrap();
        assert_eq!(outcome.changed, Some("themes_changed"));
        assert!(outcome.text.contains("id rainy-library"));

        let bad = json!({ "name": "Loud", "dark": { "canvas": "black", "accent": "#fff" } });
        assert!(call(&db, &dir, "r1", "create_theme", bad).unwrap_err().contains("hex color"));

        call(&db, &dir, "r1", "set_theme", json!({ "theme": "rainy LIBRARY" })).unwrap();
        let list: Vec<Value> = serde_json::from_str(&call(&db, &dir, "w1", "list_themes", json!({})).unwrap().text).unwrap();
        let rainy = list.iter().find(|t| t["id"] == "rainy-library").unwrap();
        assert_eq!(rainy["active"], true);
        assert_eq!(rainy["createdBy"], "Researcher");
        assert!(call(&db, &dir, "r1", "set_theme", json!({ "theme": "Neon" })).unwrap_err().contains("No theme"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
