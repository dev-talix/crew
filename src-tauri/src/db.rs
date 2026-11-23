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
    pub created_at: i64,
}

pub const SHAPES: &[&str] = &["orbit", "spark", "cloud", "drop", "ears", "classic"];
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
            shape: pick(SHAPES, 0),
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Schedule {
    Interval { minutes: u32 },
    Daily { time: String, days: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Routine {
    pub id: String,
    pub agent_id: String,
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub next_run_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStep {
    pub agent_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
    pub created_at: i64,
}

pub struct Db {
    conn: Mutex<Connection>,
}

const SCHEMA_VERSION: i64 = 5;

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
            return Err(rusqlite::Error::InvalidQuery);
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
        conn.execute(
            "UPDATE messages SET status = 'interrupted' WHERE status = 'streaming'",
            [],
        )?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let conn = self.conn.lock().map_err(|_| "database lock poisoned".to_string())?;
        f(&conn).map_err(|e| e.to_string())
    }

    pub fn list_agents(&self) -> Result<Vec<Agent>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, name, provider, model, workspace, access, session_id, created_at, avatar, effort, active_model
                 FROM agents ORDER BY created_at",
            )?;
            let rows = stmt.query_map([], row_to_agent)?;
            rows.collect()
        })
    }

    pub fn get_agent(&self, id: &str) -> Result<Option<Agent>, String> {
        self.with(|c| {
            c.prepare_cached(
                "SELECT id, name, provider, model, workspace, access, session_id, created_at, avatar, effort, active_model
                 FROM agents WHERE id = ?1",
            )?
            .query_row(params![id], row_to_agent)
            .optional()
        })
    }

    pub fn insert_agent(&self, agent: &Agent) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "INSERT INTO agents (id, name, provider, model, workspace, access, session_id, created_at, avatar, effort)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                    agent.effort
                ],
            )
            .map(|_| ())
        })
    }

    pub fn update_agent(&self, agent: &Agent) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE agents SET name = ?2, model = ?3, access = ?4, avatar = ?5, effort = ?6 WHERE id = ?1",
                params![agent.id, agent.name, agent.model, agent.access.as_str(), agent.avatar.to_column(), agent.effort],
            )
            .map(|_| ())
        })
    }

    pub fn set_active_model(&self, agent_id: &str, model: &str) -> Result<(), String> {
        self.with(|c| {
            c.execute("UPDATE agents SET active_model = ?2 WHERE id = ?1", params![agent_id, model]).map(|_| ())
        })
    }

    pub fn delete_agent(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM agents WHERE id = ?1", params![id]).map(|_| ()))
    }

    pub fn set_session(&self, agent_id: &str, session_id: Option<&str>) -> Result<(), String> {
        self.with(|c| {
            c.execute(
                "UPDATE agents SET session_id = ?2 WHERE id = ?1",
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
        let created_at = now_ms();
        self.with(|c| {
            c.execute(
                "INSERT INTO messages (agent_id, role, text, status, created_at, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![agent_id, role, text, status, created_at, source],
            )?;
            Ok(Message {
                id: c.last_insert_rowid(),
                agent_id: agent_id.to_string(),
                role: role.to_string(),
                text: text.to_string(),
                status: status.to_string(),
                source: source.map(str::to_string),
                created_at,
                updated_at: None,
            })
        })
    }

    pub fn last_messages(&self) -> Result<Vec<Message>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT m.id, m.agent_id, m.role, m.text, m.status, m.created_at, m.source, m.updated_at FROM messages m
                 JOIN (SELECT agent_id, MAX(id) AS id FROM messages WHERE role IN ('user', 'assistant') GROUP BY agent_id) last
                 ON last.id = m.id",
            )?;
            let rows = stmt.query_map([], row_to_message)?;
            rows.collect()
        })
    }

    pub fn list_routines(&self) -> Result<Vec<Routine>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, agent_id, name, prompt, schedule, enabled, last_run_at, next_run_at, created_at
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
                })
            })?;
            rows.collect()
        })
    }

    pub fn save_routine(&self, routine: &Routine) -> Result<(), String> {
        let schedule = serde_json::to_string(&routine.schedule).map_err(|e| e.to_string())?;
        self.with(|c| {
            c.execute(
                "INSERT INTO routines (id, agent_id, name, prompt, schedule, enabled, last_run_at, next_run_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET agent_id = ?2, name = ?3, prompt = ?4, schedule = ?5, enabled = ?6,
                   last_run_at = ?7, next_run_at = ?8",
                params![
                    routine.id,
                    routine.agent_id,
                    routine.name,
                    routine.prompt,
                    schedule,
                    routine.enabled,
                    routine.last_run_at,
                    routine.next_run_at,
                    routine.created_at
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
            let mut stmt = c.prepare_cached("SELECT id, name, steps, created_at FROM workflows ORDER BY created_at")?;
            let rows = stmt.query_map([], |r| {
                let steps: String = r.get(2)?;
                Ok(Workflow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    steps: serde_json::from_str(&steps).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
                    })?,
                    created_at: r.get(3)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn save_workflow(&self, workflow: &Workflow) -> Result<(), String> {
        let steps = serde_json::to_string(&workflow.steps).map_err(|e| e.to_string())?;
        self.with(|c| {
            c.execute(
                "INSERT INTO workflows (id, name, steps, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET name = ?2, steps = ?3",
                params![workflow.id, workflow.name, steps, workflow.created_at],
            )
            .map(|_| ())
        })
    }

    pub fn delete_workflow(&self, id: &str) -> Result<(), String> {
        self.with(|c| c.execute("DELETE FROM workflows WHERE id = ?1", params![id]).map(|_| ()))
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

    pub fn list_messages(&self, agent_id: &str, before_id: Option<i64>, limit: i64) -> Result<Vec<Message>, String> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, agent_id, role, text, status, created_at, source, updated_at FROM messages
                 WHERE agent_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![agent_id, before_id.unwrap_or(i64::MAX), limit], row_to_message)?;
            let mut messages: Vec<Message> = rows.collect::<rusqlite::Result<_>>()?;
            messages.reverse();
            Ok(messages)
        })
    }
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
    }

    #[test]
    fn upgrades_version_one_databases() {
        let dir = std::env::temp_dir().join(format!("crew-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("crew.db");
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
            agent_id: "a".into(),
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
            steps: vec![WorkflowStep { agent_id: "a".into(), prompt: "Write notes about {{previous}}".into() }],
            created_at: 1,
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
        let dir = std::env::temp_dir().join(format!("crew-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("crew.db");
        {
            let db = Db::open(&path).unwrap();
            db.insert_agent(&agent("a")).unwrap();
            db.insert_message("a", "assistant", "half", "streaming").unwrap();
        }
        let db = Db::open(&path).unwrap();
        assert_eq!(db.list_messages("a", None, 1).unwrap()[0].status, "interrupted");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
