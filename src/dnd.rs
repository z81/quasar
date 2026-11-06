//! Persistent local-time quiet hours applied through the station's native DND
//! configuration. No volume, microphone, alarm, timer or reminder is changed.

mod calendar;
mod model;
pub mod protocol;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use chrono::{DateTime, SecondsFormat, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::connections::{ConnectionRegistry, StationCommand};
use crate::stations::StationSummary;
use crate::storage::Storage;
use crate::time_zone::Zone;
use model::{Delivery, Document, NativeMode};

pub use model::{DndError, ManualOverride, Schedule, Status};

type Result<T> = std::result::Result<T, DndError>;

struct Receipt {
    connection_id: u64,
    mode: NativeMode,
    force: Option<bool>,
    sent_at: DateTime<Utc>,
    force_sent_at: Option<DateTime<Utc>>,
    last_poll_at: DateTime<Utc>,
}

#[derive(Default)]
struct RuntimeState {
    receipt: Option<Receipt>,
    error: Option<String>,
}

pub struct DndService {
    storage: Arc<Storage>,
    registry: ConnectionRegistry,
    changes: Mutex<HashMap<String, RuntimeState>>,
    started: AtomicBool,
}

impl DndService {
    pub async fn load(storage: Arc<Storage>, registry: ConnectionRegistry) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            storage,
            registry,
            changes: Mutex::new(HashMap::new()),
            started: AtomicBool::new(false),
        }))
    }

    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) || !self.storage.is_persistent() {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Err(error) = service.tick().await {
                    tracing::warn!("DND scheduler failed: {error}");
                }
            }
        });
    }

    /// Reading status never creates a schedule or changes the station config.
    pub async fn status(&self, duid: &str) -> Result<Status> {
        let runtime = self.changes.lock().await;
        let station = self.station(duid).await?;
        let (_, doc) = self.read(duid).await?;
        self.describe(&station, doc.as_ref(), runtime.get(duid), Utc::now())
    }

    pub async fn update(&self, duid: &str, schedule: Schedule) -> Result<Status> {
        let schedule = calendar::validate(schedule)?;
        self.require_persistent()?;
        let mut runtime = self.changes.lock().await;
        let mut station = self.station(duid).await?;
        let (previous, doc) = self.read(duid).await?;
        let now = Utc::now();
        let mut doc = doc.unwrap_or_else(|| self.new_document(&station, now));
        // Each new activation captures the manual setting to restore when the
        // schedule is disabled. Editing active hours keeps the original baseline.
        if !doc.schedule.enabled && !doc.manual_pending {
            doc.baseline = NativeMode::from_config(&station.quasar_config);
        }
        if !doc.schedule.enabled && schedule.enabled {
            doc.baseline_force = doc.manual_force;
        }
        if doc.schedule.enabled && !schedule.enabled {
            doc.manual_force = doc.baseline_force;
        }
        doc.manual_pending = !schedule.enabled && (doc.schedule.enabled || doc.manual_pending);
        doc.schedule = schedule;
        doc.manual_override = None;
        doc.updated_at = stamp(now);
        self.save(duid, previous.as_ref(), &doc).await?;
        let saved = serde_json::to_value(&doc).map_err(unavailable)?;
        let state = runtime.entry(duid.to_owned()).or_default();
        self.reconcile(&mut station, &mut doc, state, now).await?;
        self.save(duid, Some(&saved), &doc).await?;
        self.describe(&station, Some(&doc), Some(state), now)
    }

    /// While scheduled, a manual choice lasts until the next interval boundary.
    /// Without a schedule, it becomes the regular manual setting.
    pub async fn set_manual(&self, duid: &str, enabled: bool) -> Result<Status> {
        self.require_persistent()?;
        let mut runtime = self.changes.lock().await;
        let mut station = self.station(duid).await?;
        let (previous, doc) = self.read(duid).await?;
        let now = Utc::now();
        let mut doc = doc.unwrap_or_else(|| self.new_document(&station, now));
        let window = calendar::window(&doc.schedule, self.zone(&station, Some(&doc)), now)?;
        if doc.schedule.enabled {
            let until = window.next_transition.ok_or(DndError::Unavailable)?;
            doc.manual_override = Some(ManualOverride {
                enabled,
                until: stamp(until),
            });
        } else {
            if !doc.manual_pending {
                doc.baseline = NativeMode::from_config(&station.quasar_config);
            }
            doc.manual_force = Some(enabled);
            doc.manual_pending = true;
            doc.manual_override = None;
        }
        doc.updated_at = stamp(now);
        self.save(duid, previous.as_ref(), &doc).await?;
        let saved = serde_json::to_value(&doc).map_err(unavailable)?;
        let state = runtime.entry(duid.to_owned()).or_default();
        self.reconcile(&mut station, &mut doc, state, now).await?;
        self.save(duid, Some(&saved), &doc).await?;
        self.describe(&station, Some(&doc), Some(state), now)
    }

    async fn tick(&self) -> Result<()> {
        let mut runtime = self.changes.lock().await;
        let stations = self.storage.list().await.map_err(unavailable)?;
        for mut station in stations {
            let (previous, doc) = self.read(&station.duid).await?;
            let Some(mut doc) = doc else {
                continue;
            };
            let state = runtime.entry(station.duid.clone()).or_default();
            let result = self
                .reconcile(&mut station, &mut doc, state, Utc::now())
                .await;
            if let Err(error) = result {
                state.error = Some(error.to_string());
                tracing::warn!(duid = station.duid, "DND reconciliation failed: {error}");
                continue;
            }
            self.save(&station.duid, previous.as_ref(), &doc).await?;
        }
        Ok(())
    }

    async fn reconcile(
        &self,
        station: &mut StationSummary,
        doc: &mut Document,
        runtime: &mut RuntimeState,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let duid = &station.duid;
        if let Some(zone) = self.registry.reported_timezone(duid) {
            doc.station_timezone = Some(zone);
        }
        if doc
            .manual_override
            .as_ref()
            .is_some_and(|value| !override_active(value, now))
        {
            doc.manual_override = None;
        }
        if !doc.schedule.enabled && !doc.manual_pending && doc.manual_force.is_none() {
            runtime.error = None;
            return Ok(());
        }
        let target = self.target(station, doc, now)?;
        let force = self.force(station, doc, now)?;
        let config_changed = NativeMode::from_config(&station.quasar_config) != target;
        if config_changed || runtime.receipt.is_none() {
            // Immutable original snapshot precedes either a config change or a
            // typed force, including when the native schedule flag stays false.
            // It contains the server configuration only, never device credentials.
            self.storage
                .insert_control_value(
                    &format!("dnd-backup:v1:{duid}"),
                    &json!({
                        "created_at": stamp(now), "quasarConfig": station.quasar_config,
                    }),
                )
                .await
                .map_err(unavailable)?;
        }
        if config_changed {
            if !self
                .storage
                .update_dnd_mode(duid, &target.value())
                .await
                .map_err(unavailable)?
            {
                return Err(DndError::NotFound);
            }
            station.quasar_config["deviceConfig"]["dndMode"] = target.value();
        }
        runtime.error = None;
        let Some(connection_id) = self.registry.current_connection_id(duid) else {
            return Ok(());
        };
        let confirmed = self.confirmed(duid, runtime, &target, force);
        if !doc.schedule.enabled && confirmed {
            doc.manual_pending = false;
            return Ok(());
        }
        let configure = config_changed
            || runtime.receipt.as_ref().is_none_or(|receipt| {
                receipt.connection_id != connection_id || receipt.mode != target
            });
        if configure {
            let command = StationCommand::Directives {
                directives: vec![crate::api::raw_directive::push_update_config()],
            };
            if self.registry.send_to_current(duid, connection_id, command) {
                doc.last_sent_at = Some(stamp(now));
                runtime.receipt = Some(Receipt {
                    connection_id,
                    mode: target,
                    force,
                    sent_at: now,
                    force_sent_at: None,
                    last_poll_at: now,
                });
            }
            return Ok(());
        }
        let Some(receipt) = runtime.receipt.as_mut() else {
            return Ok(());
        };
        if receipt.force != force {
            receipt.force = force;
            receipt.force_sent_at = None;
        }
        // Native config reload resets the firmware's force override. Allow the
        // asynchronous config fetch to finish before applying an explicit state.
        if now.signed_duration_since(receipt.sent_at).num_seconds() < 5 {
            return Ok(());
        }
        let force_due = receipt
            .force_sent_at
            .is_none_or(|sent| !confirmed && now.signed_duration_since(sent).num_seconds() >= 30);
        if force_due {
            let directive = self
                .registry
                .dnd_capability(duid)
                .filter(|(id, _)| *id == connection_id)
                .map(|(_, capability)| capability.force_directive(force));
            match directive {
                Some(Ok(directive)) => {
                    if self.registry.send_to_current(
                        duid,
                        connection_id,
                        StationCommand::Directives {
                            directives: vec![directive],
                        },
                    ) {
                        receipt.force_sent_at = Some(now);
                        receipt.last_poll_at = now;
                        doc.last_sent_at = Some(stamp(now));
                    }
                }
                unavailable => {
                    if let Some(Err(error)) = unavailable {
                        runtime.error = Some(error.to_owned());
                    }
                    self.registry.send_to_current(
                        duid,
                        connection_id,
                        StationCommand::RefreshSmartHome,
                    );
                }
            }
        } else if !confirmed
            && now
                .signed_duration_since(receipt.last_poll_at)
                .num_seconds()
                >= 5
        {
            // The existing state query is silent; it confirms actual DNDEnabled.
            self.registry
                .send_to_current(duid, connection_id, StationCommand::RefreshPlayer);
            receipt.last_poll_at = now;
        }
        Ok(())
    }

    fn confirmed(
        &self,
        duid: &str,
        runtime: &RuntimeState,
        target: &NativeMode,
        force: Option<bool>,
    ) -> bool {
        let Some(receipt) = runtime.receipt.as_ref() else {
            return false;
        };
        if receipt.mode != *target
            || receipt.force != force
            || self.registry.current_connection_id(duid) != Some(receipt.connection_id)
        {
            return false;
        }
        let Some(sent) = receipt.force_sent_at else {
            return false;
        };
        self.registry
            .dnd_enabled(duid)
            .is_some_and(|(enabled, observed)| {
                force.is_none_or(|target| enabled == target)
                    && DateTime::parse_from_rfc3339(&observed)
                        .is_ok_and(|observed| observed.with_timezone(&Utc) >= sent)
            })
    }

    fn target(
        &self,
        station: &StationSummary,
        doc: &Document,
        _now: DateTime<Utc>,
    ) -> Result<NativeMode> {
        if !doc.schedule.enabled {
            return Ok(if doc.manual_pending {
                doc.baseline.clone()
            } else {
                NativeMode::from_config(&station.quasar_config)
            });
        }
        Ok(NativeMode {
            // The native flag enables a separate daily schedule. The server
            // owns weekdays and explicit state through typed force directives.
            enabled: false,
            allow_incoming_calls: doc.schedule.allow_incoming_calls,
        })
    }

    fn force(
        &self,
        station: &StationSummary,
        doc: &Document,
        now: DateTime<Utc>,
    ) -> Result<Option<bool>> {
        if !doc.schedule.enabled {
            return Ok(doc.manual_force);
        }
        let window = calendar::window(&doc.schedule, self.zone(station, Some(doc)), now)?;
        Ok(Some(
            doc.manual_override
                .as_ref()
                .filter(|value| override_active(value, now))
                .map_or(window.active, |value| value.enabled),
        ))
    }

    fn describe(
        &self,
        station: &StationSummary,
        doc: Option<&Document>,
        runtime: Option<&RuntimeState>,
        now: DateTime<Utc>,
    ) -> Result<Status> {
        let schedule = doc.map(|doc| doc.schedule.clone()).unwrap_or_default();
        let zone = self.zone(station, doc);
        let window = calendar::window(&schedule, zone, now)?;
        let connected = self.registry.is_connected(&station.duid);
        let native = NativeMode::from_config(&station.quasar_config);
        let target = doc
            .map(|doc| self.target(station, doc, now))
            .transpose()?
            .unwrap_or_else(|| native.clone());
        let force = doc
            .map(|doc| self.force(station, doc, now))
            .transpose()?
            .flatten();
        let confirmed =
            runtime.is_some_and(|runtime| self.confirmed(&station.duid, runtime, &target, force));
        let observation = self.registry.dnd_enabled(&station.duid);
        let status = if runtime.is_some_and(|runtime| runtime.error.is_some()) {
            "error"
        } else if doc.is_none_or(|doc| {
            !doc.schedule.enabled && !doc.manual_pending && doc.manual_force.is_none()
        }) {
            "idle"
        } else if !connected {
            "offline"
        } else if confirmed {
            "confirmed"
        } else if native != target
            || runtime
                .and_then(|runtime| runtime.receipt.as_ref())
                .is_none()
        {
            "pending"
        } else {
            "sent"
        };
        Ok(Status {
            schedule,
            timezone: zone.name().to_owned(),
            current_enabled: force
                .or_else(|| observation.as_ref().map(|(enabled, _)| *enabled))
                .unwrap_or(false),
            forced_target: force,
            scheduled_active: window.active,
            next_transition_at: window.next_transition.map(stamp),
            next_enabled: window.next_enabled,
            manual_override: doc
                .and_then(|doc| doc.manual_override.clone())
                .filter(|value| override_active(value, now)),
            connected,
            persistent: self.storage.is_persistent(),
            delivery: Delivery {
                status,
                last_sent_at: doc.and_then(|doc| doc.last_sent_at.clone()),
                confirmed_enabled: observation.as_ref().map(|(enabled, _)| *enabled),
                confirmed_at: observation.map(|(_, at)| at),
                error: runtime.and_then(|runtime| runtime.error.clone()),
            },
            server_time: stamp(now),
        })
    }

    fn zone(&self, station: &StationSummary, doc: Option<&Document>) -> Tz {
        let station_zone = station
            .timezone
            .clone()
            .or_else(|| self.registry.reported_timezone(&station.duid))
            .or_else(|| doc.and_then(|doc| doc.station_timezone.clone()));
        match Zone::of_station(station_zone.as_deref()) {
            Zone::Named(zone) => zone,
            Zone::Local => chrono_tz::UTC,
        }
    }

    fn new_document(&self, station: &StationSummary, now: DateTime<Utc>) -> Document {
        Document {
            schedule: Schedule::default(),
            baseline: NativeMode::from_config(&station.quasar_config),
            baseline_force: None,
            manual_force: None,
            manual_override: None,
            manual_pending: false,
            station_timezone: self.registry.reported_timezone(&station.duid),
            updated_at: stamp(now),
            last_sent_at: None,
        }
    }

    async fn station(&self, duid: &str) -> Result<StationSummary> {
        self.storage
            .list()
            .await
            .map_err(unavailable)?
            .into_iter()
            .find(|station| station.duid == duid)
            .ok_or(DndError::NotFound)
    }

    async fn read(&self, duid: &str) -> Result<(Option<Value>, Option<Document>)> {
        let value = self
            .storage
            .get_control_value(&format!("dnd:v1:{duid}"))
            .await
            .map_err(unavailable)?;
        let doc = value
            .as_ref()
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(unavailable)?;
        Ok((value, doc))
    }

    async fn save(&self, duid: &str, previous: Option<&Value>, doc: &Document) -> Result<()> {
        let replacement = serde_json::to_value(doc).map_err(unavailable)?;
        if previous == Some(&replacement) {
            return Ok(());
        }
        let key = format!("dnd:v1:{duid}");
        let saved = match previous {
            Some(previous) => {
                self.storage
                    .compare_control_value(&key, previous, &replacement)
                    .await
            }
            None => self.storage.insert_control_value(&key, &replacement).await,
        }
        .map_err(unavailable)?;
        if saved {
            Ok(())
        } else {
            Err(DndError::Unavailable)
        }
    }

    fn require_persistent(&self) -> Result<()> {
        if self.storage.is_persistent() {
            Ok(())
        } else {
            Err(DndError::NotPersistent)
        }
    }
}

fn override_active(value: &ManualOverride, now: DateTime<Utc>) -> bool {
    DateTime::parse_from_rfc3339(&value.until).is_ok_and(|until| until.with_timezone(&Utc) > now)
}

fn stamp(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn unavailable(error: impl std::fmt::Display) -> DndError {
    tracing::warn!("DND persistence failure: {error}");
    DndError::Unavailable
}
