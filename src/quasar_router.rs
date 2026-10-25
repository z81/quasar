//! quasar.yandex.net endpoints: registration, updates, glagol tokens
//! (HS256 JWT), device list and sync info — plus the neighbouring Yandex
//! domain stubs the firmware expects to reach.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::storage::Storage;

pub struct QuasarState {
    pub storage: Arc<Storage>,
    pub glagol_jwt_key: Vec<u8>,
    pub updates: Arc<crate::updates::UpdatesConfig>,
}

#[derive(Deserialize)]
struct DeviceParameters {
    device_id: Option<String>,
    platform: Option<String>,
    version: Option<String>,
}

pub fn router() -> Router<crate::AppState> {
    Router::new()
        .route("/quasar.yandex.net/register", post(register))
        .route("/quasar.yandex.net/check_updates", get(check_updates))
        .route("/quasar.yandex.net/update_device_state", post(update_device_state))
        .route("/quasar.yandex.net/push_subscribe", post(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
        .route("/quasar.yandex.net/glagol/token", get(glagol_token))
        .route("/quasar.yandex.net/glagol/check_token", post(check_token))
        .route("/quasar.yandex.net/glagol/v2.0/check_token", post(check_token_v2))
        .route("/quasar.yandex.net/glagol/device_list", get(device_list))
        .route("/quasar.yandex.net/get_sync_info", get(get_sync_info))
        .route("/oauth.yandex.ru/token", post(oauth_token))
        .route("/rpc.alice.yandex.net/gproxy/push_events_batch", post(empty_protobuf))
        .route("/clck.yandex.net/quasar_metrics/write/batch", post(clck_metrics_batch))
        .route("/clck.yandex.net/quasar/write/json/batch", post(clck_json_batch))
        .route("/scbh.yandex.net/generate_204", get(generate_204))
        .route("/scbh.yandex.net/{*rest}", get(scbh_404))
        .route("/startup.mobile.yandex.net/analytics/startup", get(StatusCode::INTERNAL_SERVER_ERROR))
        .route("/report.appmetrica.yandex.net/report", post(StatusCode::INTERNAL_SERVER_ERROR))
        .fallback(not_found_500)
}

async fn register(Query(_parameters): Query<DeviceParameters>) -> Json<Value> {
    Json(json!({ "hasCriticalUpdate": false, "status": "ok" }))
}

async fn check_updates(
    State(state): State<crate::AppState>,
    Query(parameters): Query<DeviceParameters>,
) -> Json<Value> {
    let platform = parameters.platform.unwrap_or_default();
    let version = parameters.version.unwrap_or_default();
    Json(state.quasar.updates.response_for(&platform, &version))
}

async fn update_device_state(
    State(state): State<crate::AppState>,
    Query(parameters): Query<DeviceParameters>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let Some(device_id) = parameters.device_id.filter(|id| !id.is_empty()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let platform = parameters.platform.unwrap_or_default();
    let _ = state.quasar.storage.register(&device_id, &platform).await;
    let network_info: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state.quasar.storage.update_network_info(&device_id, network_info).await;
    Json(json!({ "status": "ok" })).into_response()
}

async fn glagol_token(
    State(state): State<crate::AppState>,
    Query(parameters): Query<DeviceParameters>,
) -> impl IntoResponse {
    let Some(device_id) = parameters.device_id.filter(|id| !id.is_empty()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let platform = parameters.platform.unwrap_or_default();
    let _ = state.quasar.storage.register(&device_id, &platform).await;

    #[derive(serde::Serialize)]
    struct GlagolClaims {
        plt: String,
        exp: u64,
        iat: u64,
        iss: &'static str,
        aud: &'static str,
        sub: String,
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let claims = GlagolClaims {
        plt: platform,
        exp: now + 24 * 3600,
        iat: now,
        iss: "quasar-backend",
        aud: "glagol",
        sub: device_id,
    };
    match jsonwebtoken::encode(
        &Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&state.quasar.glagol_jwt_key),
    ) {
        Ok(token) => Json(json!({ "status": "ok", "token": token })).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn check_token(State(state): State<crate::AppState>, body: String) -> impl IntoResponse {
    let valid = decode_glagol(&state.quasar, &body);
    Json(json!({ "status": "ok", "valid": valid })).into_response()
}

async fn check_token_v2(State(state): State<crate::AppState>, body: String) -> impl IntoResponse {
    let owner = decode_glagol(&state.quasar, &body);
    Json(json!({ "guest": false, "owner": owner, "status": "ok" })).into_response()
}

async fn device_list(State(state): State<crate::AppState>) -> impl IntoResponse {
    Json(state.quasar.storage.device_list_json().await).into_response()
}

async fn get_sync_info(
    State(state): State<crate::AppState>,
    Query(parameters): Query<DeviceParameters>,
) -> impl IntoResponse {
    let Some(device_id) = parameters.device_id.filter(|id| !id.is_empty()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some(mut device) = state.quasar.storage.get(&device_id).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Some(object) = device.as_object_mut() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    object.insert("status".into(), json!("ok"));
    object.insert("alice_pro_subscription".into(), json!({ "enabled": false }));
    object.insert("subscription".into(), json!({ "mode": "transaction" }));
    object.insert("tags".into(), json!([]));
    Json(device).into_response()
}

fn decode_glagol(state: &QuasarState, body: &str) -> bool {
    let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.validate_aud = false;
    jsonwebtoken::decode::<Value>(
        body.trim(),
        &DecodingKey::from_secret(&state.glagol_jwt_key),
        &validation,
    )
    .is_ok()
}

#[derive(Deserialize)]
struct OAuthForm {
    grant_type: Option<String>,
    code: Option<String>,
    device_id: Option<String>,
    access_token: Option<String>,
}

async fn oauth_token(Json(form): Json<OAuthForm>) -> impl IntoResponse {
    match form.grant_type.as_deref() {
        Some("authorization_code") => {
            let mut hasher = Sha256::new();
            hasher.update(b"access");
            hasher.update(form.code.clone().unwrap_or_default());
            hasher.update(form.device_id.clone().unwrap_or_default());
            let access = format!("VAT.{}", hex::encode(hasher.finalize()));
            let mut hasher = Sha256::new();
            hasher.update(b"refresh");
            hasher.update(form.code.unwrap_or_default());
            hasher.update(form.device_id.unwrap_or_default());
            let refresh = format!("VRT.{}", hex::encode(hasher.finalize()));
            Json(json!({
                "access_token": access,
                "expires_in": 365 * 10 * 24 * 60 * 60,
                "refresh_token": refresh,
                "token_type": "bearer",
            }))
            .into_response()
        }
        Some("x-token") => Json(json!({
            "access_token": form.access_token.unwrap_or_default(),
            "expires_in": 365 * 10 * 24 * 60 * 60,
            "token_type": "bearer",
            "uid": 10_146_228,
        }))
        .into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn empty_protobuf() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "application/x-protobuf")],
        Vec::new(),
    )
}

async fn clck_metrics_batch() -> impl IntoResponse {
    Vec::from([0x0A, 0x02, 0x4F, 0x4B])
}

async fn clck_json_batch(body: axum::body::Bytes) -> impl IntoResponse {
    let parsed: Result<Vec<Value>, _> = serde_json::from_slice(&body);
    match parsed {
        Ok(items) => Json(json!(items
            .iter()
            .map(|_| json!({ "ResponseStatus": "OK" }))
            .collect::<Vec<_>>()))
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn generate_204() -> impl IntoResponse {
    let start_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    (
        StatusCode::NO_CONTENT,
        [("x-start-time", start_time.to_string())],
    )
}

async fn scbh_404() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn not_found_500() -> StatusCode {
    StatusCode::INTERNAL_SERVER_ERROR
}
