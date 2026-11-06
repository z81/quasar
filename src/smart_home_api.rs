use crate::{api::ApiState, connections::StationCommand, schedule_api::error, smart_home::Action};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;

pub fn router() -> Router<Arc<ApiState>> {
    Router::new().route("/devices/{duid}/smart-home", get(status).post(action))
}

async fn exists(state: &ApiState, duid: &str) -> Result<(), Response> {
    match state.storage.list().await {
        Ok(stations) if stations.iter().any(|s| s.duid == duid) => Ok(()),
        Ok(_) => Err(error(StatusCode::NOT_FOUND, "Колонка не найдена")),
        Err(_) => Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Хранилище недоступно",
        )),
    }
}

async fn status(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    if let Err(error) = exists(&state, &duid).await {
        return error;
    }
    let mut status = state.registry.smart_home.status(&duid);
    status["connected"] = json!(state.registry.is_connected(&duid));
    Json(status).into_response()
}

async fn action(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(action): Json<Action>,
) -> Response {
    if let Err(error) = exists(&state, &duid).await {
        return error;
    }
    if matches!(action, Action::Refresh) {
        return if state
            .registry
            .send_one(&duid, StationCommand::RefreshSmartHome)
        {
            (
                StatusCode::ACCEPTED,
                Json(json!({"status":"queued","message":"Состояние запрошено у станции."})),
            )
                .into_response()
        } else {
            error(StatusCode::CONFLICT, "Колонка не подключена")
        };
    }
    match state.registry.smart_home.dispatch(&duid, action, |id,directive| state.registry.send_to_current(&duid,id,StationCommand::Directives { directives:vec![directive] })) {
        Ok(()) => (StatusCode::ACCEPTED,Json(json!({"status":"queued","message":"Команда отправлена. Выполнение подтверждается состоянием станции."}))).into_response(),
        Err(message) => error(StatusCode::CONFLICT,message),
    }
}
