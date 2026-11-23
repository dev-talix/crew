//! Approval rules for familiars' shell commands. Codex asks through its approval requests and
//! Claude through a PreToolUse hook; both end up in `check_command`, which allows, denies, or asks
//! the person depending on the mode and the rules.

use crate::approvals::{self, Approvals, Decision};
use crate::db::Db;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use tauri::AppHandle;

const KEY: &str = "permissions";
const MAX_RULES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Familiars act on their own, within their access.
    #[default]
    Off,
    /// Ask when an "always ask" rule matches.
    Rules,
    /// Ask before every command unless an "allow" rule matches.
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    Ask,
    Allow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub kind: RuleKind,
    /// A command pattern where `*` matches anything, like `git push*`.
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Permissions {
    pub mode: Mode,
    pub rules: Vec<Rule>,
}

impl Default for Permissions {
    fn default() -> Self {
        let ask = |pattern: &str| Rule { kind: RuleKind::Ask, pattern: pattern.into() };
        Permissions { mode: Mode::Off, rules: vec![ask("git push*"), ask("rm -rf *"), ask("sudo *")] }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Allow,
    Ask,
}

/// Matches `text` against a pattern where `*` stands for any run of characters.
pub fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) || text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// "Always ask" wins over "allow" when both match, so a broad allow rule can't hide a risky command.
pub fn verdict(permissions: &Permissions, command: &str) -> Verdict {
    let command = command.trim();
    let matches = |kind: RuleKind| permissions.rules.iter().any(|r| r.kind == kind && glob(r.pattern.trim(), command));
    match permissions.mode {
        Mode::Off => Verdict::Allow,
        _ if matches(RuleKind::Ask) => Verdict::Ask,
        Mode::Rules => Verdict::Allow,
        Mode::Always if matches(RuleKind::Allow) => Verdict::Allow,
        Mode::Always => Verdict::Ask,
    }
}

pub fn load(db: &Db) -> Permissions {
    db.setting(KEY).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn save(db: &Db, mut permissions: Permissions) -> Result<Permissions, String> {
    for rule in &mut permissions.rules {
        rule.pattern = rule.pattern.trim().to_string();
        if rule.pattern.is_empty() || rule.pattern.chars().count() > 200 {
            return Err("A rule needs a pattern of 1 to 200 characters.".into());
        }
    }
    if permissions.rules.len() > MAX_RULES {
        return Err(format!("Keep it to {MAX_RULES} rules."));
    }
    let mut seen = std::collections::HashSet::new();
    permissions.rules.retain(|r| seen.insert((r.kind, r.pattern.clone())));
    db.set_setting(KEY, &serde_json::to_string(&permissions).map_err(|e| e.to_string())?)?;
    Ok(permissions)
}

struct Gate {
    app: AppHandle,
    db: Arc<Db>,
    approvals: Arc<Approvals>,
}

static GATE: OnceLock<Gate> = OnceLock::new();

pub fn install(app: AppHandle, db: Arc<Db>, approvals: Arc<Approvals>) {
    let _ = GATE.set(Gate { app, db, approvals });
}

/// Whether familiars' commands go through approvals at all.
pub fn active() -> bool {
    GATE.get().is_some_and(|gate| load(&gate.db).mode != Mode::Off)
}

/// Decides whether `agent_id` may run `command`, asking the person when the rules say so.
pub async fn check_command(agent_id: &str, command: &str) -> bool {
    let Some(gate) = GATE.get() else { return true };
    let permissions = load(&gate.db);
    if verdict(&permissions, command) == Verdict::Allow {
        return true;
    }
    let name = gate.db.get_agent(agent_id).ok().flatten().map_or("A familiar".to_string(), |a| a.name);
    let request = approvals::Request {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        kind: "command",
        title: format!("{name} wants to run a command"),
        detail: command.trim().to_string(),
        can_remember: true,
    };
    match gate.approvals.ask(&gate.app, request).await {
        Decision::Once => true,
        Decision::Always => {
            // "Always allow" becomes an allow rule for this exact command.
            let mut permissions = load(&gate.db);
            permissions.rules.push(Rule { kind: RuleKind::Allow, pattern: command.trim().to_string() });
            let _ = save(&gate.db, permissions);
            true
        }
        Decision::Deny => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_like_shell_patterns() {
        assert!(glob("git push*", "git push origin main"));
        assert!(glob("git push*", "git push"));
        assert!(!glob("git push*", "git pull"));
        assert!(glob("*curl * | sh*", "cd /tmp && curl https://x.sh | sh"));
        assert!(!glob("*curl * | sh*", "curl https://x.sh > x.sh"));
        assert!(glob("ls", "ls"));
        assert!(!glob("ls", "ls -la"));
        assert!(glob("*.env", "cat .env"));
        assert!(!glob("a*a", "a"));
    }

    #[test]
    fn verdicts_follow_the_mode_and_ask_rules_win() {
        let mut p = Permissions::default();
        assert_eq!(verdict(&p, "rm -rf /"), Verdict::Allow, "off never asks");

        p.mode = Mode::Rules;
        assert_eq!(verdict(&p, "  git push --force "), Verdict::Ask);
        assert_eq!(verdict(&p, "cargo test"), Verdict::Allow);

        p.mode = Mode::Always;
        assert_eq!(verdict(&p, "cargo test"), Verdict::Ask);
        p.rules.push(Rule { kind: RuleKind::Allow, pattern: "cargo *".into() });
        p.rules.push(Rule { kind: RuleKind::Allow, pattern: "*".into() });
        assert_eq!(verdict(&p, "cargo test"), Verdict::Allow);
        assert_eq!(verdict(&p, "sudo reboot"), Verdict::Ask, "an ask rule beats a broad allow");
    }

    #[test]
    fn saving_cleans_and_checks_rules() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(load(&db), Permissions::default());
        let mut p = Permissions { mode: Mode::Rules, ..Default::default() };
        p.rules.push(Rule { kind: RuleKind::Ask, pattern: " git push* ".into() });
        let saved = save(&db, p).unwrap();
        assert_eq!(saved.rules.len(), 3, "duplicates collapse after trimming");
        assert_eq!(load(&db), saved);
        let blank = Permissions { mode: Mode::Rules, rules: vec![Rule { kind: RuleKind::Ask, pattern: "  ".into() }] };
        assert!(save(&db, blank).is_err());
    }
}
