use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use futures_util::future::join_all;
use uuid::Uuid;

use super::model::{Claim, Cleanup, Document, Entry};
use super::time::{next, parse_stamp, stamp};
use super::{retire_station, Kind, Result, Schedule, Scheduler, PENDING_STOP};

const LATE_WINDOW: i64 = 5 * 60;
const ACTIVE_WINDOW: i64 = 5 * 60;
const CLAIM_TIMEOUT: i64 = 30;
const RETRY_SECONDS: i64 = 10;
const BATCH: usize = 32;

struct PendingDelivery {
    schedule: Schedule,
    token: String,
}

impl Scheduler {
    pub(super) async fn tick(&self) -> Result<()> {
        let now = Utc::now();
        self.change(|doc| recover(doc, now)).await?;

        // A native stop affects the station's complete alarm channel. Never
        // queue a new occurrence until every older stop has been dispatched.
        let cleanup = self.claim_cleanup(now).await?;
        let stopped = join_all(cleanup.into_iter().map(|cleanup| async move {
            let stop = async {
                if cleanup.stop_channel {
                    self.delivery.stop(&cleanup.duid, &cleanup.run_id).await
                } else {
                    self.delivery.cancel(&cleanup.duid, &cleanup.run_id).await
                }
            };
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), stop).await;
            let success = matches!(result, Ok(Ok(())));
            if !success {
                tracing::warn!("scheduled alarm stop failed or timed out");
            }
            self.finish_cleanup(&cleanup, success).await
        }))
        .await;
        for result in stopped {
            result?;
        }

        let deliveries = self.claim_due(Utc::now()).await?;
        let sent = join_all(deliveries.into_iter().map(|pending| async move {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.delivery.deliver(&pending.schedule, &pending.token),
            )
            .await;
            let success = matches!(result, Ok(Ok(())));
            if !success {
                tracing::warn!("scheduled delivery failed or timed out");
            }
            self.finish_delivery(&pending, success).await
        }))
        .await;
        for result in sent {
            result?;
        }
        Ok(())
    }

    async fn claim_cleanup(&self, now: DateTime<Utc>) -> Result<Vec<Cleanup>> {
        self.change(|doc| {
            let mut claimed = Vec::new();
            let busy: HashSet<_> = doc
                .cleanup
                .iter()
                .filter(|entry| entry.claim.is_some())
                .map(|entry| entry.duid.clone())
                .collect();
            for cleanup in &mut doc.cleanup {
                if claimed.len() >= BATCH {
                    break;
                }
                if cleanup.claim.is_some()
                    || busy.contains(&cleanup.duid)
                    || !self.delivery.connected(&cleanup.duid)
                {
                    continue;
                }
                if cleanup
                    .retry_at
                    .as_deref()
                    .map(parse_stamp)
                    .transpose()?
                    .is_some_and(|at| at > now)
                {
                    continue;
                }
                cleanup.claim = Some(Claim {
                    token: Uuid::new_v4().to_string(),
                    started_at: stamp(now),
                });
                claimed.push(cleanup.clone());
            }
            Ok(claimed)
        })
        .await
    }

    async fn finish_cleanup(&self, completed: &Cleanup, success: bool) -> Result<()> {
        self.change(|doc| {
            let Some(index) = doc.cleanup.iter().position(|entry| {
                entry.duid == completed.duid
                    && entry.run_id == completed.run_id
                    && entry.claim.as_ref().map(|claim| &claim.token)
                        == completed.claim.as_ref().map(|claim| &claim.token)
            }) else {
                return Ok(());
            };
            if success {
                doc.cleanup.remove(index);
                if !doc.cleanup.iter().any(|entry| entry.duid == completed.duid) {
                    for entry in doc
                        .entries
                        .iter_mut()
                        .filter(|entry| entry.schedule.duid == completed.duid)
                    {
                        if entry.schedule.last_error.as_deref() == Some(PENDING_STOP) {
                            entry.schedule.last_error = None;
                        }
                    }
                }
            } else {
                doc.cleanup[index].claim = None;
                doc.cleanup[index].retry_at =
                    Some(stamp(Utc::now() + Duration::seconds(RETRY_SECONDS)));
            }
            Ok(())
        })
        .await
    }

    async fn claim_due(&self, now: DateTime<Utc>) -> Result<Vec<PendingDelivery>> {
        self.change(|doc| {
            let mut blocked: HashSet<_> = doc.cleanup.iter().map(|entry| entry.duid.clone()).collect();
            blocked.extend(doc.entries.iter().filter(|entry| entry.claim.is_some()).map(|entry| entry.schedule.duid.clone()));
            let mut pending = Vec::new();
            for entry in &mut doc.entries {
                if pending.len() >= BATCH { break; }
                if !entry.schedule.enabled || entry.claim.is_some() {
                    continue;
                }
                let Some(due) = entry.schedule.next_run_at.as_deref().map(parse_stamp).transpose()? else { continue; };
                if due > now { continue; }
                if now.signed_duration_since(due).num_seconds() > LATE_WINDOW {
                    consume(entry, now)?;
                    entry.schedule.last_run_at = Some(stamp(due));
                    entry.schedule.status = "missed".into();
                    entry.schedule.last_error = Some("Пропущено: сервер или колонка были недоступны дольше пяти минут".into());
                    entry.schedule.updated_at = stamp(now);
                    continue;
                }
                if blocked.contains(&entry.schedule.duid) { continue; }
                if !self.delivery.connected(&entry.schedule.duid) {
                    entry.schedule.last_error = Some("Ожидаем подключения колонки, не дольше пяти минут после назначенного времени".into());
                    continue;
                }
                let token = Uuid::new_v4().to_string();
                consume(entry, now)?;
                entry.claim = Some(Claim { token: token.clone(), started_at: stamp(now) });
                entry.schedule.last_run_at = Some(stamp(due));
                entry.schedule.last_error = None;
                entry.schedule.updated_at = stamp(now);
                if entry.schedule.kind.rings() {
                    entry.schedule.active_run_id = Some(token.clone());
                    entry.schedule.ringing_until = Some(stamp(now + Duration::seconds(ACTIVE_WINDOW)));
                }
                blocked.insert(entry.schedule.duid.clone());
                pending.push(PendingDelivery { schedule: entry.schedule.clone(), token });
            }
            if pending.iter().any(|delivery| delivery.schedule.kind.rings()) {
                // Auto-stop applies to the shared alarm channel. Show its actual
                // deadline on every overlapping occurrence, including older ones.
                let mut deadlines = HashMap::<String, String>::new();
                for entry in doc.entries.iter().filter(|entry| entry.schedule.active_run_id.is_some()) {
                    if let Some(until) = &entry.schedule.ringing_until {
                        deadlines.entry(entry.schedule.duid.clone())
                            .and_modify(|latest| { if until > latest { *latest = until.clone(); } })
                            .or_insert_with(|| until.clone());
                    }
                }
                for entry in doc.entries.iter_mut().filter(|entry| entry.schedule.active_run_id.is_some()) {
                    if let Some(until) = deadlines.get(&entry.schedule.duid) {
                        if entry.schedule.ringing_until.as_ref() != Some(until) {
                            entry.schedule.ringing_until = Some(until.clone());
                            entry.schedule.updated_at = stamp(now);
                        }
                    }
                }
                for delivery in &mut pending {
                    if delivery.schedule.kind.rings() {
                        delivery.schedule.ringing_until = deadlines.get(&delivery.schedule.duid).cloned();
                    }
                }
            }
            Ok(pending)
        }).await
    }

    async fn finish_delivery(&self, pending: &PendingDelivery, success: bool) -> Result<()> {
        self.change(|doc| {
            let Some(entry) = doc.entries.iter_mut().find(|entry| entry.schedule.id == pending.schedule.id && entry.claim.as_ref().is_some_and(|claim| claim.token == pending.token)) else { return Ok(()); };
            entry.claim = None;
            entry.schedule.updated_at = stamp(Utc::now());
            let confirmed = entry.schedule.kind.rings()
                && (entry.schedule.status == "ringing"
                    || (entry.schedule.status == "stopped" && entry.schedule.active_run_id.is_none()));
            if success || confirmed {
                // Native confirmation can arrive before the queue result is saved.
                if !confirmed {
                    entry.schedule.status = "sent".into();
                }
                entry.schedule.last_error = None;
            } else {
                entry.schedule.status = "failed".into();
                entry.schedule.last_error = Some("Не удалось подтвердить отправку. Автоматического повтора нет, чтобы не озвучить событие дважды".into());
                if entry.schedule.kind.rings() {
                    retire_station(doc, &pending.schedule.duid, "failed", Utc::now());
                }
            }
            Ok(())
        }).await
    }
}

