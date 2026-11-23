//! Memory: what a familiar has learned across chats, one file per familiar and one per coven, in
//! `memory/` in the app data folder. Each entry is a fact (lasting, about the person or their world),
//! a rule (a standing instruction), or a project note, with the date Grimoire saved it and who saved
//! it. A familiar gets its memory, and its coven's, when a chat starts, and changes them with the
//! read_memory, remember, and forget_memory tools. Who last changed a memory, and when, is kept in the
//! settings table.
//!
//! Entries live in `<owner>.json` files rather than a table because the chat-start prompt is built
//! without a database handle, and the per-owner files, lock, and atomic rename stay as they were.
//! Memory from before entries (`<owner>.md`) reads as fact entries; the first save writes the JSON
//! file and keeps the old one as `<owner>.md.bak`.

use crate::db::{now_ms, Agent, Db};
use crate::AppState;
use rusqlite::params;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, State};

/// Each memory holds at most this many bytes of UTF-8 across its entries.
pub const LIMIT: usize = 16 * 1024;
/// Each entry holds at most this many characters.
pub const ENTRY_LIMIT: usize = 1000;
const PROJECT_LIMIT: usize = 60;
pub const TOOL_NAMES: [&str; 3] = ["read_memory", "remember", "forget_memory"];
/// The tool entries replaced. Chats started before the change may still call it.
const RETIRED: &str = "rewrite_memory";
const CHANGED: &str = "memory_changed";

static DIR: OnceLock<PathBuf> = OnceLock::new();
/// Changes read, change, and write a file, so two familiars in one coven take turns.
static WRITING: Mutex<()> = Mutex::new(());

pub fn init(data_dir: &Path) {
    let _ = DIR.set(data_dir.join("memory"));
}

/// A shared memory folder for provider tests, which read memory the way the app does.
#[cfg(test)]
pub fn test_dir() -> &'static Path {
    init(&std::env::temp_dir().join(format!("grimoire-memory-test-{}", std::process::id())));
    dir().unwrap()
}

fn dir() -> Result<&'static Path, String> {
    DIR.get().map(PathBuf::as_path).ok_or_else(|| "Memory isn't set up yet.".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Familiar,
    Coven,
}

/// Whose memory: a familiar's own, or a coven's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Owner<'a> {
    pub scope: Scope,
    pub id: &'a str,
}

impl Owner<'_> {
    /// The file name, refusing ids that could leave the memory folder.
    fn stem(&self) -> Result<String, String> {
        let safe = !self.id.is_empty() && self.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !safe {
            return Err("That memory doesn't exist.".into());
        }
        Ok(match self.scope {
            Scope::Familiar => self.id.to_string(),
            Scope::Coven => format!("coven-{}", self.id),
        })
    }

    fn path(&self, dir: &Path, extension: &str) -> Result<PathBuf, String> {
        Ok(dir.join(format!("{}.{extension}", self.stem()?)))
    }

    fn meta_key(&self) -> Result<String, String> {
        Ok(format!("memory:{}", self.stem()?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Fact,
    Rule,
    Project,
}

impl Kind {
    fn parse(kind: &str) -> Result<Kind, String> {
        match kind {
            "fact" => Ok(Kind::Fact),
            "rule" => Ok(Kind::Rule),
            "project" => Ok(Kind::Project),
            other => Err(format!("Unknown kind \"{other}\". Use fact, rule, or project.")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Fact => "fact",
            Kind::Rule => "rule",
            Kind::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    #[serde(default)]
    pub id: String,
    pub kind: Kind,
    /// The project a project entry belongs to; None for facts and rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub text: String,
    /// YYYY-MM-DD, set by Grimoire when the entry was saved.
    #[serde(default)]
    pub learned_on: String,
    /// The familiar that saved it; None when the user did. A backup holds the familiar's name.
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub entries: Vec<Entry>,
    pub limit: usize,
    pub entry_limit: usize,
    /// The familiar that last changed it; None when the user did, or nobody has yet.
    pub updated_by: Option<String>,
    pub updated_at: Option<i64>,
}

/// An entry as the memory editor sends it: an id for one that exists, none for a new one.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    #[serde(default)]
    pub id: Option<String>,
    pub kind: Kind,
    #[serde(default)]
    pub project: Option<String>,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    by: Option<String>,
    at: i64,
}

fn kb(bytes: usize) -> String {
    format!("{:.1} KB", bytes as f64 / 1024.0)
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn is_date(text: &str) -> bool {
    text.len() == 10 && chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok()
}

fn size(entries: &[Entry]) -> usize {
    entries.iter().map(|e| e.text.len() + e.project.as_ref().map_or(0, String::len)).sum()
}

/// Trims and checks an entry's text and project.
fn clean(kind: Kind, project: Option<&str>, text: &str) -> Result<(Option<String>, String), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("The entry is empty.".into());
    }
    let length = text.chars().count();
    if length > ENTRY_LIMIT {
        return Err(format!("An entry holds up to {ENTRY_LIMIT} characters, and this is {length}. Shorten it."));
    }
    match (kind, project.map(str::trim).filter(|p| !p.is_empty())) {
        (Kind::Project, None) => Err("A project entry needs the project's name.".into()),
        (Kind::Project, Some(p)) if p.chars().count() > PROJECT_LIMIT || p.chars().any(char::is_control) => {
            Err(format!("A project name is one line of up to {PROJECT_LIMIT} characters."))
        }
        (Kind::Project, Some(p)) => Ok((Some(p.to_string()), text.to_string())),
        (_, Some(_)) => Err("Only project entries take a project name.".into()),
        (_, None) => Ok((None, text.to_string())),
    }
}

fn new_id(entries: &[Entry]) -> String {
    loop {
        let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        if !entries.iter().any(|e| e.id == id) {
            return id;
        }
    }
}

/// "- Uses bun." or "1. Uses bun", without the marker.
fn strip_bullet(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest);
        }
    }
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    (digits > 0).then(|| line[digits..].strip_prefix(". ").or_else(|| line[digits..].strip_prefix(") "))).flatten()
}

