//! Station registry persistence: PostgreSQL when reachable, in-memory
//! fallback otherwise (keeps development and broken-DB setups workable).

use std::sync::Arc;

use anyhow::Context;
use serde_json::Value;
use tokio_postgres::Row;

use crate::station_config;
use crate::stations::{StationStore, StationSummary};

pub enum Storage {
    Postgres(tokio_postgres::Client),
    Memory(Arc<StationStore>),
}

const COLUMNS: &str = "duid, platform, name, network_info, certificate, private_key, quasar_config";

impl Storage {
    pub async fn connect(url: &str) -> Self {
        match tokio_postgres::connect(url, tokio_postgres::NoTls).await {
            Ok((mut client, connection)) => {
                tokio::spawn(async move {
                    if let Err(error) = connection.await {
                        tracing::error!("postgres connection error: {error}");
                    }
                });
                // own table name: the TypeScript server keeps an incompatible
                // "stations" table, and both may share one database
                if let Err(error) = client
                    .batch_execute(
                        "CREATE TABLE IF NOT EXISTS alice_stations (\
                         duid TEXT PRIMARY KEY, platform TEXT NOT NULL, name TEXT NOT NULL, \
                         network_info JSONB, certificate TEXT NOT NULL, private_key TEXT NOT NULL); \
                         ALTER TABLE alice_stations \
                         ADD COLUMN IF NOT EXISTS quasar_config JSONB NOT NULL DEFAULT '{}'::jsonb",
                    )
                    .await
                {
                    tracing::error!("failed to prepare the stations table: {error}");
                    return Self::Memory(StationStore::shared());
                }
                if let Err(error) = reparse_configs(&mut client).await {
                    tracing::error!("failed to re-parse the stored quasar configs: {error:#}");
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

    /// The station, registered with a new certificate and the default config
    /// on first contact (TS getOrCreate)
    pub async fn get_or_create(&self, duid: &str, platform: &str) -> anyhow::Result<StationSummary> {
        match self {
            Self::Memory(store) => store.get_or_create(duid, platform).await,
            Self::Postgres(client) => {
                if let Some(station) = select_station(client, duid).await? {
                    return Ok(station);
                }
                let (certificate, private_key) = crate::stations::generate_certificate()?;
                let index: i64 = client
                    .query_one("SELECT count(*) FROM alice_stations", &[])
                    .await?
                    .try_get(0)?;
                let name = crate::stations::station_name(index);
                let quasar_config = station_config::default_config();
                let inserted = client
                    .execute(
                        // two first-contact requests may race; the first one wins
                        "INSERT INTO alice_stations \
                         (duid, platform, name, certificate, private_key, quasar_config) \
                         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (duid) DO NOTHING",
                        &[&duid, &platform, &name, &certificate, &private_key, &quasar_config],
                    )
                    .await?;
                if inserted > 0 {
                    tracing::info!("registered station {duid} ({platform})");
                }
                select_station(client, duid)
                    .await?
                    .with_context(|| format!("station {duid} is missing right after its insert"))
            }
        }
    }

    pub async fn register(&self, duid: &str, platform: &str) -> anyhow::Result<()> {
        self.get_or_create(duid, platform).await.map(|_| ())
    }

    pub async fn update_network_info(&self, duid: &str, network_info: Value) -> anyhow::Result<()> {
        let updated = match self {
            Self::Memory(store) => store.update_network_info(duid, network_info).await,
            Self::Postgres(client) => {
                client
                    .execute(
                        "UPDATE alice_stations SET network_info = $2 WHERE duid = $1",
                        &[&duid, &network_info],
                    )
                    .await?
                    > 0
            }
        };
        anyhow::ensure!(updated, "station {duid} does not exist");
        Ok(())
    }

    /// TS updateNameAndQuasarConfig: None when the station does not exist; the
    /// config replaces the stored one and must be parsed already
    pub async fn update_name_and_config(
        &self,
        duid: &str,
        name: Option<&str>,
        quasar_config: Option<&Value>,
    ) -> anyhow::Result<Option<StationSummary>> {
        match self {
            Self::Memory(store) => Ok(store.update_name_and_config(duid, name, quasar_config).await),
            Self::Postgres(client) => {
                let row = client
                    .query_opt(
                        &format!(
                            "UPDATE alice_stations SET name = COALESCE($2, name), \
                             quasar_config = COALESCE($3, quasar_config) WHERE duid = $1 RETURNING {COLUMNS}"
                        ),
                        &[&duid, &name, &quasar_config],
                    )
                    .await?;
                Ok(row.as_ref().map(station_from_row).transpose()?)
            }
        }
    }

    pub async fn list(&self) -> anyhow::Result<Vec<StationSummary>> {
        match self {
            Self::Memory(store) => Ok(store.list().await),
            Self::Postgres(client) => {
                let rows = client
                    .query(&format!("SELECT {COLUMNS} FROM alice_stations"), &[])
                    .await?;
                Ok(rows.iter().map(station_from_row).collect::<Result<_, _>>()?)
            }
        }
    }
}

async fn select_station(client: &tokio_postgres::Client, duid: &str) -> anyhow::Result<Option<StationSummary>> {
    let row = client
        .query_opt(&format!("SELECT {COLUMNS} FROM alice_stations WHERE duid = $1"), &[&duid])
        .await?;
    Ok(row.as_ref().map(station_from_row).transpose()?)
}

/// The stored config is re-parsed on every read, so new defaults apply to old rows
fn station_from_row(row: &Row) -> Result<StationSummary, tokio_postgres::Error> {
    let duid: String = row.try_get("duid")?;
    let stored_config: Option<Value> = row.try_get("quasar_config")?;
    Ok(StationSummary {
        certificate_pem: row.try_get("certificate")?,
        name: row.try_get("name")?,
        network_info: row.try_get("network_info")?,
        platform: row.try_get("platform")?,
        private_key_pem: row.try_get("private_key")?,
        quasar_config: station_config::from_stored(&duid, stored_config.as_ref()),
        duid,
    })
}

/// TS initialize(): re-parses every stored config at startup and saves the
/// result, so the rows hold complete configs with the current defaults
async fn reparse_configs(client: &mut tokio_postgres::Client) -> anyhow::Result<()> {
    let transaction = client.transaction().await?;
    let rows = transaction
        .query("SELECT duid, quasar_config FROM alice_stations FOR UPDATE", &[])
        .await?;
    let mut updated = 0;
    for row in &rows {
        let duid: String = row.try_get(0)?;
        let stored: Option<Value> = row.try_get(1)?;
        let quasar_config = station_config::from_stored(&duid, stored.as_ref());
        if stored.as_ref() != Some(&quasar_config) {
            transaction
                .execute(
                    "UPDATE alice_stations SET quasar_config = $2 WHERE duid = $1",
                    &[&duid, &quasar_config],
                )
                .await?;
            updated += 1;
        }
    }
    transaction.commit().await?;
    if updated > 0 {
        tracing::info!("re-parsed the stored quasar configs of {updated} station(s)");
    }
    Ok(())
}
