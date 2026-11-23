mod approvals;
mod covens;
mod db;
mod dock;
mod mcp;
mod permissions;
mod providers;
mod routines;
mod runtime;
mod status;
mod themes;
mod tools;
mod updates;
mod vault;
mod workflow_runs;
mod workflows;

use db::{now_ms, Access, Agent, Avatar, Db, Message, Provider, Routine, Workflow, WorkflowStep};
use routines::Scheduler;
use runtime::Runtime;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use workflows::WorkflowRunner;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_opener::OpenerExt;

struct AppState {
    db: Arc<Db>,
    runtime: Arc<Runtime>,
    scheduler: Arc<Scheduler>,
    workflows: Arc<WorkflowRunner>,
    dock: dock::Dock,
    themes_dir: PathBuf,
    approvals: Arc<approvals::Approvals>,
    secrets: Arc<dyn vault::SecretStore>,
}

const CLAUDE_BILLING: &str = "claude_billing";
const DOCK_EDGE: &str = "dock_edge";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewAgent {
    name: String,
    provider: Provider,
    model: Option<String>,
    workspace: Option<String>,
    access: Access,
    avatar: Avatar,
    effort: Option<String>,
    #[serde(default)]
    coven_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentChanges {
    name: String,
    model: Option<String>,
    access: Access,
    avatar: Avatar,
    effort: Option<String>,
    /// Left out keeps the familiar's coven, null unbinds it, and an id moves it there. Other edits,
    /// like switching the model, leave covens alone this way.
    #[serde(default)]
    coven_id: Option<serde_json::Value>,
}

/// A coven a familiar is being put in, checked to exist. None or an empty id means unbound.
fn clean_coven(db: &Db, coven_id: Option<String>) -> Result<Option<String>, String> {
    match coven_id.filter(|c| !c.is_empty()) {
        Some(id) if covens::exists(db, &id)? => Ok(Some(id)),
        Some(_) => Err("That coven no longer exists.".into()),
        None => Ok(None),
    }
}

/// The coven after an edit: a change left out keeps `current`, null unbinds, an id moves.
fn next_coven(db: &Db, current: Option<String>, change: Option<serde_json::Value>) -> Result<Option<String>, String> {
    match change {
        None => Ok(current),
        Some(value) => clean_coven(db, value.as_str().map(String::from)),
    }
}

fn clean_effort(effort: Option<String>) -> Result<Option<String>, String> {
    match effort.map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()) {
        Some(e) if providers::EFFORTS.contains(&e.as_str()) => Ok(Some(e)),
        Some(e) => Err(format!("Unknown effort level: {e}")),
        None => Ok(None),
    }
}

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err("Give the familiar a name of 1 to 60 characters.".into());
    }
    Ok(name.to_string())
}

fn clean_model(model: Option<String>) -> Option<String> {
    model.map(|m| m.trim().to_string()).filter(|m| !m.is_empty())
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| "HOME is not set.".into())
}

fn slug(name: &str, fallback: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { fallback.into() } else { slug }
}

fn expand(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if path == "~" => home.to_path_buf(),
        None => PathBuf::from(path),
    }
}

fn workspace_for(requested: Option<&str>, name: &str, id: &str, home: &Path) -> Result<PathBuf, String> {
    let path = match requested.map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => expand(path, home),
        None => home.join("Grimoire").join(format!("{}-{}", slug(name, "agent"), &id[..6])),
    };
    if !path.is_absolute() {
        return Err("The workspace must be a full path, like ~/Projects/site.".into());
    }
    if path.parent().is_none() || path == home {
        return Err("Pick a folder inside your home folder, not the disk root or the home folder itself.".into());
    }
    Ok(path)
}

#[tauri::command]
fn list_agents(state: State<AppState>) -> Result<Vec<Agent>, String> {
    state.db.list_agents()
}

#[tauri::command]
fn busy_agents(state: State<AppState>) -> Vec<String> {
    state.runtime.busy_agents()
}

