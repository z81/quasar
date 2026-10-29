//! Management API (API_PORT), TS main.ts: devices and their quasar configs,
//! pushing events / raw speech / raw directives. Request bodies are JSON;
//! invalid ones get 400 or 422 with what is wrong.

mod raw_directive;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::connections::{ConnectionRegistry, StationCommand};
use crate::station_config;
use crate::stations::StationSummary;
use crate::storage::Storage;

pub struct ApiState {
    pub registry: ConnectionRegistry,
    pub storage: Arc<Storage>,
}

pub fn router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/devices", get(devices))
        .route("/devices/{duid}", patch(update_device))
        .route("/devices/{duid}/push", post(push_event))
        .route("/devices/{duid}/push-raw", post(push_raw))
        .route("/devices/{duid}/push-text", post(push_text))
        .route("/devices/{duid}/push-directive", post(push_directive))
        .route("/debug", get(|| async { Html(include_str!("api/debug.html")) }))
        .fallback(|| async { (StatusCode::NOT_FOUND, "NOT_FOUND") })
        .with_state(state)
}

async fn devices(State(state): State<Arc<ApiState>>) -> Response {
    match state.storage.list().await {
        Ok(stations) => Json(stations.iter().map(StationSummary::to_api_device).collect::<Vec<_>>()).into_response(),
        Err(error) => internal_error("GET /devices", &error),
    }
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
        Err(issues) => return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "errors": issues }))).into_response(),
    };
    match state
        .storage
        .update_name_and_config(&duid, name.as_deref(), quasar_config.as_ref())
        .await
    {
        Ok(Some(station)) => {
            let directives = vec![raw_directive::push_update_config()];
            let delivered = state.registry.send(&duid, StationCommand::Directives { directives });
            tracing::info!("updated device {duid}, push_update_config sent to {delivered} connection(s)");
            Json(station.to_api_device()).into_response()
        }
        // TS throws here, which Elysia answers with 500 and the message
        Ok(None) => (StatusCode::INTERNAL_SERVER_ERROR, "Station does not exist").into_response(),
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
        Some(Value::String(name)) => Some(name.clone()),
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
    let delivered = state.registry.send(&duid, StationCommand::Event { text });
    tracing::info!("pushed an event to {delivered} connection(s) of {duid}");
    Json(json!({})).into_response()
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
    let delivered = state.registry.send(&duid, StationCommand::Speak { text });
    tracing::info!("pushed a raw event to {delivered} connection(s) of {duid}");
    Json(json!({})).into_response()
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
    let delivered = state.registry.send(&duid, StationCommand::Utterance { text });
    tracing::info!("pushed a typed utterance to {delivered} connection(s) of {duid}");
    Json(json!({})).into_response()
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
    let delivered = state.registry.send(&duid, StationCommand::Directives { directives: vec![directive] });
    tracing::info!("pushed a raw directive to {delivered} connection(s) of {duid}");
    Json(json!({})).into_response()
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
            Self::Validation(message) => (StatusCode::UNPROCESSABLE_ENTITY, message).into_response(),
        }
    }
}

/// None for an empty body
fn read_body(body: &[u8]) -> Result<Option<Value>, Rejection> {
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
            Some(Value::String(text)) => Ok(text.clone()),
            _ => Err(Rejection::Validation("eventText must be a string".to_string())),
        },
        _ => Err(Rejection::Validation("the body must be a JSON object with eventText".to_string())),
    }
}

fn internal_error(route: &str, error: &anyhow::Error) -> Response {
    tracing::error!("Error on {route}: {error:#}");
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response()
}