/// Markdown memory from before entries, as fact entries: one per list item or line, with indented
/// lines joined to the item above. A "YYYY-MM-DD: " prefix (how remember wrote notes) becomes the
/// date; other lines get `date`. Headings prefix the items under them. Ids are stable ("old1",
/// "old2"...) so the tools can name them before the file is converted.
pub fn from_markdown(text: &str, date: &str) -> Vec<Entry> {
    let mut items: Vec<(String, String)> = Vec::new();
    let mut heading: Option<String> = None;
    let mut open = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            open = false;
            continue;
        }
        if trimmed.starts_with('#') {
            let title = trimmed.trim_start_matches('#').trim();
            heading = (!title.is_empty()).then(|| title.to_string());
            open = false;
            continue;
        }
        let bullet = strip_bullet(trimmed);
        if bullet.is_none() && open && line.starts_with(char::is_whitespace) {
            if let Some((_, item)) = items.last_mut() {
                item.push('\n');
                item.push_str(trimmed);
            }
            continue;
        }
        let body = bullet.unwrap_or(trimmed).trim();
        let (when, body) = match body.split_once(": ") {
            Some((d, rest)) if is_date(d) => (d.to_string(), rest.trim()),
            _ => (date.to_string(), body),
        };
        if body.is_empty() {
            continue;
        }
        items.push((when, heading.as_ref().map_or_else(|| body.to_string(), |h| format!("{h}: {body}"))));
        open = true;
    }
    let mut entries = Vec::new();
    for (learned_on, text) in items {
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(ENTRY_LIMIT) {
            let text = chunk.iter().collect::<String>().trim().to_string();
            if !text.is_empty() {
                let id = format!("old{}", entries.len() + 1);
                entries.push(Entry { id, kind: Kind::Fact, project: None, text, learned_on: learned_on.clone(), source: None });
            }
        }
    }
    entries
}

/// The day a file last changed, for markdown notes that carry no date.
fn file_date(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| today())
}

fn load(dir: &Path, owner: Owner) -> Result<Vec<Entry>, String> {
    match std::fs::read_to_string(owner.path(dir, "json")?) {
        Ok(text) => return serde_json::from_str(&text).map_err(|e| format!("The memory file is damaged: {e}")),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("Could not read the memory: {e}")),
        Err(_) => {}
    }
    let legacy = owner.path(dir, "md")?;
    match std::fs::read_to_string(&legacy) {
        Ok(text) => Ok(from_markdown(&text, &file_date(&legacy))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("Could not read the memory: {e}")),
    }
}

pub fn read(db: &Db, dir: &Path, owner: Owner) -> Result<Memory, String> {
    let entries = load(dir, owner)?;
    let meta: Option<Meta> = db.setting(&owner.meta_key()?)?.and_then(|m| serde_json::from_str(&m).ok());
    Ok(Memory {
        entries,
        limit: LIMIT,
        entry_limit: ENTRY_LIMIT,
        updated_by: meta.as_ref().and_then(|m| m.by.clone()),
        updated_at: meta.map(|m| m.at),
    })
}

/// Writes the entries file with a temporary file and a rename, so a crash never leaves half a memory.
/// Markdown memory from before entries is kept as `.md.bak`.
fn write_file(dir: &Path, owner: Owner, entries: &[Entry]) -> Result<PathBuf, String> {
    if size(entries) > LIMIT {
        return Err(format!(
            "A memory holds up to {}, and this would make it {}. Forget entries that no longer matter, then try again.",
            kb(LIMIT),
            kb(size(entries))
        ));
    }
    let path = owner.path(dir, "json")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create the memory folder: {e}"))?;
    let text = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text).and_then(|_| std::fs::rename(&temp, &path)).map_err(|e| format!("Could not save the memory: {e}"))?;
    match std::fs::rename(owner.path(dir, "md")?, owner.path(dir, "md.bak")?) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("Could not set the old memory aside: {e}")),
        _ => {}
    }
    Ok(path)
}

fn save_entries(db: &Db, dir: &Path, owner: Owner, entries: &[Entry], by: Option<&str>) -> Result<Memory, String> {
    write_file(dir, owner, entries)?;
    let meta = Meta { by: by.map(String::from), at: now_ms() };
    db.set_setting(&owner.meta_key()?, &serde_json::to_string(&meta).map_err(|e| e.to_string())?)?;
    read(db, dir, owner)
}