#[tauri::command]
fn create_agent(app: AppHandle, state: State<AppState>, input: NewAgent) -> Result<Agent, String> {
    let name = clean_name(&input.name)?;
    input.avatar.validate()?;
    let id = uuid::Uuid::new_v4().to_string();
    let workspace = workspace_for(input.workspace.as_deref(), &name, &id, &home()?)?;
    std::fs::create_dir_all(&workspace).map_err(|e| format!("Could not create {}: {e}", workspace.display()))?;
    let agent = Agent {
        id,
        name,
        provider: input.provider,
        model: clean_model(input.model),
        workspace: workspace.to_string_lossy().into_owned(),
        access: input.access,
        session_id: None,
        avatar: input.avatar,
        effort: clean_effort(input.effort)?,
        active_model: None,
        coven_id: clean_coven(&state.db, input.coven_id)?,
        created_at: now_ms(),
    };
    state.db.insert_agent(&agent)?;
    let _ = app.emit("agents_changed", ());
    Ok(agent)
}

#[tauri::command]
async fn update_agent(
    app: AppHandle,
    state: State<'_, AppState>,
    agent_id: String,
    changes: AgentChanges,
) -> Result<Agent, String> {
    let mut agent = state.db.get_agent(&agent_id)?.ok_or("That familiar no longer exists.")?;
    changes.avatar.validate()?;
    let settings_changed = agent.model != clean_model(changes.model.clone()) || agent.access != changes.access;
    agent.name = clean_name(&changes.name)?;
    agent.model = clean_model(changes.model);
    agent.access = changes.access;
    agent.avatar = changes.avatar;
    agent.effort = clean_effort(changes.effort)?;
    agent.coven_id = next_coven(&state.db, agent.coven_id.take(), changes.coven_id)?;
    state.db.update_agent(&agent)?;
    if settings_changed {
        state.runtime.settings_changed(&agent_id).await;
    }
    let _ = app.emit("agents_changed", ());
    Ok(agent)
}

