//! Immutable provider snapshots: a saved configuration affects new requests,
//! while an answer already being processed keeps its original clients.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use anyhow::{anyhow, Result};

use crate::agent::FunctionDefinition;
use crate::config::{Config, TtsProvider};
use crate::prompt::{builtin_directive_functions, PromptGenerator};
use crate::schedules::Scheduler;
use crate::uniproxy::UniproxyContext;
use crate::weather::WeatherService;

pub struct Runtime {
    current: RwLock<Arc<UniproxyContext>>,
    weather: Arc<WeatherService>,
    schedules: Arc<Scheduler>,
    speakers: Arc<crate::speaker::SpeakerService>,
}

impl Runtime {
    pub async fn new(
        config: Config,
        weather: Arc<WeatherService>,
        schedules: Arc<Scheduler>,
        speakers: Arc<crate::speaker::SpeakerService>,
    ) -> Result<Arc<Self>> {
        let prepared = prepare(config, weather.clone(), schedules.clone(), speakers.clone(), None).await?;
        crate::logging::apply(&prepared.config.log_filter);
        crate::time_zone::set_default(prepared.config.timezone.as_deref());
        prepared.mcp.start();
        Ok(Arc::new(Self {
            current: RwLock::new(prepared),
            weather,
            schedules,
            speakers,
        }))
    }

    pub fn snapshot(&self) -> Arc<UniproxyContext> {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Builds a candidate without starting MCP background connections. The
    /// settings service persists its transaction before activating this value.
    pub async fn prepare(&self, config: Config) -> Result<Arc<UniproxyContext>> {
        let current = self.snapshot();
        prepare(
            config,
            self.weather.clone(),
            self.schedules.clone(),
            self.speakers.clone(),
            Some(&current),
        )
        .await
    }

    pub fn activate(&self, prepared: Arc<UniproxyContext>) {
        crate::logging::apply(&prepared.config.log_filter);
        crate::time_zone::set_default(prepared.config.timezone.as_deref());
        prepared.mcp.start();
        let active_mcp = prepared.mcp.clone();
        let previous = std::mem::replace(
            &mut *self.current.write().unwrap_or_else(PoisonError::into_inner),
            prepared,
        );
        // Old requests can still call their original MCP clients, but those
        // clients must not keep independent discovery/reconnect loops alive.
        if !Arc::ptr_eq(&previous.mcp, &active_mcp) {
            previous.mcp.stop();
        }
    }
}

async fn prepare(
    config: Config,
    weather: Arc<WeatherService>,
    schedules: Arc<Scheduler>,
    speakers: Arc<crate::speaker::SpeakerService>,
    previous: Option<&UniproxyContext>,
) -> Result<Arc<UniproxyContext>> {
    let openrouter = crate::openrouter::OpenRouter::new(
        &config.openrouter_base_url,
        &config.openrouter_api_key,
        config.openrouter_timeout_ms,
        config.processor_reasoning.as_deref(),
    )?;
    let llm = match &config.anthropic {
        Some(anthropic) => crate::agent::Llm::Anthropic(crate::anthropic::Anthropic::new(
            &anthropic.base_url,
            &anthropic.api_key,
            anthropic.timeout_ms,
            anthropic.effort.as_deref(),
        )?),
        None => crate::agent::Llm::OpenRouter(openrouter.clone()),
    };
    let unchanged_remote = previous.filter(|previous| {
        previous.config.function_server_urls == config.function_server_urls
            && previous.config.state_server_urls == config.state_server_urls
    });
    let (remote, functions) = if let Some(previous) = unchanged_remote {
        (previous.remote.clone(), previous.functions.clone())
    } else {
        let remote = Arc::new(crate::remote::RemoteServers::new(
            config.function_server_urls.clone(),
            config.state_server_urls.clone(),
        ));
        let mut functions: HashMap<String, FunctionDefinition> = builtin_directive_functions()
            .iter()
            .map(|(name, definition)| (name.clone(), definition.clone()))
            .collect();
        functions.insert("get_weather".to_string(), crate::weather_tool::definition());
        functions.extend(crate::schedule_tool::definitions());
        functions.extend(crate::timer_tool::definitions());
        // A broken integration should not prevent saving its replacement. Bound
        // total preparation even when several servers are offline.
        match tokio::time::timeout(std::time::Duration::from_secs(10), remote.functions()).await {
            Ok(catalog) => {
                for (name, definition) in catalog {
                    functions.entry(name).or_insert(definition);
                }
            }
            Err(_) => tracing::warn!("remote function catalog preparation timed out"),
        }
        (remote, Arc::new(functions))
    };
    let unchanged_mcp = previous.filter(|previous| {
        previous.config.mcp_live_context == config.mcp_live_context
            && previous.config.mcp_servers.len() == config.mcp_servers.len()
            && previous
                .config
                .mcp_servers
                .iter()
                .zip(&config.mcp_servers)
                .all(|(a, b)| a.url == b.url && a.prefix == b.prefix && a.token == b.token)
    });
    let mcp = unchanged_mcp.map_or_else(
        || crate::mcp::Mcp::new(&config.mcp_servers, config.mcp_live_context),
        |previous| previous.mcp.clone(),
    );
    let prompt_generator = PromptGenerator::new(
        &config.processor_prompt_path,
        &config.processor_state_prompt_path,
    );
    prompt_generator.check_templates().map_err(|_| {
        anyhow!("Не удалось загрузить шаблоны ответов: проверьте пути и содержимое файлов")
    })?;
    let updates = Arc::new(
        crate::updates::UpdatesConfig::load(config.allow_updates, &config.updates_config_path)
            .map_err(|_| anyhow!("Не удалось загрузить файл настроек обновлений"))?,
    );
    // An unset provider address is a first-run configuration state, not a
    // reason to make the settings UI unreachable.
    let gigastt = config
        .gigastt_url
        .as_ref()
        .and_then(|_| crate::gigastt::Gigastt::from_config(&config));
    let piper = (config.tts_provider == TtsProvider::Piper)
        .then(|| crate::piper::Piper::from_config(&config))
        .flatten();
    Ok(Arc::new(UniproxyContext {
        prompt_generator,
        functions,
        cache: crate::agent::new_response_cache(config.processor_cache_size as usize),
        mcp,
        openrouter,
        llm,
        remote,
        weather,
        schedules,
        speakers,
        gigastt,
        piper,
        updates,
        config,
    }))
}
