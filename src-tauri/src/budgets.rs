//! Token budgets per familiar, and the plan limit guard that pauses rituals. Runtime enqueue asks
//! `check_turn` before every turn; the ritual scheduler asks `ritual_skip` before each run.

use crate::db::{now_ms, Agent, Db, Provider, Routine};
use crate::runtime::{Runtime, CLAUDE_LIMITS};
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, TimeZone};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

/// The settings key for the latest Codex plan limit reading, kept so Home can tell without starting Codex.
pub const CODEX_LIMITS: &str = "codex_limits";
const PLAN_GUARD: &str = "plan_guard";
const MAX_TOKENS: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Period {
    Day,
    Week,
}

impl Period {
    fn as_str(self) -> &'static str {
        match self {
            Period::Day => "day",
            Period::Week => "week",
        }
    }

    fn parse(value: &str) -> Self {
        if value == "week" { Period::Week } else { Period::Day }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Budget {
    pub tokens: u64,
    pub period: Period,
}

impl Budget {
    pub fn validate(&self) -> Result<(), String> {
        if self.tokens == 0 || self.tokens > MAX_TOKENS {
            return Err("Set the budget to at least 1 token, or turn it off.".into());
        }
        Ok(())
    }
}

/// A familiar's budget and how much of it the current window has used.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BudgetStatus {
    pub agent_id: String,
    pub tokens: u64,
    pub period: Period,
    pub used: u64,
    pub resets_at: i64,
}

impl BudgetStatus {
    pub fn over(&self) -> bool {
        self.used >= self.tokens
    }
}

pub fn get(db: &Db, agent_id: &str) -> Result<Option<Budget>, String> {
    db.with(|c| {
        c.query_row("SELECT tokens, period FROM budgets WHERE agent_id = ?1", params![agent_id], |r| {
            let period: String = r.get(1)?;
            Ok(Budget { tokens: r.get::<_, i64>(0)? as u64, period: Period::parse(&period) })
        })
        .optional()
    })
}

/// Sets a familiar's budget, or removes it with None.
pub fn set(db: &Db, agent_id: &str, budget: Option<&Budget>) -> Result<(), String> {
    match budget {
        Some(budget) => {
            budget.validate()?;
            db.with(|c| {
                c.execute(
                    "INSERT INTO budgets (agent_id, tokens, period) VALUES (?1, ?2, ?3)
                     ON CONFLICT(agent_id) DO UPDATE SET tokens = ?2, period = ?3",
                    params![agent_id, budget.tokens as i64, budget.period.as_str()],
                )
                .map(|_| ())
            })
        }
        None => db.with(|c| c.execute("DELETE FROM budgets WHERE agent_id = ?1", params![agent_id]).map(|_| ())),
    }
}

/// Writes a budget inside an open transaction, for imports.
pub fn insert_on(c: &rusqlite::Connection, agent_id: &str, budget: &Budget) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO budgets (agent_id, tokens, period) VALUES (?1, ?2, ?3)",
        params![agent_id, budget.tokens as i64, budget.period.as_str()],
    )
    .map(|_| ())
}

/// When the window holding `now` started: local midnight today, or local midnight on this week's Monday.
pub fn window_start<Tz: TimeZone>(period: Period, now: &DateTime<Tz>) -> DateTime<Tz> {
    let back = match period {
        Period::Day => 0,
        Period::Week => i64::from(now.weekday().num_days_from_monday()),
    };
    let date = now.date_naive() - ChronoDuration::days(back);
    now.timezone().from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap()).earliest().unwrap_or_else(|| now.clone())
}

pub fn window_end<Tz: TimeZone>(period: Period, now: &DateTime<Tz>) -> DateTime<Tz> {
    let start = window_start(period, now);
    let days = match period {
        Period::Day => 1,
        Period::Week => 7,
    };
    let date = start.date_naive() + ChronoDuration::days(days);
    now.timezone().from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap()).earliest().unwrap_or(start + ChronoDuration::days(days))
}

