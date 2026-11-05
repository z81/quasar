//! Server configuration is available to the owner, never integration tokens.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::api::ApiState;
use crate::auth::AuthManager;
use crate::server_settings::{Patch, RevealMcpKey, SettingsError};

pub fn router(auth: Arc<AuthManager>) -> Router<Arc<ApiState>> {
    Router::new()
        .route("/settings/server", get(read).patch(update))
        .route("/settings/server/mcp/reveal", post(reveal_mcp_key))
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

async fn reveal_mcp_key(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<RevealMcpKey>,
) -> Response {
    // The management router's outer no_cache middleware also covers auth and
    // JSON rejection responses, so secrets and errors always use no-store.
    match state.settings.reveal_mcp_key(request).await {
        Ok(token) => Json(json!({ "token": token })).into_response(),
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
