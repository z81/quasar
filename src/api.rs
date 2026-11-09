//! Management API (API_PORT), TS main.ts: devices and their quasar configs,
//! pushing events / raw speech / raw directives. Request bodies are JSON;
//! invalid ones get 400 or 422 with what is wrong.

pub(crate) mod raw_directive;
mod display;
mod sound;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::connections::{ConnectionRegistry, StationCommand};
use crate::station_config;
use crate::stations::StationSummary;
use crate::storage::Storage;

pub struct ApiState {
    pub registry: ConnectionRegistry,
    pub storage: Arc<Storage>,
    pub control: Arc<crate::control::Control>,
    pub runtime: Arc<crate::runtime::Runtime>,
    pub settings: Arc<crate::server_settings::ServerSettings>,
    pub weather: Arc<crate::weather::WeatherService>,
    pub schedules: Arc<crate::schedules::Scheduler>,
    pub dnd: Arc<crate::dnd::DndService>,
}

impl ApiState {
    fn device(&self, station: &StationSummary) -> Value {
        let mut device = station.to_api_device();
        device["connected"] = json!(self.registry.is_connected(&station.duid));
        device
    }
}

pub fn router(state: Arc<ApiState>, auth: Arc<crate::auth::AuthManager>) -> Router {
    Router::new()
        .route("/devices", get(devices))
        .route("/devices/{duid}", patch(update_device))
        .route("/devices/{duid}/push", post(push_event))
        .route("/devices/{duid}/push-raw", post(push_raw))
        .route("/devices/{duid}/push-text", post(push_text))
        .route("/devices/{duid}/push-directive", post(push_directive))
        .route("/devices/{duid}/display", get(display::status).post(display::animate))
        .route("/devices/{duid}/sound", get(sound::status).put(sound::update))
        .route("/devices/{duid}/weather", get(crate::weather_api::forecast))
        .route("/weather/locations", get(crate::weather_api::locations))
        .route("/devices/{duid}/weather/location", put(crate::weather_api::set_location))
        .merge(crate::schedule_api::router())
        .merge(crate::timer_api::router())
        .merge(crate::smart_home_api::router())
        .merge(crate::speaker_api::router(auth.clone()))
        .merge(crate::whisper_api::router(auth.clone()))
        .merge(crate::dnd_api::router(auth.clone()))
        .merge(crate::server_settings_api::router(auth.clone()))
        .route(
            "/devices/{duid}/player",
            get(player_status).post(player_action),
        )
        .route("/devices/{duid}/player/cover/{key}", get(player_cover))
        .route("/devices/{duid}/player/music", get(crate::player::music).post(crate::player::music_action).layer(axum::Extension(auth.clone())))
        .route(
            "/debug",
            get(|| async { Html(include_str!("api/debug.html")) }),
        )
        .fallback(|| async { (StatusCode::NOT_FOUND, "NOT_FOUND") })
        .with_state(state.clone())
        .merge(crate::device_access::router(state.clone(), auth.clone()))
        .merge(crate::animation_presets::router(state.clone(), auth.clone()))
        .merge(crate::automations::router(state.clone(), auth.clone()))
        .merge(crate::device_tools::router(state.clone(), auth.clone()))
        .merge(crate::maintenance::router(state.clone(), auth.clone()))
        .merge(crate::packages::router(state.clone(), auth.clone()))
        .merge(crate::system_sounds::router(state.clone(), auth.clone()))
        .merge(crate::control::router(state.control.clone()))
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            crate::auth::require_auth,
        ))
        .merge(crate::auth::router(auth))
        .merge(crate::control_ui::router())
        .layer(axum::middleware::from_fn(no_cache))
}

