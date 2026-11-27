//! Export and import. One familiar, spell, coven, or theme goes out as a small `.grimoire.json` file;
//! a backup holds all of them plus rituals and settings. Files name familiars instead of using ids,
//! so they work on another Mac. Vault items, Keychain secrets, and grants never leave.

use crate::budgets::{self, Budget, PlanGuard};
use crate::memory::{self, Entry, Owner, Scope};
use crate::db::{now_ms, Access, Agent, Avatar, Db, Provider, Routine, Schedule, Workflow, WorkflowStep};
use crate::permissions::Permissions;
use crate::skills::{self, NewSkill, SkillFile};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;

pub const VERSION: u32 = 1;
const MAX_FILE: u64 = 64 * 1024 * 1024;
const HISTORY_ROLES: [&str; 5] = ["user", "assistant", "tool", "error", "divider"];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Familiar,
    Spell,
    Coven,
    Theme,
    Backup,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GrimoireFile {
    pub kind: Kind,
    pub version: u32,
    #[serde(default)]
    pub exported_at: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub familiars: Vec<FamiliarDoc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spells: Vec<SpellDoc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rituals: Vec<RitualDoc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covens: Vec<CovenDoc>,
    /// Theme JSON in the same shape as the files in the themes folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub themes: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SettingsDoc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<SkillDoc>,
}

impl GrimoireFile {
    pub fn new(kind: Kind) -> Self {
        GrimoireFile {
            kind,
            version: VERSION,
            exported_at: now_ms(),
            familiars: Vec::new(),
            spells: Vec::new(),
            rituals: Vec::new(),
            covens: Vec::new(),
            themes: Vec::new(),
            settings: None,
            skills: Vec::new(),
        }
    }
}

/// A familiar's settings and avatar. Never its workspace, session, or (outside a backup that asks
/// for it) its chat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FamiliarDoc {
    pub name: String,
    pub provider: Provider,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    pub access: Access,
    pub avatar: Avatar,
    /// The SSH destination of the Mac it runs on, for a familiar on another Mac.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryDoc>,
    /// Memory entries, in backups. Older backups hold markdown text, read as facts.
    #[serde(default, deserialize_with = "memory::from_doc", skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDoc {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub source: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StepDoc {
    /// The familiar's name.
    pub familiar: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parallel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<crate::db::StepCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpellDoc {
    pub name: String,
    pub steps: Vec<StepDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RitualDoc {
    /// The familiar it messages. Rituals that cast a spell name the spell instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub familiar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spell: Option<String>,
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CovenDoc {
    pub name: String,
    /// Familiar names.
    pub members: Vec<String>,
    /// Shared memory entries, in backups, as in FamiliarDoc.
    #[serde(default, deserialize_with = "memory::from_doc", skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<Entry>,
}

/// A skill's text files, who made it, and which familiars have it on, by name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillDoc {
    pub name: String,
    pub files: Vec<SkillFile>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The familiar that made it.
    #[serde(default, rename = "madeBy", skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub familiars: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDoc {
    #[serde(default)]
    pub claude_billing: Option<String>,
    #[serde(default)]
    pub dock_edge: Option<String>,
    #[serde(default)]
    pub permissions: Option<Permissions>,
    #[serde(default)]
    pub plan_guard: Option<PlanGuard>,
}

/// "Researcher.grimoire.json", with characters Finder dislikes swapped out.
pub fn file_name(name: &str) -> String {
    let clean: String = name.trim().chars().map(|c| if matches!(c, '/' | ':' | '\\') || c.is_control() { '-' } else { c }).collect();
    let clean = clean.trim_start_matches('.').trim();
    format!("{}.grimoire.json", if clean.is_empty() { "Grimoire" } else { clean })
}

fn familiar_doc(db: &Db, agent: &Agent, history: bool) -> Result<FamiliarDoc, String> {
    let history = if history {
        db.list_messages(&agent.id, None, i64::MAX)?
            .into_iter()
            // Links to side conversations stay behind: those chats aren't in the backup.
            .filter(|m| m.status != "streaming" && m.role != crate::side::LINK_ROLE)
            .map(|m| HistoryDoc { role: m.role, text: m.text, source: m.source, created_at: m.created_at })
            .collect()
    } else {
        Vec::new()
    };
    Ok(FamiliarDoc {
        name: agent.name.clone(),
        provider: agent.provider.clone(),
        model: agent.model.clone(),
        effort: agent.effort.clone(),
        access: agent.access.clone(),
        avatar: agent.avatar.clone(),
        host: agent.host.clone(),
        budget: budgets::get(db, &agent.id)?,
        history,
        memory: Vec::new(),
    })
}

fn names(db: &Db) -> Result<HashMap<String, String>, String> {
    Ok(db.list_agents()?.into_iter().map(|a| (a.id, a.name)).collect())
}

fn spell_doc(workflow: &Workflow, names: &HashMap<String, String>) -> Result<SpellDoc, String> {
    let steps = workflow
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| match names.get(&s.agent_id) {
            Some(name) => Ok(StepDoc { familiar: name.clone(), prompt: s.prompt.clone(), parallel: s.parallel, check: s.check.clone() }),
            None => Err(format!("Step {} of {} has no familiar. Pick one before exporting.", i + 1, workflow.name)),
        })
        .collect::<Result<_, String>>()?;
    Ok(SpellDoc { name: workflow.name.clone(), steps })
}

fn coven_doc(db: &Db, coven: &crate::covens::Coven) -> Result<CovenDoc, String> {
    let members = db.list_agents()?.into_iter().filter(|a| a.coven_id.as_deref() == Some(&coven.id)).map(|a| a.name).collect();
    Ok(CovenDoc { name: coven.name.clone(), members, memory: Vec::new() })
}

/// A memory for a backup, with the familiars that saved entries named instead of given by id.
fn memory_doc(owner: Owner, names: &HashMap<String, String>) -> Result<Vec<Entry>, String> {
    Ok(memory::entries(owner)?.into_iter().map(|e| Entry { source: e.source.and_then(|id| names.get(&id).cloned()), ..e }).collect())
}

/// A theme as it would be written to the themes folder, without who made it, since that names a
/// familiar by id.
fn theme_doc(theme: &Value) -> Value {
    let mut theme = theme.clone();
    if let Some(object) = theme.as_object_mut() {
        object.remove("createdBy");
    }
    theme
}

/// One thing as a file, and the name to save it under.
pub fn export_one(db: &Db, themes_dir: &Path, kind: Kind, id: &str) -> Result<(GrimoireFile, String), String> {
    let mut file = GrimoireFile::new(kind);
    let name = match kind {
        Kind::Familiar => {
            let agent = db.get_agent(id)?.ok_or("That familiar no longer exists.")?;
            file.familiars.push(familiar_doc(db, &agent, false)?);
            agent.name
        }
        Kind::Spell => {
            let workflow = db.list_workflows()?.into_iter().find(|w| w.id == id).ok_or("That spell no longer exists.")?;
            file.spells.push(spell_doc(&workflow, &names(db)?)?);
            workflow.name
        }
        Kind::Coven => {
            let coven = crate::covens::list(db)?.into_iter().find(|c| c.id == id).ok_or("That coven no longer exists.")?;
            file.covens.push(coven_doc(db, &coven)?);
            coven.name
        }
        Kind::Theme => {
            let entry = crate::themes::list(db, themes_dir)?.themes.into_iter().find(|t| t.id == id).ok_or("That theme no longer exists.")?;
            let name = entry.theme["name"].as_str().unwrap_or("Theme").to_string();
            file.themes.push(theme_doc(&entry.theme));
            name
        }
        Kind::Backup => return Err("Use the backup export for everything.".into()),
    };
    Ok((file, file_name(&name)))
}