#[tauri::command]
fn open_workspace(app: AppHandle, state: State<AppState>, agent_id: String) -> Result<(), String> {
    let agent = state.db.get_agent(&agent_id)?.ok_or("That familiar no longer exists.")?;
    app.opener().open_path(&agent.workspace, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_agent(app: AppHandle, state: State<'_, AppState>, agent_id: String) -> Result<(), String> {
    state.runtime.remove_agent(&agent_id).await?;
    state.db.delete_agent(&agent_id)?;
    let _ = app.emit("agents_changed", ());
    Ok(())
}

#[tauri::command]
fn list_messages(
    state: State<AppState>,
    agent_id: String,
    before_id: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<Message>, String> {
    state.db.list_messages(&agent_id, before_id, limit.unwrap_or(100).clamp(1, 500))
}

#[tauri::command]
fn send_message(state: State<AppState>, agent_id: String, text: String) -> Result<Message, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("The message is empty.".into());
    }
    state.runtime.send(&agent_id, text.to_string(), None)
}

#[tauri::command]
async fn stop_turn(state: State<'_, AppState>, agent_id: String) -> Result<(), String> {
    state.runtime.stop(&agent_id).await
}

#[tauri::command]
fn new_chat(state: State<AppState>, agent_id: String) -> Result<Message, String> {
    if state.runtime.is_busy(&agent_id) {
        return Err("Wait for the familiar to finish, or stop it first.".into());
    }
    state.runtime.reset_session(&agent_id);
    state.db.set_session(&agent_id, None)?;
    state.db.insert_message(&agent_id, "divider", "New chat", "done")
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    claude_billing: String,
    dock_edge: String,
}

#[tauri::command]
fn dock_edge(state: State<AppState>) -> Option<dock::Edge> {
    state.dock.edge()
}

#[tauri::command]
fn dock_resize(app: AppHandle, state: State<AppState>, width: f64, height: f64, focus: bool) -> Result<(), String> {
    state.dock.resize(&app, width, height, focus)
}

#[tauri::command]
fn show_main(app: AppHandle, agent_id: Option<String>) -> Result<(), String> {
    dock::show_main(&app, agent_id)
}

#[tauri::command]
async fn list_models(state: State<'_, AppState>, provider: Provider) -> Result<Vec<providers::ModelOption>, String> {
    state.runtime.list_models(&provider).await
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Result<Settings, String> {
    Ok(Settings {
        claude_billing: state.db.setting(CLAUDE_BILLING)?.unwrap_or_else(|| "subscription".into()),
        dock_edge: state.db.setting(DOCK_EDGE)?.unwrap_or_else(|| "off".into()),
    })
}

#[tauri::command]
async fn save_settings(app: AppHandle, state: State<'_, AppState>, settings: Settings) -> Result<Settings, String> {
    if !matches!(settings.claude_billing.as_str(), "subscription" | "api") {
        return Err("Claude billing must be subscription or api.".into());
    }
    if !matches!(settings.dock_edge.as_str(), "off" | "left" | "right") {
        return Err("The dock goes on the left, the right, or off.".into());
    }
    let previous_billing = state.db.setting(CLAUDE_BILLING)?.unwrap_or_else(|| "subscription".into());
    state.db.set_setting(CLAUDE_BILLING, &settings.claude_billing)?;
    state.db.set_setting(DOCK_EDGE, &settings.dock_edge)?;
    if previous_billing != settings.claude_billing {
        providers::USE_CLAUDE_SUBSCRIPTION.store(settings.claude_billing == "subscription", Ordering::Relaxed);
        state.runtime.claude_billing_changed();
    }
    state.dock.set_edge(&app, dock::Edge::parse(&settings.dock_edge))?;
    Ok(settings)
}

#[tauri::command]
async fn provider_status() -> Vec<status::ProviderStatus> {
    let (codex, claude) = tokio::join!(status::check("codex"), status::check("claude"));
    vec![codex, claude]
}

#[tauri::command]
fn last_messages(state: State<AppState>) -> Result<Vec<Message>, String> {
    state.db.last_messages()
}

#[tauri::command]
fn list_routines(state: State<AppState>) -> Result<Vec<Routine>, String> {
    state.db.list_routines()
}

#[tauri::command]
fn save_routine(state: State<AppState>, input: routines::RoutineInput) -> Result<Routine, String> {
    let routine = routines::save(&state.db, input)?;
    state.scheduler.changed();
    Ok(routine)
}

#[tauri::command]
fn delete_routine(state: State<AppState>, id: String) -> Result<(), String> {
    state.db.delete_routine(&id)?;
    state.scheduler.changed();
    Ok(())
}

#[tauri::command]
fn run_routine(state: State<AppState>, id: String) -> Result<(), String> {
    let routine = state.db.list_routines()?.into_iter().find(|r| r.id == id).ok_or("That ritual no longer exists.")?;
    state.scheduler.run_now(&routine)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowInput {
    id: Option<String>,
    name: String,
    steps: Vec<WorkflowStep>,
}

#[tauri::command]
fn list_workflows(state: State<AppState>) -> Result<Vec<Workflow>, String> {
    state.db.list_workflows()
}

#[tauri::command]
fn running_workflows(state: State<AppState>) -> Vec<String> {
    state.workflows.running()
}

#[tauri::command]
fn save_workflow(app: AppHandle, state: State<AppState>, input: WorkflowInput) -> Result<Workflow, String> {
    let saved = workflows::save(&state.db, input.id.as_deref(), &input.name, input.steps, None)?;
    let _ = app.emit("workflows_changed", ());
    Ok(saved)
}

#[tauri::command]
fn delete_workflow(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    state.db.delete_workflow(&id)?;
    let _ = app.emit("workflows_changed", ());
    Ok(())
}

#[tauri::command]
fn run_workflow(state: State<AppState>, id: String, input: Option<String>) -> Result<String, String> {
    state.workflows.start(&id, input.unwrap_or_default().trim().to_string(), None)
}

#[tauri::command]
fn recent_workflow_runs(state: State<AppState>) -> Result<Vec<workflow_runs::Run>, String> {
    workflow_runs::recent(&state.db, workflow_runs::RECENT_PER_SPELL)
}

#[tauri::command]
fn get_workflow_run(state: State<AppState>, id: String) -> Result<workflow_runs::RunDetail, String> {
    workflow_runs::detail(&state.db, &id)?.ok_or_else(|| "That cast is gone.".into())
}

#[tauri::command]
async fn stop_workflow(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.workflows.stop(&id).await
}

#[tauri::command]
fn list_covens(state: State<AppState>) -> Result<Vec<covens::Coven>, String> {
    covens::list(&state.db)
}

#[tauri::command]
fn save_coven(app: AppHandle, state: State<AppState>, id: Option<String>, name: String, members: Vec<String>) -> Result<covens::Coven, String> {
    let saved = covens::save(&state.db, id.as_deref(), &name, &members)?;
    let _ = app.emit("covens_changed", ());
    let _ = app.emit("agents_changed", ());
    Ok(saved)
}

#[tauri::command]
fn delete_coven(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    covens::delete(&state.db, &id)?;
    let _ = app.emit("covens_changed", ());
    let _ = app.emit("agents_changed", ());
    Ok(())
}

#[tauri::command]
fn list_themes(state: State<AppState>) -> Result<themes::ThemeList, String> {
    themes::list(&state.db, &state.themes_dir)
}

#[tauri::command]
fn save_theme(app: AppHandle, state: State<AppState>, id: Option<String>, theme: serde_json::Value) -> Result<themes::ThemeEntry, String> {
    let saved = themes::save(&state.themes_dir, id.as_deref(), &theme, None)?;
    let _ = app.emit("themes_changed", ());
    Ok(saved)
}

#[tauri::command]
fn delete_theme(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    themes::delete(&state.db, &state.themes_dir, &id)?;
    let _ = app.emit("themes_changed", ());
    Ok(())
}

#[tauri::command]
fn set_theme(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    themes::set_active(&state.db, &state.themes_dir, &id)?;
    let _ = app.emit("themes_changed", ());
    Ok(())
}

/// Shows `icon`, "paper" or "night", in the Dock while Grimoire runs.
#[tauri::command]
fn set_app_icon(app: AppHandle, icon: String) -> Result<(), String> {
    let png: &'static [u8] = match icon.as_str() {
        "paper" => include_bytes!("../icons/app-paper.png"),
        "night" => include_bytes!("../icons/app-night.png"),
        _ => return Err(format!("Unknown app icon: {icon}")),
    };
    #[cfg(target_os = "macos")]
    app.run_on_main_thread(move || {
        use objc2::{AnyThread, MainThreadMarker};
        use objc2_app_kit::{NSApplication, NSImage};
        use objc2_foundation::NSData;
        let Some(main) = MainThreadMarker::new() else { return };
        if let Some(image) = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png)) {
            unsafe { NSApplication::sharedApplication(main).setApplicationIconImage(Some(&image)) };
        }
    })
    .map_err(|e| e.to_string())?;
    #[cfg(not(target_os = "macos"))]
    let _ = (app, png);
    Ok(())
}