/// Adds an entry learned on `date` (YYYY-MM-DD). `by` is the familiar saving it, or None for the user.
#[allow(clippy::too_many_arguments)]
pub fn add(db: &Db, dir: &Path, owner: Owner, kind: Kind, project: Option<&str>, text: &str, by: Option<&str>, date: &str) -> Result<(Memory, Entry), String> {
    let (project, text) = clean(kind, project, text)?;
    let _lock = WRITING.lock().map_err(|_| "memory lock poisoned")?;
    let mut entries = load(dir, owner)?;
    let entry = Entry { id: new_id(&entries), kind, project, text, learned_on: date.into(), source: by.map(String::from) };
    entries.push(entry.clone());
    Ok((save_entries(db, dir, owner, &entries, by)?, entry))
}

/// The user's edits: `drafts` update the entries they name or add new ones learned on `date`, and
/// `removed` ids are deleted. Entries in neither, like one a familiar added meanwhile, stay.
pub fn save(db: &Db, dir: &Path, owner: Owner, drafts: &[Draft], removed: &[String], date: &str) -> Result<Memory, String> {
    let cleaned = drafts.iter().map(|d| clean(d.kind, d.project.as_deref(), &d.text)).collect::<Result<Vec<_>, _>>()?;
    let _lock = WRITING.lock().map_err(|_| "memory lock poisoned")?;
    let mut entries = load(dir, owner)?;
    entries.retain(|e| !removed.contains(&e.id));
    for (draft, (project, text)) in drafts.iter().zip(cleaned) {
        match draft.id.as_ref().and_then(|id| entries.iter().position(|e| &e.id == id)) {
            Some(at) => {
                let entry = &mut entries[at];
                (entry.kind, entry.project, entry.text) = (draft.kind, project, text);
            }
            None => {
                let id = new_id(&entries);
                entries.push(Entry { id, kind: draft.kind, project, text, learned_on: date.into(), source: None });
            }
        }
    }
    save_entries(db, dir, owner, &entries, None)
}

/// Deletes one entry, returning it, or None when the memory has no entry with that id.
fn forget_entry(db: &Db, dir: &Path, owner: Owner, id: &str, by: Option<&str>) -> Result<Option<Entry>, String> {
    let _lock = WRITING.lock().map_err(|_| "memory lock poisoned")?;
    let mut entries = load(dir, owner)?;
    let Some(at) = entries.iter().position(|e| e.id == id) else { return Ok(None) };
    let entry = entries.remove(at);
    save_entries(db, dir, owner, &entries, by)?;
    Ok(Some(entry))
}

/// Removes a memory when its familiar or coven is deleted.
pub fn forget(db: &Db, owner: Owner) -> Result<(), String> {
    match dir() {
        Ok(dir) => forget_in(db, dir, owner),
        Err(_) => Ok(()),
    }
}

fn forget_in(db: &Db, dir: &Path, owner: Owner) -> Result<(), String> {
    for extension in ["json", "md", "md.bak"] {
        match std::fs::remove_file(owner.path(dir, extension)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("Could not delete the memory: {e}")),
            _ => {}
        }
    }
    let key = owner.meta_key()?;
    db.with(|c| c.execute("DELETE FROM settings WHERE key = ?1", params![key]).map(|_| ()))
}

/// A memory's entries for a backup, empty when memory isn't set up.
pub fn entries(owner: Owner) -> Result<Vec<Entry>, String> {
    dir().map_or(Ok(Vec::new()), |dir| load(dir, owner))
}

/// Entries from a backup, checked like the user's own: new ids, a valid date or `date`, and no more
/// than fit. Returns them and how many were left out.
pub fn imported(entries: Vec<Entry>, date: &str) -> (Vec<Entry>, usize) {
    let total = entries.len();
    let mut kept: Vec<Entry> = Vec::new();
    for entry in entries {
        let Ok((project, text)) = clean(entry.kind, entry.project.as_deref(), &entry.text) else { continue };
        let learned_on = if is_date(&entry.learned_on) { entry.learned_on } else { date.to_string() };
        let source = entry.source.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let next = Entry { id: new_id(&kept), kind: entry.kind, project, text, learned_on, source };
        if size(&kept) + size(std::slice::from_ref(&next)) > LIMIT {
            break;
        }
        kept.push(next);
    }
    let dropped = total - kept.len();
    (kept, dropped)
}

/// Writes an imported memory for a new familiar or coven, returning the file to remove if the
/// import fails.
pub fn write_imported(owner: Owner, entries: &[Entry]) -> Result<PathBuf, String> {
    write_file(dir()?, owner, entries)
}

/// Reads a backup's memory: a list of entries, or markdown text, which becomes fact entries.
pub fn from_doc<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Entry>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Doc {
        Markdown(String),
        Entries(Vec<Entry>),
    }
    Ok(match Doc::deserialize(deserializer)? {
        Doc::Markdown(text) => from_markdown(&text, &today()),
        Doc::Entries(entries) => entries,
    })
}

/// Entries grouped as Rules, Facts, and Projects (by project), each line with its learned-on date,
/// and with its id when `ids`.
fn render(entries: &[Entry], ids: bool) -> String {
    let line = |e: &Entry| {
        let id = if ids { format!("[{}] ", e.id) } else { String::new() };
        format!("- {id}{}: {}", e.learned_on, e.text.replace('\n', "\n  "))
    };
    let lines = |kind: Kind| entries.iter().filter(|e| e.kind == kind).map(line).collect::<Vec<_>>();
    let mut sections = Vec::new();
    let rules = lines(Kind::Rule);
    if !rules.is_empty() {
        sections.push(format!("Rules (follow these):\n{}", rules.join("\n")));
    }
    let facts = lines(Kind::Fact);
    if !facts.is_empty() {
        sections.push(format!("Facts:\n{}", facts.join("\n")));
    }
    let mut projects: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for e in entries.iter().filter(|e| e.kind == Kind::Project) {
        projects.entry(e.project.as_deref().unwrap_or("Other")).or_default().push(line(e));
    }
    if !projects.is_empty() {
        let groups: Vec<String> = projects.iter().map(|(name, lines)| format!("{name}:\n{}", lines.join("\n"))).collect();
        sections.push(format!("Projects:\n{}", groups.join("\n")));
    }
    sections.join("\n\n")
}

