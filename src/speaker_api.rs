//! Owner-only household profiles. Integration keys cannot read biometric data.
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::api::ApiState;
use crate::auth::AuthManager;

pub fn router(auth: Arc<AuthManager>) -> Router<Arc<ApiState>> {
    Router::new()
        .route("/settings/speakers/{duid}", get(view))
        .route("/settings/speakers/enrollment", post(begin).delete(cancel))
        .route(
            "/settings/speakers/profiles/{id}",
            delete(remove).patch(update),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::require_settings_access,
        ))
}

async fn view(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    let context = state.runtime.snapshot();
    Json(
        context
            .speakers
            .view(context.config.speaker_embedding_url.as_deref(), &duid)
            .await,
    )
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Begin {
    duid: String,
    name: String,
    #[serde(default)]
    preferences: String,
    consent: bool,
}

async fn begin(State(state): State<Arc<ApiState>>, Json(body): Json<Begin>) -> Response {
    if !body.consent {
        return invalid("Нужно согласие человека, чей голос вы добавляете");
    }
    match state.storage.list().await {
        Ok(stations) if stations.iter().any(|station| station.duid == body.duid) => {}
        Ok(_) => return invalid("Колонка не найдена"),
        Err(_) => return (StatusCode::SERVICE_UNAVAILABLE, "Хранилище недоступно").into_response(),
    }
    if !state.registry.is_connected(&body.duid) {
        return invalid("Выбранная колонка не подключена");
    }
    let context = state.runtime.snapshot();
    let view = context
        .speakers
        .view(context.config.speaker_embedding_url.as_deref(), &body.duid)
        .await;
    if view.backend != "ready" {
        return invalid(&view.message);
    }
    match context
        .speakers
        .begin(body.duid, body.name, body.preferences)
        .await
    {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(error) => invalid(&error.to_string()),
    }
}

async fn cancel(State(state): State<Arc<ApiState>>) -> Response {
    state.runtime.snapshot().speakers.cancel().await;
    Json(json!({"ok": true})).into_response()
}

async fn remove(State(state): State<Arc<ApiState>>, Path(id): Path<String>) -> Response {
    match state.runtime.snapshot().speakers.delete(&id).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(_) => invalid("Не удалось удалить профиль. Обновите страницу и повторите."),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    name: String,
    preferences: String,
}

async fn update(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(body): Json<Update>,
) -> Response {
    match state.runtime.snapshot().speakers.update(&id, body.name, body.preferences).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(_) => invalid("Не удалось сохранить профиль. Проверьте имя (1–80 символов) и предпочтения (до 2000 символов)."),
    }
}

fn invalid(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
}
