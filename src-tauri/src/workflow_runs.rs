//! The history of spell casts: one row per cast and one per step it ran, with the prompt actually sent and
//! the reply. In code a spell is a workflow and a ritual is a routine.

use crate::db::{now_ms, Db};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// Schema version 12: run history, and rituals that cast a spell instead of messaging a familiar.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    // A ritual's familiar becomes optional, which SQLite only allows by rebuilding the table.
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE routines_v12 (
           id TEXT PRIMARY KEY,
           agent_id TEXT REFERENCES agents(id) ON DELETE CASCADE,
           workflow_id TEXT REFERENCES workflows(id) ON DELETE CASCADE,
           name TEXT NOT NULL,
           prompt TEXT NOT NULL,
           schedule TEXT NOT NULL,
           enabled INTEGER NOT NULL,
           last_run_at INTEGER,
           next_run_at INTEGER,
           created_at INTEGER NOT NULL
         );
         INSERT INTO routines_v12 (id, agent_id, name, prompt, schedule, enabled, last_run_at, next_run_at, created_at)
           SELECT id, agent_id, name, prompt, schedule, enabled, last_run_at, next_run_at, created_at FROM routines;
         DROP TABLE routines;
         ALTER TABLE routines_v12 RENAME TO routines;
         CREATE TABLE workflow_runs (
           id TEXT PRIMARY KEY,
           workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
           input TEXT NOT NULL,
           ritual TEXT,
           state TEXT NOT NULL,
           error TEXT,
           started_at INTEGER NOT NULL,
           ended_at INTEGER
         );
         CREATE INDEX workflow_runs_by_workflow ON workflow_runs(workflow_id, started_at);
         CREATE TABLE workflow_run_steps (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
           step INTEGER NOT NULL,
           agent_id TEXT NOT NULL,
           prompt TEXT NOT NULL,
           reply TEXT,
           state TEXT NOT NULL,
           error TEXT,
           went_to INTEGER,
           started_at INTEGER NOT NULL,
           ended_at INTEGER
         );
         CREATE INDEX workflow_run_steps_by_run ON workflow_run_steps(run_id, id);
         PRAGMA user_version = 12;
         COMMIT;",
    )
}

/// One cast of a spell.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub workflow_id: String,
    pub input: String,
    /// The ritual that cast it, by name, or None when you did.
    pub ritual: Option<String>,
    /// "running", "done", "failed", or "stopped".
    pub state: String,
    pub error: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

/// One step a cast ran. A step that a check sent the spell back to shows up once per time it ran.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunStep {
    pub id: i64,
    /// Index of the step in the spell.
    pub step: usize,
    pub agent_id: String,
    /// The prompt as sent, with {{input}} and {{previous}} filled in.
    pub prompt: String,
    pub reply: Option<String>,
    pub state: String,
    pub error: Option<String>,
    /// Index of the step this one's check sent the spell to, when it matched.
    pub went_to: Option<usize>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: Run,
    pub steps: Vec<RunStep>,
}

/// How many recent casts each spell keeps on the Spells page.
pub const RECENT_PER_SPELL: i64 = 5;

pub fn start(db: &Db, workflow_id: &str, input: &str, ritual: Option<&str>) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    db.with(|c| {
        c.execute(
            "INSERT INTO workflow_runs (id, workflow_id, input, ritual, state, started_at) VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
            params![id, workflow_id, input, ritual, now_ms()],
        )
    })?;
    Ok(id)
}

pub fn finish(db: &Db, run_id: &str, state: &str, error: Option<&str>) -> Result<(), String> {
    db.with(|c| {
        c.execute(
            "UPDATE workflow_runs SET state = ?2, error = ?3, ended_at = ?4 WHERE id = ?1",
            params![run_id, state, error, now_ms()],
        )
        .map(|_| ())
    })
}

pub fn start_step(db: &Db, run_id: &str, step: usize, agent_id: &str, prompt: &str) -> Result<i64, String> {
    db.with(|c| {
        c.execute(
            "INSERT INTO workflow_run_steps (run_id, step, agent_id, prompt, state, started_at) VALUES (?1, ?2, ?3, ?4, 'running', ?5)",
            params![run_id, step as i64, agent_id, prompt, now_ms()],
        )?;
        Ok(c.last_insert_rowid())
    })
}

