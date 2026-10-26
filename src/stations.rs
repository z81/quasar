//! In-memory station registry and TLS certificate generation (the PostgreSQL
//! store in `storage.rs` reuses both).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::station_config;

#[derive(Clone)]
pub struct StationSummary {
    pub certificate_pem: String,
    pub duid: String,
    pub name: String,
    pub network_info: Option<Value>,
    pub platform: String,
    pub private_key_pem: String,
    /// Parsed quasar config: every default filled in
    pub quasar_config: Value,
}

impl StationSummary {
    /// One glagol/device_list entry (TS routers/quasar.yandex.net.ts)
    pub fn to_glagol_device(&self) -> Value {
        json!({
            "activation_code": random_activation_code(),
            "activation_region": "RU",
            "config": station_config::glagol_device_config(&self.quasar_config, &self.name),
            "glagol": {
                "security": {
                    "server_certificate": self.certificate_pem,
                    "server_private_key": self.private_key_pem,
                },
            },
            "id": self.duid,
            "name": self.name,
            "networkInfo": self.network_info,
            "platform": self.platform,
            "promocode_activated": false,
            "tags": [],
        })
    }

    /// The get_sync_info response body
    pub fn to_sync_info(&self) -> Value {
        station_config::sync_info(&self.quasar_config, &self.name, &self.certificate_pem, &self.private_key_pem)
    }

    /// The management API view (TS GET /devices, PATCH /devices/:duid)
    pub fn to_api_device(&self) -> Value {
        json!({
            "duid": self.duid,
            "name": self.name,
            "quasarConfig": self.quasar_config,
        })
    }
}

/// TS Math.floor(Math.random() * 1_000_000_000)
fn random_activation_code() -> u64 {
    (uuid::Uuid::new_v4().as_u128() % 1_000_000_000) as u64
}

/// A self-signed certificate the hacked firmware accepts (CN=localhost, O=Yandex)
pub fn generate_certificate() -> anyhow::Result<(String, String)> {
    let mut parameters = rcgen::CertificateParams::new(vec!["localhost".to_string()])?;
    parameters.distinguished_name = {
        let mut name = rcgen::DistinguishedName::new();
        name.push(rcgen::DnType::CommonName, "localhost");
        name.push(rcgen::DnType::OrganizationName, "Yandex");
        name
    };
    let key_pair = rcgen::KeyPair::generate()?;
    let certificate = parameters.self_signed(&key_pair)?;
    Ok((certificate.pem(), key_pair.serialize_pem()))
}

/// Name of the station registered as number `index`
pub fn station_name(index: impl std::fmt::Display) -> String {
    format!("My Favourite Yandex Station No. {index:05}")
}

#[derive(Default)]
pub struct StationStore {
    stations: RwLock<HashMap<String, StationSummary>>,
}

impl StationStore {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The station, registered with a new certificate and the default config
    /// on first contact (TS getOrCreate)
    pub async fn get_or_create(&self, duid: &str, platform: &str) -> anyhow::Result<StationSummary> {
        if let Some(station) = self.stations.read().await.get(duid) {
            return Ok(station.clone());
        }

        let (certificate_pem, private_key_pem) = generate_certificate()?;
        let mut stations = self.stations.write().await;
        let index = stations.len();
        // a concurrent first contact may have registered it meanwhile
        let station = stations.entry(duid.to_string()).or_insert_with(|| {
            tracing::info!("registered station {duid} ({platform})");
            StationSummary {
                duid: duid.to_string(),
                name: station_name(index),
                network_info: None,
                platform: platform.to_string(),
                certificate_pem,
                private_key_pem,
                quasar_config: station_config::default_config(),
            }
        });
        Ok(station.clone())
    }

    pub async fn update_network_info(&self, duid: &str, network_info: Value) -> bool {
        let mut stations = self.stations.write().await;
        match stations.get_mut(duid) {
            Some(station) => {
                station.network_info = Some(network_info);
                true
            }
            None => false,
        }
    }

    /// None when the station does not exist; the config must be parsed already
    pub async fn update_name_and_config(
        &self,
        duid: &str,
        name: Option<&str>,
        quasar_config: Option<&Value>,
    ) -> Option<StationSummary> {
        let mut stations = self.stations.write().await;
        let station = stations.get_mut(duid)?;
        if let Some(name) = name {
            station.name = name.to_string();
        }
        if let Some(quasar_config) = quasar_config {
            station.quasar_config = quasar_config.clone();
        }
        Some(station.clone())
    }

    pub async fn list(&self) -> Vec<StationSummary> {
        self.stations.read().await.values().cloned().collect()
    }
}
