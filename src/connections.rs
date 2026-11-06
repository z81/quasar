//! Registry of live uni.ws connections so the management API can reach
//! stations (push events and raw TTS).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use std::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub enum StationCommand {
    RefreshPlayer,
    RefreshSmartHome,
    /// Management command whose result is recorded in persistent history.
    Tracked {
        feedback: crate::control::CommandFeedback,
        command: Box<StationCommand>,
    },
    /// Text goes through the dialog agent; the answer is spoken
    Event { text: String },
    /// Text is spoken directly, bypassing the agent
    Speak { text: String },
    /// Text answered as if the user said it: the agent's answer is spoken and
    /// its functions run, as for a recognized utterance
    Utterance { text: String },
    /// Directives delivered to the station in a Push event (TS sendPush),
    /// e.g. push_update_config after a config change or an API push-directive
    Directives { directives: Vec<crate::proto::n_alice::n_alice_api::TDirective> },
}

/// Every live connection of a device, keyed by a per-connection id: a station
/// may briefly hold two sockets (reconnect while the old one is half-open),
/// and closing one must not drop the other
type StationSender = mpsc::UnboundedSender<StationCommand>;
type DeviceConnections = HashMap<u64, StationSender>;

#[derive(Clone, Default)]
pub struct ConnectionRegistry {
    connections: Arc<Mutex<HashMap<String, DeviceConnections>>>,
    next_id: Arc<AtomicU64>,
    pub player: crate::player::PlayerRegistry,
    pub smart_home: crate::smart_home::Registry,
    timezones: Arc<Mutex<HashMap<String, String>>>,
    reported_timezones: Arc<Mutex<HashMap<String, String>>>,
    alarm_support: Arc<Mutex<HashMap<String, bool>>>,
    display_capabilities: Arc<Mutex<HashMap<u64, crate::display::DisplayCapability>>>,
    sound_capabilities: Arc<Mutex<HashMap<u64, crate::sound::SoundCapability>>>,
    dnd_states: Arc<Mutex<HashMap<u64, (bool, String)>>>,
    dnd_capabilities: Arc<Mutex<HashMap<u64, crate::dnd::protocol::DndCapability>>>,
}

impl ConnectionRegistry {
    pub fn set_timezone(&self, device_id: &str, timezone: Option<&str>) {
        let mut zones = self.timezones.lock().unwrap();
        if let Some(zone) = timezone {
            zones.insert(device_id.to_owned(), zone.to_owned());
        } else {
            zones.remove(device_id);
        }
    }

    pub fn timezone(&self, device_id: &str) -> Option<String> {
        self.timezones.lock().unwrap().get(device_id).cloned()
    }

    pub fn report_timezone(&self, device_id: &str, timezone: &str) {
        if timezone.parse::<chrono_tz::Tz>().is_ok() {
            self.reported_timezones.lock().unwrap().insert(device_id.to_owned(), timezone.to_owned());
        }
    }

    pub fn effective_timezone(&self, device_id: &str) -> Option<String> {
        self.timezone(device_id).or_else(|| self.reported_timezones.lock().unwrap().get(device_id).cloned())
    }

    pub fn reported_timezone(&self, device_id: &str) -> Option<String> {
        self.reported_timezones.lock().unwrap().get(device_id).cloned()
    }

    pub fn current_connection_id(&self, device_id: &str) -> Option<u64> {
        self.connections.lock().unwrap().get(device_id)
            .and_then(|targets| targets.iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max())
    }

    pub fn report_dnd_enabled(&self, device_id: &str, connection_id: u64, enabled: bool) {
        let connections = self.connections.lock().unwrap();
        let current = connections.get(device_id)
            .and_then(|targets| targets.iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max());
        if current == Some(connection_id) {
            self.dnd_states.lock().unwrap().insert(connection_id, (enabled, chrono::Utc::now().to_rfc3339()));
        }
    }

    pub fn dnd_enabled(&self, device_id: &str) -> Option<(bool, String)> {
        let connections = self.connections.lock().unwrap();
        let connection_id = connections.get(device_id)?
            .iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max()?;
        self.dnd_states.lock().unwrap().get(&connection_id).cloned()
    }

    pub fn report_dnd_capability(&self, device_id: &str, connection_id: u64, endpoint_id: &str,
        update: crate::proto::n_alice::TDoNotDisturbCapability) {
        let connections = self.connections.lock().unwrap();
        let current = connections.get(device_id)
            .and_then(|targets| targets.iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max());
        if current == Some(connection_id) {
            let has_state = update.state.is_some();
            let enabled = {
                let mut capabilities = self.dnd_capabilities.lock().unwrap();
                let capability = capabilities.entry(connection_id).or_default();
                capability.update(endpoint_id, update);
                capability.reported_enabled().filter(|_| has_state)
            };
            if let Some(enabled) = enabled {
                self.dnd_states.lock().unwrap().insert(connection_id, (enabled, chrono::Utc::now().to_rfc3339()));
            }
        }
    }

    pub fn dnd_capability(&self, device_id: &str) -> Option<(u64, crate::dnd::protocol::DndCapability)> {
        let connections = self.connections.lock().unwrap();
        let connection_id = connections.get(device_id)?
            .iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max()?;
        self.dnd_capabilities.lock().unwrap().get(&connection_id).cloned()
            .map(|capability| (connection_id, capability))
    }

