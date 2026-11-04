//! Persistent station alarms, timers and spoken reminders. Claims are saved before any
//! command is queued, and ambiguous reminder deliveries are never replayed.

mod engine;
mod model;
mod time;
mod timers;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::storage::Storage;
use model::{Cleanup, Document, Entry};
use time::{parse_stamp, stamp, validate};

pub use model::{CreateTimer, Input, Kind, Result, Schedule, ScheduleError};

const KEY: &str = "schedules:v1";
const MAX_TOTAL: usize = 1000;
const MAX_STATION: usize = 200;
const PENDING_STOP: &str = "Остановка сигнала ожидает подключения колонки";

#[async_trait]
pub trait Delivery: Send + Sync {
    fn connected(&self, duid: &str) -> bool;
    async fn deliver(&self, item: &Schedule, run_id: &str) -> anyhow::Result<()>;
    /// The native alarm channel is shared: this may stop every active alarm on
    /// the station. The scheduler retires all of them before invoking it.
    async fn stop(&self, duid: &str, run_id: &str) -> anyhow::Result<()>;
    /// Delete an occurrence known to have ended, without stopping other alarms.
    async fn cancel(&self, duid: &str, run_id: &str) -> anyhow::Result<()>;
}

pub struct Scheduler {
    storage: Arc<Storage>,
    delivery: Arc<dyn Delivery>,
    changes: Mutex<()>,
    started: AtomicBool,
}

