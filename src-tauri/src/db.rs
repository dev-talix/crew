use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Claude,
}

impl Provider {
    fn as_str(&self) -> &'static str {
        match self {
            Provider::Codex => "codex",
            Provider::Claude => "claude",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "codex" => Ok(Provider::Codex),
            "claude" => Ok(Provider::Claude),
            other => Err(rusqlite::Error::InvalidColumnType(
                0,
                format!("unknown provider {other}"),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Workspace,
    Full,
}

impl Access {
    fn as_str(&self) -> &'static str {
        match self {
            Access::Workspace => "workspace",
            Access::Full => "full",
        }
    }

    fn parse(value: &str) -> Self {
        if value == "full" {
            Access::Full
        } else {
            Access::Workspace
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub provider: Provider,
    pub model: Option<String>,
    pub workspace: String,
    pub access: Access,
    pub session_id: Option<String>,
    pub avatar: Avatar,
    pub effort: Option<String>,
    pub active_model: Option<String>,
    /// The coven this familiar sits in, if any.
    #[serde(default)]
    pub coven_id: Option<String>,
    /// The SSH destination of the Mac it runs on. None runs it on this Mac, and then `workspace` is a
    /// local folder; otherwise `workspace` is a path on that Mac, maybe starting with ~/.
    #[serde(default)]
    pub host: Option<String>,
    pub created_at: i64,
}

pub const SHAPES: &[&str] = &[
    "orbit", "spark", "cloud", "drop", "ears", "classic", "owl", "candle", "tome", "cauldron", "crystal", "bat",
];
/// Seeded avatars keep to the first six shapes so a familiar saved without an avatar keeps its look.
const SEED_SHAPES: &[&str] = &["orbit", "spark", "cloud", "drop", "ears", "classic"];
pub const EYES: &[&str] = &["pill", "dot", "slit"];
const DEFAULT_COLORS: &[&str] = &["#4c7ef3", "#e46f4c", "#8e5bd6", "#2f9e76", "#dc9a32", "#d9577a"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Avatar {
    pub shape: String,
    pub eyes: String,
    pub color: String,
}

impl Avatar {
    pub fn validate(&self) -> Result<(), String> {
        let part = |value: &str, allowed: &[&str], name: &str| {
            if allowed.contains(&value) {
                Ok(())
            } else {
                Err(format!("Unknown avatar {name}: {value}"))
            }
        };
        part(&self.shape, SHAPES, "shape")?;
        part(&self.eyes, EYES, "eyes")?;
        let hex = self.color.strip_prefix('#').unwrap_or_default();
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("Avatar color must look like #3a7bd5, not {}", self.color));
        }
        Ok(())
    }

    pub fn for_seed(seed: &str) -> Self {
        let hash = seed.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
        let pick = |list: &[&str], shift: u32| list[((hash >> shift) as usize) % list.len()].to_string();
        Avatar {
            shape: pick(SEED_SHAPES, 0),
            eyes: pick(EYES, 5),
            color: pick(DEFAULT_COLORS, 20),
        }
    }

    fn from_column(raw: Option<String>, seed: &str) -> Self {
        raw.and_then(|r| serde_json::from_str::<Avatar>(&r).ok())
            .filter(|a| a.validate().is_ok())
            .unwrap_or_else(|| Avatar::for_seed(seed))
    }

    fn to_column(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub agent_id: String,
    pub role: String,
    pub text: String,
    pub status: String,
    pub source: Option<String>,
    pub created_at: i64,
    pub updated_at: Option<i64>,
    /// Filled in by `attachments::attach` where the window needs them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::attachments::Attachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Schedule {
    Interval { minutes: u32 },
    Daily { time: String, days: Vec<u8> },
    /// Runs when a pull request opens in a GitHub repo, "owner/name". See `triggers`.
    GithubPr { repo: String },
    /// Runs when a file, or what's directly in a folder, changes.
    File { path: String },
    /// Runs when something POSTs to the ritual's local webhook. Grimoire makes the secret.
    Webhook {
        #[serde(default)]
        secret: String,
    },
    /// Runs when new mail reaches the Mail app's inbox.
    Email,
}

impl Schedule {
    /// True for the kinds that run when something happens instead of by the clock.
    pub fn is_event(&self) -> bool {
        !matches!(self, Schedule::Interval { .. } | Schedule::Daily { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Routine {
    pub id: String,
    /// The familiar this ritual messages, or None when it casts a spell.
    pub agent_id: Option<String>,
    /// The spell this ritual casts, with `prompt` as the optional starting text.
    #[serde(default)]
    pub workflow_id: Option<String>,
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStep {
    pub agent_id: String,
    pub prompt: String,
    /// Runs alongside the previous step instead of after it.
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub check: Option<StepCheck>,
}

/// After a step, sends the spell to another step when its reply contains, or lacks, a phrase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StepCheck {
    pub when: CheckWhen,
    pub phrase: String,
    /// Index of the step to go to.
    pub go_to: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CheckWhen {
    Contains,
    Lacks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
    pub created_at: i64,
    /// The agent that created this workflow through its Grimoire tools, if any.
    #[serde(default)]
    pub created_by: Option<String>,
}

pub struct Db {
    conn: Mutex<Connection>,
}

/// One turn's tokens, for the Usage page.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    pub agent_id: String,
    pub provider: String,
    /// "you", "ritual", or "spell".
    pub origin: String,
    pub input: i64,
    pub output: i64,
    pub at: i64,
}

const SCHEMA_VERSION: i64 = 32;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            let message = "This database comes from a newer version of Grimoire. Update the app to open it.";
            return Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN), Some(message.into())));
        }
        if version < 1 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE agents (
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   provider TEXT NOT NULL,
                   model TEXT,
                   workspace TEXT NOT NULL,
                   access TEXT NOT NULL,
                   session_id TEXT,
                   created_at INTEGER NOT NULL
                 );
                 CREATE TABLE messages (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                   role TEXT NOT NULL,
                   text TEXT NOT NULL,
                   status TEXT NOT NULL,
                   created_at INTEGER NOT NULL
                 );
                 CREATE INDEX messages_by_agent ON messages(agent_id, id);
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        if version < 2 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE agents ADD COLUMN avatar TEXT;
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        }
        if version < 3 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE messages ADD COLUMN source TEXT;
                 CREATE TABLE routines (
                   id TEXT PRIMARY KEY,
                   agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                   name TEXT NOT NULL,
                   prompt TEXT NOT NULL,
                   schedule TEXT NOT NULL,
                   enabled INTEGER NOT NULL,
                   last_run_at INTEGER,
                   next_run_at INTEGER,
                   created_at INTEGER NOT NULL
                 );
                 CREATE TABLE workflows (
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   steps TEXT NOT NULL,
                   created_at INTEGER NOT NULL
                 );
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 PRAGMA user_version = 3;
                 COMMIT;",
            )?;
        }
        if version < 4 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE messages ADD COLUMN updated_at INTEGER;
                 PRAGMA user_version = 4;
                 COMMIT;",
            )?;
        }
        if version < 5 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE agents ADD COLUMN effort TEXT;
                 ALTER TABLE agents ADD COLUMN active_model TEXT;
                 PRAGMA user_version = 5;
                 COMMIT;",
            )?;
        }
        if version < 6 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE workflows ADD COLUMN created_by TEXT;
                 PRAGMA user_version = 6;
                 COMMIT;",
            )?;
        }
        if version < 7 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE vault_items (
                   id TEXT PRIMARY KEY,
                   kind TEXT NOT NULL,
                   name TEXT NOT NULL,
                   site TEXT,
                   username TEXT,
                   hint TEXT,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE vault_uses (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   item_id TEXT NOT NULL REFERENCES vault_items(id) ON DELETE CASCADE,
                   agent_id TEXT,
                   field TEXT NOT NULL,
                   outcome TEXT NOT NULL,
                   at INTEGER NOT NULL
                 );
                 CREATE TABLE grants (
                   agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                   scope TEXT NOT NULL,
                   created_at INTEGER NOT NULL,
                   PRIMARY KEY (agent_id, scope)
                 );
                 PRAGMA user_version = 7;
                 COMMIT;",
            )?;
        }
        if version < 8 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE usage (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   agent_id TEXT NOT NULL,
                   provider TEXT NOT NULL,
                   origin TEXT NOT NULL,
                   input_tokens INTEGER NOT NULL,
                   output_tokens INTEGER NOT NULL,
                   at INTEGER NOT NULL
                 );
                 CREATE INDEX usage_by_time ON usage(at);
                 PRAGMA user_version = 8;
                 COMMIT;",
            )?;
        }
        if version < 9 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE covens (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
                 ALTER TABLE agents ADD COLUMN coven_id TEXT;
                 PRAGMA user_version = 9;
                 COMMIT;",
            )?;
        }
        if version < 10 {
            crate::chats::migrate(&conn)?;
        }
        if version < 11 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE attachments (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                   name TEXT NOT NULL,
                   kind TEXT NOT NULL,
                   size INTEGER NOT NULL,
                   path TEXT NOT NULL
                 );
                 CREATE INDEX attachments_by_message ON attachments(message_id);
                 PRAGMA user_version = 11;
                 COMMIT;",
            )?;
        }
        if version < 12 {
            crate::workflow_runs::migrate(&conn)?;
        }
        if version < 13 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE budgets (
                   agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
                   tokens INTEGER NOT NULL,
                   period TEXT NOT NULL
                 );
                 CREATE TABLE ritual_skips (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   routine_id TEXT NOT NULL REFERENCES routines(id) ON DELETE CASCADE,
                   reason TEXT NOT NULL,
                   at INTEGER NOT NULL
                 );
                 CREATE INDEX usage_by_agent ON usage(agent_id, at);
                 PRAGMA user_version = 13;
                 COMMIT;",
            )?;
        }
        if version < 14 {
            crate::side::migrate(&conn)?;
        }
        if version < 15 {
            // What each event ritual's watcher has seen, so a restart doesn't run it again.
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE trigger_state (
                   routine_id TEXT PRIMARY KEY REFERENCES routines(id) ON DELETE CASCADE,
                   source TEXT NOT NULL,
                   cursor TEXT,
                   checked_at INTEGER,
                   fired_at INTEGER,
                   error TEXT
                 );
                 PRAGMA user_version = 15;
                 COMMIT;",
            )?;
        }
        if version < 16 {
            crate::inbox::migrate(&conn)?;
        }
        if version < 17 {
            crate::ledger::migrate(&conn)?;
        }
        if version < 18 {
            crate::helpers::migrate(&conn)?;
        }
        if version < 19 {
            crate::drafts::migrate(&conn)?;
        }
        if version < 20 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE agents ADD COLUMN host TEXT;
                 PRAGMA user_version = 20;
                 COMMIT;",
            )?;
        }
        if version < 21 {
            // Messages that came back while a familiar couldn't take a turn, for its next one (see runtime::hold).
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE pending_notes (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                   chat_id TEXT REFERENCES chats(id) ON DELETE CASCADE,
                   source TEXT NOT NULL,
                   text TEXT NOT NULL,
                   created_at INTEGER NOT NULL
                 );
                 CREATE INDEX pending_notes_by_agent ON pending_notes(agent_id, id);
                 PRAGMA user_version = 21;
                 COMMIT;",
            )?;
        }
        if version < 22 {
            // A remote familiar saved while its Mac was offline, whose workspace there isn't made yet.
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE agents ADD COLUMN setup_pending INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 22;
                 COMMIT;",
            )?;
        }
        if version < 23 {
            crate::handoffs::migrate(&conn)?;
        }
        if version < 24 {
            crate::handoffs::migrate_requests(&conn)?;
        }
        if version < 25 {
            // Other Macs linked over Tailscale. Their tokens live in the Keychain, not here.
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE link_peers (
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   address TEXT NOT NULL,
                   created_at INTEGER NOT NULL,
                   last_seen INTEGER,
                   version TEXT,
                   familiars TEXT NOT NULL DEFAULT '[]'
                 );
                 PRAGMA user_version = 25;
                 COMMIT;",
            )?;
        }
        if version < 26 {
            crate::handoffs::migrate_linked(&conn)?;
        }
        if version < 27 {
            crate::handoffs::migrate_remote_request(&conn)?;
        }
        if version < 28 {
            crate::connectors::migrate(&conn)?;
        }
        if version < 29 {
            crate::rooms::migrate(&conn)?;
        }
        if version < 30 {
            crate::checkins::migrate(&conn)?;
        }
        if version < 31 {
            crate::mac_apps::migrate(&conn)?;
        }
        if version < 32 {
            crate::skills::migrate(&conn)?;
        }
        conn.execute(
            "UPDATE messages SET status = 'interrupted' WHERE status = 'streaming'",
            [],
        )?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let conn = self.conn.lock().map_err(|_| "database lock poisoned".to_string())?;
        f(&conn).map_err(|e| e.to_string())
    }

    pub fn list_agents(&self) -> Result<Vec<Agent>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, name, provider, model, workspace, access, session_id, created_at, avatar, effort, active_model, coven_id, host
                 FROM agents ORDER BY created_at",
            )?;
            let rows = stmt.query_map([], row_to_agent)?;
            rows.collect()
        })
    }

    pub fn get_agent(&self, id: &str) -> Result<Option<Agent>, String> {
        self.with(|c| {
            c.prepare_cached(
                "SELECT id, name, provider, model, workspace, access, session_id, created_at, avatar, effort, active_model, coven_id, host
                 FROM agents WHERE id = ?1",
            )?
            .query_row(params![id], row_to_agent)
            .optional()
        })
    }

    pub fn insert_agent(&self, agent: &Agent) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO agents (id, name, provider, model, workspace, access, session_id, created_at, avatar, effort, coven_id, host)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    agent.id,
                    agent.name,
                    agent.provider.as_str(),
                    agent.model,
                    agent.workspace,
                    agent.access.as_str(),
                    agent.session_id,
                    agent.created_at,
                    agent.avatar.to_column(),
                    agent.effort,
                    agent.coven_id,
                    agent.host
                ],
            )
            .map(|_| ())
        })
    }

    pub fn update_agent(&self, agent: &Agent) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE agents SET name = ?2, model = ?3, access = ?4, avatar = ?5, effort = ?6, coven_id = ?7 WHERE id = ?1",
                params![agent.id, agent.name, agent.model, agent.access.as_str(), agent.avatar.to_column(), agent.effort, agent.coven_id],
            )
            .map(|_| ())
        })
    }

    /// The remote familiars whose setup waits for their Mac to connect.
    pub fn pending_setups(&self) -> Result<std::collections::HashSet<String>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached("SELECT id FROM agents WHERE setup_pending = 1")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect()
        })
    }

    pub fn set_setup_pending(&self, agent_id: &str, pending: bool) -> Result<(), String> {
        self.with(|c| c.execute("UPDATE agents SET setup_pending = ?2 WHERE id = ?1", params![agent_id, pending]).map(|_| ()))
    }

    pub fn set_active_model(&self, agent_id: &str, model: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("UPDATE agents SET active_model = ?2 WHERE id = ?1", params![agent_id, model]).map(|_| ())
        })
    }

    pub fn delete_agent(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM agents WHERE id = ?1", params![id]).map(|_| ()))
    }

    /// Saves the session on the familiar and on its current chat, so reopening the chat resumes it.
    pub fn set_session(&self, agent_id: &str, session_id: Option<&str>) -> Result<(), String> {
        self.with(|c| {
            c.execute("UPDATE agents SET session_id = ?2 WHERE id = ?1", params![agent_id, session_id])?;
            c.execute(
                "UPDATE chats SET session_id = ?2 WHERE id = (SELECT chat_id FROM agents WHERE id = ?1)",
                params![agent_id, session_id],
            )
            .map(|_| ())
        })
    }

    pub fn insert_message(&self, agent_id: &str, role: &str, text: &str, status: &str) -> Result<Message, String> {
        self.insert_message_from(agent_id, role, text, status, None)
    }

    pub fn insert_message_from(
        &self,
        agent_id: &str,
        role: &str,
        text: &str,
        status: &str,
        source: Option<&str>,
    ) -> Result<Message, String> {
        self.with(|c| {
            let chat_id = crate::chats::ensure_current(c, agent_id)?;
            insert_into(c, &chat_id, agent_id, role, text, status, source)
        })
    }

    /// Adds a message to one chat, whichever chat is current: a side conversation's, for one.
    pub fn insert_message_in(&self, chat_id: &str, agent_id: &str, role: &str, text: &str, status: &str, source: Option<&str>) -> Result<Message, String> {
        self.with(|c| insert_into(c, chat_id, agent_id, role, text, status, source))
    }

    /// Saves the CLI session on one chat only, leaving the familiar's current chat alone.
    pub fn set_chat_session(&self, chat_id: &str, session_id: &str) -> Result<(), String> {
        self.with(|c| c.execute("UPDATE chats SET session_id = ?2 WHERE id = ?1", params![chat_id, session_id]).map(|_| ()))
    }

    pub fn last_messages(&self) -> Result<Vec<Message>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT m.id, m.agent_id, m.role, m.text, m.status, m.created_at, m.source, m.updated_at FROM messages m
                 JOIN (SELECT agent_id, MAX(id) AS id FROM messages WHERE role IN ('user', 'assistant')
                       AND chat_id NOT IN (SELECT id FROM chats WHERE side_of IS NOT NULL) GROUP BY agent_id) last
                 ON last.id = m.id",
            )?;
            let rows = stmt.query_map([], row_to_message)?;
            rows.collect()
        })
    }

    pub fn list_routines(&self) -> Result<Vec<Routine>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, agent_id, name, prompt, schedule, enabled, last_run_at, next_run_at, created_at, workflow_id
                 FROM routines ORDER BY created_at",
            )?;
            let rows = stmt.query_map([], |r| {
                let schedule: String = r.get(4)?;
                Ok(Routine {
                    id: r.get(0)?,
                    agent_id: r.get(1)?,
                    name: r.get(2)?,
                    prompt: r.get(3)?,
                    schedule: serde_json::from_str(&schedule).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
                    })?,
                    enabled: r.get(5)?,
                    last_run_at: r.get(6)?,
                    next_run_at: r.get(7)?,
                    created_at: r.get(8)?,
                    workflow_id: r.get(9)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn save_routine(&self, routine: &Routine) -> Result<(), String> {
        let schedule = serde_json::to_string(&routine.schedule).map_err(|e| e.to_string())?;
        self.with(|c| {
            c.execute(
                "INSERT INTO routines (id, agent_id, name, prompt, schedule, enabled, last_run_at, next_run_at, created_at, workflow_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET agent_id = ?2, name = ?3, prompt = ?4, schedule = ?5, enabled = ?6,
                   last_run_at = ?7, next_run_at = ?8, workflow_id = ?10",
                params![
                    routine.id,
                    routine.agent_id,
                    routine.name,
                    routine.prompt,
                    schedule,
                    routine.enabled,
                    routine.last_run_at,
                    routine.next_run_at,
                    routine.created_at,
                    routine.workflow_id
                ],
            )
            .map(|_| ())
        })
    }

    pub fn delete_routine(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM routines WHERE id = ?1", params![id]).map(|_| ()))
    }

    pub fn list_workflows(&self) -> Result<Vec<Workflow>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached("SELECT id, name, steps, created_at, created_by FROM workflows ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| {
                let steps: String = r.get(2)?;
                Ok(Workflow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    steps: serde_json::from_str(&steps).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
                    })?,
                    created_at: r.get(3)?,
                    created_by: r.get(4)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn save_workflow(&self, workflow: &Workflow) -> Result<(), String> {
        let steps = serde_json::to_string(&workflow.steps).map_err(|e| e.to_string())?;
        self.with(|c| {
            c.execute(
                "INSERT INTO workflows (id, name, steps, created_at, created_by) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET name = ?2, steps = ?3",
                params![workflow.id, workflow.name, steps, workflow.created_at, workflow.created_by],
            )
            .map(|_| ())
        })
    }

    pub fn delete_workflow(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM workflows WHERE id = ?1", params![id]).map(|_| ()))
    }

    pub fn insert_usage(&self, agent_id: &str, provider: &Provider, origin: &str, input: u64, output: u64) -> Result<(), String> {
        let provider = serde_json::to_value(provider).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
        self.with(|c| {
            c.execute(
                "INSERT INTO usage (agent_id, provider, origin, input_tokens, output_tokens, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![agent_id, provider, origin, input as i64, output as i64, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn usage_since(&self, since: i64) -> Result<Vec<UsageRow>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT agent_id, provider, origin, input_tokens, output_tokens, at FROM usage WHERE at >= ?1 ORDER BY at",
            )?;
            let rows = stmt.query_map(params![since], |r| {
                Ok(UsageRow { agent_id: r.get(0)?, provider: r.get(1)?, origin: r.get(2)?, input: r.get(3)?, output: r.get(4)?, at: r.get(5)? })
            })?;
            rows.collect()
        })
    }

    /// Whether `agent_id` may do what `scope` names without asking, like "vault:<item id>".
    pub fn has_grant(&self, agent_id: &str, scope: &str) -> Result<bool, String> {
        self.with(|c| {
            c.query_row("SELECT 1 FROM grants WHERE agent_id = ?1 AND scope = ?2", params![agent_id, scope], |_| Ok(()))
                .optional()
                .map(|found| found.is_some())
        })
    }

    pub fn add_grant(&self, agent_id: &str, scope: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO grants (agent_id, scope, created_at) VALUES (?1, ?2, ?3)",
                params![agent_id, scope, now_ms()],
            )
            .map(|_| ())
        })
    }

    pub fn remove_grant(&self, agent_id: &str, scope: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM grants WHERE agent_id = ?1 AND scope = ?2", params![agent_id, scope]).map(|_| ()))
    }

    /// The familiars allowed to do what `scope` names without asking.
    pub fn grantees(&self, scope: &str) -> Result<Vec<String>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached("SELECT agent_id FROM grants WHERE scope = ?1 ORDER BY created_at")?;
            let rows = stmt.query_map(params![scope], |r| r.get(0))?;
            rows.collect()
        })
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>, String> {
        self.with(|c| {
            c.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).optional()
        })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                params![key, value],
            )
            .map(|_| ())
        })
    }

    pub fn update_message(&self, id: i64, text: &str, status: &str) -> Result<(), String> {
        self.with(|c| {
            c.prepare_cached("UPDATE messages SET text = ?2, status = ?3, updated_at = ?4 WHERE id = ?1")?
                .execute(params![id, text, status, now_ms()])
                .map(|_| ())
        })
    }

    /// A page of the familiar's current chat, oldest first.
    pub fn list_messages(&self, agent_id: &str, before_id: Option<i64>, limit: i64) -> Result<Vec<Message>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, agent_id, role, text, status, created_at, source, updated_at FROM messages
                 WHERE chat_id = (SELECT chat_id FROM agents WHERE id = ?1) AND id < ?2 ORDER BY id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![agent_id, before_id.unwrap_or(i64::MAX), limit], row_to_message)?;
            let mut messages: Vec<Message> = rows.collect::<rusqlite::Result<_>>()?;
            messages.reverse();
            Ok(messages)
        })
    }
}