    pub fn report_alarm_support(&self, device_id: &str, supported: bool) {
        self.alarm_support.lock().unwrap().insert(device_id.to_owned(), supported);
    }

    pub fn alarm_support(&self, device_id: &str) -> Option<bool> {
        self.alarm_support.lock().unwrap().get(device_id).copied()
    }

    pub fn report_display_capability(
        &self,
        device_id: &str,
        connection_id: u64,
        endpoint_id: &str,
        update: crate::proto::n_alice::TAnimationCapability,
    ) {
        let connections = self.connections.lock().unwrap();
        let current = connections.get(device_id)
            .and_then(|targets| targets.iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max());
        if current == Some(connection_id) {
            self.display_capabilities.lock().unwrap().entry(connection_id)
                .or_default().update(endpoint_id, update);
        }
    }

    pub fn display_capability(&self, device_id: &str) -> Option<(u64, crate::display::DisplayCapability)> {
        let connections = self.connections.lock().unwrap();
        let connection_id = connections.get(device_id)?
            .iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max()?;
        self.display_capabilities.lock().unwrap().get(&connection_id).cloned()
            .map(|capability| (connection_id, capability))
    }

    pub fn send_to_current(&self, device_id: &str, connection_id: u64, command: StationCommand) -> bool {
        let connections = self.connections.lock().unwrap();
        connections.get(device_id)
            .and_then(|targets| targets.iter().filter(|(_, sender)| !sender.is_closed()).max_by_key(|(id, _)| *id))
            .is_some_and(|(id, sender)| *id == connection_id && sender.send(command).is_ok())
    }

    pub fn report_sound_capability(&self, device_id: &str, connection_id: u64, endpoint_id: &str,
        update: crate::proto::n_alice::TEqualizerCapability) {
        let connections = self.connections.lock().unwrap();
        let current = connections.get(device_id)
            .and_then(|targets| targets.iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max());
        if current == Some(connection_id) {
            self.sound_capabilities.lock().unwrap().entry(connection_id)
                .or_default().update(endpoint_id, update);
        }
    }

    pub fn sound_capability(&self, device_id: &str) -> Option<(u64, crate::sound::SoundCapability)> {
        let connections = self.connections.lock().unwrap();
        let connection_id = connections.get(device_id)?
            .iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max()?;
        self.sound_capabilities.lock().unwrap().get(&connection_id).cloned()
            .map(|capability| (connection_id, capability))
    }

    pub fn is_current(&self, duid: &str, connection_id: u64) -> bool {
        self.connections.lock().unwrap().get(duid)
            .and_then(|targets| targets.iter().filter(|(_, sender)| !sender.is_closed()).map(|(id, _)| *id).max())
            == Some(connection_id)
    }

    /// A scheduled occurrence must reach only one live socket during reconnects.
    pub fn send_one(&self, duid: &str, command: StationCommand) -> bool {
        let connections = self.connections.lock().unwrap();
        let Some(targets) = connections.get(duid) else { return false; };
        targets.iter()
            .filter(|(_, sender)| !sender.is_closed())
            .max_by_key(|(id, _)| *id)
            .is_some_and(|(_, sender)| sender.send(command).is_ok())
    }
    /// Whether the server still has an open command channel for this station.
    pub fn is_connected(&self, device_id: &str) -> bool {
        self.connections
            .lock()
            .unwrap()
            .get(device_id)
            .is_some_and(|connections| connections.values().any(|sender| !sender.is_closed()))
    }

    /// Returns the connection id to pass to `unregister`
    pub fn register(&self, device_id: &str, sender: StationSender) -> u64 {
        let connection_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.connections
            .lock()
            .unwrap()
            .entry(device_id.to_string())
            .or_default()
            .insert(connection_id, sender);
        connection_id
    }

    pub fn unregister(&self, device_id: &str, connection_id: u64) {
        let mut connections = self.connections.lock().unwrap();
        if let Some(device_connections) = connections.get_mut(device_id) {
            device_connections.remove(&connection_id);
            if device_connections.is_empty() {
                connections.remove(device_id);
            }
        }
        self.display_capabilities.lock().unwrap().remove(&connection_id);
        self.sound_capabilities.lock().unwrap().remove(&connection_id);
        self.dnd_states.lock().unwrap().remove(&connection_id);
        self.dnd_capabilities.lock().unwrap().remove(&connection_id);
        drop(connections);
        self.smart_home.disconnected(device_id, connection_id);
    }

    /// Sends the command to every connection of one device or of all devices;
    /// returns how many connections received it
    pub fn send(&self, duid_or_all: &str, command: StationCommand) -> usize {
        let connections = self.connections.lock().unwrap();
        let targets: Vec<&StationSender> = if duid_or_all == "all" {
            connections.values().flat_map(HashMap::values).collect()
        } else {
            connections.get(duid_or_all).into_iter().flat_map(HashMap::values).collect()
        };
        targets.iter().filter(|sender| sender.send(command.clone()).is_ok()).count()
    }
}
