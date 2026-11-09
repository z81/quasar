//! alice-server (Rust): Yandex Station emulator with an OpenRouter-powered
//! voice assistant. The main server emulates Yandex domains (uni.ws + HTTP),
//! API_PORT serves the management API.

mod agent;
mod alarm_protocol;
mod animation_presets;
mod anthropic;
mod api;
mod audio;
mod auth;
mod automations;
mod maintenance;
mod packages;
mod system_sounds;
mod spotify_auth;
mod config;
mod connections;
mod control;
mod control_ui;
mod directives;
mod display;
mod dnd;
mod dnd_api;
mod device_access;
mod device_tools;
mod gigastt;
mod logging;
mod mcp;
mod openrouter;
mod piper;
mod player;
mod prompt;
mod proto;
mod quasar_router;
mod remote;
mod runtime;
mod server_settings;
mod server_settings_api;
mod schedule_api;
mod schedule_delivery;
mod schedule_tool;
mod schedules;
mod timer_api;
mod timer_tool;
mod station_config;
mod stations;
mod storage;
mod speaker;
mod speaker_api;
mod smart_home;
mod smart_home_api;
mod sound;
mod time_zone;
mod uniproxy;
mod updates;
mod weather;
mod weather_api;
mod weather_tool;
mod whisper;
mod whisper_api;

use std::sync::Arc;

use anyhow::Result;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::routing::get;

use crate::api::ApiState;
use crate::connections::ConnectionRegistry;
use crate::quasar_router::QuasarState;

/// Single axum state for the whole main server
#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<runtime::Runtime>,
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
    logging::init();

    let bootstrap = config::BootstrapConfig::from_env()?;
    let storage = Arc::new(storage::Storage::connect(&bootstrap.postgres_url).await);
    let environment = config::Environment::capture();
    let (config, settings_revision) = server_settings::initial_config(&storage, &environment).await?;
    let weather = Arc::new(weather::WeatherService::new(storage.clone())?);
    let registry = ConnectionRegistry::default();
    for station in storage.list().await? {
        registry.set_timezone(&station.duid, station.timezone.as_deref());
    }
    let control = control::Control::load(storage.clone()).await?;
    let auth = auth::AuthManager::load(storage.clone()).await?;
    let schedules = schedules::Scheduler::load(storage.clone(), Arc::new(schedule_delivery::StationDelivery {
        registry: registry.clone(),
        control: control.clone(),
    })).await?;

    let speakers = speaker::SpeakerService::load(storage.clone()).await?;
    let whisper = Arc::new(whisper::WhisperService::new(storage.clone()));
    let dnd = dnd::DndService::load(storage.clone(), registry.clone()).await?;
    let runtime = runtime::Runtime::new(config, weather.clone(), schedules.clone(), speakers, whisper).await?;
    let settings = server_settings::ServerSettings::new(
        storage.clone(), environment, runtime.clone(), bootstrap.port, bootstrap.api_port, settings_revision,
    );
    let quasar_state = Arc::new(QuasarState {
        storage: storage.clone(),
        glagol_jwt_key: bootstrap.glagol_jwt_key,
    });
    let port = bootstrap.port;
    let api_port = bootstrap.api_port;
    let app_state = AppState {
        runtime: runtime.clone(),
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
                        uniproxy::handle_socket(socket, state.runtime, device.0, state.registry)
                    })
                },
            ),
        )
        .route("/health", get(|| async { "ok" }))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("quasar (uni.ws + domains) listening on {}", listener.local_addr()?);

    let api = api::router(
        Arc::new(ApiState {
            registry: registry.clone(),
            storage,
            control,
            runtime,
            settings,
            weather,
            schedules: schedules.clone(),
            dnd: dnd.clone(),
        }),
        auth,
    );
    let api_listener = tokio::net::TcpListener::bind(("0.0.0.0", api_port)).await?;
    tracing::info!("management API listening on {}", api_listener.local_addr()?);
    schedules.start();
    dnd.start();

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
