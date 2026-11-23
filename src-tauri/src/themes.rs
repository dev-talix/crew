//! Themes. The built-ins ship inside the app; people and familiars keep their own as JSON files in the
//! themes folder. The folder is read each time the list is asked for, so a theme edited in a text
//! editor shows up the next time Grimoire reloads its themes.

use crate::db::Db;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Color roles a theme can set. `canvas` and `accent` are the seeds the app derives the rest from.
pub const ROLES: [&str; 15] = [
    "canvas",
    "accent",
    "sidebar",
    "surface",
    "raised",
    "text",
    "textMuted",
    "textFaint",
    "border",
    "accentForeground",
    "userBubble",
    "success",
    "warning",
    "danger",
    "dangerSurface",
];
const MODES: [&str; 2] = ["dark", "light"];
const MAX_CSS: usize = 100_000;
const ACTIVE: &str = "theme";
pub const DEFAULT_THEME: &str = "tyrian";

const BUILT_IN: [(&str, &str); 7] = [
    ("tyrian", include_str!("../themes/tyrian.json")),
    ("iron-gall", include_str!("../themes/iron-gall.json")),
    ("verdigris", include_str!("../themes/verdigris.json")),
    ("dragons-blood", include_str!("../themes/dragons-blood.json")),
    ("orpiment", include_str!("../themes/orpiment.json")),
    ("lapis", include_str!("../themes/lapis.json")),
    ("moonstone", include_str!("../themes/moonstone.json")),
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeEntry {
    pub id: String,
    pub built_in: bool,
    pub theme: Value,
}

#[derive(Debug, Serialize)]
pub struct Problem {
    pub file: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
pub struct ThemeList {
    pub active: String,
    pub themes: Vec<ThemeEntry>,
    pub problems: Vec<Problem>,
}

pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("themes")
}

fn is_hex(value: &str) -> bool {
    let digits = value.strip_prefix('#').unwrap_or("");
    matches!(digits.len(), 3 | 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit())
}

fn clean_colors(value: &Value, place: &str) -> Result<Map<String, Value>, String> {
    let object = value.as_object().ok_or_else(|| format!("{place} must be an object of color roles."))?;
    let mut colors = Map::new();
    for (role, color) in object {
        if !ROLES.contains(&role.as_str()) {
            return Err(format!("{place} has an unknown color role \"{role}\". Use one of: {}.", ROLES.join(", ")));
        }
        match color.as_str().map(str::trim) {
            Some(hex) if is_hex(hex) => colors.insert(role.clone(), json!(hex.to_lowercase())),
            _ => return Err(format!("\"{role}\" in {place} must be a hex color like #9b7bff.")),
        };
    }
    Ok(colors)
}

/// Checks a theme and returns it in the stored shape. At least one appearance needs both seed colors.
pub fn validate(theme: &Value) -> Result<Value, String> {
    let object = theme.as_object().ok_or("A theme must be a JSON object.")?;
    let name = object.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if name.is_empty() || name.chars().count() > 60 {
        return Err("Give the theme a name of 1 to 60 characters.".into());
    }
    let mut out = Map::new();
    out.insert("version".into(), json!(1));
    out.insert("name".into(), json!(name));
    let colors = match object.get("colors") {
        Some(value) => clean_colors(value, "colors")?,
        None => Map::new(),
    };
    let mut variants = Map::new();
    if let Some(value) = object.get("variants") {
        for (mode, colors) in value.as_object().ok_or("variants must be an object with dark and light.")? {
            if !MODES.contains(&mode.as_str()) {
                return Err(format!("variants can only hold dark and light, not \"{mode}\"."));
            }
            variants.insert(mode.clone(), Value::Object(clean_colors(colors, &format!("variants.{mode}"))?));
        }
    }
    let covers = |mode: &str| {
        ["canvas", "accent"].iter().all(|role| colors.contains_key(*role) || variants.get(mode).is_some_and(|v| v.get(*role).is_some()))
    };
    if !MODES.iter().any(|mode| covers(mode)) {
        return Err("Set canvas and accent for at least one appearance, in colors or in variants.dark or variants.light.".into());
    }
    if !colors.is_empty() {
        out.insert("colors".into(), Value::Object(colors));
    }
    if !variants.is_empty() {
        out.insert("variants".into(), Value::Object(variants));
    }
    match object.get("css") {
        None | Some(Value::Null) => {}
        Some(Value::String(css)) if css.len() <= MAX_CSS => {
            if !css.trim().is_empty() {
                out.insert("css".into(), json!(css));
            }
        }
        Some(Value::String(_)) => return Err(format!("css can hold at most {MAX_CSS} characters.")),
        Some(_) => return Err("css must be a string.".into()),
    }
    if let Some(by) = object.get("createdBy").and_then(Value::as_str) {
        out.insert("createdBy".into(), json!(by));
    }
    Ok(Value::Object(out))
}

/// File names are the theme id, so ids stay to lowercase letters, digits, and dashes.
fn valid_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty() && id.len() <= 80 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok { Ok(()) } else { Err(format!("\"{id}\" is not a theme id.")) }
}