fn consume(entry: &mut Entry, now: DateTime<Utc>) -> Result<()> {
    if entry
        .schedule
        .snoozed_until
        .as_deref()
        .map(parse_stamp)
        .transpose()?
        .is_some_and(|at| at <= now)
    {
        entry.schedule.snoozed_until = None;
    }
    if entry
        .regular_next_at
        .as_deref()
        .map(parse_stamp)
        .transpose()?
        .is_some_and(|at| at <= now)
    {
        entry.regular_next_at = if entry.schedule.kind == Kind::Timer {
            entry.schedule.remaining_ms = None;
            None
        } else {
            next(&entry.schedule.input(), now)?
        };
    }
    entry.schedule.enabled =
        entry.regular_next_at.is_some() || entry.schedule.snoozed_until.is_some();
    entry.refresh_next();
    Ok(())
}

fn recover(doc: &mut Document, now: DateTime<Utc>) -> Result<()> {
    let mut uncertain_stations = HashSet::new();
    for entry in &mut doc.entries {
        if entry
            .claim
            .as_ref()
            .map(|claim| parse_stamp(&claim.started_at))
            .transpose()?
            .is_some_and(|at| now.signed_duration_since(at).num_seconds() >= CLAIM_TIMEOUT)
        {
            entry.claim = None;
            entry.schedule.status = "failed".into();
            entry.schedule.last_error = Some("Обработка прервалась; результат отправки неизвестен. Событие не запускается повторно".into());
            entry.schedule.updated_at = stamp(now);
            if entry.schedule.active_run_id.is_some() {
                uncertain_stations.insert(entry.schedule.duid.clone());
            }
        }
    }
    for duid in uncertain_stations {
        retire_station(doc, &duid, "failed", now);
    }
    let mut active_until = HashMap::<String, DateTime<Utc>>::new();
    for entry in doc
        .entries
        .iter()
        .filter(|entry| entry.schedule.active_run_id.is_some())
    {
        if let Some(until) = entry
            .schedule
            .ringing_until
            .as_deref()
            .map(parse_stamp)
            .transpose()?
        {
            active_until
                .entry(entry.schedule.duid.clone())
                .and_modify(|latest| *latest = (*latest).max(until))
                .or_insert(until);
        }
    }
    for (duid, until) in active_until {
        if until <= now {
            retire_station(doc, &duid, "expired", now);
        }
    }
    for cleanup in &mut doc.cleanup {
        if cleanup
            .claim
            .as_ref()
            .map(|claim| parse_stamp(&claim.started_at))
            .transpose()?
            .is_some_and(|at| now.signed_duration_since(at).num_seconds() >= CLAIM_TIMEOUT)
        {
            cleanup.claim = None;
            cleanup.retry_at = None;
        }
    }
    Ok(())
}
