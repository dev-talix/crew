//! The vault: logins, API keys, and cards. Secret values live in the macOS Keychain; Grimoire's
//! database keeps only what the list shows (name, site, username, the last digits of a card).
//!
//! Familiars never get a secret as text. `hand_off` puts it in a one-time named pipe, and the
//! familiar reads that path inside the command that needs it, so the value stays out of the model.

use crate::db::{now_ms, Db};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const HAND_OFF_TTL: Duration = Duration::from_secs(120);

/// Where secret values are kept. The app uses the Keychain; tests use memory.
pub trait SecretStore: Send + Sync {
    fn set(&self, id: &str, value: &str) -> Result<(), String>;
    fn get(&self, id: &str) -> Result<Option<String>, String>;
    fn delete(&self, id: &str) -> Result<(), String>;
}

#[cfg(target_os = "macos")]
pub struct Keychain;

#[cfg(target_os = "macos")]
impl Keychain {
    const SERVICE: &'static str = "dev.talix.grimoire.vault";
}

#[cfg(target_os = "macos")]
impl SecretStore for Keychain {
    fn set(&self, id: &str, value: &str) -> Result<(), String> {
        security_framework::passwords::set_generic_password(Self::SERVICE, id, value.as_bytes()).map_err(|e| format!("Keychain: {e}"))
    }

    fn get(&self, id: &str) -> Result<Option<String>, String> {
        match security_framework::passwords::get_generic_password(Self::SERVICE, id) {
            Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| "The Keychain item is not text.".into()),
            // errSecItemNotFound
            Err(e) if e.code() == -25300 => Ok(None),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }

