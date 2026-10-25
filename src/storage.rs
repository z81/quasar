//! Station registry persistence: PostgreSQL when reachable, in-memory
//! fallback otherwise (keeps development and broken-DB setups workable).

use std::sync::Arc;

use serde_json::{Value, json};

use crate::stations::{StationStore, StationSummary};

pub enum Storage {
    Postgres(tokio_postgres::Client),
    Memory(Arc<StationStore>),
}

impl Storage {
    pub async fn connect(url: &str) -> Self {
        match tokio_postgres::connect(url, tokio_postgres::NoTls).await {
            Ok((client, connection)) => {
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        tracing::error!("postgres connection error: {error}");
                    }
                });
                if let Err(error) = client
                    .execute(
                        "CREATE TABLE IF NOT EXISTS stations (\
                         duid TEXT PRIMARY KEY, platform TEXT NOT NULL, name TEXT NOT NULL, \
                         network_info JSONB, certificate TEXT NOT NULL, private_key TEXT NOT NULL)",
                        &[],
                    )
                    .await
                {
                    tracing::error!("failed to create stations table: {error}");
                    return Self::Memory(StationStore::shared());
                }
                tracing::info!("postgres storage connected");
                Self::Postgres(client)
            }
            Err(error) => {
                tracing::warn!("postgres unavailable ({error}); using in-memory station registry");
                Self::Memory(StationStore::shared())
            }
        }
    }

    pub async fn register(&self, duid: &str, platform: &str) -> anyhow::Result<()> {
        match self {
            Self::Memory(store) => store.register(duid, platform).await,
            Self::Postgres(client) => {
                let row = client
                    .query_opt("SELECT duid FROM stations WHERE duid = $1", &[&duid])
                    .await?;
                if row.is_some() {
                    return Ok(());
                }
                let (certificate, private_key) = crate::stations::generate_certificate()?;
                let index: i64 = client
                    .query_one("SELECT count(*) FROM stations", &[])
                    .await?
                    .get(0);
                let name = format!("My Favourite Yandex Station No. {index:05}");
                client
                    .execute(
                        "INSERT INTO stations (duid, platform, name, certificate, private_key) \
                         VALUES ($1, $2, $3, $4, $5)",
                        &[&duid, &platform, &name, &certificate, &private_key],
                    )
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn update_network_info(&self, duid: &str, network_info: Value) -> bool {
        match self {
            Self::Memory(store) => store.update_network_info(duid, network_info).await,
            Self::Postgres(client) => {
                let info = network_info.to_string();
                client
                    .execute(
                        "UPDATE stations SET network_info = $2 WHERE duid = $1",
                        &[&duid, &info],
                    )
                    .await
                    .map(|affected| affected > 0)
                    .unwrap_or(false)
            }
        }
    }

    pub async fn list(&self) -> Vec<StationSummary> {
        match self {
            Self::Memory(store) => store.list().await,
            Self::Postgres(client) => client
                .query(
                    "SELECT duid, platform, name, network_info, certificate, private_key \
                     FROM stations",
                    &[],
                )
                .await
                .map(|rows| {
                    rows.iter()
                        .map(|row| StationSummary {
                            certificate_pem: row.get(4),
                            duid: row.get(0),
                            name: row.get(2),
                            network_info: row
                                .try_get::<_, String>(3)
                                .ok()
                                .and_then(|value| serde_json::from_str(&value).ok()),
                            platform: row.get(1),
                            private_key_pem: row.get(5),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub async fn device_list_json(&self) -> Value {
        json!({
            "devices": self.list().await.iter().map(StationSummary::to_glagol_device).collect::<Vec<_>>(),
            "status": "ok",
        })
    }

    pub async fn get(&self, duid: &str) -> Option<Value> {
        self.list()
            .await
            .iter()
            .find(|station| station.duid == duid)
            .map(StationSummary::to_glagol_device)
    }
}