pub fn finish_step(db: &Db, id: i64, state: &str, reply: Option<&str>, error: Option<&str>) -> Result<(), String> {
    db.with(|c| {
        c.execute(
            "UPDATE workflow_run_steps SET state = ?2, reply = ?3, error = ?4, ended_at = ?5 WHERE id = ?1",
            params![id, state, reply, error, now_ms()],
        )
        .map(|_| ())
    })
}

pub fn set_went_to(db: &Db, id: i64, step: usize) -> Result<(), String> {
    db.with(|c| c.execute("UPDATE workflow_run_steps SET went_to = ?2 WHERE id = ?1", params![id, step as i64]).map(|_| ()))
}

/// Casts that were running when Grimoire last quit can't finish, so they and their steps end as stopped.
pub fn close_unfinished(db: &Db) -> Result<(), String> {
    let now = now_ms();
    db.with(|c| {
        c.execute(
            "UPDATE workflow_run_steps SET state = 'stopped', ended_at = ?1 WHERE state = 'running'",
            params![now],
        )?;
        c.execute(
            "UPDATE workflow_runs SET state = 'stopped', error = 'Grimoire closed before this cast finished.', ended_at = ?1
             WHERE state = 'running'",
            params![now],
        )
        .map(|_| ())
    })
}

fn row_to_run(r: &rusqlite::Row) -> rusqlite::Result<Run> {
    Ok(Run {
        id: r.get(0)?,
        workflow_id: r.get(1)?,
        input: r.get(2)?,
        ritual: r.get(3)?,
        state: r.get(4)?,
        error: r.get(5)?,
        started_at: r.get(6)?,
        ended_at: r.get(7)?,
    })
}

const RUN_COLUMNS: &str = "id, workflow_id, input, ritual, state, error, started_at, ended_at";

/// The latest casts of every spell, newest first, at most `per_spell` each.
pub fn recent(db: &Db, per_spell: i64) -> Result<Vec<Run>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM (
               SELECT *, rowid AS seq, ROW_NUMBER() OVER (PARTITION BY workflow_id ORDER BY started_at DESC, rowid DESC) AS n FROM workflow_runs
             ) WHERE n <= ?1 ORDER BY started_at DESC, seq DESC"
        ))?;
        let rows = stmt.query_map(params![per_spell], row_to_run)?;
        rows.collect()
    })
}