fn used_since(db: &Db, agent_id: &str, since: i64) -> Result<u64, String> {
    db.with(|c| {
        c.query_row(
            "SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM usage WHERE agent_id = ?1 AND at >= ?2",
            params![agent_id, since],
            |r| r.get::<_, i64>(0),
        )
    })
    .map(|n| n.max(0) as u64)
}

fn status_at<Tz: TimeZone>(db: &Db, agent_id: &str, budget: Budget, now: &DateTime<Tz>) -> Result<BudgetStatus, String> {
    let since = window_start(budget.period, now).timestamp_millis();
    Ok(BudgetStatus {
        agent_id: agent_id.to_string(),
        tokens: budget.tokens,
        period: budget.period,
        used: used_since(db, agent_id, since)?,
        resets_at: window_end(budget.period, now).timestamp_millis(),
    })
}

/// Every familiar's budget with this window's usage.
pub fn statuses(db: &Db) -> Result<Vec<BudgetStatus>, String> {
    let now = Local::now();
    let budgets: Vec<(String, Budget)> = db.with(|c| {
        let mut stmt = c.prepare_cached("SELECT agent_id, tokens, period FROM budgets")?;
        let rows = stmt.query_map([], |r| {
            let period: String = r.get(2)?;
            Ok((r.get(0)?, Budget { tokens: r.get::<_, i64>(1)? as u64, period: Period::parse(&period) }))
        })?;
        rows.collect()
    })?;
    budgets.into_iter().map(|(id, budget)| status_at(db, &id, budget, &now)).collect()
}

/// 1234567 -> "1,234,567".
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn refusal(agent: &Agent, status: &BudgetStatus) -> String {
    let (window, resets) = match status.period {
        Period::Day => ("today", "at midnight"),
        Period::Week => ("this week", "on Monday"),
    };
    format!(
        "{} is over its token budget: {} of {} tokens used {window}. New turns start again {resets}, or raise the budget in its settings.",
        agent.name,
        grouped(status.used),
        grouped(status.tokens),
    )
}

fn check_at<Tz: TimeZone>(db: &Db, agent_id: &str, now: &DateTime<Tz>) -> Result<(), String> {
    let Some(budget) = get(db, agent_id)? else { return Ok(()) };
    let Some(agent) = db.get_agent(agent_id)? else { return Ok(()) };
    let status = status_at(db, agent_id, budget, now)?;
    if status.over() { Err(refusal(&agent, &status)) } else { Ok(()) }
}

