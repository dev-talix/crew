//! Grimoire tools that familiars call through MCP. The `grimoire mcp` process the CLI starts relays each
//! call here over a private Unix socket, so familiars work on the live app state.
//!
//! In code a familiar is an agent and a spell is a workflow; the tools use the names people see.

use crate::approvals::{self, Approvals, Decision};
use crate::db::{Agent, CheckWhen, Db, StepCheck, WorkflowStep};
use crate::link::Linked;
use crate::vault::{self, Kind, SecretStore};
use crate::{inbox, ledger, memory, themes, workflows};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub const SERVER_NAME: &str = "grimoire";
const TOOL_NAMES: [&str; 10] = [
    "list_familiars",
    "list_spells",
    "create_spell",
    "list_themes",
    "create_theme",
    "set_theme",
    "vault_search",
    "vault_use",
    "ask_familiar",
    "message_familiar",
];
/// How long the CLIs let one Grimoire tool call run. ask_familiar waits up to 10 minutes for a reply,
/// plus up to 2 for the person's OK, so this leaves room for both. Their defaults are far shorter.
pub const CALL_TIMEOUT_SECS: u64 = 15 * 60;

/// How a CLI starts the tool server: this executable and the app's socket.
pub(crate) static LAUNCH: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
/// The environment variable that hands each tool server the token for its familiar.
pub const TOKEN_ENV: &str = "GRIMOIRE_TOOLS_TOKEN";
/// A random token per familiar or side conversation, so a process that reaches the socket can't
/// act as a familiar it has no token for. New tokens each time the app starts.
static TOKENS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

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
    let mut list = json!([
        {
            "name": "list_familiars",
            "description": "List the familiars in Grimoire with their ids, names, providers, and models, then the familiars on the person's linked Macs with their Mac and whether it's online. Use an id or name in create_spell (familiars on this Mac only), ask_familiar, or message_familiar. For a familiar on a linked Mac, use its id, or \"Name on Mac\" when the name alone matches more than one.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_spells",
            "description": "List the spells saved in Grimoire and their steps. A spell is a chain of steps, each run by one familiar.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_spell",
            "description": "Save a new Grimoire spell. Each step sends its prompt to one familiar and waits for the reply before the next step. In a prompt, {{input}} is the text the spell is cast with and {{previous}} is the previous step's reply; the previous reply is appended automatically when a step does not use {{previous}}. A step with parallel: true starts together with the step before it, and the next step gets all their replies, labeled by familiar. A step's check sends the spell to another step when its reply contains (or lacks) a phrase, like a reviewer sending a draft back to the writer; each check does this at most 3 times, then the spell stops. The user casts spells from Grimoire's Spells page.",
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
                                "prompt": { "type": "string", "description": "Instructions sent to that familiar." },
                                "parallel": { "type": "boolean", "description": "Run alongside the previous step instead of after it. Not allowed on the first step." },
                                "check": {
                                    "type": "object",
                                    "description": "Optional. After this step, go to another step when the reply contains, or lacks, a phrase (case doesn't matter). Otherwise the spell continues.",
                                    "properties": {
                                        "when": { "type": "string", "enum": ["contains", "lacks"] },
                                        "phrase": { "type": "string" },
                                        "goToStep": { "type": "integer", "minimum": 1, "description": "The step to go to, counting from 1. It can't be a parallel step." }
                                    },
                                    "required": ["when", "phrase", "goToStep"]
                                }
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
            "description": "Save a new Grimoire color theme. Give colors for the dark appearance, the light one, or both; a theme with only one follows that appearance. Use calm, readable colors: text must stay legible on canvas. The user previews and applies it in Settings; you can't switch to it yourself.",
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
            "name": "ask_familiar",
            "description": "Ask another familiar something and wait for its reply, which comes back as this tool's result. Use it when another familiar knows something or can do part of the work better. The two of you talk in a side conversation, out of both your main chats; the person can read it later. The other familiar answers from a copy of its own main conversation, so it knows what it knows, but it doesn't see yours, so include what it needs. It replies within 10 minutes or the wait ends, and a later reply comes to you as a new message. The result ends with the conversation's id: pass it as thread to follow up. You can't ask yourself or a familiar that is waiting on you, chains stop at 3 familiars in a row, and the person may limit who you can ask or approve each question.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "familiar": { "type": "string", "description": "Familiar id or exact name, from list_familiars. It may be on a linked Mac: use its id, or \"Name on Mac\" when the name alone matches more than one." },
                    "message": { "type": "string", "description": "What to ask or ask it to do." },
                    "thread": { "type": "string", "description": "Optional. The id of a conversation you started with this familiar, to follow up in it. Leave out to start a new one." }
                },
                "required": ["familiar", "message"]
            }
        },
        {
            "name": "message_familiar",
            "description": "Send another familiar a message without waiting. Use it to hand off work that takes a while, or to tell it something, while you carry on. It works on the message in a side conversation, out of both your main chats, answering from a copy of its own main conversation; include what it needs, since it doesn't see yours. When it's done, its reply comes to you as a new message, which starts your next turn. The result has the conversation's id: pass it as thread to add to the same conversation. You can send at most 5 messages to other familiars in 10 minutes, and the person may limit who you can message or approve each one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "familiar": { "type": "string", "description": "Familiar id or exact name, from list_familiars. It may be on a linked Mac: use its id, or \"Name on Mac\" when the name alone matches more than one." },
                    "message": { "type": "string", "description": "What to tell it or ask it to do." },
                    "thread": { "type": "string", "description": "Optional. The id of a conversation you started with this familiar, to add to it. Leave out to start a new one." }
                },
                "required": ["familiar", "message"]
            }
        },
        {
            "name": "set_theme",
            "description": "Switch Grimoire to a built-in or user-made theme, by id or name from list_themes. A theme a familiar made must be previewed and applied by the user in Settings.",
            "inputSchema": {
                "type": "object",
                "properties": { "theme": { "type": "string", "description": "Theme id or name." } },
                "required": ["theme"]
            }
        }
    ]);
    if let Some(list) = list.as_array_mut() {
        list.extend(memory::definitions());
        list.extend(inbox::definitions());
        list.extend(ledger::definitions());
        list.extend(crate::helpers::definitions().as_array().cloned().unwrap_or_default());
        list.extend(crate::drafts::definitions());
        list.extend(crate::browser::definitions());
    }
    list
}