fn built_in(id: &str) -> Option<Value> {
    BUILT_IN.iter().find(|(key, _)| *key == id).map(|(_, text)| serde_json::from_str(text).expect("built-in theme is valid JSON"))
}

pub fn list(db: &Db, dir: &Path) -> Result<ThemeList, String> {
    let mut themes: Vec<ThemeEntry> = BUILT_IN
        .iter()
        .map(|(id, _)| ThemeEntry { id: id.to_string(), built_in: true, theme: built_in(id).unwrap() })
        .collect();
    let mut problems = Vec::new();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    for path in files {
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let id = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let loaded = valid_id(&id)
            .map_err(|_| "Rename the file to lowercase letters, digits, and dashes.".to_string())
            .and_then(|_| if built_in(&id).is_some() { Err("A built-in theme already uses this name.".into()) } else { Ok(()) })
            .and_then(|_| std::fs::read_to_string(&path).map_err(|e| e.to_string()))
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| format!("Not valid JSON: {e}")))
            .and_then(|value| validate(&value));
        match loaded {
            Ok(theme) => themes.push(ThemeEntry { id, built_in: false, theme }),
            Err(error) => problems.push(Problem { file, error }),
        }
    }
    let active = db.setting(ACTIVE)?.filter(|id| themes.iter().any(|t| &t.id == id)).unwrap_or_else(|| DEFAULT_THEME.into());
    Ok(ThemeList { active, themes, problems })
}

/// Saves a theme as a new file, or over an existing user theme when `id` is given.
pub fn save(dir: &Path, id: Option<&str>, theme: &Value, created_by: Option<String>) -> Result<ThemeEntry, String> {
    let mut theme = validate(theme)?;
    let id = match id {
        Some(id) => {
            valid_id(id)?;
            if built_in(id).is_some() {
                return Err("Built-in themes can't be changed. Duplicate it to make your own.".into());
            }
            id.to_string()
        }
        None => {
            let base = crate::slug(theme["name"].as_str().unwrap_or_default(), "theme");
            let taken = |id: &str| built_in(id).is_some() || dir.join(format!("{id}.json")).exists();
            (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|id| !taken(id)).unwrap()
        }
    };
    let path = dir.join(format!("{id}.json"));
    // Keep who made a theme when someone edits it later.
    let previous_creator = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|old| old.get("createdBy").and_then(Value::as_str).map(String::from));
    match created_by.or(previous_creator) {
        Some(by) => theme["createdBy"] = json!(by),
        None => {
            theme.as_object_mut().unwrap().remove("createdBy");
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!(".{id}.json.tmp"));
    let text = serde_json::to_string_pretty(&theme).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&temp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
    Ok(ThemeEntry { id, built_in: false, theme })
}