/// Refuses a new turn when the familiar has used up its budget, with a message that says why.
pub fn check_turn(db: &Db, agent_id: &str) -> Result<(), String> {
    check_at(db, agent_id, &Local::now())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct PlanGuard {
    pub enabled: bool,
    pub percent: u8,
}

impl Default for PlanGuard {
    fn default() -> Self {
        PlanGuard { enabled: true, percent: 90 }
    }
}

pub fn plan_guard(db: &Db) -> PlanGuard {
    db.setting(PLAN_GUARD).ok().flatten().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

pub fn save_plan_guard(db: &Db, guard: PlanGuard) -> Result<PlanGuard, String> {
    if !(1..=100).contains(&guard.percent) {
        return Err("Pick a percentage from 1 to 100.".into());
    }
    db.set_setting(PLAN_GUARD, &serde_json::to_string(&guard).map_err(|e| e.to_string())?)?;
    Ok(guard)
}

/// One plan limit window: its name, how full it is, and when it resets.
#[derive(Debug, Clone, PartialEq)]
struct LimitWindow {
    label: String,
    percent: f64,
    resets_at: Option<i64>,
}

fn window_label(minutes: i64) -> String {
    match minutes {
        10_080 => "weekly".into(),
        m if m > 0 && m % 1440 == 0 => format!("{}-day", m / 1440),
        m if m > 0 && m % 60 == 0 => format!("{}-hour", m / 60),
        m if m > 0 => format!("{m}-minute"),
        _ => "usage".into(),
    }
}

// Both CLIs report reset times in seconds; accept milliseconds too.
fn to_ms(value: Option<&Value>) -> Option<i64> {
    let t = value?.as_f64()?;
    Some(if t < 1e12 { (t * 1000.0) as i64 } else { t as i64 })
}

/// The windows in Codex's `account/rateLimits/read` reply.
fn codex_windows(limits: &Value) -> Vec<LimitWindow> {
    ["primary", "secondary"]
        .iter()
        .filter_map(|key| {
            let w = limits.get("rateLimits")?.get(key)?;
            Some(LimitWindow {
                label: window_label(w.get("windowDurationMins").and_then(Value::as_i64).unwrap_or(0)),
                percent: w.get("usedPercent")?.as_f64()?,
                resets_at: to_ms(w.get("resetsAt")),
            })
        })
        .collect()
}

/// The windows in Claude's `rate_limit_event` info. Utilization comes as a fraction.
fn claude_windows(info: &Value) -> Vec<LimitWindow> {
    let entries: Vec<(String, &Value)> = match info.get("unifiedWindows").and_then(Value::as_object) {
        Some(windows) => windows.iter().map(|(k, v)| (k.clone(), v)).collect(),
        None => match info.get("rateLimitType").and_then(Value::as_str) {
            Some(kind) => vec![(kind.to_string(), info)],
            None => Vec::new(),
        },
    };
    entries
        .into_iter()
        .filter_map(|(key, w)| {
            let utilization = w.get("utilization")?.as_f64()?;
            let label = match key.as_str() {
                "five_hour" => "5-hour".to_string(),
                "seven_day" => "weekly".to_string(),
                other => other.strip_prefix("seven_day_").map(|m| format!("weekly {m}")).unwrap_or_else(|| other.replace('_', " ")),
            };
            Some(LimitWindow { label, percent: if utilization <= 1.0 { utilization * 100.0 } else { utilization }, resets_at: to_ms(w.get("resetsAt")) })
        })
        .collect()
}

/// Why rituals should wait, when a window that hasn't reset yet is past the guard's percentage.
fn pause_for(guard: PlanGuard, provider: &str, windows: &[LimitWindow], now: i64) -> Option<String> {
    if !guard.enabled {
        return None;
    }
    windows
        .iter()
        .filter(|w| w.resets_at.is_none_or(|at| at > now))
        .filter(|w| w.percent >= f64::from(guard.percent))
        .max_by(|a, b| a.percent.total_cmp(&b.percent))
        .map(|w| format!("{provider}'s {} limit is at {}%, past the {}% guard.", w.label, w.percent.round() as i64, guard.percent))
}

fn snapshot(db: &Db, key: &str) -> Option<Value> {
    let text = db.setting(key).ok().flatten()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("info").cloned()
}

/// Saves the Codex reading so Home and Rituals can show the pause without starting Codex.
pub fn remember_codex(db: &Db, limits: &Value) {
    let reading = serde_json::json!({ "info": limits, "at": now_ms() });
    let _ = db.set_setting(CODEX_LIMITS, &reading.to_string());
}

fn provider_pause(db: &Db, provider: &Provider, codex: Option<&Value>, now: i64) -> Option<String> {
    let guard = plan_guard(db);
    match provider {
        Provider::Codex => {
            let stored = snapshot(db, CODEX_LIMITS);
            let limits = codex.or(stored.as_ref())?;
            pause_for(guard, "Codex", &codex_windows(limits), now)
        }
        Provider::Claude => pause_for(guard, "Claude", &claude_windows(&snapshot(db, CLAUDE_LIMITS)?), now),
    }
}

/// The plan limit pause from the last readings, for Home and Rituals. It never starts Codex.
pub fn current_pause(db: &Db) -> Option<String> {
    let now = now_ms();
    provider_pause(db, &Provider::Codex, None, now).or_else(|| provider_pause(db, &Provider::Claude, None, now))
}

/// Why a ritual should skip this run, if it should. `codex` is a fresh Codex reading when there is one.
pub fn ritual_skip(db: &Db, routine: &Routine, codex: Option<&Value>, now: i64) -> Option<String> {
    let agent = db.get_agent(&routine.agent_id).ok().flatten()?;
    if let Err(reason) = check_turn(db, &agent.id) {
        return Some(reason);
    }
    provider_pause(db, &agent.provider, codex, now).map(|reason| format!("Rituals are paused: {reason}"))
}

fn log_skip(db: &Db, routine_id: &str, reason: &str) -> Result<(), String> {
    let now = now_ms();
    db.with(|c| {
        c.execute("INSERT INTO ritual_skips (routine_id, reason, at) VALUES (?1, ?2, ?3)", params![routine_id, reason, now])?;
        // Keep a month of history.
        c.execute("DELETE FROM ritual_skips WHERE at < ?1", params![now - 30 * 86_400_000]).map(|_| ())
    })
}

/// A ritual run that was skipped, and why.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skip {
    pub routine_id: String,
    pub reason: String,
    pub at: i64,
}

/// The latest skip for each ritual.
pub fn latest_skips(db: &Db) -> Result<Vec<Skip>, String> {
    db.with(|c| {
        let mut stmt = c.prepare_cached(
            "SELECT routine_id, reason, MAX(at) FROM ritual_skips GROUP BY routine_id ORDER BY routine_id",
        )?;
        let rows = stmt.query_map([], |r| Ok(Skip { routine_id: r.get(0)?, reason: r.get(1)?, at: r.get(2)? }))?;
        rows.collect()
    })
}

/// Runs a due ritual unless its familiar is over budget or a plan limit is past the guard; a skip is
/// logged with its reason.
pub fn run_ritual(db: Arc<Db>, runtime: Arc<Runtime>, routine: Routine, app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let codex = match db.get_agent(&routine.agent_id).ok().flatten().map(|a| a.provider) {
            // Read Codex's limits only when they could matter, since that starts Codex.
            Some(Provider::Codex) if plan_guard(&db).enabled && check_turn(&db, &routine.agent_id).is_ok() => {
                runtime.codex_rate_limits().await.ok()
            }
            _ => None,
        };
        if let Some(limits) = &codex {
            remember_codex(&db, limits);
        }
        match ritual_skip(&db, &routine, codex.as_ref(), now_ms()) {
            Some(reason) => {
                eprintln!("Skipped ritual {}: {reason}", routine.name);
                let _ = log_skip(&db, &routine.id, &reason);
                let _ = app.emit("routines_changed", ());
            }
            None => {
                let source = format!("{}{}", crate::runtime::RITUAL_SOURCE, routine.name);
                let _ = runtime.send(&routine.agent_id, routine.prompt.clone(), Some(&source));
            }
        }
    });
}

