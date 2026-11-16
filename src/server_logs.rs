//! Server diagnostics use the same owner access as server settings.
use std::sync::Arc;

use axum::{extract::Query, http::StatusCode, response::{IntoResponse, Response}, routing::get, Json, Router};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct QueryOptions {
    #[serde(default)]
    level: String,
    #[serde(default)]
    search: String,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize { 200 }

pub(crate) fn router(auth: Arc<crate::auth::AuthManager>) -> Router {
    Router::new().route("/logs/server", get(read))
        .layer(axum::middleware::from_fn_with_state(auth, crate::auth::require_settings_access))
}

async fn read(Query(query): Query<QueryOptions>) -> Response {
    if !(1..=1000).contains(&query.limit) || query.search.chars().count() > 128
        || !matches!(query.level.as_str(), "" | "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR") {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"Выберите уровень, от 1 до 1000 записей и поиск до 128 символов"}))).into_response();
    }
    Json(crate::logging::recent::snapshot(&query.level, &query.search, query.limit)).into_response()
}
