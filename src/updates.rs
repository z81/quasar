//! Firmware updates: `updates-config.yml` pins the last working versions
//! with bad certificates; with ALLOW_UPDATES=false the station never
//! receives an update.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct UpdateEntry {
    pub version: String,
    pub download_url: String,
    pub crc32: u32,
    pub update_id: String,
    pub fallback_url: Option<String>,
}

#[derive(Deserialize)]
struct UpdatesFile {
    #[serde(default)]
    updates: BTreeMap<String, UpdateEntry>,
}

pub struct UpdatesConfig {
    updates: BTreeMap<String, UpdateEntry>,
    enabled: bool,
}

impl UpdatesConfig {
    pub fn load(enabled: bool, path: &Option<String>) -> Self {
        let updates = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|content| serde_yaml::from_str::<UpdatesFile>(&content).ok())
            .map(|file| file.updates)
            .unwrap_or_default();
        Self { enabled, updates }
    }

    /// The check_updates response for a platform at its current version
    pub fn response_for(&self, platform: &str, version: &str) -> Value {
        let Some(update) = self.updates.get(platform) else {
            return json!({ "hasUpdate": false });
        };
        if !self.enabled || update.version == version {
            return json!({ "hasUpdate": false });
        }
        json!({
            "crc32": update.crc32,
            "critical": true,
            "downloadUrl": update.download_url,
            "fallbackIp": "127.0.0.1",
            "fallbackUrl": update.fallback_url.clone().unwrap_or_else(|| update.download_url.clone()),
            "hasUpdate": true,
            "updateId": update.update_id,
            "version": update.version,
        })
    }
}
