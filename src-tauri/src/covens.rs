//! Covens group familiars. Each familiar sits in at most one; the rest are unbound.

use crate::db::{now_ms, Db};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Coven {
    pub id: String,
    pub name: String,
    pub created_at: i64,
}

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 {
        return Err("Give the coven a name of 1 to 40 characters.".into());
    }
    Ok(name.to_string())
}

pub fn list(db: &Db) -> Result<Vec<Coven>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached("SELECT id, name, created_at FROM covens ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| Ok(Coven { id: r.get(0)?, name: r.get(1)?, created_at: r.get(2)? }))?;
        rows.collect()
    })
}

pub fn exists(db: &Db, id: &str) -> Result<bool, String> {
    db.with(|c| c.query_row("SELECT 1 FROM covens WHERE id = ?1", params![id], |_| Ok(())).optional().map(|r| r.is_some()))
}

/// Creates a coven, or renames one when `id` is given, then seats exactly `members` in it. Members
/// leave whatever coven they were in, since a familiar sits in one at most.
pub fn save(db: &Db, id: Option<&str>, name: &str, members: &[String]) -> Result<Coven, String> {
    let name = clean_name(name)?;
    let taken = list(db)?.into_iter().any(|c| c.name.eq_ignore_ascii_case(&name) && Some(c.id.as_str()) != id);
    if taken {
        return Err(format!("There's already a coven called \"{name}\"."));
    }
    let coven = match id {
        Some(id) => {
            let mut coven = list(db)?.into_iter().find(|c| c.id == id).ok_or("That coven no longer exists.")?;
            coven.name = name;
            db.with(|c| c.execute("UPDATE covens SET name = ?2 WHERE id = ?1", params![coven.id, coven.name]).map(|_| ()))?;
            coven
        }
        None => {
            let coven = Coven { id: uuid::Uuid::new_v4().to_string(), name, created_at: now_ms() };
            db.with(|c| c.execute("INSERT INTO covens (id, name, created_at) VALUES (?1, ?2, ?3)", params![coven.id, coven.name, coven.created_at]).map(|_| ()))?;
            coven
        }
    };
    db.with(|c| {
        c.execute("UPDATE agents SET coven_id = NULL WHERE coven_id = ?1", params![coven.id])?;
        for member in members {
            c.execute("UPDATE agents SET coven_id = ?2 WHERE id = ?1", params![member, coven.id])?;
        }
        Ok(())
    })?;
    Ok(coven)
}

/// Deletes a coven. Its familiars become unbound; nothing else changes.
pub fn delete(db: &Db, id: &str) -> Result<(), String> {
    db.with(|c| {
        c.execute("UPDATE agents SET coven_id = NULL WHERE coven_id = ?1", params![id])?;
        c.execute("DELETE FROM covens WHERE id = ?1", params![id]).map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Agent, Avatar, Provider};

    fn familiar(db: &Db, id: &str) {
        db.insert_agent(&Agent {
            id: id.into(),
            name: id.into(),
            provider: Provider::Codex,
            model: None,
            workspace: format!("/tmp/{id}"),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            created_at: now_ms(),
        })
        .unwrap();
    }

    fn coven_of(db: &Db, id: &str) -> Option<String> {
        db.get_agent(id).unwrap().unwrap().coven_id
    }

    #[test]
    fn familiars_sit_in_one_coven_at_most() {
        let db = Db::open_in_memory().unwrap();
        for id in ["a", "b", "c"] {
            familiar(&db, id);
        }
        let research = save(&db, None, " Research ", &["a".into(), "b".into()]).unwrap();
        assert_eq!(research.name, "Research");
        let ops = save(&db, None, "Ops", &["b".into(), "c".into()]).unwrap();
        // b moved to Ops, so it left Research.
        assert_eq!(coven_of(&db, "a").as_deref(), Some(research.id.as_str()));
        assert_eq!(coven_of(&db, "b").as_deref(), Some(ops.id.as_str()));

        // Saving with a new member list drops the ones left out.
        save(&db, Some(&ops.id), "Operations", &["c".into()]).unwrap();
        assert_eq!(coven_of(&db, "b"), None);
        assert_eq!(list(&db).unwrap()[1].name, "Operations");

        assert!(save(&db, None, "research", &[]).unwrap_err().contains("already a coven"));
        assert!(save(&db, None, "  ", &[]).is_err());

        delete(&db, &research.id).unwrap();
        assert_eq!(coven_of(&db, "a"), None);
        assert_eq!(list(&db).unwrap().len(), 1);
    }
}
