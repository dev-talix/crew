use crate::db::{now_ms, Db, Routine, Schedule};
use crate::runtime::{Runtime, RITUAL_SOURCE};
use crate::workflows::WorkflowRunner;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

pub fn validate(schedule: &Schedule) -> Result<(), String> {
    match schedule {
        Schedule::Interval { minutes } if *minutes < 5 || *minutes > 7 * 24 * 60 => {
            Err("Repeat every 5 minutes to 7 days.".into())
        }
        Schedule::Daily { time, days } => {
            parse_time(time)?;
            if days.iter().any(|d| !(1..=7).contains(d)) {
                return Err("Days run from 1 (Monday) to 7 (Sunday).".into());
            }
            Ok(())
        }
        Schedule::Interval { .. } => Ok(()),
        Schedule::GithubPr { repo } if !crate::triggers::valid_repo(repo) => Err("Name the GitHub repo as owner/name.".into()),
        Schedule::File { path } => crate::triggers::home_path(path).map(|_| ()),
        Schedule::GithubPr { .. } | Schedule::Webhook { .. } | Schedule::Email => Ok(()),
    }
}

/// Tidies an event trigger before saving: expands `~` in a path, and keeps a webhook's secret, or makes
/// one. The secret always comes from Grimoire, never from the window.
pub fn prepare(schedule: Schedule, existing: Option<&Routine>) -> Result<Schedule, String> {
    let schedule = match schedule {
        Schedule::GithubPr { repo } => Schedule::GithubPr { repo: repo.trim().to_string() },
        Schedule::File { path } => Schedule::File { path: crate::triggers::home_path(&path)? },
        Schedule::Webhook { .. } => Schedule::Webhook {
            secret: match existing.map(|r| &r.schedule) {
                Some(Schedule::Webhook { secret }) if !secret.is_empty() => secret.clone(),
                _ => crate::triggers::new_secret(),
            },
        },
        other => other,
    };
    validate(&schedule)?;
    Ok(schedule)
}

fn parse_time(time: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(time, "%H:%M").map_err(|_| format!("Use a 24-hour time like 09:30, not {time}."))
}

pub fn next_run<Tz: TimeZone>(schedule: &Schedule, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
    match schedule {
        Schedule::Interval { minutes } => Some(after + ChronoDuration::minutes(i64::from(*minutes))),
        // Event rituals run when their trigger fires, not by the clock.
        Schedule::GithubPr { .. } | Schedule::File { .. } | Schedule::Webhook { .. } | Schedule::Email => None,
        Schedule::Daily { time, days } => {
            let time = parse_time(time).ok()?;
            let tz = after.timezone();
            (0..=7).find_map(|offset| {
                let date = after.date_naive() + ChronoDuration::days(offset);
                let weekday = date.weekday().number_from_monday() as u8;
                if !days.is_empty() && !days.contains(&weekday) {
                    return None;
                }
                let candidate = tz.from_local_datetime(&date.and_time(time)).earliest()?;
                (candidate > after).then_some(candidate)
            })
        }
    }
}

pub fn next_run_ms(schedule: &Schedule, after_ms: i64) -> Option<i64> {
    let after = Local.timestamp_millis_opt(after_ms).single()?;
    next_run(schedule, after).map(|t| t.timestamp_millis())
}

/// A ritual as the editor sends it. It messages a familiar (`agent_id`, with `prompt` as the message) or casts
/// a spell (`workflow_id`, with `prompt` as the optional starting text).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineInput {
    pub id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub workflow_id: Option<String>,
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    pub enabled: bool,
}