/// Everything but secrets: familiars, spells, rituals, covens, your own themes, and settings. Chat
/// history only when asked.
pub fn export_backup(db: &Db, themes_dir: &Path, history: bool) -> Result<GrimoireFile, String> {
    let mut file = GrimoireFile::new(Kind::Backup);
    let agents = db.list_agents()?;
    let names: HashMap<String, String> = agents.iter().map(|a| (a.id.clone(), a.name.clone())).collect();
    for agent in &agents {
        let mut doc = familiar_doc(db, agent, history)?;
        doc.memory = memory_doc(Owner { scope: Scope::Familiar, id: &agent.id }, &names)?;
        file.familiars.push(doc);
    }
    // A spell with a deleted familiar can't be rebuilt, so it stays behind.
    let workflows = db.list_workflows()?;
    file.spells = workflows.iter().filter_map(|w| spell_doc(w, &names).ok()).collect();
    let exported = |id: &str| file.spells.iter().any(|s| workflows.iter().any(|w| w.id == id && w.name == s.name));
    file.rituals = db
        .list_routines()?
        .into_iter()
        .filter_map(|r| {
            let (familiar, spell) = match (&r.agent_id, &r.workflow_id) {
                (Some(agent_id), _) => (Some(names.get(agent_id)?.clone()), None),
                // A ritual follows its spell into the backup, or stays behind with it.
                (None, Some(workflow_id)) if exported(workflow_id) => (None, workflows.iter().find(|w| &w.id == workflow_id).map(|w| w.name.clone())),
                _ => return None,
            };
            // A webhook's secret is its password, so it stays out of the file.
            let schedule = match r.schedule {
                Schedule::Webhook { .. } => Schedule::Webhook { secret: String::new() },
                other => other,
            };
            Some(RitualDoc { familiar, spell, name: r.name, prompt: r.prompt, schedule, enabled: r.enabled })
        })
        .collect();
    for coven in crate::covens::list(db)? {
        let mut doc = coven_doc(db, &coven)?;
        doc.memory = memory_doc(Owner { scope: Scope::Coven, id: &coven.id }, &names)?;
        file.covens.push(doc);
    }
    file.themes = crate::themes::list(db, themes_dir)?.themes.into_iter().filter(|t| !t.built_in).map(|t| theme_doc(&t.theme)).collect();
    if let Ok(root) = skills::root() {
        file.skills = skill_docs(db, root, &names)?;
    }
    file.settings = Some(SettingsDoc {
        claude_billing: db.setting(crate::CLAUDE_BILLING)?,
        dock_edge: db.setting(crate::DOCK_EDGE)?,
        permissions: Some(crate::permissions::load(db)),
        plan_guard: Some(budgets::plan_guard(db)),
    });
    Ok(file)
}

/// Every skill in the library, with familiars named instead of given by id.
fn skill_docs(db: &Db, root: &Path, names: &HashMap<String, String>) -> Result<Vec<SkillDoc>, String> {
    let links = skills::links(db)?;
    skills::rows(db)?
        .into_iter()
        .map(|r| {
            Ok(SkillDoc {
                files: skills::files_of(root, &r.name)?,
                familiars: links.iter().filter(|(s, _)| *s == r.name).filter_map(|(_, a)| names.get(a).cloned()).collect(),
                created_by: r.created_by.and_then(|id| names.get(&id).cloned()),
                name: r.name,
                source: r.source,
                origin: r.origin,
            })
        })
        .collect()
}

pub fn parse(text: &str) -> Result<GrimoireFile, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("This isn't a Grimoire file: {e}"))?;
    let version = value.get("version").and_then(Value::as_u64).ok_or("This isn't a Grimoire file: it has no version.")?;
    if version > u64::from(VERSION) {
        return Err("A newer Grimoire made this file. Update Grimoire to import it.".into());
    }
    serde_json::from_value(value).map_err(|e| format!("This Grimoire file is damaged: {e}"))
}

/// One line of the import preview.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewItem {
    /// "familiar", "spell", "ritual", "coven", "theme", "skill", or "settings".
    pub what: &'static str,
    /// The name in the file.
    pub name: String,
    /// The name it gets here, with "(2)" when the name is taken.
    pub final_name: String,
    /// Another item already has this name.
    pub conflict: bool,
    /// It can't be imported; the notes say why.
    pub skipped: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub kind: Kind,
    pub items: Vec<PreviewItem>,
    /// Chat messages that come with the familiars.
    pub history: usize,
    /// A familiar in the file asks for full access. It comes in workspace-only unless the person keeps it.
    pub full_access: bool,
}

struct NewFamiliar {
    agent: Agent,
    budget: Option<Budget>,
    history: Vec<HistoryDoc>,
    memory: Vec<Entry>,
}

/// What an import adds, worked out before anything is written.
pub struct Plan {
    pub preview: Preview,
    familiars: Vec<NewFamiliar>,
    spells: Vec<Workflow>,
    rituals: Vec<Routine>,
    /// Coven id, name, the familiar ids that sit in it, and its memory.
    covens: Vec<(String, String, Vec<String>, Vec<Entry>)>,
    themes: Vec<Value>,
    settings: Option<SettingsDoc>,
    skills: Vec<NewSkill>,
}

/// `name`, or "name (2)", "name (3)" and so on when `taken` already has it, ignoring case. The pick
/// joins `taken`.
fn unique(name: &str, taken: &mut Vec<String>, max: usize) -> String {
    let free = |candidate: &str, taken: &[String]| !taken.iter().any(|t| t.eq_ignore_ascii_case(candidate));
    let pick = (1..)
        .map(|n| {
            if n == 1 {
                return name.to_string();
            }
            let suffix = format!(" ({n})");
            let room = max.saturating_sub(suffix.chars().count());
            format!("{}{suffix}", name.chars().take(room).collect::<String>().trim_end())
        })
        .find(|candidate| free(candidate, taken))
        .unwrap();
    taken.push(pick.clone());
    pick
}

fn item(what: &'static str, name: &str, cleaned: &str, final_name: String) -> PreviewItem {
    PreviewItem { what, name: name.to_string(), conflict: final_name != cleaned, final_name, skipped: false, notes: Vec::new() }
}

