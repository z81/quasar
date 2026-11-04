//! Weather management routes share the existing API authentication boundary.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::ApiState;

#[derive(Deserialize)]
pub struct Search {
    query: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetLocation {
    id: Value,
}

pub async fn locations(
    State(state): State<Arc<ApiState>>,
    Query(search): Query<Search>,
) -> Response {
    let query = search.query.trim();
    if !(2..=120).contains(&query.chars().count()) || query.chars().any(char::is_control) {
        return error(
            StatusCode::BAD_REQUEST,
            "Название города: от 2 до 120 символов",
        );
    }
    match state.weather.search(query).await {
        Ok(locations) => Json(locations).into_response(),
        Err(cause) => provider_error("search", cause),
    }
}

pub async fn forecast(State(state): State<Arc<ApiState>>, Path(duid): Path<String>) -> Response {
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    match state.weather.for_device(&duid).await {
        Ok(view) => Json(view).into_response(),
        Err(cause) => provider_error("forecast", cause),
    }
}

pub async fn set_location(
    State(state): State<Arc<ApiState>>,
    Path(duid): Path<String>,
    Json(body): Json<SetLocation>,
) -> Response {
    let id = match body.id {
        Value::Null => None,
        value => match value.as_i64().filter(|id| *id > 0) {
            Some(id) => Some(id),
            None => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "id должен быть положительным целым числом или null",
                )
            }
        },
    };
    if let Err(response) = require_station(&state, &duid).await {
        return response;
    }
    match state.weather.set_location(&duid, id).await {
        Ok(location) => Json(location).into_response(),
        Err(cause) if cause.is::<crate::weather::LocationNotFound>() => error(
            StatusCode::BAD_REQUEST,
            "Город не найден. Выберите город из результатов поиска.",
        ),
        Err(cause) => provider_error("set location", cause),
    }
}

async fn require_station(state: &ApiState, duid: &str) -> Result<(), Response> {
    match state.storage.list().await {
        Ok(stations) if stations.iter().any(|station| station.duid == duid) => Ok(()),
        Ok(_) => Err(error(StatusCode::NOT_FOUND, "Колонка не найдена")),
        Err(cause) => {
            tracing::warn!("weather station lookup failed: {cause:#}");
            Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Хранилище временно недоступно",
            ))
        }
    }
}

fn provider_error(operation: &str, cause: anyhow::Error) -> Response {
    tracing::warn!("weather {operation} failed: {cause:#}");
    error(
        StatusCode::BAD_GATEWAY,
        "Сервис погоды временно недоступен. Попробуйте позже.",
    )
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