/// Tool names as Claude's `--allowedTools` expects them.
pub fn claude_tool_names() -> String {
    TOOL_NAMES.iter().chain(&memory::TOOL_NAMES).chain(&inbox::TOOL_NAMES).chain(&ledger::TOOL_NAMES).chain(&crate::helpers::TOOL_NAMES).chain(&[crate::drafts::TOOL]).chain(&crate::browser::TOOL_NAMES).map(|name| format!("mcp__{SERVER_NAME}__{name}")).collect::<Vec<_>>().join(",")
}

/// How a CLI starts the Grimoire tool server for one familiar.
pub struct ServerCommand {
    pub command: String,
    pub args: Vec<String>,
    /// Goes in the server's environment as `TOKEN_ENV`, never in its arguments.
    pub token: String,
}

/// Whether `agent` gets the Grimoire tools: the socket is up and the familiar runs on this Mac.
pub fn available_for(agent: &Agent) -> bool {
    agent.host.is_none() && LAUNCH.get().is_some()
}

/// The command a CLI runs to give `agent_id` the Grimoire tools, once the socket is up.
/// In a side conversation `agent_id` is its lane key, so the tools know where the familiar is working.
pub fn server_command(agent_id: &str) -> Option<ServerCommand> {
    let (exe, socket) = LAUNCH.get()?;
    Some(ServerCommand {
        command: exe.to_string_lossy().into_owned(),
        args: vec!["mcp".into(), socket.to_string_lossy().into_owned(), agent_id.into()],
        token: token_for(agent_id),
    })
}

