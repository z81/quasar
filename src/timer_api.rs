//! Duration timers use the same persistent scheduler and station alarm channel.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde_json::json;

use crate::api::ApiState;
use crate::schedule_api::{error, schedule_error};
use crate::schedules::{CreateTimer, ScheduleError};

pub fn router() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/devices/{duid}/timers", get(list).post(create))
        .route("/devices/{duid}/timers/{id}", delete(remove))
        .route("/devices/{duid}/timers/{id}/{action}", post(action))
}

async fn list(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    match state.schedules.list_timers(&duid).await {
        Ok(items) => Json(json!({
            "items": items,
            "persistent": state.storage.is_persistent(),
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
    Json(input): Json<CreateTimer>,
) -> Response {
    if state.registry.alarm_support(&duid) == Some(false) {
        return unsupported();
    }
    match state.schedules.create_timer(&duid, input).await {
        Ok(item) => (StatusCode::CREATED, Json(item)).into_response(),
        Err(cause) => schedule_error(cause),
    }
}

async fn action(
    State(state): State<Arc<ApiState>>,
    Path((duid, id, action)): Path<(String, String, String)>,
) -> Response {
    if matches!(action.as_str(), "resume" | "restart")
        && state.registry.alarm_support(&duid) == Some(false)
    {
        return unsupported();
    }
    let result = match action.as_str() {
        "pause" => state.schedules.pause_timer(&duid, &id).await,
        "resume" => state.schedules.resume_timer(&duid, &id).await,
        "cancel" => state.schedules.cancel_timer(&duid, &id).await,
        "restart" => state.schedules.restart_timer(&duid, &id).await,
        "stop" => match state.schedules.list_timers(&duid).await {
            Ok(items) if items.iter().any(|item| item.id == id) => {
                state.schedules.stop(&duid, &id).await
            }
            Ok(_) => Err(ScheduleError::NotFound),
            Err(cause) => Err(cause),
        },
        _ => return error(StatusCode::NOT_FOUND, "Действие таймера не найдено"),
    };
    match result {
        Ok(item) => Json(item).into_response(),
        Err(cause) => schedule_error(cause),
    }
}

async fn remove(
    State(state): State<Arc<ApiState>>,
    Path((duid, id)): Path<(String, String)>,
) -> Response {
    match state.schedules.delete_timer(&duid, &id).await {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(cause) => schedule_error(cause),
    }
}

fn unsupported() -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "Колонка не поддерживает сигнал таймера",
    )
}