/// Creates or updates a ritual after checking it.
pub fn save(db: &Db, input: RoutineInput) -> Result<Routine, String> {
    let name = input.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 60 {
        return Err("Give the ritual a name of 1 to 60 characters.".into());
    }
    let prompt = input.prompt.trim().to_string();
    let given = |id: Option<String>| id.filter(|id| !id.is_empty());
    let (agent_id, workflow_id) = match (given(input.agent_id), given(input.workflow_id)) {
        (_, Some(workflow_id)) => {
            if !db.list_workflows()?.iter().any(|w| w.id == workflow_id) {
                return Err("Pick a spell for this ritual.".into());
            }
            (None, Some(workflow_id))
        }
        (Some(agent_id), None) => {
            if prompt.is_empty() {
                return Err("Tell the familiar what to do on each run.".into());
            }
            if db.get_agent(&agent_id)?.is_none() {
                return Err("Pick a familiar for this ritual.".into());
            }
            (Some(agent_id), None)
        }
        (None, None) => return Err("Pick a familiar or a spell for this ritual.".into()),
    };
    let existing = match input.id.as_deref() {
        Some(id) => db.list_routines()?.into_iter().find(|r| r.id == id),
        None => None,
    };
    let schedule = prepare(input.schedule, existing.as_ref())?;
    let now = now_ms();
    let routine = Routine {
        id: existing.as_ref().map(|r| r.id.clone()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        agent_id,
        workflow_id,
        name,
        prompt,
        next_run_at: if input.enabled { next_run_ms(&schedule, now) } else { None },
        schedule,
        enabled: input.enabled,
        last_run_at: existing.as_ref().and_then(|r| r.last_run_at),
        created_at: existing.map(|r| r.created_at).unwrap_or(now),
    };
    // Saving never runs an event ritual: its watcher looks again and only runs it for what's new after that.
    crate::triggers::while_editing(|| {
        db.save_routine(&routine)?;
        crate::triggers::reset(db, &routine.id)
    })?;
    Ok(routine)
}

/// What a ritual does when it runs: message a familiar or cast a spell.
pub trait Caster {
    fn message(&self, agent_id: &str, prompt: String, source: &str) -> Result<(), String>;
    fn cast(&self, workflow_id: &str, input: String, ritual: &str) -> Result<(), String>;
}

pub fn fire(routine: &Routine, caster: &impl Caster) -> Result<(), String> {
    match (&routine.workflow_id, &routine.agent_id) {
        (Some(workflow_id), _) => caster.cast(workflow_id, routine.prompt.clone(), &routine.name),
        (None, Some(agent_id)) => caster.message(agent_id, routine.prompt.clone(), &format!("{RITUAL_SOURCE}{}", routine.name)),
        (None, None) => Err("This ritual has no familiar or spell.".into()),
    }
}

/// Adds what set off an event ritual to its message.
pub fn with_trigger(prompt: &str, what: &str) -> String {
    let what = format!("Triggered by: {what}");
    if prompt.is_empty() {
        what
    } else {
        format!("{prompt}\n\n{what}")
    }
}

struct AppCaster {
    runtime: Arc<Runtime>,
    spells: Arc<WorkflowRunner>,
}

impl Caster for AppCaster {
    fn message(&self, agent_id: &str, prompt: String, source: &str) -> Result<(), String> {
        self.runtime.send(agent_id, prompt, Some(source)).map(|_| ())
    }

    fn cast(&self, workflow_id: &str, input: String, ritual: &str) -> Result<(), String> {
        self.spells.start(workflow_id, input, Some(ritual)).map(|_| ())
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RoutinesChanged;

pub struct Scheduler {
    pub(crate) db: Arc<Db>,
    runtime: Weak<Runtime>,
    spells: Weak<WorkflowRunner>,
    pub(crate) app: AppHandle,
    wake: Notify,
    /// Wakes the event watcher in `triggers` after a ritual changes.
    pub(crate) trigger_wake: Notify,
    /// Why the webhook listener isn't running, if it isn't.
    pub(crate) webhook_error: Mutex<Option<String>>,
}

impl Scheduler {
    pub fn start(db: Arc<Db>, runtime: &Arc<Runtime>, spells: &Arc<WorkflowRunner>, app: AppHandle) -> Arc<Self> {
        let (runtime, spells) = (Arc::downgrade(runtime), Arc::downgrade(spells));
        let scheduler = Arc::new(Scheduler {
            db,
            runtime,
            spells,
            app,
            wake: Notify::new(),
            trigger_wake: Notify::new(),
            webhook_error: Mutex::new(None),
        });
        let worker = scheduler.clone();
        tauri::async_runtime::spawn(async move { worker.run().await });
        tauri::async_runtime::spawn(crate::triggers::watch(scheduler.clone()));
        tauri::async_runtime::spawn(crate::triggers::serve(scheduler.clone()));
        scheduler
    }

    pub fn changed(&self) {
        self.wake.notify_one();
        self.trigger_wake.notify_one();
    }

    /// Runs a ritual through the budget and plan limit checks, as a scheduled run would.
    fn dispatch(&self, routine: &Routine) {
        if let (Some(runtime), Ok(caster)) = (self.runtime.upgrade(), self.caster()) {
            let (app, ritual) = (self.app.clone(), routine.clone());
            crate::budgets::run_ritual(self.db.clone(), runtime, routine.clone(), self.app.clone(), move || {
                fire(&ritual, &caster)?;
                // A spell ritual is announced when the spell ends instead.
                if let Some(agent_id) = &ritual.agent_id {
                    crate::notify::ritual_fired(&app, &ritual.name, agent_id);
                }
                Ok(())
            });
        }
    }

    /// Runs an event ritual, telling the familiar, or the spell's input, what set it off.
    pub(crate) fn fire_with(&self, routine: &Routine, what: &str) {
        let mut run = routine.clone();
        run.prompt = with_trigger(&routine.prompt, what);
        self.dispatch(&run);
        let now = now_ms();
        let _ = self.db.with(|c| c.execute("UPDATE routines SET last_run_at = ?2 WHERE id = ?1", rusqlite::params![routine.id, now]));
    }

    fn caster(&self) -> Result<AppCaster, String> {
        match (self.runtime.upgrade(), self.spells.upgrade()) {
            (Some(runtime), Some(spells)) => Ok(AppCaster { runtime, spells }),
            _ => Err("Grimoire is shutting down.".into()),
        }
    }

    pub fn run_now(&self, routine: &Routine) -> Result<(), String> {
        fire(routine, &self.caster()?)?;
        let mut updated = routine.clone();
        updated.last_run_at = Some(now_ms());
        self.db.save_routine(&updated)?;
        let _ = self.app.emit("routines_changed", RoutinesChanged);
        Ok(())
    }

    async fn run(self: Arc<Self>) {
        loop {
            let wait = self.fire_due().unwrap_or(None);
            match wait {
                Some(ms) => {
                    let _ = tokio::time::timeout(Duration::from_millis(ms.max(0) as u64), self.wake.notified()).await;
                }
                None => self.wake.notified().await,
            }
        }
    }

    fn fire_due(&self) -> Result<Option<i64>, String> {
        let now = now_ms();
        let mut soonest: Option<i64> = None;
        let mut fired = false;
        for mut routine in self.db.list_routines()? {
            if !routine.enabled || routine.schedule.is_event() {
                continue;
            }
            let due = match routine.next_run_at {
                Some(due) => due,
                None => {
                    routine.next_run_at = next_run_ms(&routine.schedule, now);
                    self.db.save_routine(&routine)?;
                    fired = true;
                    match routine.next_run_at {
                        Some(due) => due,
                        None => continue,
                    }
                }
            };
            if due <= now {
                // Budgets and the plan limit guard can skip the run, and so can a spell still being cast.
                self.dispatch(&routine);
                routine.last_run_at = Some(now);
                routine.next_run_at = next_run_ms(&routine.schedule, now);
                self.db.save_routine(&routine)?;
                fired = true;
            }
            if let Some(next) = routine.next_run_at {
                soonest = Some(soonest.map_or(next, |s| s.min(next)));
            }
        }
        if fired {
            let _ = self.app.emit("routines_changed", RoutinesChanged);
        }
        Ok(soonest.map(|s| s - now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Weekday};

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    #[test]
    fn daily_runs_later_today_or_tomorrow() {
        let schedule = Schedule::Daily { time: "09:30".into(), days: vec![] };
        assert_eq!(next_run(&schedule, at("2026-11-23T08:00:00-05:00")).unwrap(), at("2026-11-23T09:30:00-05:00"));
        assert_eq!(next_run(&schedule, at("2026-11-23T09:30:00-05:00")).unwrap(), at("2026-11-24T09:30:00-05:00"));
    }

    #[test]
    fn weekday_schedule_skips_the_weekend() {
        let schedule = Schedule::Daily { time: "09:00".into(), days: vec![1, 2, 3, 4, 5] };
        let friday_evening = at("2026-11-27T18:00:00-05:00");
        assert_eq!(friday_evening.weekday(), Weekday::Fri);
        let next = next_run(&schedule, friday_evening).unwrap();
        assert_eq!(next.weekday(), Weekday::Mon);
        assert_eq!(next, at("2026-11-30T09:00:00-05:00"));
    }

    #[test]
    fn interval_adds_minutes() {
        let schedule = Schedule::Interval { minutes: 90 };
        assert_eq!(next_run(&schedule, at("2026-11-23T23:00:00-05:00")).unwrap(), at("2026-11-24T00:30:00-05:00"));
    }

    #[derive(Default)]
    struct Fake(std::cell::RefCell<Vec<String>>);

    impl Caster for Fake {
        fn message(&self, agent_id: &str, prompt: String, source: &str) -> Result<(), String> {
            self.0.borrow_mut().push(format!("message {agent_id}: {prompt} ({source})"));
            Ok(())
        }
        fn cast(&self, workflow_id: &str, input: String, ritual: &str) -> Result<(), String> {
            self.0.borrow_mut().push(format!("cast {workflow_id}: {input} ({ritual})"));
            Ok(())
        }
    }

    fn input(agent_id: Option<&str>, workflow_id: Option<&str>, prompt: &str) -> RoutineInput {
        RoutineInput {
            id: None,
            agent_id: agent_id.map(String::from),
            workflow_id: workflow_id.map(String::from),
            name: "Morning brief".into(),
            prompt: prompt.into(),
            schedule: Schedule::Daily { time: "09:00".into(), days: vec![] },
            enabled: true,
        }
    }

    fn db_with_spell() -> Db {
        use crate::db::{Access, Agent, Avatar, Provider, Workflow, WorkflowStep};
        let db = Db::open_in_memory().unwrap();
        db.insert_agent(&Agent {
            id: "a".into(),
            name: "Ada".into(),
            provider: Provider::Codex,
            model: None,
            workspace: "/tmp".into(),
            access: Access::Workspace,
            session_id: None,
            avatar: Avatar::for_seed("a"),
            effort: None,
            active_model: None,
            coven_id: None,
            host: None,
            created_at: now_ms(),
        })
        .unwrap();
        let step = WorkflowStep { agent_id: "a".into(), prompt: "Research {{input}}".into(), ..Default::default() };
        db.save_workflow(&Workflow { id: "w".into(), name: "Brief".into(), steps: vec![step], created_at: 0, created_by: None }).unwrap();
        db
    }

    #[test]
    fn rituals_message_a_familiar_or_cast_a_spell() {
        let db = db_with_spell();
        let message = save(&db, input(Some("a"), None, " Check the build ")).unwrap();
        assert_eq!((message.agent_id.as_deref(), message.workflow_id.as_deref()), (Some("a"), None));
        let spell = save(&db, input(None, Some("w"), "")).unwrap();
        assert_eq!((spell.agent_id.as_deref(), spell.workflow_id.as_deref()), (None, Some("w")));
        assert!(spell.next_run_at.is_some());

        let fake = Fake::default();
        fire(&message, &fake).unwrap();
        fire(&spell, &fake).unwrap();
        fire(&Routine { prompt: "AI news".into(), ..spell.clone() }, &fake).unwrap();
        assert_eq!(
            fake.0.into_inner(),
            vec!["message a: Check the build (Ritual: Morning brief)", "cast w:  (Morning brief)", "cast w: AI news (Morning brief)"]
        );

        assert_eq!(db.list_routines().unwrap().len(), 2);
        db.delete_workflow("w").unwrap();
        assert_eq!(db.list_routines().unwrap(), vec![message], "deleting a spell deletes its rituals");
    }

    #[test]
    fn ritual_targets_are_checked() {
        let db = db_with_spell();
        assert_eq!(save(&db, input(None, None, "x")).unwrap_err(), "Pick a familiar or a spell for this ritual.");
        assert_eq!(save(&db, input(None, Some("gone"), "")).unwrap_err(), "Pick a spell for this ritual.");
        assert_eq!(save(&db, input(Some("a"), None, " ")).unwrap_err(), "Tell the familiar what to do on each run.");
        assert_eq!(save(&db, input(Some("ghost"), None, "Go")).unwrap_err(), "Pick a familiar for this ritual.");
        // Older rituals arrive without a spell id and still save.
        let old: RoutineInput = serde_json::from_str(
            r#"{"agentId":"a","name":"Old","prompt":"Go","schedule":{"kind":"interval","minutes":60},"enabled":true}"#,
        )
        .unwrap();
        assert_eq!(save(&db, old).unwrap().agent_id.as_deref(), Some("a"));
        assert!(save(&db, input(Some("a"), Some(""), "Go")).is_ok());
    }

    #[test]
    fn event_rituals_save_with_a_server_secret_and_no_clock() {
        let db = db_with_spell();
        let hook = |id: Option<String>, secret: &str| RoutineInput {
            id,
            schedule: Schedule::Webhook { secret: secret.into() },
            ..input(Some("a"), None, "Look at it")
        };
        let saved = save(&db, hook(None, "chosen-by-the-window")).unwrap();
        let Schedule::Webhook { secret } = &saved.schedule else { panic!("not a webhook") };
        assert_eq!(secret.len(), 32);
        assert_eq!(saved.next_run_at, None);
        // Editing keeps the secret, whatever the window sends.
        let again = save(&db, hook(Some(saved.id.clone()), "other")).unwrap();
        assert_eq!(again.schedule, saved.schedule);

        let home = std::env::var("HOME").unwrap();
        let file = save(&db, RoutineInput { schedule: Schedule::File { path: "~/Notes".into() }, ..input(Some("a"), None, "Go") }).unwrap();
        assert_eq!(file.schedule, Schedule::File { path: format!("{home}/Notes") });
        let bad = RoutineInput { schedule: Schedule::GithubPr { repo: "nope".into() }, ..input(Some("a"), None, "Go") };
        assert_eq!(save(&db, bad).unwrap_err(), "Name the GitHub repo as owner/name.");

        let email: RoutineInput = serde_json::from_str(r#"{"agentId":"a","name":"Mail","prompt":"Sort it","schedule":{"kind":"email"},"enabled":true}"#).unwrap();
        assert_eq!(save(&db, email).unwrap().schedule, Schedule::Email);
        let pr: Schedule = serde_json::from_str(r#"{"kind":"githubPr","repo":"o/r"}"#).unwrap();
        assert!(pr.is_event() && !Schedule::Interval { minutes: 60 }.is_event());
    }

    #[test]
    fn trigger_text_follows_the_message() {
        assert_eq!(with_trigger("Review it", "PR #1 'x' opened in o/r"), "Review it\n\nTriggered by: PR #1 'x' opened in o/r");
        assert_eq!(with_trigger("", "New email"), "Triggered by: New email");
    }

    #[test]
    fn rejects_bad_schedules() {
        assert!(validate(&Schedule::Interval { minutes: 1 }).is_err());
        assert!(validate(&Schedule::Daily { time: "25:00".into(), days: vec![] }).is_err());
        assert!(validate(&Schedule::Daily { time: "09:00".into(), days: vec![8] }).is_err());
        assert!(validate(&Schedule::Daily { time: "07:05".into(), days: vec![6, 7] }).is_ok());
    }
}
