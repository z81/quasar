//! Authenticated CRUD and active alarm controls for each station.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;

use crate::api::ApiState;
use crate::connections::ConnectionRegistry;
use crate::schedules::{Input, Kind, ScheduleError};
use crate::time_zone::Zone;

pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/devices/{duid}/schedules", get(list).post(create))
        .route(
            "/devices/{duid}/schedules/{id}",
            put(update).patch(set_enabled).delete(delete),
        )
        .route("/devices/{duid}/schedules/{id}/stop", post(stop))
        .route("/devices/{duid}/schedules/{id}/snooze", post(snooze))
}

pub fn default_timezone(registry: &ConnectionRegistry, duid: &str) -> String {
    match Zone::of_station(registry.effective_timezone(duid).as_deref()) {
        Zone::Named(zone) => zone.name().to_owned(),
        Zone::Local => "UTC".to_owned(),
    }
}

async fn require_station(state: &ApiState, duid: &str) -> Result<(), Response> {
    match state.storage.list().await {
        Ok(stations) if stations.iter().any(|station| station.duid == duid) => Ok(()),
        Ok(_) => Err(error(StatusCode::NOT_FOUND, "Колонка не найдена")),
        Err(cause) => {
            tracing::warn!("schedule station lookup failed: {cause:#}");
            Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Хранилище временно недоступно",
            ))
        }
    }
}

async fn list(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    match state.schedules.list(&duid).await {
        Ok(items) => Json(json!({
            "items": items,
            "persistent": state.storage.is_persistent(),
            "timezone": default_timezone(&state.registry, &duid),
            "server_time": Utc::now().to_rfc3339(),
            "native_alarm_supported": state.registry.alarm_support(&duid),
        }))
        .into_response(),
        Err(cause) => schedule_error(cause),
    }
}

async fn create(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(input): Json<Input>,
) -> Response {
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    if input.kind == Kind::Alarm && state.registry.alarm_support(&duid) == Some(false) {
        return error(
            StatusCode::BAD_REQUEST,
            "Колонка не поддерживает нативные будильники",
        );
    }
    match state.schedules.create(&duid, input).await {
        Ok(item) => (StatusCode::CREATED, Json(item)).into_response(),
        Err(cause) => schedule_error(cause),
    }
}

async fn update(
    State(state): State<Arc<ApiState>>,
    Path((duid, id)): Path<(String, String)>,
    Json(input): Json<Input>,
) -> Response {
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    if input.kind == Kind::Alarm && state.registry.alarm_support(&duid) == Some(false) {
        return error(
            StatusCode::BAD_REQUEST,
            "Колонка не поддерживает нативные будильники",
        );
    }
    match state.schedules.update(&duid, &id, input).await {
        Ok(item) => Json(item).into_response(),
        Err(cause) => schedule_error(cause),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enabled {
    enabled: bool,
}

async fn set_enabled(
    State(state): State<Arc<ApiState>>,
    Path((duid, id)): Path<(String, String)>,
    Json(input): Json<Enabled>,
) -> Response {
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    if input.enabled && state.registry.alarm_support(&duid) == Some(false) {
        match state.schedules.list(&duid).await {
            Ok(items)
                if items
                    .iter()
                    .any(|item| item.id == id && item.kind == Kind::Alarm) =>
            {
                return error(
                    StatusCode::BAD_REQUEST,
                    "Колонка не поддерживает нативные будильники",
                );
            }
            Err(cause) => return schedule_error(cause),
            _ => {}
        }
    }
    match state.schedules.set_enabled(&duid, &id, input.enabled).await {
        Ok(item) => Json(item).into_response(),
        Err(cause) => schedule_error(cause),
    }
}

async fn delete(
    State(state): State<Arc<ApiState>>,
    Path((duid, id)): Path<(String, String)>,
) -> Response {
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    match state.schedules.delete(&duid, &id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(cause) => schedule_error(cause),
    }
}

async fn stop(
    State(state): State<Arc<ApiState>>,
    Path((duid, id)): Path<(String, String)>,
) -> Response {
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    match state.schedules.stop(&duid, &id).await {
        Ok(item) => Json(item).into_response(),
        Err(cause) => schedule_error(cause),
    }
}

async fn snooze(
    State(state): State<Arc<ApiState>>,
    Path((duid, id)): Path<(String, String)>,
) -> Response {
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    match state.schedules.snooze(&duid, &id).await {
        Ok(item) => Json(item).into_response(),
        Err(cause) => schedule_error(cause),
    }
}

pub(crate) fn schedule_error(cause: ScheduleError) -> Response {
    let status =
        StatusCode::from_u16(cause.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    error(status, &cause.to_string())
}

pub(crate) fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}
