//! In-memory station registry with per-device self-signed TLS certificates
//! (the same shape the TypeScript version keeps in PostgreSQL; a real
//! PostgreSQL store is phase 3).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::RwLock;

pub struct StationInfo {
    pub certificate_pem: String,
    pub duid: String,
    pub name: String,
    pub network_info: Option<Value>,
    pub platform: String,
    pub private_key_pem: String,
}

#[derive(Default)]
pub struct StationStore {
    stations: RwLock<HashMap<String, StationInfo>>,
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

        let mut parameters = rcgen::CertificateParams::new(vec!["localhost".to_string()])?;
        parameters.distinguished_name = {
            let mut name = rcgen::DistinguishedName::new();
            name.push(rcgen::DnType::CommonName, "localhost");
            name.push(rcgen::DnType::OrganizationName, "Yandex");
            name
        };

        let key_pair = rcgen::KeyPair::generate()?;
        let certificate = parameters.self_signed(&key_pair)?;
        let index = self.stations.write().await.len();

        self.stations.write().await.insert(
            duid.to_string(),
            StationInfo {
                certificate_pem: certificate.pem(),
                duid: duid.to_string(),
                name: format!("My Favourite Yandex Station No. {index:05}"),
                network_info: None,
                platform: platform.to_string(),
                private_key_pem: key_pair.serialize_pem(),
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

    /// The glagol/device_list response payload
    pub async fn device_list(&self) -> Value {
        let stations = self.stations.read().await;
        json!({
            "devices": stations.values().map(StationInfo::to_glagol_device).collect::<Vec<_>>(),
            "status": "ok",
        })
    }

    pub async fn get(&self, duid: &str) -> Option<Value> {
        let stations = self.stations.read().await;
        stations.get(duid).map(StationInfo::to_glagol_device)
    }
}

impl StationInfo {
    fn to_glagol_device(&self) -> Value {
        json!({
            "activation_code": 0,
            "activation_region": "RU",
            "config": {
                "locale": "ru-RU",
                "name": self.name,
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