async fn no_cache(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

async fn devices(State(state): State<Arc<ApiState>>) -> Response {
    let mut response = match state.storage.list().await {
        Ok(stations) => Json(stations.iter().map(|station| state.device(station)).collect::<Vec<_>>()).into_response(),
        Err(error) => internal_error("GET /devices", &error),
    };
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Replaces the name and/or the whole quasar config (defaults filled in, as
/// zod parses it), then tells the station to fetch its config again
async fn update_device(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    body: Bytes,
) -> Response {
    let body = match read_body(&body) {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let (name, quasar_config) = match update_fields(body.as_ref()) {
        Ok(fields) => fields,
        Err(issues) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "errors": issues })),
            )
                .into_response()
        }
    };
    let timezone = match body.as_ref().and_then(|body| body.get("timezone")) {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(zone)) if zone.trim().is_empty() => Some(None),
        Some(Value::String(zone)) if zone.parse::<chrono_tz::Tz>().is_ok() => {
            Some(Some(zone.clone()))
        }
        _ => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "timezone must be an IANA zone or null",
            )
                .into_response()
        }
    };
    match state
        .storage
        .update_name_and_config(
            &duid,
            name.as_deref(),
            quasar_config.as_ref(),
            timezone.as_ref(),
        )
        .await
    {
        Ok(Some(station)) => {
            state
                .registry
                .set_timezone(&duid, station.timezone.as_deref());
            let directives = vec![raw_directive::push_update_config()];
            let delivered = state.registry.send(&duid, StationCommand::Directives { directives });
            tracing::info!("updated device {duid}, push_update_config sent to {delivered} connection(s)");
            Json(state.device(&station)).into_response()
        }
        // TS throws here, which Elysia answers with 500 and the message
        Ok(None) => (StatusCode::NOT_FOUND, "Station does not exist").into_response(),
        Err(error) => internal_error("PATCH /devices/:duid", &error),
    }
}

/// zod schema of the TS PATCH body: `{ name?: string, quasarConfig?: QuasarConfig }`
fn update_fields(body: Option<&Value>) -> Result<(Option<String>, Option<Value>), Vec<Value>> {
    let Some(Value::Object(fields)) = body else {
        return Err(vec![station_config::invalid_type_issue("object", body, &[])]);
    };
    let mut issues = Vec::new();
    let name = match fields.get("name") {
        None => None,
        Some(Value::String(name)) if !name.trim().is_empty() && name.chars().count() <= 200 => {
            Some(name.trim().to_owned())
        }
        Some(Value::String(_)) => {
            issues.push(json!({"path":["name"],"message":"Name must contain 1 to 200 characters"}));
            None
        }
        Some(other) => {
            issues.push(station_config::invalid_type_issue("string", Some(other), &[json!("name")]));
            None
        }
    };
    let quasar_config = fields.get("quasarConfig").map(|quasar_config| {
        let (quasar_config, config_issues) = station_config::parse(Some(quasar_config), &["quasarConfig"]);
        issues.extend(config_issues);
        quasar_config
    });
    if issues.is_empty() {
        Ok((name, quasar_config))
    } else {
        Err(issues)
    }
}

/// The text goes through the dialog agent (TS pushEvent); `duid` may be `all`
async fn push_event(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    body: Bytes,
) -> Response {
    let text = match event_text(&body) {
        Ok(text) => text,
        Err(rejection) => return rejection.into_response(),
    };
    dispatch(
        &state,
        &duid,
        "push",
        json!({"eventText":text}),
        StationCommand::Event { text },
    )
    .await
}

/// The text is spoken as is (TS pushTts); `duid` may be `all`
async fn push_raw(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    body: Bytes,
) -> Response {
    let text = match event_text(&body) {
        Ok(text) => text,
        Err(rejection) => return rejection.into_response(),
    };
    dispatch(
        &state,
        &duid,
        "push-raw",
        json!({"eventText":text}),
        StationCommand::Speak { text },
    )
    .await
}

/// The text is answered as if the user said it: spoken answer, functions run; `duid` may be `all`
async fn push_text(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    body: Bytes,
) -> Response {
    let text = match event_text(&body) {
        Ok(text) => text,
        Err(rejection) => return rejection.into_response(),
    };
    dispatch(
        &state,
        &duid,
        "push-text",
        json!({"eventText":text}),
        StationCommand::Utterance { text },
    )
    .await
}

