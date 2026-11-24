//! Reads bot templates from the Grok Bot marketplace at x.ai/bot/marketplace.
//!
//! The marketplace has no public API. Its pages are Next.js server renders that carry the template
//! data in the React flight payload (`self.__next_f.push([1, "..."])` scripts). The catalog page
//! lists every template with its share link (`addHref`) but empty lists; a detail page carries one
//! template with its memories, skills, routines, and integrations. Share links (`/bot/{id}`) only
//! render a name and an "Add to Grok Bot" button, so Grimoire resolves them through the catalog.
//!
//! The payload format is undocumented and can change without notice. Everything read here is
//! untrusted text written by third parties: the frontend shows it for review and never turns it
//! into tool grants.

use crate::db::{Access, Avatar, Provider, Schedule};
use crate::exports::{FamiliarDoc, GrimoireFile, Kind, RitualDoc};
use crate::memory::Entry;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

const HOST: &str = "https://x.ai";
const CATALOG: &str = "https://x.ai/bot/marketplace";
/// Pages run about 450 KB today. Anything far past that isn't a marketplace page.
const MAX_PAGE_BYTES: &str = "4000000";
/// Caps on what one template can bring in, so a hostile page can't flood the database.
const MAX_ITEMS: usize = 60;
const MAX_TEXT: usize = 8_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// One marketplace template, trimmed to the fields Grimoire maps onto a familiar.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub slug: String,
    pub name: String,
    pub creator: String,
    pub description: String,
    pub categories: Vec<String>,
    pub instructions: String,
    pub memories: Vec<Item>,
    pub skills: Vec<Item>,
    pub routines: Vec<Item>,
    pub integrations: Vec<Item>,
    /// The marketplace page this came from, for the familiar's notes.
    pub source_url: String,
    pub share_path: String,
}

/// What a pasted link points at.
#[derive(Debug, PartialEq)]
pub enum Link {
    Catalog,
    Detail(String),
    Share(String),
}

fn slug_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Accepts only x.ai marketplace URLs: the catalog or a category, a detail page, or a share link.
pub fn parse_link(input: &str) -> Result<Link, String> {
    let bad = || "Paste a link from x.ai/bot/marketplace, like https://x.ai/bot/marketplace/bots/haggle-bot or a share link like https://x.ai/bot/pwQ612YrX3R0eACnIMlom.".to_string();
    let trimmed = input.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest.strip_prefix("x.ai/").ok_or_else(bad)?;
    let path = path.split(['?', '#']).next().unwrap_or("").trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["bot", "marketplace"] | ["bot", "marketplace", _] => Ok(Link::Catalog),
        ["bot", "marketplace", "bots", slug] if slug_ok(slug) => Ok(Link::Detail(slug.to_string())),
        ["bot", id] if slug_ok(id) && *id != "marketplace" => Ok(Link::Share(id.to_string())),
        _ => Err(bad()),
    }
}

/// Joins the flight chunks a Next.js page streams into its HTML.
fn flight_text(html: &str) -> String {
    const OPEN: &str = "self.__next_f.push([1,";
    let mut out = String::new();
    let mut rest = html;
    while let Some(at) = rest.find(OPEN) {
        rest = &rest[at + OPEN.len()..];
        let Some(end) = rest.find("])</script>") else { break };
        if let Ok(Value::String(chunk)) = serde_json::from_str::<Value>(rest[..end].trim()) {
            out.push_str(&chunk);
        }
        rest = &rest[end..];
    }
    out
}

