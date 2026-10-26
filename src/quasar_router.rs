//! quasar.yandex.net endpoints: registration, updates, glagol tokens
//! (HS256 JWT), device list and sync info — plus the neighbouring Yandex
//! domains the firmware expects to reach (oauth, passport, stubs).

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

use crate::stations::StationSummary;
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
    firmware_version: Option<String>,
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
        .route("/mobileproxy.passport.yandex.net/1/token", post(oauth_token))
        .route("/mobileproxy.passport.yandex.net/1/bundle/account/short_info", get(passport_short_info))
        .route("/rpc.alice.yandex.net/gproxy/push_events_batch", post(empty_protobuf))
        .route("/clck.yandex.net/quasar_metrics/write/batch", post(clck_metrics_batch))
        .route("/clck.yandex.net/quasar/write/json/batch", post(clck_json_batch))
        .route("/scbh.yandex.net/generate_204", get(generate_204))
        .route("/scbh.yandex.net/{*rest}", get(scbh_404))
        .route("/startup.mobile.yandex.net/analytics/startup", get(StatusCode::INTERNAL_SERVER_ERROR))
        .route("/report.appmetrica.yandex.net/report", post(StatusCode::INTERNAL_SERVER_ERROR))
        .fallback(not_found_500)
}

async fn register(
    State(state): State<crate::AppState>,
    Query(parameters): Query<DeviceParameters>,
) -> Json<Value> {
    let has_critical_update = state.quasar.updates.has_critical_update(
        parameters.platform.as_deref().unwrap_or_default(),
        parameters.firmware_version.as_deref().unwrap_or_default(),
    );
    Json(json!({ "hasCriticalUpdate": has_critical_update, "status": "ok" }))
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
    let network_info: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let storage = &state.quasar.storage;
    let result = async {
        storage.register(&device_id, &platform).await?;
        storage.update_network_info(&device_id, network_info).await
    }
    .await;
    match result {
        Ok(()) => Json(json!({ "status": "ok" })).into_response(),
        Err(error) => {
            tracing::error!("Error on POST /update_device_state for {device_id}: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn glagol_token(
    State(state): State<crate::AppState>,
    Query(parameters): Query<DeviceParameters>,
) -> impl IntoResponse {
    let Some(device_id) = parameters.device_id.filter(|id| !id.is_empty()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let platform = parameters.platform.unwrap_or_default();
    if let Err(error) = state.quasar.storage.register(&device_id, &platform).await {
        tracing::error!("failed to register station {device_id}: {error:#}");
    }

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
    match state.quasar.storage.list().await {
        Ok(stations) => Json(json!({
            "devices": stations.iter().map(StationSummary::to_glagol_device).collect::<Vec<_>>(),
            "status": "ok",
        }))
        .into_response(),
        Err(error) => {
            tracing::error!("Error on GET /glagol/device_list: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// The station's config; a station seen for the first time is registered
async fn get_sync_info(
    State(state): State<crate::AppState>,
    Query(parameters): Query<DeviceParameters>,
) -> impl IntoResponse {
    let Some(device_id) = parameters.device_id.filter(|id| !id.is_empty()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let platform = parameters.platform.unwrap_or_default();
    match state.quasar.storage.get_or_create(&device_id, &platform).await {
        Ok(station) => Json(station.to_sync_info()).into_response(),
        Err(error) => {
            tracing::error!("Error on GET /get_sync_info for {device_id}: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
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

/// oauth.yandex.ru/token and mobileproxy.passport.yandex.net/1/token: the
/// station posts application/x-www-form-urlencoded (TS accepts it and JSON)
async fn oauth_token(headers: axum::http::HeaderMap, body: axum::body::Bytes) -> impl IntoResponse {
    let is_json = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("json"));
    let form: OAuthForm = match if is_json {
        serde_json::from_slice(&body).map_err(|error| error.to_string())
    } else {
        serde_urlencoded::from_bytes(&body).map_err(|error| error.to_string())
    } {
        Ok(form) => form,
        Err(error) => {
            tracing::warn!("invalid oauth token request: {error}");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
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

/// The account of the station owner (TS mobileproxy.passport.yandex.net)
async fn passport_short_info() -> Json<Value> {
    // Math.round(Date.now() / 1000)
    let issued_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .saturating_add(500)
        / 1000;
    Json(json!({
        "avatar_url": "https://avatars.mds.yandex.net/get-yapic/53031/0s-5/islands-300",
        "birthday": "1984-01-01",
        "display_login": "YandexStationOwnerNo146",
        "display_name": "YandexStationOwnerNo146",
        "filter_params": { "subtype": ["portal"] },
        "firstname": "Owner",
        "gender": "m",
        "has_cards": true,
        "has_family": true,
        "has_password": true,
        "has_plus": true,
        "has_telemost": true,
        "have_webauthn_credential": true,
        "is_browser_account": true,
        "is_rfc_2fa_enabled": true,
        "lastname": "No146",
        "location_id": 0,
        "machine_readable_login": "YandexStationOwnerNo146",
        "master_members": [{
            "avatar_url": "https://avatars.mds.yandex.net/get-yapic/53031/0s-5/islands-300",
            "display_login": "YandexStationOwnerNo146",
            "has_cards": true,
            "has_family": true,
            "has_plus": true,
            "has_telemost": true,
            "is_browser_account": true,
            "plus_card": true,
            "primary_alias_type": 1,
            "public_name": "Owner No146",
            "secure_phone_number": "+78005553535",
            "uid": 10_146_228,
        }],
        "master_uid": 1_234_567_228,
        "native_default_email": "yandexowner@yandex.ru",
        "normalized_display_login": "YandexStationOwnerNo146",
        "partitions": ["default"],
        "plus_card": true,
        "primary_alias_type": 1,
        "public_id": "oj4bx3cwbowdyz4eb5lt1v0z7v",
        "public_name": "Owner No146",
        "secure_phone_number": "+78005553535",
        "status": "ok",
        "uid": 10_146_228,
        "x_token_client_id": "ef0e663c4b4d38debd41f76ed511e73f",
        "x_token_issued_at": u64::try_from(issued_at).unwrap_or_default(),
        "xtoken_has_muid": true,
    }))
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