/// The memory a familiar starts a chat with, or None when it has nothing to read or no way to write.
pub fn session_prompt(agent: &Agent) -> Option<String> {
    let dir = dir().ok()?;
    prompt_in(dir, agent, crate::tools::available_for(agent))
}

fn prompt_in(dir: &Path, agent: &Agent, tools: bool) -> Option<String> {
    let own = load(dir, Owner { scope: Scope::Familiar, id: &agent.id }).unwrap_or_default();
    let coven = agent.coven_id.as_deref().map(|id| load(dir, Owner { scope: Scope::Coven, id }).unwrap_or_default());
    if !tools && own.is_empty() && coven.as_ref().is_none_or(Vec::is_empty) {
        return None;
    }
    let mut prompt = String::from(
        "Grimoire memory: what you've learned across chats, loaded when this chat started, with the day each entry was learned. \
         Follow its rules. The user can read and edit it in Grimoire.",
    );
    if tools {
        prompt.push_str(
            " It may have changed since, so call read_memory for the latest. Save what's worth knowing in later chats with remember: \
             kind \"fact\" for a lasting fact about the user or their world, \"rule\" for a standing instruction on how to behave, or \
             \"project\" with the project's name for a note about one project. Call forget_memory with an entry's id when it's wrong or \
             out of date. When the user corrects you or tells you how they want something done going forward, save it with remember \
             kind \"rule\" in their words.",
        );
        if coven.is_some() {
            prompt.push_str(" Pass scope \"coven\" for the memory your coven shares.");
        }
    }
    let section = |who: &str, entries: &[Entry]| {
        let text = render(entries, false);
        format!("\n\n<memory of=\"{who}\">\n{}\n</memory>", if text.is_empty() { "(empty)" } else { &text })
    };
    prompt.push_str(&section("you", &own));
    if let Some(coven) = coven {
        prompt.push_str(&section("your coven", &coven));
    }
    Some(prompt)
}

pub fn definitions() -> Vec<Value> {
    let scope = json!({
        "type": "string",
        "enum": ["mine", "coven"],
        "description": "mine (the default) for your own memory, or coven for the memory your coven shares."
    });
    vec![
        json!({
            "name": "read_memory",
            "description": "Read your Grimoire memory: rules, facts, and project notes, each with its id and the day it was learned. You got a copy when this chat started, but the user or other familiars may have changed it since.",
            "inputSchema": { "type": "object", "properties": { "scope": scope } }
        }),
        json!({
            "name": "remember",
            "description": "Save one entry to your Grimoire memory for later chats. Grimoire records the date. Each entry holds up to 1000 characters and each memory up to 16 KB.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["fact", "rule", "project"],
                        "description": "fact: a lasting fact about the user or their world. rule: a standing instruction on how to behave, such as a correction the user gave, in their words. project: a note about one named project."
                    },
                    "note": { "type": "string", "description": "The entry, in a sentence or two." },
                    "project": { "type": "string", "description": "The project's name. Required for kind project, not allowed otherwise." },
                    "scope": scope
                },
                "required": ["kind", "note"]
            }
        }),
        json!({
            "name": "forget_memory",
            "description": "Delete one entry from your memory or your coven's, by the id read_memory shows. To correct an entry, forget it and remember the new version.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "description": "The entry's id, like 3f9a1c2e." } },
                "required": ["id"]
            }
        }),
    ]
}

/// Whether call_tool answers `tool`: the memory tools, and the retired rewrite_memory.
pub fn handles(tool: &str) -> bool {
    TOOL_NAMES.contains(&tool) || tool == RETIRED
}

/// Runs a memory tool for the familiar `caller`.
pub fn call_tool(db: &Db, caller: &str, tool: &str, arguments: &Value) -> Result<String, String> {
    tool_in(db, dir()?, caller, tool, arguments, &today())
}