#[tauri::command]
fn open_themes_folder(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    std::fs::create_dir_all(&state.themes_dir).map_err(|e| e.to_string())?;
    app.opener().open_path(state.themes_dir.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_vault(state: State<AppState>) -> Result<Vec<vault::VaultItem>, String> {
    vault::list(&state.db)
}

/// The secret fields of an item, for revealing or copying in the vault.
#[tauri::command]
fn vault_secret(state: State<AppState>, id: String) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    vault::secret(state.secrets.as_ref(), &id)
}

#[tauri::command]
fn save_vault_item(app: AppHandle, state: State<AppState>, input: vault::ItemInput) -> Result<vault::VaultItem, String> {
    let saved = vault::save(&state.db, state.secrets.as_ref(), input)?;
    let _ = app.emit("vault_changed", ());
    Ok(saved)
}

#[tauri::command]
fn delete_vault_item(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    vault::delete(&state.db, state.secrets.as_ref(), &id)?;
    let _ = app.emit("vault_changed", ());
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VaultActivity {
    uses: Vec<vault::Use>,
    /// Familiars that use this item without asking.
    always_allowed: Vec<String>,
}

#[tauri::command]
fn vault_activity(state: State<AppState>, id: String) -> Result<VaultActivity, String> {
    Ok(VaultActivity { uses: vault::uses(&state.db, &id)?, always_allowed: state.db.grantees(&vault::scope(&id))? })
}

#[tauri::command]
fn revoke_vault_grant(app: AppHandle, state: State<AppState>, id: String, agent_id: String) -> Result<(), String> {
    state.db.remove_grant(&agent_id, &vault::scope(&id))?;
    let _ = app.emit("vault_changed", ());
    Ok(())
}

#[tauri::command]
fn usage_since(state: State<AppState>, since: i64) -> Result<Vec<db::UsageRow>, String> {
    state.db.usage_since(since)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanLimits {
    codex: Option<serde_json::Value>,
    codex_error: Option<String>,
    /// Claude's latest `rate_limit_event`, with when it arrived. Claude only reports it during a turn.
    claude: Option<serde_json::Value>,
}

#[tauri::command]
async fn plan_limits(state: State<'_, AppState>) -> Result<PlanLimits, String> {
    let (codex, codex_error) = match state.runtime.codex_rate_limits().await {
        Ok(limits) => (Some(limits), None),
        Err(e) => (None, Some(e)),
    };
    let claude = state.db.setting(runtime::CLAUDE_LIMITS)?.and_then(|text| serde_json::from_str(&text).ok());
    Ok(PlanLimits { codex, codex_error, claude })
}

#[tauri::command]
async fn check_for_updates() -> Result<updates::UpdateCheck, String> {
    updates::check().await
}

#[tauri::command]
fn get_permissions(state: State<AppState>) -> permissions::Permissions {
    permissions::load(&state.db)
}

#[tauri::command]
fn save_permissions(state: State<AppState>, permissions: permissions::Permissions) -> Result<permissions::Permissions, String> {
    // Codex picks up the mode on its next turn; Claude's hook asks the rules on every command.
    permissions::save(&state.db, permissions)
}

#[tauri::command]
fn answer_approval(state: State<AppState>, id: String, decision: approvals::Decision) -> Result<(), String> {
    state.approvals.answer(&id, decision)
}

#[tauri::command]
fn waiting_approvals(state: State<AppState>) -> Vec<approvals::Request> {
    state.approvals.waiting()
}

/// Entry point for `grimoire mcp`, the tool server a CLI starts for an agent.
pub fn mcp_main(args: &[String]) -> i32 {
    mcp::main(args)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db = Arc::new(Db::open(&dir.join("grimoire.db"))?);
            let billing = db.setting(CLAUDE_BILLING).ok().flatten();
            providers::USE_CLAUDE_SUBSCRIPTION.store(billing.as_deref() != Some("api"), Ordering::Relaxed);
            let runtime = Runtime::new(db.clone(), app.handle().clone());
            let workflows = WorkflowRunner::new(db.clone(), runtime.clone(), app.handle().clone());
            let edge = db.setting(DOCK_EDGE).ok().flatten().and_then(|e| dock::Edge::parse(&e));
            // Agents get the Grimoire tools only when this socket is up; Grimoire still works without them.
            let themes_dir = themes::dir(&dir);
            vault::clear_hand_offs();
            let approvals = Arc::new(approvals::Approvals::default());
            permissions::install(app.handle().clone(), db.clone(), approvals.clone());
            #[cfg(target_os = "macos")]
            let secrets: Arc<dyn vault::SecretStore> = Arc::new(vault::Keychain);
            let tools = tools::Context {
                app: app.handle().clone(),
                db: db.clone(),
                themes_dir: themes_dir.clone(),
                approvals: approvals.clone(),
                secrets: secrets.clone(),
            };
            if let Err(e) = tools::serve(Arc::new(tools), &dir.join("grimoire.sock")) {
                eprintln!("Grimoire tools are off: {e}");
            }
            // Rituals can fire as soon as the scheduler starts, so the approval gate and the tool socket
            // have to be up first.
            let scheduler = Scheduler::start(db.clone(), &runtime, &workflows, app.handle().clone());
            app.manage(AppState { db, runtime, scheduler, workflows, dock: dock::Dock::default(), themes_dir, approvals, secrets });
            if edge.is_some() {
                let state = app.state::<AppState>();
                state.dock.set_edge(app.handle(), edge)?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_agents,
            busy_agents,
            create_agent,
            update_agent,
            open_workspace,
            delete_agent,
            list_messages,
            send_message,
            stop_turn,
            new_chat,
            get_settings,
            list_models,
            dock_edge,
            dock_resize,
            show_main,
            save_settings,
            provider_status,
            last_messages,
            list_routines,
            save_routine,
            delete_routine,
            run_routine,
            list_workflows,
            running_workflows,
            save_workflow,
            delete_workflow,
            run_workflow,
            stop_workflow,
            recent_workflow_runs,
            get_workflow_run,
            list_themes,
            save_theme,
            delete_theme,
            set_theme,
            open_themes_folder,
            set_app_icon,
            list_vault,
            vault_secret,
            save_vault_item,
            delete_vault_item,
            vault_activity,
            revoke_vault_grant,
            answer_approval,
            waiting_approvals,
            get_permissions,
            save_permissions,
            usage_since,
            plan_limits,
            check_for_updates,
            list_covens,
            save_coven,
            delete_coven
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let dock_on = window.app_handle().state::<AppState>().dock.edge().is_some();
                if window.label() == "main" && dock_on {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Grimoire")
        .run(|app, event| {
            if let RunEvent::Reopen { .. } = event {
                let _ = dock::show_main(app, None);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coven_edits_keep_unbind_or_move() {
        let db = Db::open_in_memory().unwrap();
        let ops = covens::save(&db, None, "Ops", &[]).unwrap();
        let current = Some("old".to_string());
        assert_eq!(next_coven(&db, current.clone(), None).unwrap(), current, "a model change leaves the coven alone");
        assert_eq!(next_coven(&db, current.clone(), Some(serde_json::Value::Null)).unwrap(), None);
        assert_eq!(next_coven(&db, None, Some(serde_json::json!(ops.id))).unwrap(), Some(ops.id.clone()));
        assert!(next_coven(&db, None, Some(serde_json::json!("gone"))).is_err());
    }

    #[test]
    fn picks_a_default_workspace_under_grimoire() {
        let home = Path::new("/Users/me");
        let path = workspace_for(None, "Site Builder!", "abcdef123", home).unwrap();
        assert_eq!(path, Path::new("/Users/me/Grimoire/site-builder-abcdef"));
    }

    #[test]
    fn accepts_known_effort_levels_only() {
        assert_eq!(clean_effort(Some(" High ".into())).unwrap().as_deref(), Some("high"));
        assert_eq!(clean_effort(Some(String::new())).unwrap(), None);
        assert!(clean_effort(Some("turbo".into())).is_err());
    }

    #[test]
    fn expands_home_and_refuses_unsafe_folders() {
        let home = Path::new("/Users/me");
        assert_eq!(workspace_for(Some("~/code/app"), "a", "abcdef", home).unwrap(), Path::new("/Users/me/code/app"));
        assert!(workspace_for(Some("~"), "a", "abcdef", home).is_err());
        assert!(workspace_for(Some("/"), "a", "abcdef", home).is_err());
        assert!(workspace_for(Some("relative/dir"), "a", "abcdef", home).is_err());
    }
}
