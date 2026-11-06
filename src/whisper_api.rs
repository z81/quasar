//! Owner settings for explicitly whispered replies. Saving never plays audio.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::api::ApiState;
use crate::auth::AuthManager;

pub fn router(auth: Arc<AuthManager>) -> Router<Arc<ApiState>> {
    Router::new()
        .route("/devices/{duid}/whisper", get(view).put(update))
        .layer(DefaultBodyLimit::max(1024))
        .layer(axum::middleware::from_fn_with_state(auth, crate::auth::require_settings_access))
}

async fn station(state: &ApiState, duid: &str) -> Result<(), Response> {
    match state.storage.list().await {
        Ok(stations) if stations.iter().any(|station| station.duid == duid) => Ok(()),
        Ok(_) => Err(error(StatusCode::NOT_FOUND, "Колонка не найдена")),
        Err(_) => Err(error(StatusCode::SERVICE_UNAVAILABLE, "Хранилище недоступно")),
    }
}

async fn view(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    if let Err(error) = station(&state, &duid).await { return error; }
    let context = state.runtime.snapshot();
    match context.whisper.view(&context.config, &duid).await {
        Ok(view) => Json(view).into_response(),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "Не удалось прочитать режим шёпота"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    enabled: bool,
}

async fn update(State(state): State<Arc<ApiState>>, Path(duid): Path<String>, Json(body): Json<Update>) -> Response {
    if let Err(error) = station(&state, &duid).await { return error; }
    let context = state.runtime.snapshot();
    if body.enabled && !crate::whisper::available(&context.config) {
        return error(StatusCode::BAD_REQUEST, "Для шёпота нужен ключ OpenRouter в настройках сервера");
    }
    if let Err(cause) = context.whisper.set(&context.config, &duid, body.enabled).await {
        tracing::warn!("cannot save whisper preference: {cause}");
        return error(StatusCode::SERVICE_UNAVAILABLE, "Не удалось сохранить режим шёпота");
    }
    match context.whisper.view(&context.config, &duid).await {
        Ok(view) => Json(view).into_response(),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "Не удалось прочитать режим шёпота"),
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