/// The command for a relay to one connector. It shares `agent_id`'s token, so the app knows who calls.
pub fn connector_command(agent_id: &str, connector_id: &str) -> Option<ServerCommand> {
    let mut command = server_command(agent_id)?;
    command.args.push(connector_id.into());
    Some(command)
}

/// Where Claude MCP configs go: next to the socket, readable only by this user.
fn config_dir(socket: &Path) -> PathBuf {
    socket.with_file_name("mcp-config")
}

/// Saves `config` as `agent_id`'s Claude MCP config in an owner-only file and returns its path, so the
/// token in it stays out of process arguments, which `ps` shows to every user.
pub fn claude_config_file(agent_id: &str, config: &Value) -> Option<PathBuf> {
    let (_, socket) = LAUNCH.get()?;
    let path = write_config(&config_dir(socket), agent_id, config);
    if path.is_none() {
        eprintln!("Grimoire tools are off for this turn: could not write the MCP config in {}", config_dir(socket).display());
    }
    path
}

fn write_config(dir: &Path, agent_id: &str, config: &Value) -> Option<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).ok()?;
    let name: String = agent_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let path = dir.join(format!("{name}.json"));
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path).ok()?;
    file.write_all(config.to_string().as_bytes()).ok()?;
    Some(path)
}

fn tokens() -> std::sync::MutexGuard<'static, HashMap<String, String>> {
    TOKENS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn token_for(agent_id: &str) -> String {
    tokens().entry(agent_id.to_string()).or_insert_with(|| uuid::Uuid::new_v4().simple().to_string()).clone()
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
#[serde(rename_all = "camelCase")]
struct NewStep {
    familiar: String,
    prompt: String,
    #[serde(default)]
    parallel: bool,
    #[serde(default)]
    check: Option<NewCheck>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewCheck {
    when: CheckWhen,
    phrase: String,
    /// Counts from 1, like the steps people see.
    go_to_step: usize,
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
            let linked = crate::link::current().map(|l| l.linked()).unwrap_or_default();
            (pretty(&familiars_list(db, &agents, &linked, caller)?), None)
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
                        "steps": w.steps.iter().map(|s| {
                            let mut step = json!({ "familiar": name_of(&s.agent_id), "prompt": s.prompt });
                            if s.parallel {
                                step["parallel"] = json!(true);
                            }
                            if let Some(c) = &s.check {
                                step["check"] = json!({ "when": c.when, "phrase": c.phrase, "goToStep": c.go_to + 1 });
                            }
                            step
                        }).collect::<Vec<_>>(),
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
                    let check = match step.check {
                        Some(c) => Some(StepCheck {
                            when: c.when,
                            phrase: c.phrase,
                            go_to: c.go_to_step.checked_sub(1).ok_or(format!("Step {}: goToStep counts from 1.", index + 1))?,
                        }),
                        None => None,
                    };
                    Ok(WorkflowStep { agent_id: agent.id.clone(), prompt: step.prompt, parallel: step.parallel, check })
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
            let text = format!(
                "Saved the theme \"{}\" with id {}. Ask the user to preview and apply it in Settings, under Appearance.",
                saved.theme["name"].as_str().unwrap_or_default(),
                saved.id
            );
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
            let name = theme.theme["name"].as_str().unwrap_or_default();
            // Theme CSS restyles the whole window, so the user sees a familiar's theme before it applies.
            if theme.theme.get("createdBy").is_some_and(|by| !by.is_null()) {
                return Err(format!("A familiar made \"{name}\", so only the user can apply it, after a preview in Settings, under Appearance. Ask them to."));
            }
            themes::set_active(db, themes_dir, &theme.id)?;
            (format!("Grimoire now uses the theme \"{name}\"."), Some("themes_changed"))
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
        tool if memory::handles(tool) => (memory::call_tool(db, caller, tool, &arguments)?, memory::changed_event(tool)),
        tool if inbox::TOOL_NAMES.contains(&tool) => (inbox::call_tool(db, caller, tool, &arguments)?, Some(inbox::CHANGED)),
        tool if ledger::TOOL_NAMES.contains(&tool) => (ledger::call_tool(db, caller, tool, &arguments)?, ledger::changed_event(tool)),
        _ => return Err(format!("Grimoire has no tool named {tool}.")),
    };
    Ok(Outcome { text, changed })
}

pub(crate) fn resolve_agent<'a>(agents: &'a [Agent], wanted: &str) -> Result<&'a Agent, String> {
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

