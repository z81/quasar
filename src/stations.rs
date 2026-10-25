//! In-memory station registry and TLS certificate generation (the PostgreSQL
//! store in `storage.rs` reuses both).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::RwLock;

pub struct StationSummary {
    pub certificate_pem: String,
    pub duid: String,
    pub name: String,
    pub network_info: Option<Value>,
    pub platform: String,
    pub private_key_pem: String,
}

impl StationSummary {
    pub fn to_glagol_device(&self) -> Value {
        json!({
            "activation_code": 0,
            "activation_region": "RU",
            "config": {
                "dndMode": { "enabled": false, "features": { "allowIncomingCalls": false } },
                "led": {
                    "brightness": { "auto": true, "value": 0.5 },
                    "idle_animation": false,
                    "time_visualization": { "format": "24h" },
                },
                "locale": "ru-RU",
                "name": self.name,
                "standby": { "deepStandbyEnabled": true, "deepStandbyTimeoutMinutes": 240 },
            },
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

#[derive(Default)]
pub struct StationStore {
    stations: RwLock<HashMap<String, StationSummary>>,
}

impl StationStore {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Registers the station on first contact and (re)issues its certificate
    pub async fn register(&self, duid: &str, platform: &str) -> anyhow::Result<()> {
        if self.stations.read().await.contains_key(duid) {
            return Ok(());
        }

        let (certificate_pem, private_key_pem) = generate_certificate()?;
        let index = self.stations.write().await.len();

        self.stations.write().await.insert(
            duid.to_string(),
            StationSummary {
                duid: duid.to_string(),
                name: format!("My Favourite Yandex Station No. {index:05}"),
                network_info: None,
                platform: platform.to_string(),
                certificate_pem,
                private_key_pem,
            },
        );
        tracing::info!("registered station {duid} ({platform})");
        Ok(())
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

    pub async fn list(&self) -> Vec<StationSummary> {
        self.stations
            .read()
            .await
            .values()
            .map(|station| StationSummary {
                certificate_pem: station.certificate_pem.clone(),
                duid: station.duid.clone(),
                name: station.name.clone(),
                network_info: station.network_info.clone(),
                platform: station.platform.clone(),
                private_key_pem: station.private_key_pem.clone(),
            })
            .collect()
    }
}
