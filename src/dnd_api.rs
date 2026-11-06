//! Authenticated quiet-hours controls; the API router supplies authentication.

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::api::ApiState;
use crate::auth::AuthManager;
use crate::dnd::{DndError, Schedule, Status};

pub fn router(auth: Arc<AuthManager>) -> Router<Arc<ApiState>> {
    Router::new()
        .route("/devices/{duid}/dnd", get(status).put(update))
        .route("/devices/{duid}/dnd/manual", post(manual))
        .layer(DefaultBodyLimit::max(1024))
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::auth::require_settings_access,
        ))
}

async fn status(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    respond(state.dnd.status(&duid).await)
}

async fn update(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(input): Json<Schedule>,
) -> Response {
    respond(state.dnd.update(&duid, input).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualInput {
    enabled: bool,
}

async fn manual(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(input): Json<ManualInput>,
) -> Response {
    respond(state.dnd.set_manual(&duid, input.enabled).await)
}

fn respond(result: Result<Status, DndError>) -> Response {
    match result {
        Ok(status) => Json(status).into_response(),
        Err(error) => {
            let status = match error {
                DndError::Invalid(_) => StatusCode::BAD_REQUEST,
                DndError::NotFound => StatusCode::NOT_FOUND,
                DndError::NotPersistent | DndError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            };
            (status, Json(json!({"error": error.to_string()}))).into_response()
        }
    }
}