/// A familiar picked for `ask_familiar` or `message_familiar`.
#[derive(Debug)]
pub(crate) enum Found<'a> {
    Here(&'a Agent),
    Away(&'a Linked),
}

/// Finds a familiar here or on a linked Mac by id or name. A name on its own must be unambiguous
/// across Macs; "Name on Mac" picks one Mac's, with `this_mac` naming this one.
pub(crate) fn resolve_familiar<'a>(agents: &'a [Agent], linked: &'a [Linked], this_mac: Option<&str>, wanted: &str) -> Result<Found<'a>, String> {
    let wanted = wanted.trim();
    if let Some(agent) = agents.iter().find(|a| a.id == wanted) {
        return Ok(Found::Here(agent));
    }
    if let Some(familiar) = linked.iter().find(|f| f.id == wanted) {
        return Ok(Found::Away(familiar));
    }
    let named = |name: &str, mac: Option<&str>| -> Vec<Found<'a>> {
        let on = |home: Option<&str>| mac.is_none_or(|m| home.is_some_and(|h| h.eq_ignore_ascii_case(m)));
        let here = agents.iter().filter(|a| a.name.eq_ignore_ascii_case(name) && on(this_mac)).map(Found::Here);
        let away = linked.iter().filter(|f| f.name.eq_ignore_ascii_case(name) && on(Some(&f.mac))).map(Found::Away);
        here.chain(away).collect()
    };
    let mut found = named(wanted, None);
    if found.is_empty() {
        // "Scribe on NigelBot": the last " on " splits the name from its Mac. Lowercasing ASCII keeps
        // every byte where it was.
        if let Some(at) = wanted.to_ascii_lowercase().rfind(" on ") {
            found = named(wanted[..at].trim(), Some(wanted[at + 4..].trim()));
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!("No familiar is called \"{wanted}\". Use list_familiars to see who exists.")),
        _ => Err(format!(
            "More than one familiar is called \"{wanted}\". Use its id from list_familiars, or add its Mac, like \"{wanted} on {}\".",
            found.iter().find_map(|f| match f { Found::Away(f) => Some(f.mac.as_str()), Found::Here(_) => None }).or(this_mac).unwrap_or("Mac")
        )),
    }
}

/// `list_familiars`: this Mac's familiars, then every linked Mac's, online or not.
fn familiars_list(db: &Db, agents: &[Agent], linked: &[Linked], caller: &str) -> Result<Vec<Value>, String> {
    let covens = crate::covens::list(db)?;
    let here = agents.iter().map(|a| {
        json!({
            "id": a.id,
            "name": a.name,
            "provider": a.provider,
            "model": a.active_model.as_deref().or(a.model.as_deref()).unwrap_or("default"),
            "coven": a.coven_id.as_deref().and_then(|id| covens.iter().find(|c| c.id == id)).map(|c| c.name.clone()),
            "you": a.id == caller,
        })
    });
    let away = linked.iter().map(|f| {
        json!({
            "id": f.id,
            "name": f.name,
            "provider": f.provider,
            "model": f.model.as_deref().unwrap_or("default"),
            "mac": f.mac,
            "where": if f.online { format!("on {}", f.mac) } else { format!("on {}, which is offline", f.mac) },
            "online": f.online,
        })
    });
    Ok(here.chain(away).collect())
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
    pub runtime: Arc<crate::runtime::Runtime>,
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
    // Configs from the last run hold tokens that no longer work.
    let _ = std::fs::remove_dir_all(config_dir(socket));
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
    #[serde(default)]
    token: String,
    #[serde(default)]
    tool: String,
    #[serde(default)]
    arguments: Value,
    /// Set by a connector relay, with the raw MCP message to forward.
    #[serde(default)]
    connector: Option<String>,
    #[serde(default)]
    message: Value,
}