impl Scheduler {
    pub async fn load(storage: Arc<Storage>, delivery: Arc<dyn Delivery>) -> Result<Arc<Self>> {
        let service = Arc::new(Self {
            storage,
            delivery,
            changes: Mutex::new(()),
            started: AtomicBool::new(false),
        });
        if service.storage.is_persistent() {
            let value = serde_json::to_value(Document::default()).map_err(unavailable)?;
            service
                .storage
                .insert_control_value(KEY, &value)
                .await
                .map_err(unavailable)?;
            service.read().await?;
        }
        Ok(service)
    }

    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) || !self.storage.is_persistent() {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(std::time::Duration::from_secs(1));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                if let Err(error) = service.tick().await {
                    tracing::warn!("scheduler tick failed: {error}");
                }
            }
        });
    }

    pub async fn list(&self, duid: &str) -> Result<Vec<Schedule>> {
        Ok(self
            .list_all(duid)
            .await?
            .into_iter()
            .filter(|item| item.kind != Kind::Timer)
            .collect())
    }

    pub async fn list_all(&self, duid: &str) -> Result<Vec<Schedule>> {
        self.require_station(duid).await?;
        let (_, doc) = self.read().await?;
        let mut items: Vec<_> = doc
            .entries
            .into_iter()
            .filter(|entry| entry.schedule.duid == duid)
            .map(|entry| entry.schedule)
            .collect();
        items.sort_by_cached_key(|item| {
            (
                item.next_run_at.is_none(),
                item.next_run_at
                    .as_deref()
                    .and_then(|at| parse_stamp(at).ok()),
                parse_stamp(&item.created_at).ok(),
            )
        });
        Ok(items)
    }

    pub async fn create(&self, duid: &str, input: Input) -> Result<Schedule> {
        self.require_station(duid).await?;
        let now = Utc::now();
        let (input, next) = validate(input, now)?;
        let id = Uuid::new_v4().to_string();
        self.change(|doc| {
            ensure_capacity(doc, duid)?;
            let schedule = Schedule {
                id: id.clone(),
                duid: duid.to_owned(),
                kind: input.kind,
                title: input.title.clone(),
                text: input.text.clone(),
                time: input.time.clone(),
                date: input.date.clone(),
                weekdays: input.weekdays.clone(),
                timezone: input.timezone.clone(),
                enabled: input.enabled,
                next_run_at: next.clone(),
                last_run_at: None,
                status: if input.enabled {
                    "scheduled"
                } else {
                    "disabled"
                }
                .into(),
                last_error: None,
                active_run_id: None,
                ringing_until: None,
                snoozed_until: None,
                duration_seconds: None,
                remaining_ms: None,
                created_at: stamp(now),
                updated_at: stamp(now),
            };
            doc.entries.push(Entry {
                schedule: schedule.clone(),
                regular_next_at: next.clone(),
                claim: None,
            });
            Ok(schedule)
        })
        .await
    }

    pub async fn update(&self, duid: &str, id: &str, input: Input) -> Result<Schedule> {
        self.require_station(duid).await?;
        let now = Utc::now();
        let (input, next) = validate(input, now)?;
        self.change(|doc| {
            let entry = find_mut(doc, duid, id)?;
            calendar(entry)?;
            editable(entry)?;
            let item = &mut entry.schedule;
            item.kind = input.kind;
            item.title = input.title.clone();
            item.text = input.text.clone();
            item.time = input.time.clone();
            item.date = input.date.clone();
            item.weekdays = input.weekdays.clone();
            item.timezone = input.timezone.clone();
            item.enabled = input.enabled;
            item.snoozed_until = None;
            item.status = if input.enabled {
                "scheduled"
            } else {
                "disabled"
            }
            .into();
            item.last_error = None;
            item.updated_at = stamp(now);
            entry.regular_next_at = next.clone();
            entry.refresh_next();
            Ok(entry.schedule.clone())
        })
        .await
    }

    pub async fn set_enabled(&self, duid: &str, id: &str, enabled: bool) -> Result<Schedule> {
        self.require_station(duid).await?;
        self.change(|doc| {
            let entry = find_mut(doc, duid, id)?;
            calendar(entry)?;
            editable(entry)?;
            let mut input = entry.schedule.input();
            input.enabled = enabled;
            let (_, next) = validate(input, Utc::now())?;
            entry.schedule.enabled = enabled;
            entry.schedule.snoozed_until = None;
            entry.schedule.status = if enabled { "scheduled" } else { "disabled" }.into();
            entry.schedule.last_error = None;
            entry.schedule.updated_at = stamp(Utc::now());
            entry.regular_next_at = next;
            entry.refresh_next();
            Ok(entry.schedule.clone())
        })
        .await
    }

    pub async fn delete(&self, duid: &str, id: &str) -> Result<()> {
        self.require_station(duid).await?;
        self.change(|doc| {
            let entry = find_mut(doc, duid, id)?;
            calendar(entry)?;
            editable(entry)?;
            doc.entries
                .retain(|entry| !(entry.schedule.duid == duid && entry.schedule.id == id));
            Ok(())
        })
        .await
    }

    pub async fn stop(&self, duid: &str, id: &str) -> Result<Schedule> {
        self.end_alarm(duid, id, false).await
    }

    pub async fn snooze(&self, duid: &str, id: &str) -> Result<Schedule> {
        self.end_alarm(duid, id, true).await
    }

    async fn end_alarm(&self, duid: &str, id: &str, snooze: bool) -> Result<Schedule> {
        self.require_station(duid).await?;
        self.change(|doc| {
            let target = find_mut(doc, duid, id)?;
            if !target.schedule.kind.rings()
                || (snooze && target.schedule.kind != Kind::Alarm)
                || target.schedule.active_run_id.is_none()
            {
                return Err(ScheduleError::Conflict(
                    "Этот будильник или таймер сейчас не звонит либо не поддерживает повтор".into(),
                ));
            }
            ensure_no_claims(doc, duid)?;
            retire_station(doc, duid, "stopped", Utc::now());
            let target = find_mut(doc, duid, id)?;
            if snooze {
                target.schedule.snoozed_until = Some(stamp(Utc::now() + Duration::minutes(10)));
                target.schedule.enabled = true;
                target.schedule.status = "snoozed".into();
                target.refresh_next();
            }
            Ok(target.schedule.clone())
        })
        .await
    }

    pub async fn acknowledge_alarm(&self, duid: &str, run_id: &str, playing: bool) -> Result<()> {
        self.change(|doc| {
            if let Some(entry) = doc.entries.iter_mut().find(|entry| {
                entry.schedule.duid == duid
                    && entry.schedule.active_run_id.as_deref() == Some(run_id)
            }) {
                if playing {
                    entry.schedule.status = "ringing".into();
                    entry.schedule.last_error = None;
                } else if entry.schedule.status == "ringing" {
                    // A native stop is authoritative. A pre-ring state after an
                    // add is not: it arrives before the native trigger time.
                    entry.schedule.active_run_id = None;
                    entry.schedule.ringing_until = None;
                    entry.schedule.status = "stopped".into();
                    entry.schedule.last_error = None;
                    doc.cleanup.push(Cleanup {
                        duid: duid.to_owned(),
                        run_id: run_id.to_owned(),
                        stop_channel: false,
                        claim: None,
                        retry_at: None,
                    });
                }
                entry.schedule.updated_at = stamp(Utc::now());
            }
            Ok(())
        })
        .await
    }

    async fn require_station(&self, duid: &str) -> Result<()> {
        if !self.storage.is_persistent() {
            return Err(ScheduleError::Unavailable);
        }
        let stations = self.storage.list().await.map_err(unavailable)?;
        if stations.iter().any(|station| station.duid == duid) {
            Ok(())
        } else {
            Err(ScheduleError::NotFound)
        }
    }

    async fn read(&self) -> Result<(Value, Document)> {
        if !self.storage.is_persistent() {
            return Err(ScheduleError::Unavailable);
        }
        let value = self
            .storage
            .get_control_value(KEY)
            .await
            .map_err(unavailable)?
            .ok_or(ScheduleError::Unavailable)?;
        let doc: Document = serde_json::from_value(value.clone()).map_err(unavailable)?;
        if doc.version != 1 || doc.entries.len() > MAX_TOTAL {
            return Err(ScheduleError::Unavailable);
        }
        Ok((value, doc))
    }

    async fn change<T>(&self, mut change: impl FnMut(&mut Document) -> Result<T>) -> Result<T> {
        let _guard = self.changes.lock().await;
        for _ in 0..16 {
            let (previous, mut doc) = self.read().await?;
            let result = change(&mut doc)?;
            let unchanged = serde_json::to_value(&doc).map_err(unavailable)?;
            if unchanged == previous {
                return Ok(result);
            }
            doc.revision = doc
                .revision
                .checked_add(1)
                .ok_or(ScheduleError::Unavailable)?;
            let replacement = serde_json::to_value(doc).map_err(unavailable)?;
            if self
                .storage
                .compare_control_value(KEY, &previous, &replacement)
                .await
                .map_err(unavailable)?
            {
                return Ok(result);
            }
        }
        Err(ScheduleError::Conflict(
            "Расписание изменилось. Повторите действие".into(),
        ))
    }
}