pub fn detail(db: &Db, run_id: &str) -> Result<Option<RunDetail>, String> {
    db.with(|c| {
        let Some(run) = c
            .prepare_cached(&format!("SELECT {RUN_COLUMNS} FROM workflow_runs WHERE id = ?1"))?
            .query_row(params![run_id], row_to_run)
            .optional()?
        else {
            return Ok(None);
        };
        let mut stmt = c.prepare_cached(
            "SELECT id, step, agent_id, prompt, reply, state, error, went_to, started_at, ended_at
             FROM workflow_run_steps WHERE run_id = ?1 ORDER BY id",
        )?;
        let steps = stmt
            .query_map(params![run_id], |r| {
                Ok(RunStep {
                    id: r.get(0)?,
                    step: r.get::<_, i64>(1)? as usize,
                    agent_id: r.get(2)?,
                    prompt: r.get(3)?,
                    reply: r.get(4)?,
                    state: r.get(5)?,
                    error: r.get(6)?,
                    went_to: r.get::<_, Option<i64>>(7)?.map(|n| n as usize),
                    started_at: r.get(8)?,
                    ended_at: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(RunDetail { run, steps }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Workflow, WorkflowStep};

    fn spell(db: &Db, id: &str) {
        db.save_workflow(&Workflow {
            id: id.into(),
            name: id.into(),
            steps: vec![WorkflowStep { agent_id: "a".into(), prompt: "go".into(), ..Default::default() }],
            created_at: 0,
            created_by: None,
        })
        .unwrap();
    }

    #[test]
    fn upgrades_version_nine_rituals_and_spells() {
        let dir = std::env::temp_dir().join(format!("grimoire-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grimoire.db");
        {
            // The schema as of version 9, with a ritual and a spell saved before steps had checks.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE agents (id TEXT PRIMARY KEY, name TEXT NOT NULL, provider TEXT NOT NULL, model TEXT,
                   workspace TEXT NOT NULL, access TEXT NOT NULL, session_id TEXT, created_at INTEGER NOT NULL, avatar TEXT,
                   effort TEXT, active_model TEXT, coven_id TEXT);
                 CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                   role TEXT NOT NULL, text TEXT NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL, source TEXT, updated_at INTEGER);
                 CREATE INDEX messages_by_agent ON messages(agent_id, id);
                 CREATE TABLE routines (id TEXT PRIMARY KEY, agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
                   name TEXT NOT NULL, prompt TEXT NOT NULL, schedule TEXT NOT NULL, enabled INTEGER NOT NULL, last_run_at INTEGER,
                   next_run_at INTEGER, created_at INTEGER NOT NULL);
                 CREATE TABLE workflows (id TEXT PRIMARY KEY, name TEXT NOT NULL, steps TEXT NOT NULL, created_at INTEGER NOT NULL, created_by TEXT);
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE vault_items (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, site TEXT, username TEXT, hint TEXT,
                   created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
                 CREATE TABLE vault_uses (id INTEGER PRIMARY KEY AUTOINCREMENT, item_id TEXT NOT NULL REFERENCES vault_items(id) ON DELETE CASCADE,
                   agent_id TEXT, field TEXT NOT NULL, outcome TEXT NOT NULL, at INTEGER NOT NULL);
                 CREATE TABLE grants (agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE, scope TEXT NOT NULL,
                   created_at INTEGER NOT NULL, PRIMARY KEY (agent_id, scope));
                 CREATE TABLE usage (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_id TEXT NOT NULL, provider TEXT NOT NULL, origin TEXT NOT NULL,
                   input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL, at INTEGER NOT NULL);
                 CREATE INDEX usage_by_time ON usage(at);
                 CREATE TABLE covens (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at INTEGER NOT NULL);
                 INSERT INTO agents (id, name, provider, workspace, access, created_at) VALUES ('a', 'Ada', 'codex', '/tmp', 'workspace', 1);
                 INSERT INTO routines VALUES ('r', 'a', 'Morning', 'Check the build', '{\"kind\":\"interval\",\"minutes\":60}', 1, 5, 9, 1);
                 INSERT INTO workflows VALUES ('w', 'Brief', '[{\"agentId\":\"a\",\"prompt\":\"Research {{input}}\"}]', 1, NULL);
                 PRAGMA user_version = 9;",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let routine = &db.list_routines().unwrap()[0];
        assert_eq!((routine.agent_id.as_deref(), routine.workflow_id.as_deref()), (Some("a"), None));
        assert_eq!((routine.prompt.as_str(), routine.last_run_at, routine.next_run_at), ("Check the build", Some(5), Some(9)));
        let spell = &db.list_workflows().unwrap()[0];
        assert_eq!(spell.steps, vec![WorkflowStep { agent_id: "a".into(), prompt: "Research {{input}}".into(), ..Default::default() }]);
        let run = start(&db, "w", "", None).unwrap();
        assert_eq!(recent(&db, 5).unwrap()[0].id, run);
        // Deleting the familiar still deletes its rituals.
        db.delete_agent("a").unwrap();
        assert!(db.list_routines().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_recent_casts_per_spell_and_closes_unfinished_ones() {
        let db = Db::open_in_memory().unwrap();
        spell(&db, "w1");
        spell(&db, "w2");
        for i in 0..7 {
            let id = start(&db, "w1", &format!("cast {i}"), None).unwrap();
            finish(&db, &id, "done", None).unwrap();
        }
        let live = start(&db, "w2", "news", Some("Morning brief")).unwrap();
        let step = start_step(&db, &live, 0, "a", "Research news").unwrap();

        let recent = recent(&db, 5).unwrap();
        assert_eq!(recent.iter().filter(|r| r.workflow_id == "w1").count(), 5);
        assert_eq!(recent.iter().find(|r| r.workflow_id == "w1").unwrap().input, "cast 6");
        assert_eq!(recent.iter().find(|r| r.id == live).unwrap().ritual.as_deref(), Some("Morning brief"));

        close_unfinished(&db).unwrap();
        let after = detail(&db, &live).unwrap().unwrap();
        assert_eq!(after.run.state, "stopped");
        assert!(after.run.error.unwrap().contains("closed"));
        assert_eq!(after.steps[0].id, step);
        assert_eq!(after.steps[0].state, "stopped");

        db.delete_workflow("w2").unwrap();
        assert_eq!(detail(&db, &live).unwrap(), None);
    }
}