#[tauri::command]
pub(crate) fn list_budgets(state: State<crate::AppState>) -> Result<Vec<BudgetStatus>, String> {
    statuses(&state.db)
}

#[tauri::command]
pub(crate) fn set_budget(app: AppHandle, state: State<crate::AppState>, agent_id: String, budget: Option<Budget>) -> Result<(), String> {
    if state.db.get_agent(&agent_id)?.is_none() {
        return Err("That familiar no longer exists.".into());
    }
    set(&state.db, &agent_id, budget.as_ref())?;
    let _ = app.emit("budgets_changed", ());
    Ok(())
}

#[tauri::command]
pub(crate) fn get_plan_guard(state: State<crate::AppState>) -> PlanGuard {
    plan_guard(&state.db)
}

#[tauri::command]
pub(crate) fn save_plan_guard_setting(app: AppHandle, state: State<crate::AppState>, guard: PlanGuard) -> Result<PlanGuard, String> {
    let saved = save_plan_guard(&state.db, guard)?;
    let _ = app.emit("routines_changed", ());
    Ok(saved)
}

/// Why rituals are paused right now, if they are.
#[tauri::command]
pub(crate) fn ritual_pause(state: State<crate::AppState>) -> Option<String> {
    current_pause(&state.db)
}

#[tauri::command]
pub(crate) fn ritual_skips(state: State<crate::AppState>) -> Result<Vec<Skip>, String> {
    latest_skips(&state.db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Access, Avatar, Schedule};
    use chrono::FixedOffset;
    use serde_json::json;

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn familiar(db: &Db, id: &str, provider: Provider) {
        db.insert_agent(&Agent {
            id: id.into(),
            name: "Researcher".into(),
            provider,
            model: None,
            workspace: "/tmp/r".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed(id),
            effort: None,
            active_model: None,
            coven_id: None,
            created_at: 0,
        })
        .unwrap();
    }

    fn spend(db: &Db, agent_id: &str, tokens: i64, when: i64) {
        db.with(|c| {
            c.execute(
                "INSERT INTO usage (agent_id, provider, origin, input_tokens, output_tokens, at) VALUES (?1, 'codex', 'you', ?2, 0, ?3)",
                params![agent_id, tokens, when],
            )
            .map(|_| ())
        })
        .unwrap();
    }

    #[test]
    fn windows_start_at_midnight_and_on_monday() {
        let wednesday = at("2026-11-25T15:30:00-05:00");
        assert_eq!(window_start(Period::Day, &wednesday), at("2026-11-25T00:00:00-05:00"));
        assert_eq!(window_end(Period::Day, &wednesday), at("2026-11-26T00:00:00-05:00"));
        assert_eq!(window_start(Period::Week, &wednesday), at("2026-11-23T00:00:00-05:00"));
        assert_eq!(window_end(Period::Week, &wednesday), at("2026-11-30T00:00:00-05:00"));
        let monday_morning = at("2026-11-23T00:00:01-05:00");
        assert_eq!(window_start(Period::Week, &monday_morning), at("2026-11-23T00:00:00-05:00"));
    }

    #[test]
    fn refuses_turns_once_the_window_is_used_up() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Provider::Codex);
        let wednesday = at("2026-11-25T15:30:00-05:00");
        let ms = |s: &str| at(s).timestamp_millis();
        assert!(check_at(&db, "a", &wednesday).is_ok(), "no budget means no cap");

        set(&db, "a", Some(&Budget { tokens: 1000, period: Period::Day })).unwrap();
        spend(&db, "a", 5000, ms("2026-11-24T23:59:00-05:00")); // yesterday, outside today's window
        spend(&db, "a", 600, ms("2026-11-25T09:00:00-05:00"));
        assert!(check_at(&db, "a", &wednesday).is_ok());
        spend(&db, "a", 400, ms("2026-11-25T10:00:00-05:00"));
        let refused = check_at(&db, "a", &wednesday).unwrap_err();
        assert!(refused.contains("Researcher is over its token budget: 1,000 of 1,000 tokens used today"), "{refused}");
        assert!(refused.contains("at midnight"));

        // The week window counts Monday through now, so yesterday's 5,000 counts too.
        set(&db, "a", Some(&Budget { tokens: 7000, period: Period::Week })).unwrap();
        assert!(check_at(&db, "a", &wednesday).is_ok());
        spend(&db, "a", 1000, ms("2026-11-23T08:00:00-05:00"));
        assert!(check_at(&db, "a", &wednesday).unwrap_err().contains("7,000 of 7,000 tokens used this week"));
        spend(&db, "a", 99_999, ms("2026-11-22T12:00:00-05:00")); // last Sunday
        assert_eq!(status_at(&db, "a", get(&db, "a").unwrap().unwrap(), &wednesday).unwrap().used, 7000);

        set(&db, "a", None).unwrap();
        assert!(check_at(&db, "a", &wednesday).is_ok());
        assert!(set(&db, "a", Some(&Budget { tokens: 0, period: Period::Day })).is_err());
    }

    #[test]
    fn plan_guard_pauses_past_its_percentage() {
        let codex = json!({ "rateLimits": {
            "primary": { "usedPercent": 40, "windowDurationMins": 300, "resetsAt": 2_000_000_000 },
            "secondary": { "usedPercent": 93, "windowDurationMins": 10080, "resetsAt": 2_000_000_000 }
        } });
        let now = 1_900_000_000_000;
        let guard = PlanGuard::default();
        assert_eq!(
            pause_for(guard, "Codex", &codex_windows(&codex), now).as_deref(),
            Some("Codex's weekly limit is at 93%, past the 90% guard.")
        );
        assert_eq!(pause_for(PlanGuard { enabled: true, percent: 95 }, "Codex", &codex_windows(&codex), now), None);
        assert_eq!(pause_for(PlanGuard { enabled: false, percent: 90 }, "Codex", &codex_windows(&codex), now), None);
        // A window that already reset no longer counts.
        assert_eq!(pause_for(guard, "Codex", &codex_windows(&codex), 2_000_000_001_000), None);

        let claude = json!({ "unifiedWindows": { "five_hour": { "utilization": 0.95, "resetsAt": 2_000_000_000 }, "seven_day_opus": { "utilization": 0.2 } } });
        assert_eq!(pause_for(guard, "Claude", &claude_windows(&claude), now).as_deref(), Some("Claude's 5-hour limit is at 95%, past the 90% guard."));
        let single = json!({ "rateLimitType": "seven_day", "utilization": 0.91 });
        assert!(pause_for(guard, "Claude", &claude_windows(&single), now).unwrap().contains("weekly limit is at 91%"));
    }

    #[test]
    fn rituals_skip_for_budgets_and_plan_limits() {
        let db = Db::open_in_memory().unwrap();
        familiar(&db, "a", Provider::Claude);
        familiar(&db, "c", Provider::Codex);
        let routine = |agent: &str| Routine {
            id: format!("r-{agent}"),
            agent_id: agent.into(),
            name: "Morning".into(),
            prompt: "Summarize".into(),
            schedule: Schedule::Interval { minutes: 60 },
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            created_at: 0,
        };
        let now = now_ms();
        assert_eq!(ritual_skip(&db, &routine("a"), None, now), None);

        // Claude's stored snapshot pauses Claude familiars only.
        db.set_setting(CLAUDE_LIMITS, &json!({ "info": { "rateLimitType": "five_hour", "utilization": 0.97 }, "at": now }).to_string()).unwrap();
        assert!(ritual_skip(&db, &routine("a"), None, now).unwrap().starts_with("Rituals are paused: Claude's 5-hour limit is at 97%"));
        assert_eq!(ritual_skip(&db, &routine("c"), None, now), None);
        assert!(current_pause(&db).is_some());

        // A fresh Codex reading wins over the stored one.
        let full = json!({ "rateLimits": { "primary": { "usedPercent": 99, "windowDurationMins": 300 } } });
        remember_codex(&db, &json!({ "rateLimits": { "primary": { "usedPercent": 10, "windowDurationMins": 300 } } }));
        assert_eq!(ritual_skip(&db, &routine("c"), None, now), None);
        assert!(ritual_skip(&db, &routine("c"), Some(&full), now).is_some());

        save_plan_guard(&db, PlanGuard { enabled: false, percent: 90 }).unwrap();
        assert_eq!(ritual_skip(&db, &routine("a"), None, now), None);
        assert!(save_plan_guard(&db, PlanGuard { enabled: true, percent: 0 }).is_err());

        // Over budget skips whatever the plan says.
        set(&db, "a", Some(&Budget { tokens: 10, period: Period::Day })).unwrap();
        spend(&db, "a", 10, now);
        assert!(ritual_skip(&db, &routine("a"), None, now).unwrap().contains("over its token budget"));

        let r = routine("a");
        db.save_routine(&r).unwrap();
        log_skip(&db, &r.id, "first").unwrap();
        log_skip(&db, &r.id, "second").unwrap();
        let skips = latest_skips(&db).unwrap();
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].routine_id, r.id);
    }
}