/// A raw TDirective in protobufjs object form (TS pushRawDirectives); `duid` may be `all`
async fn push_directive(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    body: Bytes,
) -> Response {
    let body = match read_body(&body) {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    // TS converts the body per connection and fails with a 500 there; it is
    // validated up front instead
    let directive = match raw_directive::from_object(body.as_ref().unwrap_or(&Value::Null)) {
        Ok(directive) => directive,
        Err(message) => return Rejection::Validation(message).into_response(),
    };
    dispatch(
        &state,
        &duid,
        "push-directive",
        body.unwrap_or_default(),
        StationCommand::Directives {
            directives: vec![directive],
        },
    )
    .await
}

async fn command_record(
    state: &ApiState,
    duid: &str,
    action: &str,
    body: Value,
) -> Result<crate::control::CommandFeedback, Response> {
    let stations = state
        .storage
        .list()
        .await
        .map_err(|error| internal_error("command station", &error))?;
    let station = if duid == "all" {
        "Все колонки"
    } else {
        stations
            .iter()
            .find(|station| station.duid == duid)
            .map(|station| station.name.as_str())
            .ok_or_else(|| (StatusCode::NOT_FOUND, "Station does not exist").into_response())?
    };
    state
        .control
        .create(duid, station, action, body)
        .await
        .map_err(|error| internal_error("record command", &error))
}

async fn dispatch(
    state: &ApiState,
    duid: &str,
    action: &str,
    body: Value,
    command: StationCommand,
) -> Response {
    let feedback = match command_record(state, duid, action, body).await {
        Ok(feedback) => feedback,
        Err(response) => return response,
    };
    let delivered = state.registry.send(
        duid,
        StationCommand::Tracked {
            feedback: feedback.clone(),
            command: Box::new(command),
        },
    );
    feedback.delivered(delivered).await;
    if delivered == 0 {
        feedback.failed("Нет открытого соединения с колонкой").await;
    }
    Json(json!({"delivered":delivered,"commandId":feedback.id})).into_response()
}

async fn player_status(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
) -> Json<crate::player::PlayerStatus> {
    Json(crate::player::status(&state.registry, &state.runtime.snapshot().remote, &duid).await)
}

async fn player_cover(
    State(state): State<Arc<ApiState>>,
    Path((duid, key)): Path<(String, String)>,
) -> Response {
    let Some(cover) = state
        .runtime
        .snapshot()
        .remote
        .player_cover(&duid, &key)
        .await
    else {
        return (StatusCode::NOT_FOUND, "Cover unavailable").into_response();
    };
    (
        [
            (header::CONTENT_TYPE, cover.content_type),
            (
                header::CACHE_CONTROL,
                "private, max-age=3600, immutable".to_owned(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        cover.body,
    )
        .into_response()
}

async fn player_action(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let feedback = match command_record(&state, &duid, "player", body.clone()).await {
        Ok(feedback) => feedback,
        Err(response) => return response,
    };
    feedback.running().await;
    match crate::player::action(&state.registry, &state.runtime.snapshot().remote, &duid, &body).await {
        Ok(receipt) => {
            feedback.delivered(receipt.delivered).await;
            feedback
                .action(
                    body["action"].as_str().unwrap_or("player"),
                    "sent",
                    Some(&receipt.result),
                )
                .await;
            feedback.completed().await;
            Json(json!({"delivered":receipt.delivered,"result":receipt.result,"commandId":feedback.id})).into_response()
        }
        Err(error) => {
            feedback.failed(error.1).await;
            (error.0, error.1).into_response()
        }
    }
}

/// A request body that is rejected
enum Rejection {
    /// malformed JSON
    BadRequest,
    /// not the expected shape: what is wrong
    Validation(String),
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest => (StatusCode::BAD_REQUEST, "Bad Request").into_response(),
            Self::Validation(message) => {
                (StatusCode::UNPROCESSABLE_ENTITY, message).into_response()
            }
        }
    }
}

/// None for an empty body
fn read_body(body: &[u8]) -> Result<Option<Value>, Rejection> {
    if body.len() > 64 * 1024 { return Err(Rejection::Validation("Request body exceeds 64 KiB".into())); }
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(body).map(Some).map_err(|error| {
        tracing::warn!("invalid management API request body: {error}");
        Rejection::BadRequest
    })
}

/// The push bodies: `{ "eventText": "..." }`
fn event_text(body: &[u8]) -> Result<String, Rejection> {
    match read_body(body)? {
        Some(Value::Object(fields)) => match fields.get("eventText") {
            Some(Value::String(text))
                if !text.trim().is_empty() && text.chars().count() <= 8000 =>
            {
                Ok(text.clone())
            }
            Some(Value::String(_)) => Err(Rejection::Validation(
                "eventText must contain 1 to 8000 characters".into(),
            )),
            _ => Err(Rejection::Validation(
                "eventText must be a string".to_string(),
            )),
        },
        _ => Err(Rejection::Validation("the body must be a JSON object with eventText".to_string())),
    }
}

fn internal_error(route: &str, error: &anyhow::Error) -> Response {
    tracing::error!("Error on {route}: {error:#}");
    (StatusCode::INTERNAL_SERVER_ERROR, "Не удалось обработать запрос. Проверьте журнал сервера.").into_response()
}
