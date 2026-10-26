//! Firmware updates: `updates-config.yml` pins the last working versions
//! with bad certificates; with ALLOW_UPDATES=false the station never
//! receives an update.

use std::collections::BTreeMap;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
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
    /// Like TS: the file is read only with ALLOW_UPDATES and UPDATES_CONFIG_PATH
    /// set, and a broken file stops the server instead of silently offering nothing
    pub fn load(enabled: bool, path: &Option<String>) -> anyhow::Result<Self> {
        let updates = match path {
            Some(path) if enabled => {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("reading updates config {path}"))?;
                serde_yaml::from_str::<UpdatesFile>(&content)
                    .with_context(|| format!("parsing updates config {path}"))?
                    .updates
            }
            _ => BTreeMap::new(),
        };
        tracing::info!("updates: {} for {} platform(s)", if enabled { "allowed" } else { "disabled" }, updates.len());
        Ok(Self { enabled, updates })
    }

    /// register's hasCriticalUpdate: the station runs another version than the pinned one
    pub fn has_critical_update(&self, platform: &str, firmware_version: &str) -> bool {
        self.enabled
            && self
                .updates
                .get(platform)
                .is_some_and(|update| update.version != firmware_version)
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
