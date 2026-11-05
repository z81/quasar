//! Server configuration is available to the owner, never integration tokens.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::api::ApiState;
use crate::auth::AuthManager;
use crate::server_settings::{Patch, SettingsError};

pub fn router(auth: Arc<AuthManager>) -> Router<Arc<ApiState>> {
    Router::new()
        .route("/settings/server", get(read).patch(update))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::require_settings_access,
        ))
}

async fn read(State(state): State<Arc<ApiState>>) -> Response {
    match state.settings.get().await {
        Ok(view) => Json(view).into_response(),
        Err(cause) => error(cause),
    }
}

async fn update(State(state): State<Arc<ApiState>>, Json(patch): Json<Patch>) -> Response {
    match state.settings.patch(patch).await {
        Ok(view) => Json(view).into_response(),
        Err(cause) => error(cause),
    }
}

fn error(cause: SettingsError) -> Response {
    let status = match &cause {
        SettingsError::Invalid(_) => StatusCode::BAD_REQUEST,
        SettingsError::Conflict => StatusCode::CONFLICT,
        SettingsError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(json!({"error":cause.to_string()}))).into_response()
}