fn tool_in(db: &Db, dir: &Path, caller: &str, tool: &str, arguments: &Value, today: &str) -> Result<String, String> {
    let agent = db.get_agent(caller)?.ok_or("Only a familiar has a memory.")?;
    let mine = Owner { scope: Scope::Familiar, id: &agent.id };
    let coven = agent.coven_id.as_deref().map(|id| Owner { scope: Scope::Coven, id });
    let pick = || match arguments.get("scope").and_then(Value::as_str).unwrap_or("mine") {
        "mine" => Ok((mine, "your memory")),
        "coven" => coven.map(|c| (c, "your coven's memory")).ok_or_else(|| "You aren't in a coven, so there's no coven memory.".to_string()),
        other => Err(format!("Unknown scope \"{other}\". Use mine or coven.")),
    };
    let text_arg = |name: &str| arguments.get(name).and_then(Value::as_str).ok_or(format!("Give the {name}."));
    let usage = |m: &Memory| format!("{} of {} used", kb(size(&m.entries)), kb(LIMIT));
    match tool {
        "read_memory" => {
            let (owner, whose) = pick()?;
            let memory = read(db, dir, owner)?;
            Ok(if memory.entries.is_empty() { format!("{whose} is empty.") } else { format!("{}\n\n({})", render(&memory.entries, true), usage(&memory)) })
        }
        "remember" => {
            let (owner, whose) = pick()?;
            let kind = Kind::parse(arguments.get("kind").and_then(Value::as_str).ok_or("Give the kind: fact, rule, or project.")?)?;
            let project = arguments.get("project").and_then(Value::as_str);
            let (memory, entry) = add(db, dir, owner, kind, project, text_arg("note")?, Some(caller), today)?;
            Ok(format!("Saved {} {} to {whose}, learned {} ({}).", kind.name(), entry.id, entry.learned_on, usage(&memory)))
        }
        "forget_memory" => {
            let id = text_arg("id")?.trim();
            for (owner, whose) in [Some((mine, "your memory")), coven.map(|c| (c, "your coven's memory"))].into_iter().flatten() {
                if let Some(entry) = forget_entry(db, dir, owner, id, Some(caller))? {
                    return Ok(format!("Forgot {} {id} from {whose}: {}", entry.kind.name(), entry.text));
                }
            }
            Err(format!("There's no entry {id} in your memory{}. Call read_memory for the ids.", if coven.is_some() { " or your coven's" } else { "" }))
        }
        RETIRED => Err("rewrite_memory is gone: memory is now a list of entries. Call read_memory for their ids, forget_memory to drop one, and remember to add one.".into()),
        _ => Err(format!("Grimoire has no tool named {tool}.")),
    }
}

/// Checks that the familiar or coven exists before the user saves its memory.
fn owner_exists(db: &Db, owner: Owner) -> Result<(), String> {
    let exists = match owner.scope {
        Scope::Familiar => db.get_agent(owner.id)?.is_some(),
        Scope::Coven => crate::covens::exists(db, owner.id)?,
    };
    if exists { Ok(()) } else { Err("That memory's owner no longer exists.".into()) }
}

#[tauri::command]
pub fn get_memory(state: State<AppState>, scope: Scope, id: String) -> Result<Memory, String> {
    read(&state.db, dir()?, Owner { scope, id: &id })
}

#[tauri::command]
pub fn save_memory(app: AppHandle, state: State<AppState>, scope: Scope, id: String, entries: Vec<Draft>, removed: Vec<String>) -> Result<Memory, String> {
    let owner = Owner { scope, id: &id };
    owner_exists(&state.db, owner)?;
    let saved = save(&state.db, dir()?, owner, &entries, &removed, &today())?;
    let _ = app.emit(CHANGED, ());
    Ok(saved)
}

/// Adds one entry from the user, like a chat message made into a rule.
#[tauri::command]
pub fn add_memory(app: AppHandle, state: State<AppState>, scope: Scope, id: String, kind: Kind, project: Option<String>, text: String) -> Result<Memory, String> {
    let owner = Owner { scope, id: &id };
    owner_exists(&state.db, owner)?;
    let (saved, _) = add(&state.db, dir()?, owner, kind, project.as_deref(), &text, None, &today())?;
    let _ = app.emit(CHANGED, ());
    Ok(saved)
}

