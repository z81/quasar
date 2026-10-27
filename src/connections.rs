//! Registry of live uni.ws connections so the management API can reach
//! stations (push events and raw TTS).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use std::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub enum StationCommand {
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
}

impl ConnectionRegistry {
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