fn find_mut<'a>(doc: &'a mut Document, duid: &str, id: &str) -> Result<&'a mut Entry> {
    doc.entries
        .iter_mut()
        .find(|entry| entry.schedule.duid == duid && entry.schedule.id == id)
        .ok_or(ScheduleError::NotFound)
}

fn editable(entry: &Entry) -> Result<()> {
    if entry.claim.is_some() {
        return busy();
    }
    if entry.schedule.active_run_id.is_some() {
        return Err(ScheduleError::Conflict("Сначала остановите сигнал".into()));
    }
    Ok(())
}

fn calendar(entry: &Entry) -> Result<()> {
    if entry.schedule.kind == Kind::Timer {
        Err(ScheduleError::Invalid(
            "Для таймера используйте управление таймерами".into(),
        ))
    } else {
        Ok(())
    }
}

fn ensure_capacity(doc: &Document, duid: &str) -> Result<()> {
    if doc.entries.len() >= MAX_TOTAL
        || doc
            .entries
            .iter()
            .filter(|entry| entry.schedule.duid == duid)
            .count()
            >= MAX_STATION
    {
        Err(ScheduleError::Conflict(
            "Достигнут лимит записей расписания".into(),
        ))
    } else {
        Ok(())
    }
}

fn ensure_no_claims(doc: &Document, duid: &str) -> Result<()> {
    if doc
        .entries
        .iter()
        .any(|entry| entry.schedule.duid == duid && entry.claim.is_some())
    {
        busy()
    } else {
        Ok(())
    }
}

fn busy<T>() -> Result<T> {
    Err(ScheduleError::Conflict(
        "Команда уже отправляется колонке. Повторите действие через несколько секунд".into(),
    ))
}

fn retire_station(doc: &mut Document, duid: &str, status: &str, now: chrono::DateTime<Utc>) {
    for entry in doc
        .entries
        .iter_mut()
        .filter(|entry| entry.schedule.duid == duid)
    {
        if let Some(run_id) = entry.schedule.active_run_id.take() {
            if !doc
                .cleanup
                .iter()
                .any(|cleanup| cleanup.run_id == run_id && cleanup.duid == duid)
            {
                doc.cleanup.push(Cleanup {
                    duid: duid.to_owned(),
                    run_id,
                    stop_channel: true,
                    claim: None,
                    retry_at: None,
                });
            }
            entry.schedule.ringing_until = None;
            entry.schedule.status = status.into();
            entry.schedule.last_error = Some(PENDING_STOP.into());
            entry.schedule.updated_at = stamp(now);
        }
    }
}

fn unavailable(error: impl std::fmt::Display) -> ScheduleError {
    tracing::warn!("schedule storage operation failed: {error}");
    ScheduleError::Unavailable
}
