//! Registry of live uni.ws connections so the management API can reach
//! stations (push events and raw TTS).

use std::collections::HashMap;
use std::sync::Arc;

use std::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub enum StationCommand {
    /// Text goes through the dialog agent; the answer is spoken
    Event { text: String },
    /// Text is spoken directly, bypassing the agent
    Speak { text: String },
}

#[derive(Clone, Default)]
pub struct ConnectionRegistry {
    connections: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<StationCommand>>>>,
}

impl ConnectionRegistry {
    pub fn register(&self, device_id: &str, sender: mpsc::UnboundedSender<StationCommand>) {
        self.connections
            .lock()
            .unwrap()
            .insert(device_id.to_string(), sender);
    }

    pub fn unregister(&self, device_id: &str) {
        self.connections.lock().unwrap().remove(device_id);
    }

    /// Sends the command to one device or to all connected ones; returns how many received it
    pub fn send(&self, duid_or_all: &str, command: StationCommand) -> usize {
        let connections = self.connections.lock().unwrap();
        let targets: Vec<&mpsc::UnboundedSender<StationCommand>> = if duid_or_all == "all" {
            connections.values().collect()
        } else {
            connections.get(duid_or_all).into_iter().collect()
        };
        targets.iter().filter(|sender| sender.send(command.clone()).is_ok()).count()
    }
}