fn insert_into(
    c: &Connection,
    chat_id: &str,
    agent_id: &str,
    role: &str,
    text: &str,
    status: &str,
    source: Option<&str>,
) -> rusqlite::Result<Message> {
    let created_at = now_ms();
    c.execute(
        "INSERT INTO messages (agent_id, role, text, status, created_at, source, chat_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![agent_id, role, text, status, created_at, source, chat_id],
    )?;
    crate::chats::touch(c, chat_id, role, text, created_at)?;
    Ok(Message {
        id: c.last_insert_rowid(),
        agent_id: agent_id.to_string(),
        role: role.to_string(),
        text: text.to_string(),
        status: status.to_string(),
        source: source.map(str::to_string),
        created_at,
        updated_at: None,
        attachments: Vec::new(),
    })
}

fn row_to_message(r: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        role: r.get(2)?,
        text: r.get(3)?,
        status: r.get(4)?,
        created_at: r.get(5)?,
        source: r.get(6)?,
        updated_at: r.get(7)?,
        attachments: Vec::new(),
    })
}

fn row_to_agent(r: &rusqlite::Row) -> rusqlite::Result<Agent> {
    let provider: String = r.get(2)?;
    let access: String = r.get(5)?;
    let id: String = r.get(0)?;
    let avatar = Avatar::from_column(r.get(8)?, &id);
    Ok(Agent {
        id,
        name: r.get(1)?,
        provider: Provider::parse(&provider)?,
        model: r.get(3)?,
        workspace: r.get(4)?,
        access: Access::parse(&access),
        session_id: r.get(6)?,
        created_at: r.get(7)?,
        avatar,
        effort: r.get(9)?,
        active_model: r.get(10)?,
        coven_id: r.get(11)?,
        host: r.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str) -> Agent {
        Agent {
            id: id.into(),
            name: "Ada".into(),
            provider: Provider::Claude,
            model: None,
            workspace: "/tmp/ada".into(),
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

    #[test]
    fn saves_and_validates_avatars() {
        let db = Db::open_in_memory().unwrap();
        let mut a = agent("a");
        a.avatar = Avatar { shape: "cloud".into(), eyes: "slit".into(), color: "#AA33cc".into() };
        db.insert_agent(&a).unwrap();
        assert_eq!(db.get_agent("a").unwrap().unwrap().avatar, a.avatar);
        db.set_active_model("a", "claude-opus-5-5").unwrap();
        assert_eq!(db.get_agent("a").unwrap().unwrap().active_model.as_deref(), Some("claude-opus-5-5"));
        a.avatar.color = "red".into();
        assert!(a.avatar.validate().is_err());
        a.avatar.color = "#123456".into();
        a.avatar.shape = "hex".into();
        assert!(a.avatar.validate().is_err());
        assert!(Avatar::for_seed("anything").validate().is_ok());
        a.avatar.shape = "cauldron".into();
        assert!(a.avatar.validate().is_ok());
        for seed in ["a", "b", "ada", "old", "zed", "familiar"] {
            assert!(SEED_SHAPES.contains(&Avatar::for_seed(seed).shape.as_str()));
        }
    }

    #[test]
    fn upgrades_version_one_databases() {
        let dir = std::env::temp_dir().join(format!("grimoire-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grimoire.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE agents (id TEXT PRIMARY KEY, name TEXT NOT NULL, provider TEXT NOT NULL, model TEXT,
                   workspace TEXT NOT NULL, access TEXT NOT NULL, session_id TEXT, created_at INTEGER NOT NULL);
                 CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                   role TEXT NOT NULL, text TEXT NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL);
                 CREATE INDEX messages_by_agent ON messages(agent_id, id);
                 INSERT INTO agents VALUES ('old', 'Old', 'codex', NULL, '/tmp', 'workspace', 's9', 1);
                 INSERT INTO messages (agent_id, role, text, status, created_at) VALUES ('old', 'user', 'hi', 'done', 1);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let old = db.get_agent("old").unwrap().unwrap();
        assert_eq!(old.session_id.as_deref(), Some("s9"));
        assert_eq!(old.avatar, Avatar::for_seed("old"));
        assert_eq!(db.list_messages("old", None, 5).unwrap()[0].text, "hi");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stores_routines_workflows_and_settings() {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("a")).unwrap();
        let mut routine = Routine {
            id: "r1".into(),
            agent_id: Some("a".into()),
            workflow_id: None,
            name: "Morning check".into(),
            prompt: "Check the build".into(),
            schedule: Schedule::Daily { time: "09:00".into(), days: vec![1, 2, 3, 4, 5] },
            enabled: true,
            last_run_at: None,
            next_run_at: Some(5),
            created_at: 1,
        };
        db.save_routine(&routine).unwrap();
        routine.enabled = false;
        db.save_routine(&routine).unwrap();
        assert_eq!(db.list_routines().unwrap(), vec![routine]);

        let workflow = Workflow {
            id: "w1".into(),
            name: "Ship".into(),
            steps: vec![WorkflowStep { agent_id: "a".into(), prompt: "Write notes about {{previous}}".into(), ..Default::default() }],
            created_at: 1,
            created_by: Some("a".into()),
        };
        db.save_workflow(&workflow).unwrap();
        assert_eq!(db.list_workflows().unwrap(), vec![workflow]);

        assert_eq!(db.setting("claude_billing").unwrap(), None);
        db.set_setting("claude_billing", "api").unwrap();
        db.set_setting("claude_billing", "subscription").unwrap();
        assert_eq!(db.setting("claude_billing").unwrap().as_deref(), Some("subscription"));

        db.insert_message_from("a", "user", "Check the build", "done", Some("Morning check")).unwrap();
        assert_eq!(db.last_messages().unwrap()[0].source.as_deref(), Some("Morning check"));
        db.delete_agent("a").unwrap();
        assert!(db.list_routines().unwrap().is_empty());
    }

    #[test]
    fn saves_the_host_of_a_remote_familiar() {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("local")).unwrap();
        let remote = Agent { host: Some("me@studio.local".into()), workspace: "~/Grimoire/r".into(), ..agent("remote") };
        db.insert_agent(&remote).unwrap();
        assert_eq!(db.get_agent("local").unwrap().unwrap().host, None);
        let saved = db.get_agent("remote").unwrap().unwrap();
        assert_eq!((saved.host.as_deref(), saved.workspace.as_str()), (Some("me@studio.local"), "~/Grimoire/r"));
        db.update_agent(&Agent { name: "Renamed".into(), ..saved }).unwrap();
        assert_eq!(db.list_agents().unwrap()[1].host.as_deref(), Some("me@studio.local"), "edits keep the host");
    }

    #[test]
    fn an_existing_database_gains_pending_setups() {
        let dir = std::env::temp_dir().join(format!("grimoire-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grimoire.db");
        {
            let db = Db::open(&path).unwrap();
            db.insert_agent(&agent("a")).unwrap();
        }
        // Roll the file back to version 21, as the previous release left it.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "DROP TABLE agent_skills; DROP TABLE skills; DROP TABLE agent_connectors; DROP TABLE connectors;
                 ALTER TABLE agents DROP COLUMN setup_pending; ALTER TABLE chats DROP COLUMN handoff; DROP TABLE handoffs; DROP TABLE link_peers;
                 ALTER TABLE chats DROP COLUMN remote_peer; ALTER TABLE chats DROP COLUMN remote_asker; ALTER TABLE chats DROP COLUMN remote_name;
                 DROP TABLE chat_reviews; DROP TABLE checkin_runs; DROP TABLE checkins;
                 DROP TABLE room_messages; DROP TABLE room_members; DROP TABLE rooms;
                 DROP TABLE agent_mac_apps;
                 PRAGMA user_version = 21;",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let version: i64 = db.with(|c| c.pragma_query_value(None, "user_version", |r| r.get(0))).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(db.pending_setups().unwrap().is_empty(), "existing familiars aren't pending");
        db.set_setup_pending("a", true).unwrap();
        assert!(db.pending_setups().unwrap().contains("a"));
        db.set_setup_pending("a", false).unwrap();
        assert!(db.pending_setups().unwrap().is_empty());
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_existing_database_keeps_its_handoffs_and_gains_linked_ones() {
        let dir = std::env::temp_dir().join(format!("grimoire-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grimoire.db");
        let chat = {
            let db = Db::open(&path).unwrap();
            db.insert_agent(&agent("a")).unwrap();
            db.insert_agent(&agent("b")).unwrap();
            let chat = crate::side::create(&db, "a", "b", "Old question").unwrap().id;
            crate::handoffs::record(&db, &chat, "ask", "Old question", crate::handoffs::Status::Done).unwrap();
            chat
        };
        // Roll the file back to version 25, as the previous release left it.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "DROP TABLE agent_skills; DROP TABLE skills; DROP TABLE agent_connectors; DROP TABLE connectors;
                 ALTER TABLE chats DROP COLUMN remote_peer; ALTER TABLE chats DROP COLUMN remote_asker; ALTER TABLE chats DROP COLUMN remote_name;
                 CREATE TABLE old (id INTEGER PRIMARY KEY AUTOINCREMENT, chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
                   kind TEXT NOT NULL, text TEXT NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL);
                 INSERT INTO old SELECT id, chat_id, kind, text, status, created_at FROM handoffs;
                 DROP TABLE handoffs; ALTER TABLE old RENAME TO handoffs; CREATE INDEX handoffs_by_chat ON handoffs(chat_id);
                 DROP TABLE chat_reviews; DROP TABLE checkin_runs; DROP TABLE checkins;
                 DROP TABLE room_messages; DROP TABLE room_members; DROP TABLE rooms;
                 DROP TABLE agent_mac_apps;
                 PRAGMA user_version = 25;",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let version: i64 = db.with(|c| c.pragma_query_value(None, "user_version", |r| r.get(0))).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let list = crate::handoffs::list(&db, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].chat_id.as_str(), list[0].from_id.as_str(), list[0].to_id.as_str(), list[0].what.as_str()), (chat.as_str(), "a", "b", "Old question"));
        assert_eq!(crate::side::get(&db, &chat).unwrap().unwrap().asker_id, "a");
        let remote = crate::side::create_remote(&db, "mac-b", "ada", "Ada", "b", "New question").unwrap();
        assert_eq!(remote.asker_id, "peer:mac-b:ada");
        // Deleting the asked familiar takes its handoffs with it, as before.
        crate::handoffs::record(&db, &remote.id, "ask", "New question", crate::handoffs::Status::Waiting).unwrap();
        db.delete_agent("b").unwrap();
        assert!(crate::handoffs::list(&db, 10).unwrap().is_empty());
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pages_messages_oldest_first() {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("a")).unwrap();
        let ids: Vec<i64> = (0..5)
            .map(|i| db.insert_message("a", "user", &i.to_string(), "done").unwrap().id)
            .collect();
        let last_two = db.list_messages("a", None, 2).unwrap();
        assert_eq!(last_two.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["3", "4"]);
        let older = db.list_messages("a", Some(ids[3]), 10).unwrap();
        assert_eq!(older.len(), 3);
        assert_eq!(older[0].text, "0");
    }

    #[test]
    fn deleting_agent_removes_its_messages() {
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&agent("a")).unwrap();
        db.insert_message("a", "user", "hi", "done").unwrap();
        db.delete_agent("a").unwrap();
        assert!(db.list_messages("a", None, 10).unwrap().is_empty());
    }

    #[test]
    fn reopening_marks_streaming_messages_interrupted() {
        let dir = std::env::temp_dir().join(format!("grimoire-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grimoire.db");
        {
            let db = Db::open(&path).unwrap();
            db.insert_agent(&agent("a")).unwrap();
            db.insert_message("a", "assistant", "half", "streaming").unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.list_messages("a", None, 1).unwrap()[0].status, "interrupted");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_database_from_a_newer_app_asks_for_an_update() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        let error = Db::init(conn).err().unwrap();
        assert_eq!(error.to_string(), "This database comes from a newer version of Grimoire. Update the app to open it.");
    }
}
