//! Management API (API_PORT): device list and pushing events / raw speech.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::connections::{ConnectionRegistry, StationCommand};
use crate::storage::Storage;

pub struct ApiState {
    pub registry: ConnectionRegistry,
    pub storage: Arc<Storage>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct PushBody {
    event_text: Option<String>,
    eventText: Option<String>,
}

pub fn router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/devices", get(devices))
        .route("/devices/{duid}/push", post(push_event))
        .route("/devices/{duid}/push-raw", post(push_raw))
        .with_state(state)
}

async fn devices(State(state): State<Arc<ApiState>>) -> impl axum::response::IntoResponse {
    Json(json!(state.storage.list().await.iter().map(|station| json!({
        "duid": station.duid,
        "name": station.name,
        "platform": station.platform,
        "networkInfo": station.network_info,
    })).collect::<Vec<_>>()))
}

async fn push_event(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(body): Json<PushBody>,
) -> impl axum::response::IntoResponse {
    let text = body.event_text.or(body.eventText).unwrap_or_default();
    let delivered = state.registry.send(&duid, StationCommand::Event { text });
    (axum::http::StatusCode::OK, format!("{{\"delivered\":{delivered}}}"))
}

async fn push_raw(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(body): Json<PushBody>,
) -> impl axum::response::IntoResponse {
    let text = body.event_text.or(body.eventText).unwrap_or_default();
    let delivered = state.registry.send(&duid, StationCommand::Speak { text });
    (axum::http::StatusCode::OK, format!("{{\"delivered\":{delivered}}}"))
}