/// `path` with the home folder written as ~.
fn shortened(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// "Brings 3 memory entries.", and how many didn't fit or weren't valid.
fn memory_note(line: &mut PreviewItem, kept: usize, dropped: usize) {
    if kept > 0 {
        line.notes.push(format!("Brings {kept} memory {}.", if kept == 1 { "entry" } else { "entries" }));
    }
    if dropped > 0 {
        line.notes.push(format!("Leaves out {dropped} memory {} that didn't fit or weren't valid.", if dropped == 1 { "entry" } else { "entries" }));
    }
}

fn skipped(what: &'static str, name: &str, why: String) -> PreviewItem {
    PreviewItem { what, name: name.to_string(), final_name: name.to_string(), conflict: false, skipped: true, notes: vec![why] }
}

fn valid_settings(settings: &SettingsDoc) -> Result<(), String> {
    if settings.claude_billing.as_deref().is_some_and(|b| !matches!(b, "subscription" | "api")) {
        return Err("Claude billing must be subscription or api.".into());
    }
    if settings.dock_edge.as_deref().is_some_and(|e| !matches!(e, "off" | "left" | "right")) {
        return Err("The dock goes on the left, the right, or off.".into());
    }
    if let Some(guard) = settings.plan_guard {
        if !(1..=100).contains(&guard.percent) {
            return Err("The plan limit guard needs a percentage from 1 to 100.".into());
        }
    }
    if let Some(p) = &settings.permissions {
        if p.rules.len() > 100 || p.rules.iter().any(|r| r.pattern.trim().is_empty() || r.pattern.chars().count() > 200) {
            return Err("The permission rules don't look right.".into());
        }
    }
    Ok(())
}

/// Works out an import: new ids for everything, "(2)" for taken names, and spell steps, rituals, and
/// coven members pointed at the familiars this file brings, or else at yours with the same name.
/// `keep_full_access` lets familiars that ask for full access keep it; otherwise they get workspace access.
pub fn plan(db: &Db, themes_dir: &Path, file: GrimoireFile, home: &Path, keep_full_access: bool) -> Result<Plan, String> {
    let existing = db.list_agents()?;
    let mut items = Vec::new();
    let now = now_ms();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let full_access = file.familiars.iter().any(|f| f.access == Access::Full);
    // Names this file gives to more than one familiar, which a skill's owner or assignment can't name safely.
    let mut seen_names = std::collections::HashSet::new();
    let shared_names: std::collections::HashSet<String> = file.familiars.iter().map(|f| f.name.trim().to_lowercase()).filter(|n| !seen_names.insert(n.clone())).collect();
    let ambiguous = |name: &str| shared_names.contains(&name.trim().to_lowercase());
    // Familiars first, so everything else can point at them.
    let mut taken: Vec<String> = existing.iter().map(|a| a.name.clone()).collect();
    let mut familiars = Vec::new();
    let mut imported: HashMap<String, (String, String)> = HashMap::new();
    for doc in file.familiars {
        let checked = crate::clean_name(&doc.name)
            .and_then(|name| doc.avatar.validate().map(|_| name))
            .and_then(|name| crate::clean_effort(doc.effort.clone()).map(|effort| (name, effort)))
            .and_then(|pair| doc.budget.as_ref().map_or(Ok(()), Budget::validate).map(|_| pair))
            .and_then(|(name, effort)| crate::remote::clean_host(doc.host.as_deref()).map(|host| (name, effort, host)));
        let (name, effort, host) = match checked {
            Ok(checked) => checked,
            Err(why) => {
                items.push(skipped("familiar", &doc.name, why));
                continue;
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        let final_name = unique(&name, &mut taken, 60);
        let mut line = item("familiar", &doc.name, &name, final_name.clone());
        if line.conflict {
            line.notes.push(format!("You already have a familiar named {name}, so this one becomes {final_name}."));
        }
        let workspace = match &host {
            Some(host) => {
                let workspace = crate::remote::clean_workspace(None, &final_name, &id)?;
                line.notes.push(format!("Runs on {host} over SSH, in {workspace} there, without Grimoire's tools."));
                workspace
            }
            None => {
                let workspace = crate::workspace_for(None, &final_name, &id, home)?;
                line.notes.push(format!("Gets a new workspace folder in {}.", shortened(workspace.parent().unwrap_or(home), home)));
                workspace.to_string_lossy().into_owned()
            }
        };
        if !doc.history.is_empty() {
            line.notes.push(format!("Brings {} chat messages.", doc.history.len()));
        }
        let access = match doc.access {
            Access::Full if keep_full_access => {
                line.notes.push("Keeps full access: it can run any command on this Mac, outside its folder and without a sandbox.".into());
                Access::Full
            }
            Access::Full => {
                line.notes.push("Asks for full access, which runs any command without a sandbox. It comes in with workspace access unless you keep full access.".into());
                Access::Workspace
            }
            other => other,
        };
        let (memory, dropped) = memory::imported(doc.memory, &today);
        memory_note(&mut line, memory.len(), dropped);
        items.push(line);
        imported.entry(name.to_lowercase()).or_insert((id.clone(), final_name.clone()));
        let history = doc.history.into_iter().filter(|m| HISTORY_ROLES.contains(&m.role.as_str())).collect();
        familiars.push(NewFamiliar {
            agent: Agent {
                id,
                name: final_name,
                provider: doc.provider,
                model: crate::clean_model(doc.model),
                workspace,
                access,
                session_id: None,
                avatar: doc.avatar,
                effort,
                active_model: None,
                coven_id: None,
                host,
                created_at: now,
            },
            budget: doc.budget,
            history,
            memory,
        });
    }
    // A familiar this file brings wins over one of yours with the same name.
    let resolve = |name: &str| -> Option<(String, String, bool)> {
        let key = name.trim().to_lowercase();
        if let Some((id, final_name)) = imported.get(&key) {
            return Some((id.clone(), final_name.clone(), true));
        }
        existing.iter().find(|a| a.name.to_lowercase() == key).map(|a| (a.id.clone(), a.name.clone(), false))
    };
    // Entries name the familiar that saved them; one that isn't here counts as the user's.
    let source = |entries: Vec<Entry>| -> Vec<Entry> {
        entries.into_iter().map(|e| Entry { source: e.source.as_deref().and_then(&resolve).map(|(id, _, _)| id), ..e }).collect()
    };
    for new in &mut familiars {
        new.memory = source(std::mem::take(&mut new.memory));
    }
    let where_to = |final_name: &str, is_new: bool| if is_new { format!("the imported {final_name}") } else { format!("your {final_name}") };

    let mut taken: Vec<String> = db.list_workflows()?.into_iter().map(|w| w.name).collect();
    let mut spells = Vec::new();
    let mut imported_spells: Vec<(String, String, String)> = Vec::new();
    for doc in file.spells {
        let mut steps = Vec::new();
        let mut notes = Vec::new();
        let mut missing = None;
        for (i, step) in doc.steps.iter().enumerate() {
            match resolve(&step.familiar) {
                Some((id, final_name, is_new)) => {
                    notes.push(format!("Step {} goes to {}.", i + 1, where_to(&final_name, is_new)));
                    steps.push(WorkflowStep { agent_id: id, prompt: step.prompt.trim().to_string(), parallel: step.parallel, check: step.check.clone() });
                }
                None => {
                    missing = Some(format!("Step {} needs a familiar named {}, and there isn't one.", i + 1, step.familiar));
                    break;
                }
            }
        }
        let name = doc.name.trim().to_string();
        let workflow = Workflow { id: uuid::Uuid::new_v4().to_string(), name: name.clone(), steps, created_at: now, created_by: None };
        if let Some(why) = missing.or_else(|| crate::workflows::validate(&workflow, |_| true).err()) {
            items.push(skipped("spell", &doc.name, why));
            continue;
        }
        let final_name = unique(&name, &mut taken, 60);
        let mut line = item("spell", &doc.name, &name, final_name.clone());
        if line.conflict {
            line.notes.push(format!("You already have a spell named {name}, so this one becomes {final_name}."));
        }
        line.notes.extend(notes);
        items.push(line);
        imported_spells.push((doc.name.trim().to_string(), workflow.id.clone(), final_name.clone()));
        spells.push(Workflow { name: final_name, ..workflow });
    }

    let existing_spells = db.list_workflows()?;
    let mut rituals = Vec::new();
    for doc in file.rituals {
        // A webhook ritual gets a new secret here; backups never carry one.
        let checked = crate::clean_name(&doc.name).and_then(|name| crate::routines::prepare(doc.schedule.clone(), None).map(|s| (name, s))).and_then(|ok| {
            if doc.prompt.trim().is_empty() { Err("It has no prompt.".to_string()) } else { Ok(ok) }
        });
        let (name, schedule) = match checked {
            Ok(ok) => ok,
            Err(why) => {
                items.push(skipped("ritual", &doc.name, why));
                continue;
            }
        };
        // A ritual messages a familiar, or casts a spell from this file or one you already have.
        let (agent_id, workflow_id, target) = match (&doc.familiar, &doc.spell) {
            (Some(familiar), _) => match resolve(familiar) {
                Some((agent_id, final_familiar, is_new)) => (Some(agent_id), None, format!("Runs for {}", where_to(&final_familiar, is_new))),
                None => {
                    items.push(skipped("ritual", &doc.name, format!("It needs a familiar named {familiar}, and there isn't one.")));
                    continue;
                }
            },
            (None, Some(spell)) => {
                let found = imported_spells
                    .iter()
                    .find(|(original, _, _)| original == spell.trim())
                    .map(|(_, id, final_name)| (id.clone(), format!("Casts the imported {final_name}")))
                    .or_else(|| existing_spells.iter().find(|w| w.name == spell.trim()).map(|w| (w.id.clone(), format!("Casts your {}", w.name))));
                match found {
                    Some((id, target)) => (None, Some(id), target),
                    None => {
                        items.push(skipped("ritual", &doc.name, format!("It casts a spell named {spell}, and there isn't one.")));
                        continue;
                    }
                }
            }
            (None, None) => {
                items.push(skipped("ritual", &doc.name, "It has no familiar or spell.".to_string()));
                continue;
            }
        };
        let mut line = item("ritual", &doc.name, &name, name.clone());
        line.notes.push(format!("{target}{}.", if doc.enabled { "" } else { ", paused" }));
        items.push(line);
        rituals.push(Routine {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id,
            workflow_id,
            name,
            prompt: doc.prompt.trim().to_string(),
            next_run_at: if doc.enabled { crate::routines::next_run_ms(&schedule, now) } else { None },
            schedule,
            enabled: doc.enabled,
            last_run_at: None,
            created_at: now,
        });
    }

    let mut taken: Vec<String> = crate::covens::list(db)?.into_iter().map(|c| c.name).collect();
    let mut covens = Vec::new();
    for doc in file.covens {
        let name = doc.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 40 {
            items.push(skipped("coven", &doc.name, "A coven needs a name of 1 to 40 characters.".into()));
            continue;
        }
        let final_name = unique(&name, &mut taken, 40);
        let mut line = item("coven", &doc.name, &name, final_name.clone());
        if line.conflict {
            line.notes.push(format!("You already have a coven named {name}, so this one becomes {final_name}."));
        }
        let mut members = Vec::new();
        for member in &doc.members {
            match resolve(member) {
                Some((id, final_member, is_new)) => {
                    if !is_new {
                        line.notes.push(format!("Your {final_member} moves into it."));
                    }
                    members.push(id);
                }
                None => line.notes.push(format!("No familiar named {member}, so it's left out.")),
            }
        }
        let (memory, dropped) = memory::imported(doc.memory, &today);
        memory_note(&mut line, memory.len(), dropped);
        items.push(line);
        covens.push((uuid::Uuid::new_v4().to_string(), final_name, members, source(memory)));
    }

    let mut taken: Vec<String> = crate::themes::list(db, themes_dir)?.themes.iter().filter_map(|t| t.theme["name"].as_str().map(String::from)).collect();
    let mut themes = Vec::new();
    for doc in file.themes {
        let shown = doc.get("name").and_then(Value::as_str).unwrap_or("A theme").to_string();
        match crate::themes::validate(&theme_doc(&doc)) {
            Ok(mut theme) => {
                let name = theme["name"].as_str().unwrap_or_default().to_string();
                let final_name = unique(&name, &mut taken, 60);
                let mut line = item("theme", &shown, &name, final_name.clone());
                if line.conflict {
                    line.notes.push(format!("A theme named {name} already exists, so this one becomes {final_name}."));
                }
                theme["name"] = Value::String(final_name);
                items.push(line);
                themes.push(theme);
            }
            Err(why) => items.push(skipped("theme", &shown, why)),
        }
    }

    // A skill is someone's instructions, so it comes in on only for the familiars this file brings.
    let mut skills = Vec::new();
    let mut taken = match skills::root() {
        Ok(root) => skills::taken_names(db, root)?,
        Err(_) => Vec::new(),
    };
    for doc in file.skills {
        if skills::root().is_err() {
            items.push(skipped("skill", &doc.name, "Skills aren't set up yet.".into()));
            continue;
        }
        if !matches!(doc.source.as_str(), "you" | "folder" | "url" | "familiar" | "template") {
            items.push(skipped("skill", &doc.name, format!("Grimoire doesn't know where a skill from \"{}\" comes from.", doc.source)));
            continue;
        }
        let mut files = doc.files;
        let front = match skills::check_files(&mut files) {
            Ok(front) => front,
            Err(why) => {
                items.push(skipped("skill", &doc.name, why));
                continue;
            }
        };
        let final_name = skills::unique_name(&front.name, &mut taken);
        let mut line = item("skill", &doc.name, &front.name, final_name.clone());
        if line.conflict {
            line.notes.push(format!("You already have a skill named {}, so this one becomes {final_name}.", front.name));
            if let Err(why) = skills::rename_main(&mut files, &final_name) {
                items.push(skipped("skill", &doc.name, why));
                continue;
            }
        }
        let mut agents = Vec::new();
        for familiar in &doc.familiars {
            if ambiguous(familiar) {
                line.notes.push(format!("Off for every {familiar}: more than one familiar in the file has that name."));
                continue;
            }
            match resolve(familiar) {
                Some((id, final_familiar, true)) => {
                    line.notes.push(format!("On for the imported {final_familiar}."));
                    agents.push(id);
                }
                Some((_, final_familiar, false)) => line.notes.push(format!("Off for your {final_familiar}. Turn it on in its settings if you want it.")),
                None => line.notes.push(format!("Was on for {familiar}, who isn't here.")),
            }
        }
        if agents.is_empty() && doc.familiars.is_empty() {
            line.notes.push("Comes in turned off for every familiar.".into());
        }
        items.push(line);
        skills.push(NewSkill {
            name: final_name,
            files,
            source: doc.source,
            origin: doc.origin,
            created_by: doc.created_by.as_deref().filter(|name| !ambiguous(name)).and_then(&resolve).map(|(id, _, _)| id),
            agents,
        });
    }

    let settings = match file.settings {
        Some(settings) => match valid_settings(&settings) {
            Ok(()) => {
                let mut line = item("settings", "Settings", "Settings", "Settings".into());
                line.notes.push("Replaces Claude billing, the dock, permission rules, and the plan limit guard.".into());
                items.push(line);
                Some(settings)
            }
            Err(why) => {
                items.push(skipped("settings", "Settings", why));
                None
            }
        },
        None => None,
    };

    let history = familiars.iter().map(|f| f.history.len()).sum();
    Ok(Plan { preview: Preview { kind: file.kind, items, history, full_access }, familiars, spells, rituals, covens, themes, settings, skills })
}

/// What an import added.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Imported {
    pub familiars: usize,
    pub spells: usize,
    pub rituals: usize,
    pub covens: usize,
    pub themes: usize,
    pub settings: bool,
    pub skills: usize,
}

/// Enum columns hold their serde names, like "codex" and "workspace".
fn column<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

fn write_rows(c: &rusqlite::Connection, plan: &Plan) -> rusqlite::Result<()> {
    for (id, name, _, _) in &plan.covens {
        c.execute("INSERT INTO covens (id, name, created_at) VALUES (?1, ?2, ?3)", params![id, name, now_ms()])?;
    }
    for new in &plan.familiars {
        let a = &new.agent;
        c.execute(
            "INSERT INTO agents (id, name, provider, model, workspace, access, session_id, created_at, avatar, effort, coven_id, host)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, NULL, ?10)",
            params![a.id, a.name, column(&a.provider), a.model, a.workspace, column(&a.access), a.created_at, serde_json::to_string(&a.avatar).unwrap_or_default(), a.effort, a.host],
        )?;
        if let Some(budget) = &new.budget {
            budgets::insert_on(c, &a.id, budget)?;
        }
        // Imported history lands in the familiar's first chat, titled by its first message.
        let chat_id = crate::chats::ensure_current(c, &a.id)?;
        for m in &new.history {
            c.execute(
                "INSERT INTO messages (agent_id, chat_id, role, text, status, created_at, source) VALUES (?1, ?2, ?3, ?4, 'done', ?5, ?6)",
                params![a.id, chat_id, m.role, m.text, m.created_at, m.source],
            )?;
            crate::chats::touch(c, &chat_id, &m.role, &m.text, m.created_at)?;
        }
    }
    for (id, _, members, _) in &plan.covens {
        for member in members {
            c.execute("UPDATE agents SET coven_id = ?2 WHERE id = ?1", params![member, id])?;
        }
    }
    for w in &plan.spells {
        let steps = serde_json::to_string(&w.steps).unwrap_or_default();
        c.execute("INSERT INTO workflows (id, name, steps, created_at, created_by) VALUES (?1, ?2, ?3, ?4, NULL)", params![w.id, w.name, steps, w.created_at])?;
    }
    for r in &plan.rituals {
        c.execute(
            "INSERT INTO routines (id, agent_id, workflow_id, name, prompt, schedule, enabled, last_run_at, next_run_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9)",
            params![r.id, r.agent_id, r.workflow_id, r.name, r.prompt, serde_json::to_string(&r.schedule).unwrap_or_default(), r.enabled, r.next_run_at, r.created_at],
        )?;
    }
    for skill in &plan.skills {
        skills::insert_rows(c, skill)?;
    }
    if let Some(s) = &plan.settings {
        let set = |key: &str, value: String| {
            c.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2", params![key, value]).map(|_| ())
        };
        if let Some(billing) = &s.claude_billing {
            set(crate::CLAUDE_BILLING, billing.clone())?;
        }
        if let Some(edge) = &s.dock_edge {
            set(crate::DOCK_EDGE, edge.clone())?;
        }
        if let Some(p) = &s.permissions {
            set("permissions", serde_json::to_string(p).unwrap_or_default())?;
        }
        if let Some(guard) = &s.plan_guard {
            set("plan_guard", serde_json::to_string(guard).unwrap_or_default())?;
        }
    }
    Ok(())
}

/// Writes a plan: theme files, memory files, and workspace folders first, then every row in one
/// transaction. If the transaction fails, the files and empty folders it made are removed again.
pub fn apply(db: &Db, themes_dir: &Path, plan: &Plan) -> Result<Imported, String> {
    let mut written: Vec<PathBuf> = Vec::new();
    let mut folders: Vec<PathBuf> = Vec::new();
    let mut skill_folders: Vec<PathBuf> = Vec::new();
    let undo = |written: &[PathBuf], folders: &[PathBuf], skill_folders: &[PathBuf]| {
        written.iter().for_each(|p| drop(std::fs::remove_file(p)));
        folders.iter().for_each(|p| drop(std::fs::remove_dir(p)));
        skill_folders.iter().for_each(|p| drop(std::fs::remove_dir_all(p)));
    };
    let mut prepare = || -> Result<(), String> {
        for theme in &plan.themes {
            let saved = crate::themes::save(themes_dir, None, theme, None)?;
            written.push(themes_dir.join(format!("{}.json", saved.id)));
        }
        let memories = plan.familiars.iter().map(|f| (Owner { scope: Scope::Familiar, id: &f.agent.id }, &f.memory));
        for (owner, entries) in memories.chain(plan.covens.iter().map(|(id, _, _, m)| (Owner { scope: Scope::Coven, id }, m))) {
            if !entries.is_empty() {
                written.push(memory::write_imported(owner, entries)?);
            }
        }
        skill_folders = skills::write_imported(&plan.skills)?;
        // A familiar on another Mac makes its workspace there when it first starts.
        for new in plan.familiars.iter().filter(|f| f.agent.host.is_none()) {
            let path = PathBuf::from(&new.agent.workspace);
            if !path.exists() {
                std::fs::create_dir_all(&path).map_err(|e| format!("Could not create {}: {e}", path.display()))?;
                folders.push(path);
            }
        }
        Ok(())
    };
    let result = prepare().and_then(|_| {
        db.with(|c| {
            let tx = c.unchecked_transaction()?;
            write_rows(&tx, plan)?;
            tx.commit()
        })
    });
    if let Err(e) = result {
        undo(&written, &folders, &skill_folders);
        return Err(e);
    }
    if !plan.skills.is_empty() {
        skills::sync(db);
    }
    Ok(Imported {
        familiars: plan.familiars.len(),
        spells: plan.spells.len(),
        rituals: plan.rituals.len(),
        covens: plan.covens.len(),
        themes: plan.themes.len(),
        settings: plan.settings.is_some(),
        skills: plan.skills.len(),
    })
}

fn read(path: &Path) -> Result<GrimoireFile, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?.len();
    if size > MAX_FILE {
        return Err("That file is too big to be a Grimoire file.".into());
    }
    parse(&std::fs::read_to_string(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?)
}

fn write(path: &Path, file: &GrimoireFile) -> Result<(), String> {
    let text = serde_json::to_string_pretty(file).map_err(|e| e.to_string())? + "\n";
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text).map_err(|e| format!("Could not save {}: {e}", path.display()))?;
    std::fs::rename(&temp, path).map_err(|e| format!("Could not save {}: {e}", path.display()))
}

async fn save_dialog(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().set_file_name(name).add_filter("Grimoire file", &["json"]).save_file(move |path| drop(tx.send(path)));
    rx.await.ok().flatten().and_then(|p| p.into_path().ok())
}

/// The file the person previewed, as read then, so applying it can't pick up a changed or different file.
static PICKED: Mutex<Option<GrimoireFile>> = Mutex::new(None);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Picked {
    file: String,
    preview: Preview,
}

/// Exports one familiar, spell, coven, or theme. Returns where it went, or nothing when cancelled.
#[tauri::command]
pub(crate) async fn export_item(app: AppHandle, state: State<'_, crate::AppState>, kind: Kind, id: String) -> Result<Option<String>, String> {
    crate::link::local_only(&id)?;
    let (file, name) = export_one(&state.db, &state.themes_dir, kind, &id)?;
    let Some(path) = save_dialog(&app, &name).await else { return Ok(None) };
    write(&path, &file)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub(crate) async fn export_backup_file(app: AppHandle, state: State<'_, crate::AppState>, include_history: bool) -> Result<Option<String>, String> {
    let file = export_backup(&state.db, &state.themes_dir, include_history)?;
    let name = file_name(&format!("Grimoire backup {}", chrono::Local::now().format("%Y-%m-%d")));
    let Some(path) = save_dialog(&app, &name).await else { return Ok(None) };
    write(&path, &file)?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Asks for a file and shows what importing it would do. Nothing changes yet.
#[tauri::command]
pub(crate) async fn pick_import(app: AppHandle, state: State<'_, crate::AppState>) -> Result<Option<Picked>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().add_filter("Grimoire file", &["json"]).pick_file(move |path| drop(tx.send(path)));
    let Some(path) = rx.await.ok().flatten().and_then(|p| p.into_path().ok()) else { return Ok(None) };
    let picked = read(&path)?;
    let plan = plan(&state.db, &state.themes_dir, picked.clone(), &crate::home()?, false)?;
    let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    *PICKED.lock().map_err(|_| "import state poisoned")? = Some(picked);
    Ok(Some(Picked { file, preview: plan.preview }))
}

/// Imports the previewed file. The plan is worked out again, so changes to your Grimoire since the preview count.
#[tauri::command]
pub(crate) async fn apply_import(app: AppHandle, state: State<'_, crate::AppState>, keep_full_access: bool) -> Result<Imported, String> {
    let picked = PICKED.lock().map_err(|_| "import state poisoned")?.take().ok_or("Pick a file to import first.")?;
    let plan = plan(&state.db, &state.themes_dir, picked, &crate::home()?, keep_full_access)?;
    let done = apply(&state.db, &state.themes_dir, &plan)?;
    if done.settings {
        // Settings the app holds in memory follow the database again.
        let billing = state.db.setting(crate::CLAUDE_BILLING)?;
        crate::providers::USE_CLAUDE_SUBSCRIPTION.store(billing.as_deref() != Some("api"), std::sync::atomic::Ordering::Relaxed);
        state.runtime.claude_billing_changed();
        let edge = state.db.setting(crate::DOCK_EDGE)?;
        state.dock.set_edge(&app, edge.as_deref().and_then(crate::dock::Edge::parse))?;
    }
    state.scheduler.changed();
    for event in ["agents_changed", "workflows_changed", "covens_changed", "themes_changed", "routines_changed", "budgets_changed", "memory_changed", skills::CHANGED] {
        let _ = app.emit(event, ());
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budgets::Period;
    use serde_json::json;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("grimoire-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn familiar(db: &Db, id: &str, name: &str) -> Agent {
        let agent = Agent {
            id: id.into(),
            name: name.into(),
            provider: Provider::Claude,
            model: Some("claude-opus-5-5".into()),
            workspace: format!("/private/work/{id}"),
            access: Access::Full,
            session_id: Some("secret-session".into()),
            avatar: Avatar { shape: "cloud".into(), eyes: "dot".into(), color: "#8e5bd6".into() },
            effort: Some("high".into()),
            active_model: None,
            coven_id: None,
            host: None,
            created_at: 1,
        };
        db.insert_agent(&agent).unwrap();
        agent
    }

    /// A Grimoire with two familiars, a spell between them, a ritual, a coven, a theme, a vault
    /// item, and some chat.
    fn stocked(themes: &Path) -> Db {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "r", "Researcher");
        familiar(&db, "s", "Scribe");
        budgets::set(&db, "r", Some(&Budget { tokens: 50_000, period: Period::Day })).unwrap();
        crate::workflows::save(
            &db,
            None,
            "Topic brief",
            vec![WorkflowStep { agent_id: "r".into(), prompt: "Research {{input}}".into(), ..Default::default() }, WorkflowStep { agent_id: "s".into(), prompt: "Write it up".into(), ..Default::default() }],
            None,
        )
        .unwrap();
        db.save_routine(&Routine {
            id: "rit".into(),
            agent_id: Some("r".into()),
            workflow_id: None,
            name: "Morning".into(),
            prompt: "Summarize the news".into(),
            schedule: Schedule::Daily { time: "09:00".into(), days: vec![] },
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            created_at: 1,
        })
        .unwrap();
        crate::covens::save(&db, None, "Research", &["r".into(), "s".into()]).unwrap();
        crate::themes::save(themes, None, &json!({ "name": "Rainy library", "colors": { "canvas": "#14181c", "accent": "#8fb3c9" } }), Some("r".into())).unwrap();
        db.insert_message("r", "user", "What's new?", "done").unwrap();
        db.insert_message("r", "assistant", "Solid 2 shipped.", "done").unwrap();
        let store = crate::vault::MemoryStore::new();
        let vault = crate::vault::save(
            &db,
            &store,
            serde_json::from_value(json!({ "id": null, "kind": "login", "name": "Bank login", "site": "bank.example", "username": "nik@bank", "secret": { "password": "hunter2-very-secret" } })).unwrap(),
        )
        .unwrap();
        db.add_grant("r", &crate::vault::scope(&vault.id)).unwrap();
        db.set_setting(crate::CLAUDE_BILLING, "api").unwrap();
        db.set_setting(crate::runtime::CLAUDE_LIMITS, r#"{"info":{"secretish":"limits"},"at":1}"#).unwrap();
        db
    }

    #[test]
    fn backups_never_carry_secrets_or_private_state() {
        let themes = temp_dir("themes");
        let db = stocked(&themes);
        for history in [false, true] {
            let text = serde_json::to_string(&export_backup(&db, &themes, history).unwrap()).unwrap();
            for private in ["Bank login", "bank.example", "nik@bank", "hunter2", "vault", "secret-session", "/private/work", "secretish", "createdBy"] {
                assert!(!text.contains(private), "backup leaked {private}: {text}");
            }
            assert_eq!(text.contains("Solid 2 shipped."), history, "chat only when asked");
        }
        let (one, name) = export_one(&db, &themes, Kind::Familiar, "r").unwrap();
        assert_eq!(name, "Researcher.grimoire.json");
        assert!(one.familiars[0].history.is_empty() && one.spells.is_empty());
        std::fs::remove_dir_all(themes).unwrap();
    }

    #[test]
    fn spell_rituals_and_step_options_survive_a_round_trip() {
        let themes = temp_dir("themes");
        let home = temp_dir("home");
        let db = stocked(&themes);
        let spell = db.list_workflows().unwrap().remove(0);
        let mut steps = spell.steps.clone();
        steps[1].parallel = true;
        crate::workflows::save(&db, Some(&spell.id), &spell.name, steps, None).unwrap();
        db.save_routine(&Routine {
            id: "cast".into(),
            agent_id: None,
            workflow_id: Some(spell.id.clone()),
            name: "Weekly brief".into(),
            prompt: "AI news".into(),
            schedule: Schedule::Daily { time: "08:00".into(), days: vec![1] },
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            created_at: 1,
        })
        .unwrap();
        let text = serde_json::to_string(&export_backup(&db, &themes, false).unwrap()).unwrap();
        let plan = plan(&db, &themes, parse(&text).unwrap(), &home, false).unwrap();
        let line = plan.preview.items.iter().find(|i| i.what == "ritual" && i.name == "Weekly brief").unwrap();
        assert_eq!(line.notes, ["Casts the imported Topic brief (2)."]);
        apply(&db, &themes, &plan).unwrap();
        let copy = db.list_workflows().unwrap().into_iter().find(|w| w.name == "Topic brief (2)").unwrap();
        assert!(copy.steps[1].parallel, "step options come along");
        let ritual = db.list_routines().unwrap().into_iter().find(|r| r.name == "Weekly brief" && r.id != "cast").unwrap();
        assert_eq!((ritual.agent_id, ritual.workflow_id.as_deref()), (None, Some(copy.id.as_str())));
        std::fs::remove_dir_all(themes).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn round_trip_renames_conflicts_and_remaps_spells() {
        let themes = temp_dir("themes");
        let home = temp_dir("home");
        let db = stocked(&themes);
        let text = serde_json::to_string_pretty(&export_backup(&db, &themes, true).unwrap()).unwrap();

        // Import into the same Grimoire: every name is taken.
        let plan = plan(&db, &themes, parse(&text).unwrap(), &home, false).unwrap();
        let line = |what: &str, name: &str| plan.preview.items.iter().find(|i| i.what == what && i.name == name).unwrap().clone();
        assert_eq!(line("familiar", "Researcher").final_name, "Researcher (2)");
        assert!(line("familiar", "Researcher").conflict);
        assert_eq!(line("spell", "Topic brief").final_name, "Topic brief (2)");
        assert_eq!(line("spell", "Topic brief").notes[1..], ["Step 1 goes to the imported Researcher (2).", "Step 2 goes to the imported Scribe (2)."]);
        assert_eq!(line("coven", "Research").final_name, "Research (2)");
        assert_eq!(line("theme", "Rainy library").final_name, "Rainy library (2)");
        assert_eq!(line("ritual", "Morning").notes, ["Runs for the imported Researcher (2)."]);
        assert_eq!(plan.preview.history, 2);
        assert!(plan.preview.items.iter().all(|i| !i.skipped));
        assert!(plan.preview.full_access);
        assert!(line("familiar", "Researcher").notes.iter().any(|n| n.contains("comes in with workspace access")));

        let done = apply(&db, &themes, &plan).unwrap();
        assert_eq!((done.familiars, done.spells, done.rituals, done.covens, done.themes, done.settings), (2, 1, 1, 1, 1, true));

        let agents = db.list_agents().unwrap();
        let copy = agents.iter().find(|a| a.name == "Researcher (2)").unwrap();
        let scribe = agents.iter().find(|a| a.name == "Scribe (2)").unwrap();
        assert_ne!(copy.id, "r");
        assert_eq!(copy.session_id, None);
        assert!(copy.workspace.starts_with(&home.to_string_lossy().into_owned()));
        assert!(Path::new(&copy.workspace).is_dir());
        assert_eq!((copy.model.as_deref(), copy.effort.as_deref(), &copy.access), (Some("claude-opus-5-5"), Some("high"), &Access::Workspace));
        assert_eq!(budgets::get(&db, &copy.id).unwrap(), Some(Budget { tokens: 50_000, period: Period::Day }));
        assert_eq!(db.list_messages(&copy.id, None, 10).unwrap().len(), 2);

        let spell = db.list_workflows().unwrap().into_iter().find(|w| w.name == "Topic brief (2)").unwrap();
        assert_eq!(spell.steps.iter().map(|s| s.agent_id.as_str()).collect::<Vec<_>>(), [copy.id.as_str(), scribe.id.as_str()]);
        let ritual = db.list_routines().unwrap().into_iter().find(|r| r.agent_id.as_deref() == Some(copy.id.as_str())).unwrap();
        assert!(ritual.enabled && ritual.next_run_at.is_some());
        let coven = crate::covens::list(&db).unwrap().into_iter().find(|c| c.name == "Research (2)").unwrap();
        assert_eq!(copy.coven_id.as_deref(), Some(coven.id.as_str()));
        assert_eq!(db.get_agent("r").unwrap().unwrap().name, "Researcher", "the originals stay as they were");
        assert!(crate::themes::list(&db, &themes).unwrap().themes.iter().any(|t| t.theme["name"] == "Rainy library (2)"));
        assert!(crate::vault::list(&db).unwrap().len() == 1, "the vault is untouched");

        std::fs::remove_dir_all(themes).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn full_access_comes_in_only_when_kept() {
        let themes = temp_dir("themes");
        let home = temp_dir("home");
        let db = stocked(&themes);
        let text = serde_json::to_string_pretty(&export_one(&db, &themes, Kind::Familiar, "r").unwrap().0).unwrap();
        let access = |keep: bool| {
            let plan = plan(&db, &themes, parse(&text).unwrap(), &home, keep).unwrap();
            assert!(plan.preview.full_access);
            plan.familiars[0].agent.access.clone()
        };
        assert_eq!(access(false), Access::Workspace);
        assert_eq!(access(true), Access::Full);
        std::fs::remove_dir_all(themes).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn remote_familiars_keep_their_host_and_make_no_local_folder() {
        let themes = temp_dir("themes");
        let home = temp_dir("home");
        let db = Db::open_in_memory().unwrap();
        let mut studio = familiar(&db, "st", "Studio");
        studio.host = Some("me@studio.local".into());
        db.delete_agent("st").unwrap();
        db.insert_agent(&studio).unwrap();
        let (file, _) = export_one(&db, &themes, Kind::Familiar, "st").unwrap();
        let text = serde_json::to_string(&file).unwrap();
        assert!(text.contains(r#""host":"me@studio.local""#), "{text}");

        let mut file = parse(&text).unwrap();
        let mut bad = file.familiars[0].clone();
        bad.name = "Sneaky".into();
        bad.host = Some("-oProxyCommand=sh".into());
        file.familiars.push(bad);
        let p = plan(&db, &themes, file, &home, false).unwrap();
        assert!(p.preview.items.iter().find(|i| i.name == "Sneaky").unwrap().skipped, "a bad host is refused");
        assert_eq!(p.familiars.len(), 1);
        apply(&db, &themes, &p).unwrap();
        let copy = db.list_agents().unwrap().into_iter().find(|a| a.name == "Studio (2)").unwrap();
        assert_eq!(copy.host.as_deref(), Some("me@studio.local"));
        assert!(copy.workspace.starts_with("~/Grimoire/studio-"), "{}", copy.workspace);
        assert!(!home.join("Grimoire").exists(), "no folder on this Mac");
        std::fs::remove_dir_all(themes).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn single_spells_use_your_familiars_by_name() {
        let themes = temp_dir("themes");
        let home = temp_dir("home");
        let source = stocked(&themes);
        let spell = source.list_workflows().unwrap()[0].id.clone();
        let (file, name) = export_one(&source, &themes, Kind::Spell, &spell).unwrap();
        assert_eq!(name, "Topic brief.grimoire.json");
        let text = serde_json::to_string(&file).unwrap();
        assert!(text.contains(r#""familiar":"Researcher""#), "{text}");

        // Another Grimoire with only a Researcher can't place step 2.
        let other = Db::open_in_memory().unwrap();
        familiar(&other, "x", "researcher");
        let p = plan(&other, &themes, parse(&text).unwrap(), &home, false).unwrap();
        assert!(p.preview.items[0].skipped);
        assert_eq!(p.preview.items[0].notes, ["Step 2 needs a familiar named Scribe, and there isn't one."]);

        familiar(&other, "y", "Scribe");
        let p = plan(&other, &themes, parse(&text).unwrap(), &home, false).unwrap();
        assert_eq!(p.preview.items[0].notes, ["Step 1 goes to your researcher.", "Step 2 goes to your Scribe."]);
        apply(&other, &themes, &p).unwrap();
        let steps = &other.list_workflows().unwrap()[0].steps;
        assert_eq!((steps[0].agent_id.as_str(), steps[1].agent_id.as_str()), ("x", "y"));
        std::fs::remove_dir_all(themes).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn rejects_files_it_cannot_read_and_rolls_back() {
        assert!(parse("{").unwrap_err().contains("isn't a Grimoire file"));
        assert!(parse(r#"{"kind":"spell"}"#).unwrap_err().contains("no version"));
        assert!(parse(r#"{"kind":"spell","version":9}"#).unwrap_err().contains("newer Grimoire"));
        assert!(parse(r#"{"kind":"potion","version":1}"#).unwrap_err().contains("damaged"));
        assert_eq!(file_name("a/b: c"), "a-b- c.grimoire.json");

        let mut taken = vec!["Ops".to_string(), "ops (2)".to_string()];
        assert_eq!(unique("OPS", &mut taken, 40), "OPS (3)");
        assert_eq!(unique(&"x".repeat(40), &mut vec!["x".repeat(40)], 40).chars().count(), 40);

        // A row that fails mid-import leaves nothing behind.
        let themes = temp_dir("themes");
        let home = temp_dir("home");
        let db = Db::open_in_memory().unwrap();
        let mut file = GrimoireFile::new(Kind::Backup);
        file.familiars.push(FamiliarDoc {
            name: "Ada".into(),
            provider: Provider::Codex,
            model: None,
            effort: None,
            access: Access::Workspace,
            avatar: Avatar::for_seed("ada"),
            host: None,
            budget: None,
            history: vec![],
            memory: memory::from_markdown("- Likes tea.", "2026-11-23"),
        });
        file.themes.push(json!({ "name": "Ink", "colors": { "canvas": "#000000", "accent": "#ffffff" } }));
        let memory_dir = memory::test_dir();
        let mut p = plan(&db, &themes, file, &home, false).unwrap();
        let ada = memory_dir.join(format!("{}.json", p.familiars[0].agent.id));
        p.spells.push(Workflow { id: "dup".into(), name: "A".into(), steps: vec![], created_at: 0, created_by: None });
        p.spells.push(Workflow { id: "dup".into(), name: "B".into(), steps: vec![], created_at: 0, created_by: None });
        assert!(apply(&db, &themes, &p).is_err());
        assert!(db.list_agents().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(&themes).unwrap().count(), 0);
        assert!(!ada.exists(), "the memory file is removed too");
        assert_eq!(std::fs::read_dir(&home).unwrap().flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap()).count(), 0);
        std::fs::remove_dir_all(themes).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn memory_survives_a_backup_and_old_markdown_backups_import() {
        let themes = temp_dir("themes");
        let home = temp_dir("home");
        let dir = memory::test_dir();
        let db = Db::open_in_memory().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        familiar(&db, &id, "Archivist");
        let coven = crate::covens::save(&db, None, "Archive", std::slice::from_ref(&id)).unwrap();
        let mine = Owner { scope: Scope::Familiar, id: &id };
        memory::add(&db, dir, mine, memory::Kind::Rule, None, "Be brief.", Some(&id), "2026-11-01").unwrap();
        memory::add(&db, dir, mine, memory::Kind::Project, Some("grimoire"), "Uses bun.", None, "2026-11-02").unwrap();
        memory::add(&db, dir, Owner { scope: Scope::Coven, id: &coven.id }, memory::Kind::Fact, None, "Meets Thursdays.", None, "2026-11-03").unwrap();

        let file = export_backup(&db, &themes, false).unwrap();
        let doc = file.familiars.iter().find(|f| f.name == "Archivist").unwrap();
        assert_eq!(doc.memory[0].source.as_deref(), Some("Archivist"), "backups name the familiar, not its id");
        let text = serde_json::to_string(&file).unwrap();
        assert!(!text.contains(&format!("\"{id}\"")), "no familiar ids in the backup");

        let backup = plan(&db, &themes, parse(&text).unwrap(), &home, false).unwrap();
        let line = backup.preview.items.iter().find(|i| i.name == "Archivist").unwrap();
        assert!(line.notes.contains(&"Brings 2 memory entries.".to_string()), "{:?}", line.notes);
        apply(&db, &themes, &backup).unwrap();
        let copy = db.list_agents().unwrap().into_iter().find(|a| a.name == "Archivist (2)").unwrap();
        let entries = memory::read(&db, dir, Owner { scope: Scope::Familiar, id: &copy.id }).unwrap().entries;
        let got: Vec<_> = entries.iter().map(|e| (e.kind, e.project.as_deref(), e.text.as_str(), e.learned_on.as_str(), e.source.as_deref())).collect();
        assert_eq!(
            got,
            [
                (memory::Kind::Rule, None, "Be brief.", "2026-11-01", Some(copy.id.as_str())),
                (memory::Kind::Project, Some("grimoire"), "Uses bun.", "2026-11-02", None)
            ]
        );
        let new_coven = copy.coven_id.unwrap();
        let shared = memory::read(&db, dir, Owner { scope: Scope::Coven, id: &new_coven }).unwrap().entries;
        assert_eq!(shared[0].text, "Meets Thursdays.");

        // A backup from before typed memory holds markdown.
        let old = json!({
            "kind": "backup",
            "version": 1,
            "familiars": [{ "name": "Old", "provider": "claude", "access": "workspace", "avatar": Avatar::for_seed("old"), "memory": "- 2026-10-01: Uses bun.\n- Likes tea." }],
            "covens": [{ "name": "Elders", "members": ["Old"], "memory": "Cite sources." }]
        });
        let older = plan(&db, &themes, parse(&old.to_string()).unwrap(), &home, false).unwrap();
        apply(&db, &themes, &older).unwrap();
        let old = db.list_agents().unwrap().into_iter().find(|a| a.name == "Old").unwrap();
        let entries = memory::read(&db, dir, Owner { scope: Scope::Familiar, id: &old.id }).unwrap().entries;
        assert_eq!(entries.iter().map(|e| (e.kind, e.text.as_str())).collect::<Vec<_>>(), [(memory::Kind::Fact, "Uses bun."), (memory::Kind::Fact, "Likes tea.")]);
        assert_eq!(entries[0].learned_on, "2026-10-01");
        let elders = memory::read(&db, dir, Owner { scope: Scope::Coven, id: old.coven_id.as_deref().unwrap() }).unwrap().entries;
        assert_eq!(elders[0].text, "Cite sources.");
        std::fs::remove_dir_all(themes).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn skills_and_who_has_them_survive_a_backup() {
        let root = skills::test_root();
        let themes = temp_dir("themes");
        let home = temp_dir("home");
        let db = stocked(&themes);
        skills::create_in(&db, root, "r", "roundtrip-brief", "Short briefs.", "Keep it short.").unwrap();
        skills::set_for_agent_in(&db, root, "s", &["roundtrip-brief".into()]).unwrap();
        let file = export_backup(&db, &themes, false).unwrap();
        let doc = file.skills.iter().find(|d| d.name == "roundtrip-brief").unwrap();
        assert_eq!((doc.created_by.as_deref(), doc.familiars.clone()), (Some("Researcher"), vec!["Researcher".to_string(), "Scribe".to_string()]));
        assert!(serde_json::to_string(&file).unwrap().contains("\"madeBy\":\"Researcher\""), "backups name the familiar, not its id");

        // Importing into the same Grimoire renames the skill and points it at the imported familiars.
        let p = plan(&db, &themes, file, &home, false).unwrap();
        let line = p.preview.items.iter().find(|i| i.what == "skill" && i.name == "roundtrip-brief").unwrap();
        assert_eq!(line.final_name, "roundtrip-brief-2");
        apply(&db, &themes, &p).unwrap();
        let agents = db.list_agents().unwrap();
        let id_of = |name: &str| agents.iter().find(|a| a.name == name).unwrap().id.clone();
        let copy = skills::list_in(&db, root).unwrap().into_iter().find(|s| s.name == "roundtrip-brief-2").unwrap();
        assert_eq!(copy.created_by, Some(id_of("Researcher (2)")));
        let mut on = copy.agents.clone();
        on.sort();
        let mut want = vec![id_of("Researcher (2)"), id_of("Scribe (2)")];
        want.sort();
        assert_eq!(on, want);
        let text = std::fs::read_to_string(root.join("skills/roundtrip-brief-2/SKILL.md")).unwrap();
        assert_eq!(skills::parse(&text).unwrap().name, "roundtrip-brief-2");

        // A file that names only your familiars brings its skill in off, and broken skills stay out.
        let mut shared = GrimoireFile::new(Kind::Backup);
        let main = |name: &str| SkillFile { path: skills::MAIN_FILE.into(), text: format!("---\nname: {name}\ndescription: d\n---\nBody\n") };
        shared.skills.push(SkillDoc { name: "lonely-skill".into(), files: vec![main("lonely-skill")], source: "url".into(), origin: None, created_by: None, familiars: vec!["Researcher".into()] });
        shared.skills.push(SkillDoc { name: "escape".into(), files: vec![main("escape"), SkillFile { path: "../../evil".into(), text: "x".into() }], source: "url".into(), origin: None, created_by: None, familiars: vec![] });
        shared.skills.push(SkillDoc { name: "forged".into(), files: vec![main("forged")], source: "root".into(), origin: None, created_by: None, familiars: vec![] });
        let p = plan(&db, &themes, shared, &home, false).unwrap();
        let lonely = p.preview.items.iter().find(|i| i.name == "lonely-skill").unwrap();
        assert!(lonely.notes.iter().any(|n| n.contains("Off for your Researcher")), "{:?}", lonely.notes);
        assert!(p.preview.items.iter().filter(|i| i.name != "lonely-skill").all(|i| i.skipped));
        apply(&db, &themes, &p).unwrap();
        assert!(skills::list_in(&db, root).unwrap().iter().find(|s| s.name == "lonely-skill").unwrap().agents.is_empty());
        assert!(!root.join("evil").exists() && !root.join("skills/escape").exists());

        // Two familiars with one name: the skill can't tell them apart, so it gets no owner and stays off.
        let mut twins = GrimoireFile::new(Kind::Backup);
        for _ in 0..2 {
            twins.familiars.push(familiar_doc(&db, &db.get_agent("r").unwrap().unwrap(), false).unwrap());
        }
        twins.skills.push(SkillDoc { name: "twin-skill".into(), files: vec![main("twin-skill")], source: "familiar".into(), origin: None, created_by: Some("Researcher".into()), familiars: vec!["Researcher".into()] });
        let p = plan(&db, &themes, twins, &home, false).unwrap();
        assert!(p.preview.items.iter().find(|i| i.name == "twin-skill").unwrap().notes.iter().any(|n| n.contains("more than one familiar")));
        apply(&db, &themes, &p).unwrap();
        let twin = skills::list_in(&db, root).unwrap().into_iter().find(|s| s.name == "twin-skill").unwrap();
        assert!(twin.created_by.is_none() && twin.agents.is_empty());
        std::fs::remove_dir_all(themes).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }
}
