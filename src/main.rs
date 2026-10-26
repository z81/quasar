//! alice-server (Rust): Yandex Station emulator with an OpenRouter-powered
//! voice assistant. The main server emulates Yandex domains (uni.ws + HTTP),
//! API_PORT serves the management API.

mod agent;
mod api;
mod audio;
mod config;
mod connections;
mod directives;
mod music;
mod openrouter;
mod prompt;
mod proto;
mod quasar_router;
mod remote;
mod station_config;
mod stations;
mod storage;
mod uniproxy;
mod updates;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::routing::get;

use crate::agent::FunctionDefinition;
use crate::api::ApiState;
use crate::connections::ConnectionRegistry;
use crate::prompt::{builtin_directive_functions, PromptGenerator};
use crate::quasar_router::QuasarState;
use crate::remote::RemoteServers;
use crate::uniproxy::UniproxyContext;
use crate::updates::UpdatesConfig;

/// Single axum state for the whole main server
#[derive(Clone)]
pub struct AppState {
    pub context: Arc<UniproxyContext>,
    pub quasar: Arc<QuasarState>,
    pub registry: ConnectionRegistry,
}

/// The station identifies itself with the x-uprx-device-id header on upgrade
struct DeviceId(String);

impl<S: Send + Sync> FromRequestParts<S> for DeviceId {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let device_id = parts
            .headers
            .get("x-uprx-device-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        Ok(Self(device_id))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env();
    let openrouter = openrouter::OpenRouter::new(
        &config.openrouter_base_url,
        &config.openrouter_api_key,
        config.openrouter_timeout_ms,
    )?;

    let remote = Arc::new(RemoteServers::new(
        csv_urls("PROCESSOR_FUNCTION_SERVER_URLS"),
        csv_urls("PROCESSOR_STATE_SERVER_URLS"),
    ));

    let music = music::MusicManager::new(config.music_service_basic_url.as_deref());

    // function catalog: station directives + remote servers
    let mut functions: HashMap<String, FunctionDefinition> =
        builtin_directive_functions(music.is_configured())
            .iter()
            .map(|(name, definition)| (name.clone(), definition.clone()))
            .collect();
    for (name, definition) in remote.functions().await {
        if functions.contains_key(&name) {
            tracing::warn!("duplicate function '{name}' from a remote server, keeping the builtin one");
            continue;
        }
        functions.insert(name, definition);
    }

    let storage = Arc::new(storage::Storage::connect(&config.postgres_url).await);
    let updates = Arc::new(UpdatesConfig::load(
        config.allow_updates,
        &config.updates_config_path,
    )?);
    let registry = ConnectionRegistry::default();

    let prompt_generator = PromptGenerator::new(
        &config.processor_prompt_path,
        &config.processor_state_prompt_path,
    );
    prompt_generator.check_templates()?;

    let context = Arc::new(UniproxyContext {
        prompt_generator,
        functions: Arc::new(functions),
        cache: agent::new_response_cache(config.processor_cache_size as usize),
        openrouter,
        remote,
        music,
        config,
    });

    let quasar_state = Arc::new(QuasarState {
        storage: storage.clone(),
        glagol_jwt_key: context.config.glagol_jwt_key.clone(),
        updates,
    });

    let port = context.config.port;
    let api_port = context.config.api_port;

    let app_state = AppState {
        context: context.clone(),
        quasar: quasar_state,
        registry: registry.clone(),
    };
    let app = quasar_router::router()
        .route(
            "/uniproxy.alice.yandex.net/uni.ws",
            get(
                |State(state): State<AppState>, device: DeviceId, upgrade: WebSocketUpgrade| async move {
                    tracing::info!("uni.ws connection for device {}", device.0);
                    upgrade.on_upgrade(move |socket| {
                        uniproxy::handle_socket(socket, state.context, device.0, state.registry)
                    })
                },
            ),
        )
        .route("/health", get(|| async { "ok" }))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("quasar (uni.ws + domains) listening on {}", listener.local_addr()?);

    let api = api::router(Arc::new(ApiState {
        registry: registry.clone(),
        storage,
    }));
    let api_listener = tokio::net::TcpListener::bind(("0.0.0.0", api_port)).await?;
    tracing::info!("management API listening on {}", api_listener.local_addr()?);

    // stop on SIGINT/SIGTERM: as PID 1 in Docker there is no default handler,
    // so without this `docker stop` waits 10 s and kills the process
    tokio::select! {
        result = axum::serve(listener, app) => result?,
        result = axum::serve(api_listener, api) => result?,
        () = shutdown_signal() => tracing::info!("shutting down"),
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!("cannot listen for SIGTERM: {error}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

fn csv_urls(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
        .collect()
}