/// The event that tells the app's windows a familiar changed a memory.
pub fn changed_event(tool: &str) -> Option<&'static str> {
    (tool != "read_memory").then_some(CHANGED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Avatar, Provider};

    fn familiar(db: &Db, id: &str, coven: Option<&str>) -> Agent {
        let agent = Agent {
            id: id.into(),
            name: id.into(),
            provider: Provider::Claude,
            model: None,
            workspace: format!("/tmp/{id}"),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: coven.map(String::from),
            host: None,
            created_at: now_ms(),
        };
        db.insert_agent(&agent).unwrap();
        agent
    }

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("grimoire-memory-{}", uuid::Uuid::new_v4()))
    }

    const MINE: fn(&str) -> Owner<'_> = |id| Owner { scope: Scope::Familiar, id };
    const DAY: &str = "2026-11-23";

    fn draft(id: Option<&str>, kind: Kind, project: Option<&str>, text: &str) -> Draft {
        Draft { id: id.map(String::from), kind, project: project.map(String::from), text: text.into() }
    }

    #[test]
    fn stores_entries_and_records_who_changed_them() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        familiar(&db, "scribe", None);
        let empty = read(&db, &dir, MINE("scribe")).unwrap();
        assert_eq!((empty.entries.len(), empty.updated_at), (0, None));

        let (saved, entry) = add(&db, &dir, MINE("scribe"), Kind::Rule, None, "  Answer briefly. ", Some("scribe"), DAY).unwrap();
        assert_eq!((entry.text.as_str(), entry.learned_on.as_str(), entry.source.as_deref()), ("Answer briefly.", DAY, Some("scribe")));
        assert_eq!(entry.id.len(), 8);
        assert_eq!(saved.updated_by.as_deref(), Some("scribe"));
        assert!(dir.join("scribe.json").exists());

        let (by_user, _) = add(&db, &dir, Owner { scope: Scope::Coven, id: "c1" }, Kind::Fact, None, "Ships on Fridays.", None, DAY).unwrap();
        assert_eq!((by_user.updated_by, by_user.entries[0].source.clone()), (None, None));
        assert!(dir.join("coven-c1.json").exists());

        assert!(MINE("../etc/passwd").path(&dir, "json").is_err());
        assert!(MINE("").path(&dir, "json").is_err());

        forget_in(&db, &dir, MINE("scribe")).unwrap();
        let gone = read(&db, &dir, MINE("scribe")).unwrap();
        assert_eq!((gone.entries.len(), gone.updated_at), (0, None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn validates_entries_and_enforces_the_limits() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        let add_one = |kind, project, text: &str| add(&db, &dir, MINE("scribe"), kind, project, text, None, DAY);
        assert!(add_one(Kind::Fact, None, "  ").unwrap_err().contains("empty"));
        assert!(add_one(Kind::Project, None, "Uses bun.").unwrap_err().contains("project's name"));
        assert!(add_one(Kind::Project, Some("  "), "Uses bun.").unwrap_err().contains("project's name"));
        assert!(add_one(Kind::Rule, Some("grimoire"), "Uses bun.").unwrap_err().contains("Only project entries"));
        let long_name = "p".repeat(61);
        assert!(add_one(Kind::Project, Some(&long_name), "Uses bun.").is_err());
        assert!(add_one(Kind::Project, Some("a\nb"), "Uses bun.").is_err());
        assert!(add_one(Kind::Fact, None, &"é".repeat(ENTRY_LIMIT + 1)).unwrap_err().contains("up to 1000 characters"));
        let (_, entry) = add_one(Kind::Project, Some(" grimoire "), &"é".repeat(ENTRY_LIMIT)).unwrap();
        assert_eq!(entry.project.as_deref(), Some("grimoire"));

        // 2000 bytes each: eight fit in 16 KB, the ninth does not.
        for _ in 0..7 {
            add_one(Kind::Fact, None, &"é".repeat(1000)).unwrap();
        }
        let full = add_one(Kind::Fact, None, &"é".repeat(1000)).unwrap_err();
        assert!(full.contains("up to 16.0 KB") && full.contains("Forget"), "{full}");
        assert_eq!(read(&db, &dir, MINE("scribe")).unwrap().entries.len(), 8, "a refused change leaves the file as it was");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn user_edits_change_kind_delete_and_keep_concurrent_additions() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        let (_, fact) = add(&db, &dir, MINE("scribe"), Kind::Fact, None, "Likes tables.", Some("scribe"), "2026-11-01").unwrap();
        let (_, gone) = add(&db, &dir, MINE("scribe"), Kind::Fact, None, "Out of date.", None, "2026-11-01").unwrap();
        // A familiar adds this while the editor is open; the editor never saw it.
        let (_, meanwhile) = add(&db, &dir, MINE("scribe"), Kind::Fact, None, "Added meanwhile.", Some("scribe"), "2026-11-02").unwrap();

        let drafts = [draft(Some(&fact.id), Kind::Project, Some("grimoire"), "Likes tables in docs."), draft(None, Kind::Rule, None, "Use bun.")];
        let saved = save(&db, &dir, MINE("scribe"), &drafts, std::slice::from_ref(&gone.id), DAY).unwrap();
        assert_eq!(saved.updated_by, None);
        let texts: Vec<_> = saved.entries.iter().map(|e| (e.kind, e.project.as_deref(), e.text.as_str(), e.learned_on.as_str())).collect();
        assert_eq!(
            texts,
            [
                (Kind::Project, Some("grimoire"), "Likes tables in docs.", "2026-11-01"),
                (Kind::Fact, None, "Added meanwhile.", "2026-11-02"),
                (Kind::Rule, None, "Use bun.", DAY),
            ]
        );
        assert_eq!(saved.entries[0].id, fact.id, "an edited entry keeps its id");
        assert_eq!(saved.entries[1].id, meanwhile.id);

        // Nothing is written when one draft is bad.
        let bad = [draft(None, Kind::Fact, None, "fine"), draft(None, Kind::Project, None, "no project")];
        assert!(save(&db, &dir, MINE("scribe"), &bad, &[], DAY).is_err());
        assert_eq!(read(&db, &dir, MINE("scribe")).unwrap().entries.len(), 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_markdown_memory_as_facts_and_converts_it_on_save() {
        let markdown = "# Notes\n- 2026-10-01: Uses bun.\n  Not npm.\n* Lives in Lisbon\n\nLikes tea.\n1. First\n2) Second\n- 2026-13-40: Bad date\n";
        let entries = from_markdown(markdown, "2026-11-20");
        let got: Vec<_> = entries.iter().map(|e| (e.id.as_str(), e.text.as_str(), e.learned_on.as_str())).collect();
        assert_eq!(
            got,
            [
                ("old1", "Notes: Uses bun.\nNot npm.", "2026-10-01"),
                ("old2", "Notes: Lives in Lisbon", "2026-11-20"),
                ("old3", "Notes: Likes tea.", "2026-11-20"),
                ("old4", "Notes: First", "2026-11-20"),
                ("old5", "Notes: Second", "2026-11-20"),
                ("old6", "Notes: 2026-13-40: Bad date", "2026-11-20"),
            ]
        );
        assert!(entries.iter().all(|e| e.kind == Kind::Fact && e.source.is_none()));
        let long = from_markdown(&"y".repeat(ENTRY_LIMIT * 2 + 5), DAY);
        assert_eq!(long.iter().map(|e| e.text.chars().count()).collect::<Vec<_>>(), [ENTRY_LIMIT, ENTRY_LIMIT, 5], "long lines split, nothing lost");

        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scribe.md"), "- 2026-10-01: Uses bun.\n- Likes tea.\n").unwrap();
        let before = read(&db, &dir, MINE("scribe")).unwrap();
        assert_eq!(before.entries.len(), 2);
        assert_eq!(before.entries, read(&db, &dir, MINE("scribe")).unwrap().entries, "ids stay the same between reads");

        // Forgetting by an old id converts the file and keeps the markdown aside.
        assert!(forget_entry(&db, &dir, MINE("scribe"), "old2", None).unwrap().is_some());
        assert!(dir.join("scribe.json").exists() && !dir.join("scribe.md").exists());
        assert_eq!(std::fs::read_to_string(dir.join("scribe.md.bak")).unwrap(), "- 2026-10-01: Uses bun.\n- Likes tea.\n");
        let after = read(&db, &dir, MINE("scribe")).unwrap().entries;
        assert_eq!((after.len(), after[0].id.as_str(), after[0].learned_on.as_str()), (1, "old1", "2026-10-01"));
        forget_in(&db, &dir, MINE("scribe")).unwrap();
        assert!(!dir.join("scribe.md.bak").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn imports_check_entries_and_accept_markdown() {
        let raw = vec![
            Entry { id: "x".into(), kind: Kind::Rule, project: None, text: " Be brief. ".into(), learned_on: "2026-01-02".into(), source: Some("Scribe".into()) },
            Entry { id: "x".into(), kind: Kind::Project, project: None, text: "No project.".into(), learned_on: DAY.into(), source: None },
            Entry { id: "x".into(), kind: Kind::Fact, project: None, text: "Odd date.".into(), learned_on: "yesterday".into(), source: Some(" ".into()) },
        ];
        let (kept, dropped) = imported(raw, DAY);
        assert_eq!(dropped, 1);
        assert_eq!((kept[0].text.as_str(), kept[0].learned_on.as_str(), kept[0].source.as_deref()), ("Be brief.", "2026-01-02", Some("Scribe")));
        assert_eq!((kept[1].learned_on.as_str(), kept[1].source.as_deref()), (DAY, None));
        assert_ne!(kept[0].id, kept[1].id);

        #[derive(Deserialize)]
        struct Doc {
            #[serde(deserialize_with = "from_doc")]
            memory: Vec<Entry>,
        }
        let old: Doc = serde_json::from_value(json!({ "memory": "- 2026-10-01: Uses bun." })).unwrap();
        assert_eq!((old.memory[0].kind, old.memory[0].text.as_str(), old.memory[0].learned_on.as_str()), (Kind::Fact, "Uses bun.", "2026-10-01"));
        let new: Doc = serde_json::from_value(json!({ "memory": [{ "kind": "rule", "text": "Be brief.", "learnedOn": DAY }] })).unwrap();
        assert_eq!(new.memory[0].kind, Kind::Rule);
        assert!(serde_json::from_value::<Doc>(json!({ "memory": [{ "kind": "wish", "text": "x" }] })).is_err());
    }

    #[test]
    fn tools_reach_own_and_coven_memory() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        let coven = crate::covens::save(&db, None, "Research", &[]).unwrap();
        familiar(&db, "scribe", Some(&coven.id));
        familiar(&db, "loner", None);
        let call = |caller: &str, tool: &str, args: Value| tool_in(&db, &dir, caller, tool, &args, DAY);

        assert_eq!(call("scribe", "read_memory", json!({})).unwrap(), "your memory is empty.");
        let saved = call("scribe", "remember", json!({ "kind": "rule", "note": "Call the user Nik." })).unwrap();
        assert!(saved.starts_with("Saved rule ") && saved.contains("to your memory, learned 2026-11-23"), "{saved}");
        call("scribe", "remember", json!({ "kind": "project", "project": "grimoire", "note": "Ships Fridays." })).unwrap();
        call("scribe", "remember", json!({ "kind": "fact", "note": "Has a cat." })).unwrap();
        let read_back = call("scribe", "read_memory", json!({ "scope": "mine" })).unwrap();
        let rule_id = &read(&db, &dir, MINE("scribe")).unwrap().entries[0].id;
        assert!(read_back.starts_with(&format!("Rules (follow these):\n- [{rule_id}] 2026-11-23: Call the user Nik.\n\nFacts:\n")), "{read_back}");
        assert!(read_back.contains("Projects:\ngrimoire:\n- ["), "{read_back}");

        assert!(call("scribe", "remember", json!({ "note": "x" })).unwrap_err().contains("Give the kind"));
        assert!(call("scribe", "remember", json!({ "kind": "wish", "note": "x" })).unwrap_err().contains("Unknown kind"));
        assert!(call("scribe", "remember", json!({ "kind": "project", "note": "x" })).unwrap_err().contains("project's name"));
        assert!(call("scribe", "remember", json!({ "kind": "fact" })).unwrap_err().contains("Give the note"));

        call("scribe", "remember", json!({ "scope": "coven", "kind": "rule", "note": "Cite sources." })).unwrap();
        let shared = read(&db, &dir, Owner { scope: Scope::Coven, id: &coven.id }).unwrap();
        assert_eq!((shared.entries[0].text.as_str(), shared.updated_by.as_deref()), ("Cite sources.", Some("scribe")));

        // forget_memory finds the id in either memory, and only in the caller's.
        let forgot = call("scribe", "forget_memory", json!({ "id": shared.entries[0].id })).unwrap();
        assert!(forgot.contains("from your coven's memory: Cite sources."), "{forgot}");
        assert!(call("loner", "forget_memory", json!({ "id": rule_id })).unwrap_err().contains("no entry"));
        assert!(call("scribe", "forget_memory", json!({ "id": rule_id })).unwrap().contains("Forgot rule"));
        assert!(call("scribe", "forget_memory", json!({ "id": rule_id })).unwrap_err().contains("or your coven's"));

        assert!(call("scribe", "rewrite_memory", json!({ "text": "x" })).unwrap_err().contains("forget_memory"));
        assert!(handles("rewrite_memory") && handles("forget_memory") && !handles("list_themes"));
        assert!(call("loner", "read_memory", json!({ "scope": "coven" })).unwrap_err().contains("aren't in a coven"));
        assert!(call("scribe", "read_memory", json!({ "scope": "everyone" })).is_err());
        assert!(call("ghost", "read_memory", json!({})).is_err());
        assert_eq!(changed_event("forget_memory"), Some(CHANGED));
        assert_eq!(changed_event("read_memory"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn builds_the_session_prompt() {
        let (db, dir) = (Db::open_in_memory().unwrap(), temp_dir());
        let loner = familiar(&db, "loner", None);
        assert_eq!(prompt_in(&dir, &loner, false), None, "nothing to read and no tools");

        let prompt = prompt_in(&dir, &loner, true).unwrap();
        assert!(prompt.contains("remember kind \"rule\" in their words") && prompt.contains("<memory of=\"you\">\n(empty)\n</memory>"));
        assert!(!prompt.contains("coven"));

        let scribe = familiar(&db, "scribe", Some("c1"));
        add(&db, &dir, MINE("scribe"), Kind::Fact, None, "Likes tables.", None, "2026-11-01").unwrap();
        add(&db, &dir, MINE("scribe"), Kind::Project, Some("zeta"), "Due in March.", None, "2026-11-03").unwrap();
        add(&db, &dir, MINE("scribe"), Kind::Rule, None, "No emoji.\nEver.", None, "2026-11-02").unwrap();
        add(&db, &dir, MINE("scribe"), Kind::Project, Some("alpha"), "Uses bun.", None, "2026-11-04").unwrap();
        add(&db, &dir, Owner { scope: Scope::Coven, id: "c1" }, Kind::Fact, None, "Meets Thursdays.", None, DAY).unwrap();
        let prompt = prompt_in(&dir, &scribe, false).unwrap();
        assert!(
            prompt.contains(
                "<memory of=\"you\">\nRules (follow these):\n- 2026-11-02: No emoji.\n  Ever.\n\nFacts:\n- 2026-11-01: Likes tables.\n\n\
                 Projects:\nalpha:\n- 2026-11-04: Uses bun.\nzeta:\n- 2026-11-03: Due in March.\n</memory>"
            ),
            "{prompt}"
        );
        assert!(prompt.contains("<memory of=\"your coven\">\nFacts:\n- 2026-11-23: Meets Thursdays.\n</memory>"));
        assert!(!prompt.contains("read_memory") && !prompt.contains('['), "no tools, so no tool hints or ids");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Starts a new chat with a memory and a coven memory, and returns the familiar's reply.
    async fn live_recall(provider: Provider, model: &str) -> String {
        use crate::providers::{claude::Claude, codex::Codex, ProviderEvent};
        let dir = test_dir();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("live-memory.md"), "- 2026-11-20: The user's cat is named Miso.\n").unwrap();
        std::fs::write(dir.join("coven-live-coven.md"), "- The coven meets on Thursdays.\n").unwrap();
        let workspace = std::env::temp_dir().join(format!("grimoire-live-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        let agent = Agent {
            id: "live-memory".into(),
            name: "Scribe".into(),
            provider: provider.clone(),
            model: Some(model.into()),
            workspace: workspace.to_string_lossy().into_owned(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: Some("live-coven".into()),
            host: None,
            created_at: now_ms(),
        };
        let prompt = "What do you remember about me and about my coven? Answer in one short sentence and don't use any tools.";
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (codex, claude) = (std::sync::Arc::new(Codex::default()), Claude::default());
        match provider {
            Provider::Codex => codex.start_turn(&agent, prompt, &[], tx).await.unwrap(),
            Provider::Claude => claude.start_turn(&agent, prompt, &[], tx).await.unwrap(),
        }
        let mut reply = String::new();
        let collect = async {
            while let Some(event) = rx.recv().await {
                match event {
                    ProviderEvent::Delta(text) => reply.push_str(&text),
                    ProviderEvent::Done => break,
                    ProviderEvent::Failed(e) => panic!("turn failed: {e}"),
                    _ => {}
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(180), collect).await.expect("no reply in time");
        claude.forget("live-memory");
        std::fs::remove_dir_all(workspace).unwrap();
        println!("{provider:?} replied: {reply}");
        reply
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real claude CLI"]
    async fn live_claude_reads_its_memory() {
        let reply = live_recall(Provider::Claude, "claude-haiku-5-5").await;
        assert!(reply.contains("Miso") && reply.contains("Thursday"), "{reply}");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "runs the real codex CLI"]
    async fn live_codex_reads_its_memory() {
        let reply = live_recall(Provider::Codex, "gpt-6.2-sol").await;
        assert!(reply.contains("Miso") && reply.contains("Thursday"), "{reply}");
    }
}
