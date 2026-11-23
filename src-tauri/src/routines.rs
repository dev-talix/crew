use crate::db::{now_ms, Db, Routine, Schedule};
use crate::runtime::Runtime;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveTime, TimeZone};
use serde::Serialize;
use std::sync::{Arc, Weak};
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
    }
}

fn parse_time(time: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(time, "%H:%M").map_err(|_| format!("Use a 24-hour time like 09:30, not {time}."))
}

pub fn next_run<Tz: TimeZone>(schedule: &Schedule, after: DateTime<Tz>) -> Option<DateTime<Tz>> {
    match schedule {
        Schedule::Interval { minutes } => Some(after + ChronoDuration::minutes(i64::from(*minutes))),
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

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RoutinesChanged;

pub struct Scheduler {
    db: Arc<Db>,
    runtime: Weak<Runtime>,
    app: AppHandle,
    wake: Notify,
}

impl Scheduler {
    pub fn start(db: Arc<Db>, runtime: &Arc<Runtime>, app: AppHandle) -> Arc<Self> {
        let scheduler = Arc::new(Scheduler { db, runtime: Arc::downgrade(runtime), app, wake: Notify::new() });
        let worker = scheduler.clone();
        tauri::async_runtime::spawn(async move { worker.run().await });
        scheduler
    }

    pub fn changed(&self) {
        self.wake.notify_one();
    }

    pub fn run_now(&self, routine: &Routine) -> Result<(), String> {
        let runtime = self.runtime.upgrade().ok_or("Grimoire is shutting down.")?;
        runtime.send(&routine.agent_id, routine.prompt.clone(), Some(&format!("Ritual: {}", routine.name)))?;
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
            if !routine.enabled {
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
                if let Some(runtime) = self.runtime.upgrade() {
                    let _ = runtime.send(&routine.agent_id, routine.prompt.clone(), Some(&format!("Ritual: {}", routine.name)));
                }
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

    #[test]
    fn rejects_bad_schedules() {
        assert!(validate(&Schedule::Interval { minutes: 1 }).is_err());
        assert!(validate(&Schedule::Daily { time: "25:00".into(), days: vec![] }).is_err());
        assert!(validate(&Schedule::Daily { time: "09:00".into(), days: vec![8] }).is_err());
        assert!(validate(&Schedule::Daily { time: "07:05".into(), days: vec![6, 7] }).is_ok());
    }
}
