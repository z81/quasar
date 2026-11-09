use crate::{
    api::ApiState,
    auth::AuthManager,
    device_access::{display, operations},
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

struct PresetsState {
    api: Arc<ApiState>,
    auth: Arc<AuthManager>,
}

pub(crate) fn router(api: Arc<ApiState>, auth: Arc<AuthManager>) -> Router {
    Router::new()
        .route(
            "/devices/{duid}/animations",
            get(list).post(save).delete(remove),
        )
        .with_state(Arc::new(PresetsState { api, auth }))
}

async fn list(
    State(state): State<Arc<PresetsState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = operations::authorize(&state.api, &state.auth, &duid, &headers, false).await
    {
        return error;
    }
    match state
        .api
        .storage
        .get_control_value(&format!("animations:{duid}"))
        .await
    {
        Ok(value) => Json(value.unwrap_or_else(|| json!([]))).into_response(),
        Err(_) => operations::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Не удалось прочитать пресеты",
        ),
    }
}

#[derive(Deserialize)]
struct Preset {
    name: String,
    frames: Vec<display::AnimationFrame>,
    repeat: u8,
}

async fn save(
    State(state): State<Arc<PresetsState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if let Err(error) = operations::authorize(&state.api, &state.auth, &duid, &headers, true).await
    {
        return error;
    }
    let Ok(preset) = serde_json::from_value::<Preset>(value) else {
        return operations::error(StatusCode::UNPROCESSABLE_ENTITY, "Некорректный пресет");
    };
    let name = preset.name.trim();
    if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return operations::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Название: от 1 до 80 символов",
        );
    }
    if let Err(error) = display::sequence_script(&preset.frames, preset.repeat) {
        return operations::error(StatusCode::UNPROCESSABLE_ENTITY, error);
    }
    let value = json!({"name":name,"frames":preset.frames,"repeat":preset.repeat});
    update(&state, &duid, name, Some(value)).await
}

async fn remove(
    State(state): State<Arc<PresetsState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if let Err(error) = operations::authorize(&state.api, &state.auth, &duid, &headers, true).await
    {
        return error;
    }
    let Some(name) = value["name"].as_str() else {
        return operations::error(StatusCode::UNPROCESSABLE_ENTITY, "Укажите название пресета");
    };
    update(&state, &duid, name, None).await
}

async fn update(state: &PresetsState, duid: &str, name: &str, value: Option<Value>) -> Response {
    let key = format!("animations:{duid}");
    for _ in 0..4 {
        let old = match state.api.storage.get_control_value(&key).await {
            Ok(value) => value,
            Err(_) => break,
        };
        let mut items = old
            .as_ref()
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        items.retain(|item| item["name"].as_str() != Some(name));
        if let Some(value) = &value {
            items.push(value.clone());
        }
        let updated = json!(items);
        if items.len() > 32 || updated.to_string().len() > 512 * 1024 {
            return operations::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Лимит: 32 пресета и 512 КиБ на колонку",
            );
        }
        let saved = if let Some(old) = &old {
            state
                .api
                .storage
                .compare_control_value(&key, old, &updated)
                .await
        } else {
            state.api.storage.insert_control_value(&key, &updated).await
        };
        match saved {
            Ok(true) => return Json(updated).into_response(),
            Ok(false) => continue,
            Err(_) => break,
        }
    }
    operations::error(
        StatusCode::CONFLICT,
        "Не удалось сохранить пресеты; обновите страницу и повторите",
    )
}