/// Reads one relayed call and checks that its token belongs to the familiar it names.
fn authorize(line: &str) -> Result<Request, String> {
    let request: Request = serde_json::from_str(line).map_err(|e| format!("Bad request: {e}"))?;
    let known = tokens().get(&request.agent).is_some_and(|token| !request.token.is_empty() && *token == request.token);
    if !known {
        return Err("This tool server isn't allowed to act for that familiar.".into());
    }
    Ok(request)
}

async fn handle_connection(stream: UnixStream, ctx: Arc<Context>) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let reply = match authorize(&line) {
            Ok(Request { agent, connector: Some(connector), message, .. }) => {
                // The agent is the one the token belongs to; a familiar in a side conversation calls as "familiar~chat".
                let familiar = crate::side::split(&agent).0.to_string();
                match crate::connectors::forward(&ctx.db, ctx.secrets.as_ref(), &agent, &familiar, &connector, message).await {
                    Ok(messages) => json!({ "ok": true, "messages": messages }),
                    Err(error) => json!({ "ok": false, "error": error }),
                }
            }
            Ok(request) => {
                // A familiar answering in a side conversation calls as "familiar~chat".
                let (caller, lane) = crate::side::split(&request.agent);
                let caller = caller.to_string();
                let result = if let Err(e) = crate::helpers::check_lane(&ctx.db, lane, &request.tool) {
                    Err(e)
                } else if crate::helpers::TOOL_NAMES.contains(&request.tool.as_str()) {
                    crate::helpers::call_tool(&ctx, &caller, lane, &request.tool, request.arguments)
                } else if request.tool == "ask_familiar" {
                    crate::delegation::run(&ctx, &caller, lane, request.arguments).await.map(|text| Outcome { text, changed: None })
                } else if request.tool == "message_familiar" {
                    crate::delegation::run_message(&ctx, &caller, lane, request.arguments).await.map(|text| Outcome { text, changed: None })
                } else if request.tool == crate::drafts::TOOL {
                    crate::drafts::run(&ctx.app, &ctx.db, &caller, &request.arguments).map(|text| Outcome { text, changed: None })
                } else if crate::browser::TOOL_NAMES.contains(&request.tool.as_str()) {
                    crate::browser::call_tool(&ctx.db, &caller, &request.tool, request.arguments).await.map(|text| Outcome { text, changed: None })
                } else if request.tool == "vault_use" {
                    let ask = |question| ctx.approvals.ask(&ctx.app, question);
                    let used = vault_use(&ctx.db, ctx.secrets.as_ref(), &caller, request.arguments, ask).await;
                    // Every attempt lands in the item's log, allowed or not.
                    let _ = ctx.app.emit("vault_changed", ());
                    used.map(|text| Outcome { text, changed: None })
                } else {
                    let ctx = ctx.clone();
                    tokio::task::spawn_blocking(move || call(&ctx.db, &ctx.themes_dir, &caller, &request.tool, request.arguments))
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
            Err(error) => json!({ "ok": false, "error": error }),
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
            coven_id: None,
            host: None,
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
        assert_eq!(saved.steps[0], WorkflowStep { agent_id: "r1".into(), prompt: "Research {{input}}".into(), ..Default::default() });
        assert_eq!(saved.steps[1].agent_id, "w1");

        // Editing it from the Spells page keeps the creator.
        workflows::save(&db, Some(&saved.id), "Brief v2", saved.steps.clone(), None).unwrap();
        assert_eq!(db.list_workflows().unwrap()[0].created_by.as_deref(), Some("r1"));
    }

    #[test]
    fn creates_a_spell_with_parallel_steps_and_a_check() {
        let (db, dir) = setup();
        let args = json!({ "name": "Post", "steps": [
            { "familiar": "Writer", "prompt": "Draft {{input}}" },
            { "familiar": "Researcher", "prompt": "Find sources", "parallel": true },
            { "familiar": "Researcher", "prompt": "Review", "check": { "when": "lacks", "phrase": " APPROVED ", "goToStep": 1 } }
        ]});
        call(&db, &dir, "r1", "create_spell", args).unwrap();
        let saved = &db.list_workflows().unwrap()[0];
        assert!(!saved.steps[0].parallel && saved.steps[1].parallel);
        assert_eq!(saved.steps[2].check, Some(StepCheck { when: CheckWhen::Lacks, phrase: "APPROVED".into(), go_to: 0 }));
        let listed: Vec<Value> = serde_json::from_str(&call(&db, &dir, "r1", "list_spells", json!({})).unwrap().text).unwrap();
        assert_eq!(listed[0]["steps"][1]["parallel"], true);
        assert_eq!(listed[0]["steps"][2]["check"]["goToStep"], 1);
        assert!(listed[0]["steps"][0].get("check").is_none());

        let bad = |step: Value| call(&db, &dir, "r1", "create_spell", json!({ "name": "X", "steps": [{ "familiar": "Writer", "prompt": "Go" }, step] }));
        let check = |go: i64| json!({ "familiar": "Writer", "prompt": "Go", "check": { "when": "contains", "phrase": "x", "goToStep": go } });
        assert_eq!(bad(check(0)).unwrap_err(), "Step 2: goToStep counts from 1.");
        assert!(bad(check(9)).unwrap_err().contains("doesn't exist"));
        let first_parallel = json!({ "name": "X", "steps": [{ "familiar": "Writer", "prompt": "Go", "parallel": true }] });
        assert!(call(&db, &dir, "r1", "create_spell", first_parallel).unwrap_err().contains("Step 1 can't run alongside"));
        assert_eq!(db.list_workflows().unwrap().len(), 1);
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

    fn linked(peer: &str, agent: &str, name: &str, mac: &str, online: bool) -> Linked {
        Linked {
            id: crate::link::remote_id(peer, agent),
            peer: peer.into(),
            agent: agent.into(),
            name: name.into(),
            mac: mac.into(),
            online,
            provider: Some("claude".into()),
            model: None,
        }
    }

    #[test]
    fn lists_familiars_on_linked_macs_online_or_not() {
        let (db, _) = setup();
        let agents = db.list_agents().unwrap();
        let away = [linked("m1", "s1", "Scribe", "NigelBot", true), linked("m2", "s2", "Critic", "Air", false)];
        let list = familiars_list(&db, &agents, &away, "w1").unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(list[2]["id"], "peer:m1:s1");
        assert_eq!((list[2]["name"].as_str(), list[2]["where"].as_str()), (Some("Scribe"), Some("on NigelBot")));
        assert_eq!((list[3]["where"].as_str(), list[3]["online"].as_bool()), (Some("on Air, which is offline"), Some(false)));
        assert!(list[0].get("mac").is_none(), "this Mac's familiars have no Mac");
    }

    #[test]
    fn resolves_familiars_on_linked_macs_by_id_name_or_name_on_mac() {
        let (db, _) = setup();
        let agents = db.list_agents().unwrap();
        let away = [linked("m1", "s1", "Scribe", "NigelBot", true), linked("m2", "s2", "Scribe", "Air", true), linked("m1", "c1", "Critic", "NigelBot", false)];
        let id = |found: Found| match found {
            Found::Here(a) => a.id.clone(),
            Found::Away(f) => f.id.clone(),
        };
        let pick = |wanted: &str| resolve_familiar(&agents, &away, Some("nanmbp"), wanted).map(id);
        assert_eq!(pick("critic").unwrap(), "peer:m1:c1", "a name alone works when it's the only one");
        assert_eq!(pick("peer:m2:s2").unwrap(), "peer:m2:s2");
        assert_eq!(pick("Scribe on nigelbot").unwrap(), "peer:m1:s1");
        assert_eq!(pick(" Scribe on Air ").unwrap(), "peer:m2:s2");
        assert_eq!(pick("Writer").unwrap(), "w1");
        assert_eq!(pick("Writer on nanmbp").unwrap(), "w1", "this Mac's name works too");
        let err = pick("Scribe").unwrap_err();
        assert!(err.contains("More than one familiar is called \"Scribe\"") && err.contains("\"Scribe on NigelBot\""), "{err}");
        assert!(pick("Writer on NigelBot").unwrap_err().contains("No familiar is called"));
        assert!(pick("Scribe on Studio").unwrap_err().contains("No familiar is called"));
        // A familiar here and one on a linked Mac with the same name need the Mac.
        let twin = [linked("m1", "w9", "Writer", "NigelBot", true)];
        assert!(resolve_familiar(&agents, &twin, Some("nanmbp"), "Writer").is_err());
        assert_eq!(resolve_familiar(&agents, &twin, Some("nanmbp"), "writer on NIGELBOT").map(id).unwrap(), "peer:m1:w9");
    }

    #[test]
    fn familiars_on_another_mac_are_not_offered_the_tools() {
        let mut remote = agent("r1", "Remote");
        remote.host = Some("me@studio.local".into());
        assert!(!available_for(&remote));
    }

    #[test]
    fn offers_the_memory_tools() {
        let names: Vec<String> = definitions().as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect();
        for tool in memory::TOOL_NAMES {
            assert!(names.iter().any(|n| n == tool));
            assert!(claude_tool_names().contains(&format!("mcp__grimoire__{tool}")));
        }
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
    fn refuses_calls_that_name_another_familiar() {
        let (researcher, writer) = (token_for("tok-r1"), token_for("tok-w1"));
        assert_ne!(researcher, writer);
        assert_eq!(token_for("tok-r1"), researcher, "one token per familiar for the app's run");
        let line = |agent: &str, token: &str| json!({ "agent": agent, "token": token, "tool": "list_familiars" }).to_string();

        assert_eq!(authorize(&line("tok-r1", &researcher)).unwrap().agent, "tok-r1");
        let refused = "This tool server isn't allowed to act for that familiar.";
        assert_eq!(authorize(&line("tok-w1", &researcher)).err().unwrap(), refused, "the researcher's token can't act as the writer");
        assert_eq!(authorize(&line("tok-r1", "")).err().unwrap(), refused);
        assert_eq!(authorize(&line("nobody", "")).err().unwrap(), refused, "no token was ever issued for this id");
        let no_token = json!({ "agent": "tok-r1", "tool": "list_familiars" }).to_string();
        assert_eq!(authorize(&no_token).err().unwrap(), refused);
        assert!(authorize("not json").err().unwrap().starts_with("Bad request"));
    }

    #[test]
    fn claude_configs_are_owner_only_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("grimoire-mcp-{}", uuid::Uuid::new_v4())).join("mcp-config");
        let config = json!({ "mcpServers": { "grimoire": { "env": { TOKEN_ENV: "secret" } } } });
        let path = write_config(&dir, "a1~../chat", &config).unwrap();
        assert_eq!(path.parent().unwrap(), dir, "a lane key can't leave the folder");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), config.to_string());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
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

        // A familiar's theme waits for the user's preview, even for the familiar that made it.
        let refused = call(&db, &dir, "r1", "set_theme", json!({ "theme": "rainy LIBRARY" })).unwrap_err();
        assert!(refused.contains("only the user can apply it"), "{refused}");
        let list: Vec<Value> = serde_json::from_str(&call(&db, &dir, "w1", "list_themes", json!({})).unwrap().text).unwrap();
        let rainy = list.iter().find(|t| t["id"] == "rainy-library").unwrap();
        assert_eq!(rainy["active"], false);
        assert_eq!(rainy["createdBy"], "Researcher");

        let switched = call(&db, &dir, "w1", "set_theme", json!({ "theme": "lapis" })).unwrap();
        assert_eq!(switched.changed, Some("themes_changed"));
        assert_eq!(themes::list(&db, &dir).unwrap().active, "lapis");
        assert!(call(&db, &dir, "r1", "set_theme", json!({ "theme": "Neon" })).unwrap_err().contains("No theme"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