    fn delete(&self, id: &str) -> Result<(), String> {
        match security_framework::passwords::delete_generic_password(Self::SERVICE, id) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(e) => Err(format!("Keychain: {e}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Login,
    ApiKey,
    Card,
}

impl Kind {
    /// The secret fields, required ones first.
    pub fn fields(self) -> &'static [&'static str] {
        match self {
            Kind::Login => &["password"],
            Kind::ApiKey => &["key"],
            Kind::Card => &["number", "expiry", "cvc"],
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kind::Login => "login",
            Kind::ApiKey => "api_key",
            Kind::Card => "card",
        }
    }

    fn parse(value: &str) -> Option<Kind> {
        [Kind::Login, Kind::ApiKey, Kind::Card].into_iter().find(|k| k.as_str() == value)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VaultItem {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    /// The website or service, for logins and API keys.
    pub site: Option<String>,
    /// The username for a login, or the name on a card.
    pub username: Option<String>,
    /// Safe to show: the last four digits of a card.
    pub hint: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemInput {
    pub id: Option<String>,
    pub kind: Kind,
    pub name: String,
    pub site: Option<String>,
    pub username: Option<String>,
    /// Secret fields to set. On an edit, fields left out keep their value.
    #[serde(default)]
    pub secret: Map<String, Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Use {
    pub agent_id: Option<String>,
    pub field: String,
    pub outcome: String,
    pub at: i64,
}

fn clean(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn row_to_item(r: &rusqlite::Row) -> rusqlite::Result<VaultItem> {
    let kind: String = r.get(1)?;
    Ok(VaultItem {
        id: r.get(0)?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Login),
        name: r.get(2)?,
        site: r.get(3)?,
        username: r.get(4)?,
        hint: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

const COLUMNS: &str = "id, kind, name, site, username, hint, created_at, updated_at";

pub fn list(db: &Db) -> Result<Vec<VaultItem>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached(&format!("SELECT {COLUMNS} FROM vault_items ORDER BY name COLLATE NOCASE"))?;
        let rows = stmt.query_map([], row_to_item)?;
        rows.collect()
    })
}

pub fn get(db: &Db, id: &str) -> Result<Option<VaultItem>, String> {
    db.with(|c| c.query_row(&format!("SELECT {COLUMNS} FROM vault_items WHERE id = ?1"), params![id], row_to_item).optional())
}

/// The secret fields of an item, for the person using Grimoire.
pub fn secret(store: &dyn SecretStore, id: &str) -> Result<Map<String, Value>, String> {
    let text = store.get(id)?.ok_or("This item's secret is missing from the Keychain.")?;
    serde_json::from_str(&text).map_err(|_| "This item's secret could not be read.".into())
}

pub fn save(db: &Db, store: &dyn SecretStore, input: ItemInput) -> Result<VaultItem, String> {
    let name = input.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("Give the item a name of 1 to 80 characters.".into());
    }
    let existing = match input.id.as_deref() {
        Some(id) => Some(get(db, id)?.ok_or("That vault item no longer exists.")?),
        None => None,
    };
    if existing.as_ref().is_some_and(|e| e.kind != input.kind) {
        return Err("An item can't change its kind.".into());
    }
    let mut secret = match &existing {
        Some(item) => self::secret(store, &item.id).unwrap_or_default(),
        None => Map::new(),
    };
    for (field, value) in input.secret {
        if !input.kind.fields().contains(&field.as_str()) {
            return Err(format!("A {} has no field called {field}.", input.kind.as_str().replace('_', " ")));
        }
        let value = value.as_str().map(str::trim).unwrap_or_default().to_string();
        if value.is_empty() {
            secret.remove(&field);
        } else {
            secret.insert(field, Value::String(value));
        }
    }
    let required: &[&str] = match input.kind {
        Kind::Card => &["number"],
        kind => &kind.fields()[..1],
    };
    if let Some(missing) = required.iter().find(|f| !secret.contains_key(**f)) {
        return Err(format!("Add the {missing}."));
    }
    let hint = match input.kind {
        Kind::Card => {
            let digits: String = secret["number"].as_str().unwrap_or_default().chars().filter(char::is_ascii_digit).collect();
            if !(12..=19).contains(&digits.len()) {
                return Err("A card number has 12 to 19 digits.".into());
            }
            secret.insert("number".into(), Value::String(digits.clone()));
            Some(format!("•••• {}", &digits[digits.len() - 4..]))
        }
        _ => None,
    };
    let now = now_ms();
    let item = VaultItem {
        id: existing.as_ref().map(|e| e.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        kind: input.kind,
        name,
        site: clean(input.site),
        username: clean(input.username),
        hint,
        created_at: existing.as_ref().map(|e| e.created_at).unwrap_or(now),
        updated_at: now,
    };
    store.set(&item.id, &Value::Object(secret).to_string())?;
    let saved = db.with(|c| {
        c.execute(
            "INSERT INTO vault_items (id, kind, name, site, username, hint, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET name = ?3, site = ?4, username = ?5, hint = ?6, updated_at = ?8",
            params![item.id, item.kind.as_str(), item.name, item.site, item.username, item.hint, item.created_at, item.updated_at],
        )
    });
    if let Err(e) = saved {
        if existing.is_none() {
            let _ = store.delete(&item.id);
        }
        return Err(e);
    }
    Ok(item)
}

pub fn delete(db: &Db, store: &dyn SecretStore, id: &str) -> Result<(), String> {
    store.delete(id)?;
    db.with(|c| c.execute("DELETE FROM vault_items WHERE id = ?1", params![id]).map(|_| ()))?;
    db.with(|c| c.execute("DELETE FROM grants WHERE scope = ?1", params![scope(id)]).map(|_| ()))
}

/// The grant scope that lets a familiar use an item without asking.
pub fn scope(id: &str) -> String {
    format!("vault:{id}")
}

pub fn record_use(db: &Db, item_id: &str, agent_id: Option<&str>, field: &str, outcome: &str) -> Result<(), String> {
    db.with(|c| {
        c.execute(
            "INSERT INTO vault_uses (item_id, agent_id, field, outcome, at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![item_id, agent_id, field, outcome, now_ms()],
        )
        .map(|_| ())
    })
}

pub fn uses(db: &Db, item_id: &str) -> Result<Vec<Use>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached("SELECT agent_id, field, outcome, at FROM vault_uses WHERE item_id = ?1 ORDER BY at DESC LIMIT 20")?;
        let rows = stmt.query_map(params![item_id], |r| Ok(Use { agent_id: r.get(0)?, field: r.get(1)?, outcome: r.get(2)?, at: r.get(3)? }))?;
        rows.collect()
    })
}

/// Finds an item by id, or by a name or site that matches exactly one item.
pub fn resolve(db: &Db, wanted: &str) -> Result<VaultItem, String> {
    let wanted = wanted.trim();
    let items = list(db)?;
    if let Some(item) = items.iter().find(|i| i.id == wanted) {
        return Ok(item.clone());
    }
    let lower = wanted.to_lowercase();
    let host = |site: &str| site.trim_start_matches("https://").trim_start_matches("http://").trim_start_matches("www.").trim_end_matches('/').to_lowercase();
    let matches: Vec<&VaultItem> = items
        .iter()
        .filter(|i| i.name.to_lowercase() == lower || i.site.as_deref().is_some_and(|s| host(s) == host(&lower)))
        .collect();
    match matches.as_slice() {
        [item] => Ok((*item).clone()),
        [] => Err(format!("Nothing in the vault matches \"{wanted}\". Use vault_search to see what's there.")),
        _ => Err(format!("More than one vault item matches \"{wanted}\". Use its id instead.")),
    }
}

fn secrets_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    // The per-user temporary folder has no spaces in its path, which keeps shell commands simple.
    let dir = std::env::temp_dir().join("grimoire-secrets");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Puts `value` in a named pipe that gives it out to one reader, then removes itself. The pipe is
/// removed unread after `ttl`.
pub fn hand_off(value: String, ttl: Duration) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::ffi::OsStrExt;

    let path = secrets_dir()?.join(uuid::Uuid::new_v4().simple().to_string());
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: c_path is a valid NUL-terminated path, and mkfifo only reads it.
    if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } != 0 {
        return Err(format!("Could not make a one-time pipe: {}", std::io::Error::last_os_error()));
    }
    let pipe = path.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + ttl;
        while Instant::now() < deadline {
            // Opening a pipe for writing without blocking fails until a reader has it open.
            match std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&pipe) {
                Ok(mut writer) => {
                    let _ = std::fs::remove_file(&pipe);
                    let _ = writer.write_all(value.as_bytes());
                    return;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        let _ = std::fs::remove_file(&pipe);
    });
    Ok(path)
}

#[cfg(test)]
pub struct MemoryStore(pub std::sync::Mutex<std::collections::HashMap<String, String>>);

#[cfg(test)]
impl MemoryStore {
    pub fn new() -> Self {
        MemoryStore(Default::default())
    }
}

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn set(&self, id: &str, value: &str) -> Result<(), String> {
        self.0.lock().unwrap().insert(id.into(), value.into());
        Ok(())
    }
    fn get(&self, id: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(id).cloned())
    }
    fn delete(&self, id: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(kind: Kind, name: &str, secret: Value) -> ItemInput {
        ItemInput { id: None, kind, name: name.into(), site: Some(" github.com ".into()), username: Some("me".into()), secret: secret.as_object().unwrap().clone() }
    }

    #[test]
    fn keeps_secrets_out_of_the_database() {
        let (db, store) = (Db::open_in_memory().unwrap(), MemoryStore::new());
        let login = save(&db, &store, input(Kind::Login, "GitHub", json!({ "password": "hunter2" }))).unwrap();
        assert_eq!(login.site.as_deref(), Some("github.com"));
        assert_eq!(secret(&store, &login.id).unwrap()["password"], "hunter2");
        let dump: Vec<String> = db.with(|c| {
            let mut stmt = c.prepare("SELECT id || kind || name || ifnull(site,'') || ifnull(username,'') || ifnull(hint,'') FROM vault_items")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect()
        }).unwrap();
        assert!(!dump.concat().contains("hunter2"));

        let card = save(&db, &store, input(Kind::Card, "Amex", json!({ "number": "3782 822463 10005", "expiry": "08/29" }))).unwrap();
        assert_eq!(card.hint.as_deref(), Some("•••• 0005"));
        assert_eq!(secret(&store, &card.id).unwrap()["number"], "378282246310005");

        // Editing without a secret keeps it; an empty field clears it.
        let edit = ItemInput { id: Some(card.id.clone()), secret: json!({ "expiry": "" }).as_object().unwrap().clone(), ..input(Kind::Card, "Amex Gold", json!({})) };
        assert_eq!(save(&db, &store, edit).unwrap().name, "Amex Gold");
        let kept = secret(&store, &card.id).unwrap();
        assert_eq!(kept["number"], "378282246310005");
        assert!(kept.get("expiry").is_none());

        delete(&db, &store, &login.id).unwrap();
        assert!(store.get(&login.id).unwrap().is_none());
        assert_eq!(list(&db).unwrap().len(), 1);
    }

    #[test]
    fn refuses_incomplete_or_wrong_items() {
        let (db, store) = (Db::open_in_memory().unwrap(), MemoryStore::new());
        let err = |i| save(&db, &store, i).unwrap_err();
        assert_eq!(err(input(Kind::Login, "GitHub", json!({}))), "Add the password.");
        assert_eq!(err(input(Kind::ApiKey, "OpenAI", json!({ "password": "x" }))), "A api key has no field called password.");
        assert!(err(input(Kind::Card, "Amex", json!({ "number": "1234" }))).contains("12 to 19 digits"));
        assert!(err(input(Kind::Login, " ", json!({ "password": "x" }))).contains("name"));
        assert!(list(&db).unwrap().is_empty());
        assert!(store.0.lock().unwrap().is_empty());
    }

    #[test]
    fn resolves_by_id_name_or_site() {
        let (db, store) = (Db::open_in_memory().unwrap(), MemoryStore::new());
        let item = save(&db, &store, input(Kind::Login, "GitHub", json!({ "password": "x" }))).unwrap();
        assert_eq!(resolve(&db, &item.id).unwrap().id, item.id);
        assert_eq!(resolve(&db, "github").unwrap().id, item.id);
        assert_eq!(resolve(&db, "https://www.github.com/").unwrap().id, item.id);
        assert!(resolve(&db, "gitlab").unwrap_err().contains("Nothing in the vault"));
    }

    #[test]
    fn a_hand_off_gives_the_secret_once_then_disappears() {
        let path = hand_off("s3cret".into(), Duration::from_secs(5)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "s3cret");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!path.exists());

        let unread = hand_off("late".into(), Duration::from_millis(200)).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert!(!unread.exists());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod keychain_tests {
    /// Writes, reads, and deletes one item under a test-only service in the login Keychain.
    #[test]
    #[ignore = "touches the real login Keychain"]
    fn round_trips_through_the_keychain() {
        use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};
        let (service, account) = ("dev.talix.grimoire.vault-test", "round-trip");
        set_generic_password(service, account, b"s3cret").unwrap();
        assert_eq!(get_generic_password(service, account).unwrap(), b"s3cret");
        delete_generic_password(service, account).unwrap();
        assert_eq!(get_generic_password(service, account).unwrap_err().code(), -25300);
    }
}
