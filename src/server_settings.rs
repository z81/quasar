//! Durable operational overrides. The process environment remains the baseline.

mod mcp;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::config::{self, Config, Environment, SettingDefinition, Values};
use crate::runtime::Runtime;
use crate::storage::Storage;

const KEY: &str = "server_settings:v1";

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    revision: u64,
    overrides: Values,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error(
        "Настройки уже изменены в другой вкладке. Загрузите актуальные значения перед сохранением."
    )]
    Conflict,
    #[error("Хранилище настроек временно недоступно. Изменения не применены.")]
    Unavailable,
}

type Result<T> = std::result::Result<T, SettingsError>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub revision: u64,
    #[serde(default)]
    pub changes: BTreeMap<String, Option<String>>,
    pub mcp: Option<mcp::Patch>,
}

#[derive(Serialize)]
pub struct Field {
    #[serde(flatten)]
    definition: SettingDefinition,
    value: Option<String>,
    is_set: bool,
    source: &'static str,
    env_present: bool,
    env_name: Option<String>,
    baseline_value: Option<String>,
    baseline_is_set: bool,
}

#[derive(Serialize)]
pub struct Bootstrap {
    port: u16,
    api_port: u16,
    env_only: [&'static str; 4],
}

#[derive(Serialize)]
pub struct SettingsView {
    revision: u64,
    persistent: bool,
    ready: bool,
    issues: Vec<String>,
    fields: Vec<Field>,
    mcp: mcp::View,
    bootstrap: Bootstrap,
}

pub struct ServerSettings {
    storage: Arc<Storage>,
    environment: Environment,
    runtime: Arc<Runtime>,
    applied_revision: Mutex<u64>,
    port: u16,
    api_port: u16,
}

pub async fn initial_config(
    storage: &Storage,
    environment: &Environment,
) -> anyhow::Result<(Config, u64)> {
    if storage.is_persistent() {
        storage
            .insert_control_value(KEY, &serde_json::to_value(Document::default())?)
            .await?;
    }
    let (_, document) = read(storage).await?;
    let config = Config::from_values(&effective(environment, &document))?;
    Ok((config, document.revision))
}

impl ServerSettings {
    pub fn new(
        storage: Arc<Storage>,
        environment: Environment,
        runtime: Arc<Runtime>,
        port: u16,
        api_port: u16,
        revision: u64,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            storage,
            environment,
            runtime,
            port,
            api_port,
            applied_revision: Mutex::new(revision),
        });
        let weak = Arc::downgrade(&service);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut failed = false;
            loop {
                interval.tick().await;
                let Some(service) = weak.upgrade() else {
                    break;
                };
                if !service.storage.is_persistent() {
                    break;
                }
                match service.refresh().await {
                    Ok(()) => failed = false,
                    Err(_) if !failed => {
                        // Errors may originate in credentials/URLs; never print their values.
                        tracing::warn!("Cannot refresh server settings; keeping the last applied configuration");
                        failed = true;
                    }
                    Err(_) => {}
                }
            }
        });
        service
    }

    async fn refresh(&self) -> Result<()> {
        let mut applied = self.applied_revision.lock().await;
        let (_, document) = read(&self.storage).await?;
        self.apply_external(&document, &mut applied).await
    }

    async fn apply_external(&self, document: &Document, applied: &mut u64) -> Result<()> {
        if document.revision != *applied {
            let config = Config::from_values(&effective(&self.environment, document))
                .map_err(|_| SettingsError::Unavailable)?;
            let prepared = self
                .runtime
                .prepare(config)
                .await
                .map_err(|_| SettingsError::Unavailable)?;
            self.runtime.activate(prepared);
            *applied = document.revision;
        }
        Ok(())
    }

    pub async fn get(&self) -> Result<SettingsView> {
        let mut applied = self.applied_revision.lock().await;
        let (_, document) = read(&self.storage).await?;
        self.apply_external(&document, &mut applied).await?;
        self.view(&document)
    }

    pub async fn patch(&self, patch: Patch) -> Result<SettingsView> {
        if !self.storage.is_persistent() {
            return Err(SettingsError::Unavailable);
        }
        let mut applied = self.applied_revision.lock().await;
        let (expected, mut document) = read(&self.storage).await?;
        if patch.revision != document.revision {
            return Err(SettingsError::Conflict);
        }
        if let Some(mcp) = patch.mcp {
            if patch.changes.contains_key(mcp::URLS) || patch.changes.contains_key(mcp::TOKENS) {
                return Err(SettingsError::Invalid(
                    "MCP: используйте либо редактор серверов, либо отдельные значения настроек"
                        .into(),
                ));
            }
            mcp::apply(mcp, &effective(&self.environment, &document), &mut document)?;
        }
        let definitions = config::definitions();
        for (key, value) in patch.changes {
            if !definitions.iter().any(|field| field.key == key) {
                return Err(SettingsError::Invalid(
                    "Неизвестная настройка или параметр запуска, доступный только через env".into(),
                ));
            }
            match value {
                Some(value) if value.len() <= 32 * 1024 => {
                    document.overrides.insert(key, value);
                }
                Some(_) => {
                    return Err(SettingsError::Invalid(
                        "Значение настройки слишком длинное".into(),
                    ))
                }
                None => {
                    document.overrides.remove(&key);
                }
            }
        }
        let updated = serde_json::to_value(&document).map_err(|_| SettingsError::Unavailable)?;
        if updated == expected {
            self.apply_external(&document, &mut applied).await?;
            return self.view(&document);
        }
        let config = Config::from_values(&effective(&self.environment, &document))
            .map_err(|error| SettingsError::Invalid(error.to_string()))?;
        // Build clients/resources first; a validation failure never persists a partial change.
        // prepare does not activate MCP maintenance tasks or replace the live snapshot.
        let prepared = self.runtime.prepare(config).await.map_err(|_| SettingsError::Invalid(
            "Не удалось подготовить настройки. Проверьте адреса сервисов и доступность файлов шаблонов/обновлений.".into()
        ))?;
        document.revision = document
            .revision
            .checked_add(1)
            .ok_or(SettingsError::Unavailable)?;
        let replacement =
            serde_json::to_value(&document).map_err(|_| SettingsError::Unavailable)?;
        if !self
            .storage
            .compare_control_value(KEY, &expected, &replacement)
            .await
            .map_err(|_| SettingsError::Unavailable)?
        {
            return Err(SettingsError::Conflict);
        }
        self.runtime.activate(prepared);
        *applied = document.revision;
        self.view(&document)
    }

    fn view(&self, document: &Document) -> Result<SettingsView> {
        let baseline = baseline(&self.environment);
        let values = effective(&self.environment, document);
        let fields = config::definitions()
            .into_iter()
            .map(|definition| {
                let key = &definition.key;
                let value = values.get(key).cloned().unwrap_or_default();
                let base = baseline.get(key).cloned().unwrap_or_default();
                let env_present = self.environment.values.contains_key(key);
                Field {
                    value: (!definition.secret).then(|| value.clone()),
                    is_set: !value.trim().is_empty(),
                    source: if document.overrides.contains_key(key) {
                        "override"
                    } else if env_present {
                        "env"
                    } else {
                        "default"
                    },
                    env_present,
                    env_name: self.environment.sources.get(key).cloned(),
                    baseline_value: (!definition.secret).then(|| base.clone()),
                    baseline_is_set: !base.trim().is_empty(),
                    definition,
                }
            })
            .collect();
        let issues = config::readiness_issues(&self.runtime.snapshot().config);
        Ok(SettingsView {
            revision: document.revision,
            persistent: self.storage.is_persistent(),
            ready: issues.is_empty(),
            issues,
            fields,
            mcp: mcp::view(&values, &baseline)?,
            bootstrap: Bootstrap {
                port: self.port,
                api_port: self.api_port,
                env_only: ["PORT", "API_PORT", "POSTGRES_URL", "GLAGOL_JWT_KEY"],
            },
        })
    }
}

fn baseline(environment: &Environment) -> Values {
    let mut values = config::defaults();
    values.extend(environment.values.clone());
    values
}

fn effective(environment: &Environment, document: &Document) -> Values {
    let mut values = baseline(environment);
    values.extend(document.overrides.clone());
    values
}

async fn read(storage: &Storage) -> Result<(Value, Document)> {
    let value = storage
        .get_control_value(KEY)
        .await
        .map_err(|_| SettingsError::Unavailable)?
        .unwrap_or_else(|| serde_json::json!({"revision":0,"overrides":{}}));
    let document: Document =
        serde_json::from_value(value.clone()).map_err(|_| SettingsError::Unavailable)?;
    let definitions = config::definitions();
    if document
        .overrides
        .keys()
        .any(|key| !definitions.iter().any(|field| &field.key == key))
    {
        return Err(SettingsError::Unavailable);
    }
    Ok((value, document))
}
