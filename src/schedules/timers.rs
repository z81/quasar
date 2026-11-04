use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use super::model::{Document, Entry};
use super::time::{parse_stamp, stamp_millis};
use super::{
    editable, ensure_capacity, ensure_no_claims, find_mut, retire_station, CreateTimer, Kind,
    Result, Schedule, ScheduleError, Scheduler,
};

const MAX_DURATION_SECONDS: u32 = 7 * 24 * 60 * 60;

impl Scheduler {
    pub async fn list_timers(&self, duid: &str) -> Result<Vec<Schedule>> {
        Ok(self
            .list_all(duid)
            .await?
            .into_iter()
            .filter(|item| item.kind == Kind::Timer)
            .collect())
    }

    pub async fn create_timer(&self, duid: &str, mut input: CreateTimer) -> Result<Schedule> {
        self.require_station(duid).await?;
        if !(1..=MAX_DURATION_SECONDS).contains(&input.duration_seconds) {
            return Err(ScheduleError::Invalid(
                "Длительность таймера: от 1 секунды до 7 дней".into(),
            ));
        }
        input.title = input.title.trim().to_owned();
        if input.title.is_empty() {
            input.title = "Таймер".into();
        }
        if input.title.chars().count() > 120 || input.title.chars().any(char::is_control) {
            return Err(ScheduleError::Invalid(
                "Название должно содержать не больше 120 символов без переносов строк".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.change(|doc| {
            ensure_capacity(doc, duid)?;
            let now = timer_now();
            let deadline = stamp_millis(now + Duration::seconds(i64::from(input.duration_seconds)));
            let schedule = Schedule {
                id: id.clone(),
                duid: duid.to_owned(),
                kind: Kind::Timer,
                title: input.title.clone(),
                text: String::new(),
                time: String::new(),
                date: None,
                weekdays: Vec::new(),
                timezone: "UTC".into(),
                enabled: true,
                next_run_at: Some(deadline.clone()),
                last_run_at: None,
                status: "scheduled".into(),
                last_error: None,
                active_run_id: None,
                ringing_until: None,
                snoozed_until: None,
                duration_seconds: Some(input.duration_seconds),
                remaining_ms: None,
                created_at: stamp_millis(now),
                updated_at: stamp_millis(now),
            };
            doc.entries.push(Entry {
                schedule: schedule.clone(),
                regular_next_at: Some(deadline),
                claim: None,
            });
            Ok(schedule)
        })
        .await
    }

    pub async fn pause_timer(&self, duid: &str, id: &str) -> Result<Schedule> {
        self.require_station(duid).await?;
        self.change(|doc| {
            let entry = timer_mut(doc, duid, id)?;
            editable(entry)?;
            if !entry.schedule.enabled || entry.schedule.status != "scheduled" {
                return Err(ScheduleError::Conflict(
                    "Этот таймер сейчас не запущен".into(),
                ));
            }
            let now = timer_now();
            let deadline = entry
                .schedule
                .next_run_at
                .as_deref()
                .ok_or(ScheduleError::Unavailable)
                .and_then(parse_stamp)?;
            let remaining_ms = deadline.signed_duration_since(now).num_milliseconds();
            if remaining_ms <= 0 {
                return Err(ScheduleError::Conflict(
                    "Время таймера уже истекло; его можно отменить или запустить заново".into(),
                ));
            }
            entry.regular_next_at = None;
            entry.schedule.enabled = false;
            entry.schedule.remaining_ms = Some(remaining_ms);
            entry.schedule.status = "paused".into();
            entry.schedule.last_error = None;
            entry.schedule.updated_at = stamp_millis(now);
            entry.refresh_next();
            Ok(entry.schedule.clone())
        })
        .await
    }

    pub async fn resume_timer(&self, duid: &str, id: &str) -> Result<Schedule> {
        self.require_station(duid).await?;
        self.change(|doc| {
            let entry = timer_mut(doc, duid, id)?;
            editable(entry)?;
            if entry.schedule.status != "paused" {
                return Err(ScheduleError::Conflict(
                    "Этот таймер не приостановлен".into(),
                ));
            }
            let remaining_ms = entry
                .schedule
                .remaining_ms
                .filter(|remaining| {
                    (1..=i64::from(MAX_DURATION_SECONDS) * 1000).contains(remaining)
                })
                .ok_or(ScheduleError::Unavailable)?;
            start(entry, remaining_ms, timer_now());
            Ok(entry.schedule.clone())
        })
        .await
    }

    pub async fn cancel_timer(&self, duid: &str, id: &str) -> Result<Schedule> {
        self.require_station(duid).await?;
        self.change(|doc| {
            let target = timer_mut(doc, duid, id)?;
            if target.claim.is_some() {
                return super::busy();
            }
            let ringing = target.schedule.active_run_id.is_some();
            let now = timer_now();
            if ringing {
                // A timer uses the same native signal channel as alarms.
                ensure_no_claims(doc, duid)?;
                retire_station(doc, duid, "stopped", now);
            }
            let target = timer_mut(doc, duid, id)?;
            target.regular_next_at = None;
            target.schedule.enabled = false;
            target.schedule.remaining_ms = None;
            target.schedule.snoozed_until = None;
            if !ringing {
                target.schedule.status = "cancelled".into();
                // Keep a queued stop visible until its cleanup is acknowledged.
                if target.schedule.last_error.as_deref() != Some(super::PENDING_STOP) {
                    target.schedule.last_error = None;
                }
            }
            target.schedule.updated_at = stamp_millis(now);
            target.refresh_next();
            Ok(target.schedule.clone())
        })
        .await
    }

    pub async fn restart_timer(&self, duid: &str, id: &str) -> Result<Schedule> {
        self.require_station(duid).await?;
        self.change(|doc| {
            let entry = timer_mut(doc, duid, id)?;
            editable(entry)?;
            let duration = entry
                .schedule
                .duration_seconds
                .filter(|seconds| (1..=MAX_DURATION_SECONDS).contains(seconds))
                .ok_or(ScheduleError::Unavailable)?;
            start(entry, i64::from(duration) * 1000, timer_now());
            Ok(entry.schedule.clone())
        })
        .await
    }

    pub async fn delete_timer(&self, duid: &str, id: &str) -> Result<()> {
        self.require_station(duid).await?;
        self.change(|doc| {
            editable(timer_mut(doc, duid, id)?)?;
            doc.entries
                .retain(|entry| !(entry.schedule.duid == duid && entry.schedule.id == id));
            Ok(())
        })
        .await
    }
}

fn timer_mut<'a>(doc: &'a mut Document, duid: &str, id: &str) -> Result<&'a mut Entry> {
    let entry = find_mut(doc, duid, id)?;
    if entry.schedule.kind != Kind::Timer {
        return Err(ScheduleError::NotFound);
    }
    Ok(entry)
}

fn start(entry: &mut Entry, remaining_ms: i64, now: DateTime<Utc>) {
    entry.regular_next_at = Some(stamp_millis(now + Duration::milliseconds(remaining_ms)));
    entry.schedule.enabled = true;
    entry.schedule.remaining_ms = None;
    entry.schedule.snoozed_until = None;
    entry.schedule.status = "scheduled".into();
    entry.schedule.last_error = None;
    entry.schedule.updated_at = stamp_millis(now);
    entry.refresh_next();
}

fn timer_now() -> DateTime<Utc> {
    let now = Utc::now();
    // Both deadlines and pause instants use integer milliseconds. Repeated
    // pauses never discard a fractional millisecond on every transition.
    now - Duration::nanoseconds(i64::from(now.timestamp_subsec_nanos() % 1_000_000))
}
