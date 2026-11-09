//! Public OAuth return endpoint. The pending, expiring, single-use state in the
//! selected bridge authorizes this request; ordinary API access remains protected.
use std::{collections::HashMap, sync::Arc};

use axum::{
    extract::{RawQuery, State},
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::json;

use crate::api::ApiState;

pub(crate) fn router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/spotify/callback", get(callback))
        .with_state(state)
}

fn failure(status: StatusCode) -> Response {
    (status, Html("<!doctype html><html lang=ru><meta charset=utf-8><meta name=viewport content='width=device-width, initial-scale=1'><meta name=color-scheme content='light dark'><title>Подключение Spotify</title><h1>Не удалось завершить вход в Spotify</h1><p>Ссылка могла устареть, уже использоваться или доступ был отменён. Откройте пульт и начните подключение заново.</p><p><a href='/ui/'>Вернуться в Quasar</a></p></html>".to_owned())).into_response()
}

async fn callback(State(state): State<Arc<ApiState>>, RawQuery(query): RawQuery) -> Response {
    let mut response = complete(&state, query.as_deref().unwrap_or_default()).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; base-uri 'none'; frame-ancestors 'none'"),
    );
    response
}

async fn complete(state: &ApiState, raw_query: &str) -> Response {
    if raw_query.len() > 8192 {
        return failure(StatusCode::BAD_REQUEST);
    }
    let Ok(pairs) = serde_urlencoded::from_str::<Vec<(String, String)>>(raw_query) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    let mut query = HashMap::new();
    for (key, value) in pairs {
        if !matches!(
            key.as_str(),
            "state" | "code" | "error" | "error_description"
        ) || value.len() > 4096
            || query.insert(key, value).is_some()
        {
            return failure(StatusCode::BAD_REQUEST);
        }
    }
    let Some(token) = query.get("state") else {
        return failure(StatusCode::BAD_REQUEST);
    };
    let Some((encoded_device, nonce)) = token.split_once('.') else {
        return failure(StatusCode::BAD_REQUEST);
    };
    if encoded_device.len() > 256
        || nonce.len() != 43
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return failure(StatusCode::BAD_REQUEST);
    }
    let Ok(device_bytes) = URL_SAFE_NO_PAD.decode(encoded_device) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    let Ok(duid) = String::from_utf8(device_bytes) else {
        return failure(StatusCode::BAD_REQUEST);
    };
    if duid.is_empty()
        || duid.len() > 192
        || duid.chars().any(char::is_control)
        || query.contains_key("code") == query.contains_key("error")
    {
        return failure(StatusCode::BAD_REQUEST);
    }
    let Ok(stations) = state.storage.list().await else {
        return failure(StatusCode::SERVICE_UNAVAILABLE);
    };
    if !stations.iter().any(|station| station.duid == duid) {
        return failure(StatusCode::BAD_REQUEST);
    }
    // Do not log the query or the remote response: authorization codes are credentials.
    let result = state.runtime.snapshot().remote.player_music(&duid, &HashMap::new(), Some(&json!({
        "action":"authorize_callback", "state":token, "code":query.get("code"), "error":query.get("error"),
    }))).await;
    match result {
        Ok(_) => Redirect::to("/ui/").into_response(),
        Err((status, _)) => failure(if status.is_server_error() {
            StatusCode::BAD_GATEWAY
        } else {
            StatusCode::BAD_REQUEST
        }),
    }
}
