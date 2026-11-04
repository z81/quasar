//! Queue scheduled occurrences through the existing station connection.

use std::sync::Arc;

use anyhow::{ensure, Result};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::json;

use crate::connections::{ConnectionRegistry, StationCommand};
use crate::control::Control;
use crate::schedules::{Delivery, Kind, Schedule};

/// Called by one FIFO consumer per station socket, preserving native start/stop
/// ordering even when a database round trip is slower than the next event.
pub async fn observe(
    service: &crate::schedules::Scheduler,
    duid: &str,
    alarms: Vec<(String, bool)>,
) {
    match service.list(duid).await {
        Ok(items) => {
            for item in items {
                if let Some(run_id) = item.active_run_id.as_deref() {
                    let playing = alarms
                        .iter()
                        .find(|(id, _)| id == run_id)
                        .is_some_and(|(_, playing)| *playing);
                    // The scheduler ignores false until this occurrence was observed ringing.
                    if let Err(error) = service.acknowledge_alarm(duid, run_id, playing).await {
                        tracing::warn!("cannot update native alarm state: {error}");
                    }
                }
            }
        }
        Err(error) => tracing::warn!("cannot read managed alarms for native state: {error}"),
    }
}

pub struct StationDelivery {
    pub registry: ConnectionRegistry,
    pub control: Arc<Control>,
}

#[async_trait]
impl Delivery for StationDelivery {
    fn connected(&self, duid: &str) -> bool {
        self.registry.is_connected(duid)
    }

    async fn deliver(&self, item: &Schedule, run_id: &str) -> Result<()> {
        match item.kind {
            Kind::Alarm => {
                ensure!(
                    self.registry.alarm_support(&item.duid) != Some(false),
                    "Колонка не поддерживает нативные будильники"
                );
                let directive = crate::alarm_protocol::start(
                    run_id,
                    &item.title,
                    &item.timezone,
                    Utc::now() + Duration::seconds(2),
                )?;
                ensure!(
                    self.registry.send_one(
                        &item.duid,
                        StationCommand::Directives {
                            directives: vec![directive]
                        }
                    ),
                    "Колонка отключилась до отправки будильника"
                );
            }
            Kind::Reminder => {
                let text = format!("Напоминание. {}", item.text);
                let feedback = self
                    .control
                    .create(
                        &item.duid,
                        &item.duid,
                        "push-raw",
                        json!({"eventText": text, "scheduleId": item.id}),
                    )
                    .await?;
                let delivered = self.registry.send_one(
                    &item.duid,
                    StationCommand::Tracked {
                        feedback: feedback.clone(),
                        command: Box::new(StationCommand::Speak { text }),
                    },
                );
                feedback.delivered(usize::from(delivered)).await;
                if !delivered {
                    feedback
                        .failed("Колонка отключилась до отправки напоминания")
                        .await;
                    anyhow::bail!("Колонка отключилась до отправки напоминания");
                }
            }
        }
        // A successful enqueue is not confirmation that sound was played.
        Ok(())
    }

    async fn stop(&self, duid: &str, run_id: &str) -> Result<()> {
        ensure!(
            self.registry.send_one(
                duid,
                StationCommand::Directives {
                    directives: vec![
                        crate::alarm_protocol::stop(),
                        crate::alarm_protocol::cancel(run_id)
                    ],
                }
            ),
            "Остановка будет отправлена после подключения колонки"
        );
        Ok(())
    }

    async fn cancel(&self, duid: &str, run_id: &str) -> Result<()> {
        ensure!(
            self.registry.send_one(
                duid,
                StationCommand::Directives {
                    directives: vec![crate::alarm_protocol::cancel(run_id)],
                }
            ),
            "Удаление завершённого будильника ожидает подключения колонки"
        );
        Ok(())
    }
}