/// The end of the JSON value that starts at `start` (an object or array), skipping strings.
fn value_end(text: &str, start: usize) -> Option<usize> {
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    for (i, b) in text.as_bytes()[start..].iter().enumerate() {
        if in_str {
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The first JSON value stored under `"key":` in the flight text that `want` accepts. Real pages
/// reuse key names for layout props (`"template":["$","$L34",...]`), so callers say what they need.
fn find_value(text: &str, key: &str, want: impl Fn(&Value) -> bool) -> Option<Value> {
    let needle = format!("\"{key}\":");
    let mut from = 0;
    while let Some(at) = text[from..].find(&needle) {
        let start = from + at + needle.len();
        if matches!(text.as_bytes().get(start), Some(b'{' | b'[')) {
            if let Some(end) = value_end(text, start) {
                if let Ok(v) = serde_json::from_str::<Value>(&text[start..end]) {
                    if want(&v) {
                        return Some(v);
                    }
                }
            }
        }
        from = start;
    }
    None
}

fn clip(s: &str) -> String {
    let s = s.trim();
    match s.char_indices().nth(MAX_TEXT) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

fn text(v: &Value, key: &str) -> String {
    clip(v[key].as_str().unwrap_or(""))
}

fn items(v: &Value, key: &str, body: &[&str]) -> Vec<Item> {
    let Some(list) = v[key].as_array() else { return Vec::new() };
    list.iter()
        .take(MAX_ITEMS)
        .filter_map(|x| {
            let name = text(x, "name");
            // Skills repeat their description as content on public pages; prefer the longer one.
            let description = body.iter().map(|k| text(x, k)).max_by_key(|s| s.len()).unwrap_or_default();
            (!name.is_empty() || !description.is_empty()).then_some(Item { name, description })
        })
        .collect()
}

fn template_from(v: &Value) -> Option<Template> {
    let slug = v["id"].as_str().filter(|s| slug_ok(s))?.to_string();
    let share_path = v["addHref"].as_str().unwrap_or("").to_string();
    Some(Template {
        source_url: format!("{CATALOG}/bots/{slug}"),
        slug,
        name: text(v, "name"),
        creator: text(v, "creatorName"),
        description: { let d = text(v, "description"); if d.is_empty() { text(v, "summary") } else { d } },
        categories: v["categories"].as_array().map(|c| c.iter().filter_map(|x| x.as_str()).map(clip).take(8).collect()).unwrap_or_default(),
        instructions: text(v, "instructions"),
        memories: items(v, "memories", &["description", "content"]),
        skills: items(v, "skills", &["description", "content"]),
        routines: items(v, "routines", &["summary", "description"]),
        integrations: items(v, "integrations", &["description"]),
        share_path: if share_path.starts_with("/bot/") { share_path } else { String::new() },
    })
}

pub fn parse_detail(html: &str) -> Result<Template, String> {
    find_value(&flight_text(html), "template", |v| v["id"].is_string())
        .as_ref()
        .and_then(template_from)
        .ok_or_else(|| "That page didn't contain a template Grimoire can read. The marketplace may have changed its format.".into())
}

pub fn parse_catalog(html: &str) -> Result<Vec<Template>, String> {
    let list = find_value(&flight_text(html), "templates", |v| v.as_array().is_some_and(|a| a.iter().any(|t| t["id"].is_string())))
        .and_then(|v| v.as_array().cloned())
        .ok_or("The marketplace page didn't list any templates Grimoire can read. Its format may have changed.")?;
    let mut seen = std::collections::HashSet::new();
    Ok(list.iter().filter_map(template_from).filter(|t| seen.insert(t.slug.clone())).collect())
}

async fn fetch(url: &str) -> Result<String, String> {
    // Same reason as updates.rs: macOS ships curl. No redirects, https only, size capped.
    let output = tokio::process::Command::new("/usr/bin/curl")
        .args(["-sS", "--proto", "=https", "--max-time", "20", "--max-filesize", MAX_PAGE_BYTES, "-w", "\n%{http_code}", "-H", "User-Agent: Grimoire", url])
        .output()
        .await
        .map_err(|e| format!("Could not reach the marketplace: {e}"))?;
    if !output.status.success() {
        return Err(format!("Could not reach the marketplace: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let (body, status) = text.rsplit_once('\n').unwrap_or((&text, ""));
    match status.trim() {
        "200" => Ok(body.to_string()),
        "404" => Err("The marketplace has no page at that link.".into()),
        other => Err(format!("The marketplace answered with status {other}.")),
    }
}

pub async fn catalog() -> Result<Vec<Template>, String> {
    parse_catalog(&fetch(CATALOG).await?)
}

/// Loads the full template a pasted link points at.
pub async fn template(link: &str) -> Result<Template, String> {
    let slug = match parse_link(link)? {
        Link::Detail(slug) => slug,
        Link::Share(id) => {
            let path = format!("/bot/{id}");
            catalog()
                .await?
                .into_iter()
                .find(|t| t.share_path == path)
                .map(|t| t.slug)
                .ok_or("That share link isn't one of the public marketplace templates. Private bots can't be imported.")?
        }
        Link::Catalog => return Err("That's the marketplace itself. Pick a template from Browse, or paste a link to one bot.".into()),
    };
    parse_detail(&fetch(&format!("{HOST}/bot/marketplace/bots/{slug}")).await?)
}

/// A template after the person reviewed and edited it, ready to become a familiar.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub name: String,
    pub provider: Provider,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    pub access: Access,
    pub avatar: Avatar,
    pub memory: Vec<Entry>,
    pub rituals: Vec<DraftRitual>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DraftRitual {
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
}

/// The draft as a one-familiar Grimoire file, so it goes through the same checks, renames, and
/// all-or-nothing write as any other import. Imported rituals always start turned off, and only
/// clock schedules are accepted: a template can't open a webhook or watch files on its own.
pub fn to_file(draft: Draft) -> Result<GrimoireFile, String> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let memory: Vec<Entry> = draft.memory.into_iter().map(|e| Entry { id: String::new(), learned_on: String::new(), source: None, ..e }).collect();
    let (_, dropped) = crate::memory::imported(memory.clone(), &today);
    if dropped > 0 {
        return Err(format!("{dropped} memory entries are empty, too long, or don't fit. Shorten or leave some out."));
    }
    if let Some(r) = draft.rituals.iter().find(|r| r.schedule.is_event()) {
        return Err(format!("The ritual {} needs a daily or repeating schedule.", r.name));
    }
    let name = draft.name.trim().to_string();
    let rituals = draft
        .rituals
        .into_iter()
        .map(|r| RitualDoc { familiar: Some(name.clone()), spell: None, name: r.name, prompt: r.prompt, schedule: r.schedule, enabled: false })
        .collect();
    let mut file = GrimoireFile::new(Kind::Familiar);
    file.familiars.push(FamiliarDoc {
        name,
        provider: draft.provider,
        model: draft.model,
        effort: draft.effort,
        access: draft.access,
        avatar: draft.avatar,
        host: None,
        budget: None,
        history: Vec::new(),
        memory,
    });
    file.rituals = rituals;
    Ok(file)
}

#[tauri::command]
pub(crate) async fn marketplace_catalog() -> Result<Vec<Template>, String> {
    catalog().await
}

#[tauri::command]
pub(crate) async fn marketplace_template(link: String) -> Result<Template, String> {
    template(&link).await
}

/// Creates the familiar and its rituals from a reviewed template. Returns the new familiar's id.
#[tauri::command]
pub(crate) async fn import_template(app: AppHandle, state: State<'_, crate::AppState>, draft: Draft) -> Result<String, String> {
    // Full access only when the person turned it on in the preview; the template can't ask for it.
    let keep_full_access = draft.access == Access::Full;
    let file = to_file(draft)?;
    let plan = crate::exports::plan(&state.db, &state.themes_dir, file, &crate::home()?, keep_full_access)?;
    // The person reviewed every row, so anything the planner would leave out is an error, not a note.
    if let Some(skipped) = plan.preview.items.iter().find(|i| i.skipped) {
        return Err(format!("{} can't be imported: {}", skipped.name, skipped.notes.join(" ")));
    }
    let familiar = plan.preview.items.iter().find(|i| i.what == "familiar").ok_or("This template can't become a familiar.")?;
    let name = familiar.final_name.clone();
    crate::exports::apply(&state.db, &state.themes_dir, &plan)?;
    state.scheduler.changed();
    for event in ["agents_changed", "routines_changed", "memory_changed"] {
        let _ = app.emit(event, ());
    }
    let agents = state.db.list_agents()?;
    agents.into_iter().find(|a| a.name == name).map(|a| a.id).ok_or_else(|| "Imported, but Grimoire couldn't find the new familiar.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETAIL: &str = include_str!("testdata/marketplace_detail.html");
    const CATALOG_PAGE: &str = include_str!("testdata/marketplace_catalog.html");

    #[test]
    fn reads_links_from_the_marketplace_only() {
        assert_eq!(parse_link("https://x.ai/bot/marketplace/bots/haggle-bot"), Ok(Link::Detail("haggle-bot".into())));
        assert_eq!(parse_link(" x.ai/bot/marketplace/bots/haggle-bot/?ref=1 "), Ok(Link::Detail("haggle-bot".into())));
        assert_eq!(parse_link("https://x.ai/bot/pwQ612YrX3R0eACnIMlom"), Ok(Link::Share("pwQ612YrX3R0eACnIMlom".into())));
        assert_eq!(parse_link("https://x.ai/bot/marketplace"), Ok(Link::Catalog));
        assert_eq!(parse_link("https://x.ai/bot/marketplace/engineering"), Ok(Link::Catalog));
        for bad in ["https://evil.example/bot/marketplace/bots/x", "https://x.ai.evil.example/bot/abc", "https://x.ai/bot/marketplace/bots/../../etc", "https://x.ai/news/abc", "javascript:alert(1)", ""] {
            assert!(parse_link(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reads_a_detail_page_across_flight_chunks() {
        let t = parse_detail(DETAIL).unwrap();
        assert_eq!(t.slug, "haggle-bot");
        assert_eq!(t.name, "Haggle Bot");
        assert_eq!(t.creator, "Daniel Gartshein");
        assert_eq!(t.share_path, "/bot/pwQ612YrX3R0eACnIMlom");
        assert_eq!((t.memories.len(), t.skills.len(), t.routines.len(), t.integrations.len()), (3, 2, 2, 3));
        assert_eq!(t.skills[0].name, "haggle-setup");
        assert!(t.routines[0].description.starts_with("Disabled by default."));
        assert_eq!(t.integrations[0].name, "Ramp");
        assert_eq!(t.source_url, "https://x.ai/bot/marketplace/bots/haggle-bot");
    }

    #[test]
    fn reads_the_catalog_with_share_links() {
        let list = parse_catalog(CATALOG_PAGE).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].slug, "overheard");
        assert_eq!(list[1].share_path, "/bot/NIEguoGUjA648fUPle8F5");
    }

    #[test]
    fn refuses_pages_without_a_template() {
        assert!(parse_detail("<html>nothing here</html>").is_err());
        assert!(parse_catalog("<html></html>").is_err());
    }

    #[test]
    fn caps_hostile_templates() {
        let huge = "x".repeat(MAX_TEXT + 50);
        let memories: Vec<Value> = (0..500).map(|i| serde_json::json!({"name": format!("m{i}"), "description": huge})).collect();
        let v = serde_json::json!({"id": "big", "name": "Big", "memories": memories, "addHref": "javascript:alert(1)"});
        let t = template_from(&v).unwrap();
        assert_eq!(t.memories.len(), MAX_ITEMS);
        assert_eq!(t.memories[0].description.chars().count(), MAX_TEXT + 1);
        assert_eq!(t.share_path, "");
        assert!(template_from(&serde_json::json!({"id": "../x"})).is_none());
    }

    fn draft(rituals: Vec<DraftRitual>, memory: Vec<Entry>) -> Draft {
        Draft {
            name: " Haggle Bot ".into(),
            provider: Provider::Claude,
            model: None,
            effort: None,
            access: Access::Workspace,
            avatar: Avatar { shape: "orbit".into(), eyes: "dot".into(), color: "#4c7ef3".into() },
            memory,
            rituals,
        }
    }

    fn rule(text: &str) -> Entry {
        Entry { id: "x".into(), kind: crate::memory::Kind::Rule, project: None, text: text.into(), learned_on: "bogus".into(), source: Some("Haggle".into()) }
    }

    #[test]
    fn drafts_become_one_familiar_with_rituals_off() {
        let ritual = DraftRitual { name: "Weekly watch".into(), prompt: "Scan renewals.".into(), schedule: Schedule::Daily { time: "09:00".into(), days: vec![1] } };
        let file = to_file(draft(vec![ritual], vec![rule("Never send without a go.")])).unwrap();
        assert_eq!(file.familiars.len(), 1);
        assert_eq!(file.familiars[0].name, "Haggle Bot");
        assert_eq!(file.familiars[0].memory[0].source, None);
        assert_eq!(file.rituals[0].familiar.as_deref(), Some("Haggle Bot"));
        assert!(!file.rituals[0].enabled);
    }

    #[test]
    fn drafts_refuse_event_rituals_and_oversized_memory() {
        let hook = DraftRitual { name: "Hook".into(), prompt: "p".into(), schedule: Schedule::Webhook { secret: String::new() } };
        assert!(to_file(draft(vec![hook], Vec::new())).is_err());
        assert!(to_file(draft(Vec::new(), vec![rule(&"x".repeat(5000))])).is_err());
        assert!(to_file(draft(Vec::new(), vec![rule("  ")])).is_err());
    }

    #[test]
    fn drafts_plan_like_any_import() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let home = std::env::temp_dir().join(format!("grimoire-tpl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let ritual = DraftRitual { name: "Monthly refresh".into(), prompt: "Refresh the inventory.".into(), schedule: Schedule::Interval { minutes: 10080 } };
        let mut asks_full = draft(vec![ritual], vec![rule("Never sign.")]);
        asks_full.access = Access::Full;
        let plan = crate::exports::plan(&db, &home, to_file(asks_full).unwrap(), &home, false).unwrap();
        let what: Vec<_> = plan.preview.items.iter().map(|i| (i.what, i.skipped)).collect();
        assert_eq!(what, vec![("familiar", false), ("ritual", false)]);
        assert!(plan.preview.full_access);
        assert!(plan.preview.items[0].notes.iter().any(|n| n.contains("workspace access")));
        let empty = DraftRitual { name: "Blank".into(), prompt: "  ".into(), schedule: Schedule::Interval { minutes: 60 } };
        let plan = crate::exports::plan(&db, &home, to_file(draft(vec![empty], Vec::new())).unwrap(), &home, false).unwrap();
        assert!(plan.preview.items.iter().any(|i| i.what == "ritual" && i.skipped), "import_template refuses plans like this");
        std::fs::remove_dir_all(&home).unwrap();
    }
}
