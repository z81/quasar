//! UI entry point for the existing remote-function protocol. The selected
//! station is supplied by the server, never accepted from function arguments.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::api::ApiState;

pub async fn status(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    invoke(&state, &duid, "generated_music_status", json!({})).await
}

pub async fn action(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    let name = match body["action"].as_str() {
        Some("generate") => "generate_music",
        Some("radio") => "start_music_radio",
        Some("stop") => "stop_music_radio",
        Some("pause") => "pause_generated_music",
        Some("resume") => "resume_generated_music",
        _ => return failure(StatusCode::UNPROCESSABLE_ENTITY, "Неизвестное действие"),
    };
    if body.to_string().len() > 8192 {
        return failure(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Слишком длинное описание музыки",
        );
    }
    if name != "stop_music_radio" && !state.registry.is_connected(&duid) {
        return failure(StatusCode::CONFLICT, "Колонка не подключена");
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("action");
    }
    invoke(&state, &duid, name, body).await
}

async fn invoke(state: &ApiState, duid: &str, name: &str, arguments: Value) -> Response {
    let context = state.runtime.snapshot();
    if !context.remote.owns(name) {
        if name != "generated_music_status" {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "Stable Audio не подключён");
        }
        return Json(json!({"configured": false, "ready": false, "state": "idle"})).into_response();
    }
    match context
        .remote
        .call_by_name(
            name,
            &Some(arguments),
            duid,
            &json!({"serverType":"quasar", "deviceId":duid}),
        )
        .await
    {
        Ok(result) => match serde_json::from_str::<Value>(&result) {
            Ok(value) if value["duid"].as_str() == Some(duid) => Json(value).into_response(),
            _ => failure(
                StatusCode::BAD_GATEWAY,
                "Непонятный ответ генератора музыки",
            ),
        },
        Err(_) => failure(
            StatusCode::BAD_GATEWAY,
            "Генератор музыки не выполнил запрос. Проверьте его состояние перед повтором.",
        ),
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}