pub fn delete(db: &Db, dir: &Path, id: &str) -> Result<(), String> {
    valid_id(id)?;
    if built_in(id).is_some() {
        return Err("Built-in themes can't be deleted.".into());
    }
    std::fs::remove_file(dir.join(format!("{id}.json"))).map_err(|e| e.to_string())?;
    if db.setting(ACTIVE)?.as_deref() == Some(id) {
        db.set_setting(ACTIVE, DEFAULT_THEME)?;
    }
    Ok(())
}

pub fn set_active(db: &Db, dir: &Path, id: &str) -> Result<(), String> {
    if !list(db, dir)?.themes.iter().any(|t| t.id == id) {
        return Err(format!("There is no theme \"{id}\"."));
    }
    db.set_setting(ACTIVE, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("grimoire-themes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn built_ins_are_valid() {
        for (id, _) in BUILT_IN {
            let theme = built_in(id).unwrap();
            assert_eq!(validate(&theme).unwrap(), theme, "{id} should already be in the stored shape");
        }
    }

    #[test]
    fn validation_explains_what_is_wrong() {
        let seeds = json!({ "canvas": "#101010", "accent": "#9B7BFF" });
        let ok = validate(&json!({ "name": " Night ", "variants": { "dark": seeds }, "css": "" })).unwrap();
        assert_eq!(ok, json!({ "version": 1, "name": "Night", "variants": { "dark": { "canvas": "#101010", "accent": "#9b7bff" } } }));

        let err = |theme: Value| validate(&theme).unwrap_err();
        assert!(err(json!({ "name": "", "colors": seeds })).contains("name"));
        assert!(err(json!({ "name": "X", "colors": { "canvas": "#101010" } })).contains("canvas and accent"));
        assert!(err(json!({ "name": "X", "colors": { "canvas": "red", "accent": "#fff" } })).contains("\"canvas\" in colors must be a hex"));
        assert!(err(json!({ "name": "X", "colors": { "glow": "#fff" } })).contains("unknown color role \"glow\""));
        assert!(err(json!({ "name": "X", "variants": { "sepia": seeds } })).contains("\"sepia\""));
        assert!(err(json!({ "name": "X", "colors": seeds, "css": "a".repeat(MAX_CSS + 1) })).contains("at most"));
    }

    #[test]
    fn saves_lists_and_deletes_user_themes() {
        let db = Db::open_in_memory().unwrap();
        let dir = temp_dir();
        let theme = json!({ "name": "Tyrian", "colors": { "canvas": "#111111", "accent": "#ff8800" } });

        // A name that matches a built-in gets its own id, and the creator sticks through later edits.
        let saved = save(&dir, None, &theme, Some("f1".into())).unwrap();
        assert_eq!(saved.id, "tyrian-2");
        let edited = save(&dir, Some("tyrian-2"), &json!({ "name": "Ember", "colors": theme["colors"] }), None).unwrap();
        assert_eq!(edited.theme["createdBy"], "f1");

        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
        std::fs::write(dir.join("Bad Name.json"), "{}").unwrap();
        set_active(&db, &dir, "tyrian-2").unwrap();
        let listed = list(&db, &dir).unwrap();
        assert_eq!(listed.active, "tyrian-2");
        assert_eq!(listed.themes.len(), BUILT_IN.len() + 1);
        let files: Vec<&str> = listed.problems.iter().map(|p| p.file.as_str()).collect();
        assert_eq!(files, ["Bad Name.json", "broken.json"]);

        assert!(save(&dir, Some("tyrian"), &theme, None).unwrap_err().contains("Built-in"));
        assert!(save(&dir, Some("../escape"), &theme, None).unwrap_err().contains("not a theme id"));
        assert!(set_active(&db, &dir, "missing").is_err());

        delete(&db, &dir, "tyrian-2").unwrap();
        assert_eq!(list(&db, &dir).unwrap().active, DEFAULT_THEME);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
