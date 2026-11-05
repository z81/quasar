//! Capability-gated display commands. A queued command is not a firmware ACK.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use super::ApiState;
use crate::connections::StationCommand;
use crate::schedule_api::error;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayAction {
    Animation { animation: String, duration_ms: u64 },
}

pub async fn status(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    if let Err(response) = device_exists(&state, &duid).await {
        return response;
    }
    let connected = state.registry.is_connected(&duid);
    let capability = state.registry.display_capability(&duid);
    let animations = capability
        .as_ref()
        .map_or_else(Vec::new, |(_, capability)| capability.animations());
    let supported = !animations.is_empty();
    let reason = if !connected {
        Some("Колонка не подключена")
    } else if capability.is_none() {
        Some("Колонка ещё не сообщила о возможностях дисплея")
    } else if !supported {
        Some("Колонка не сообщила о поддержке готовых анимаций")
    } else {
        None
    };
    Json(json!({
        "connected": connected,
        "capability_received": capability.is_some(),
        "supported": supported,
        "reason": reason,
        "animations": animations,
        "screens": capability.as_ref().map_or_else(Vec::new, |(_, capability)| capability.screens()),
        "custom": {
            "supported": false,
            "reason": "Штатный протокол не даёт подтверждённого способа отправки своих кадров. Надписи и сегментные рисунки на Миди доступны через подключение к устройству; обычные картинки и GIF её сегментный дисплей не поддерживает."
        }
    })).into_response()
}

pub async fn animate(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(input): Json<DisplayAction>,
) -> Response {
    if let Err(response) = device_exists(&state, &duid).await {
        return response;
    }
    let Some((connection_id, capability)) = state.registry.display_capability(&duid) else {
        return error(
            StatusCode::CONFLICT,
            "Нет актуальных данных о возможностях дисплея. Дождитесь подключения колонки",
        );
    };
    let DisplayAction::Animation {
        animation,
        duration_ms,
    } = input;
    let directive = match capability.animation(&animation, duration_ms) {
        Ok(directive) => directive,
        Err(message) => return error(StatusCode::UNPROCESSABLE_ENTITY, message),
    };
    // Bind delivery to the same connection that reported support. A reconnect
    // must not dispatch a command using the previous firmware's capabilities.
    if !state.registry.send_to_current(
        &duid,
        connection_id,
        StationCommand::Directives {
            directives: vec![directive],
        },
    ) {
        return error(
            StatusCode::CONFLICT,
            "Подключение колонки изменилось. Обновите возможности дисплея",
        );
    }
    (StatusCode::ACCEPTED, Json(json!({"status": "queued"}))).into_response()
}

async fn device_exists(state: &ApiState, duid: &str) -> Result<(), Response> {
    match state.storage.list().await {
        Ok(devices) if devices.iter().any(|device| device.duid == duid) => Ok(()),
        Ok(_) => Err(error(StatusCode::NOT_FOUND, "Колонка не найдена")),
        Err(cause) => Err(super::internal_error("display device lookup", &cause)),
    }
}
