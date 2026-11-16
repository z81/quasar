//! Station preferences with per-field conflict detection and pre-change backups.
mod catalog;
mod firmware;

use crate::{api::ApiState, auth::AuthManager, connections::StationCommand, device_access::operations as ops};
use axum::{extract::{DefaultBodyLimit, Path, State}, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}, routing::get, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;
use catalog::Change;

struct Context { api: Arc<ApiState>, auth: Arc<AuthManager>, changes: Mutex<()> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update { group: String, changes: Vec<Change> }

pub(crate) fn router(api: Arc<ApiState>, auth: Arc<AuthManager>) -> Router {
    Router::new()
        .route("/devices/{duid}/preferences/{scope}", get(read).patch(update))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(Arc::new(Context { api, auth, changes: Mutex::new(()) }))
}

async fn station(context: &Context, duid: &str) -> Result<crate::stations::StationSummary, String> {
    context.api.storage.list().await.map_err(|_| "Хранилище недоступно")?.into_iter().find(|s| s.duid == duid).ok_or_else(|| "Колонка не найдена".into())
}
fn backup_prefix(duid: &str, scope: &str) -> String { format!("preferences-last:{duid}:{scope}:") }
async fn snapshot(context: &Context, duid: &str, scope: &str, config: &Value) -> Value {
    let fields = catalog::fields(scope).unwrap_or_default().iter().map(|f| f.view(config)).collect::<Vec<_>>();
    let backups = context.api.storage.list_control_values(&backup_prefix(duid, scope), 40).await.unwrap_or_default();
    json!({"fields":fields,"backups":backups,"persistent":context.api.storage.is_persistent(),"scope":scope})
}

async fn read(State(context): State<Arc<Context>>, Path((duid, scope)): Path<(String, String)>, headers: HeaderMap) -> Response {
    if let Err(r) = ops::authorize(&context.api, &context.auth, &duid, &headers, false).await { return r; }
    if let Err(e) = catalog::fields(&scope) { return ops::error(StatusCode::NOT_FOUND, e); }
    let station = match station(&context, &duid).await { Ok(s) => s, Err(e) => return ops::error(StatusCode::SERVICE_UNAVAILABLE, e) };
    if scope == "user" { return Json(snapshot(&context, &duid, &scope, &station.quasar_config).await).into_response(); }
    if station.platform != "cucumber" { return ops::error(StatusCode::CONFLICT, "Чтение системных настроек проверено только для Станции Миди"); }
    let target = match ops::resolve(&context.api, &duid, None).await { Ok(t) => t, Err(r) => return r };
    match firmware::read(&target).await {
        Ok((_, config)) => {
            let mut result = snapshot(&context, &duid, &scope, &config).await;
            result["info"] = firmware::info(&target).await;
            Json(result).into_response()
        }
        Err(e) => ops::error(StatusCode::BAD_GATEWAY, e),
    }
}

async fn update(State(context): State<Arc<Context>>, Path((duid, scope)): Path<(String, String)>, headers: HeaderMap, Json(input): Json<Update>) -> Response {
    if let Err(r) = ops::authorize(&context.api, &context.auth, &duid, &headers, true).await { return r; }
    if let Err(e) = catalog::fields(&scope) { return ops::error(StatusCode::NOT_FOUND, e); }
    let Ok(_guard) = context.changes.try_lock() else { return ops::error(StatusCode::CONFLICT, "Дождитесь завершения сохранения настроек"); };
    let station = match station(&context, &duid).await { Ok(s) => s, Err(e) => return ops::error(StatusCode::SERVICE_UNAVAILABLE, e) };
    let target = if scope == "firmware" {
        if station.platform != "cucumber" { return ops::error(StatusCode::CONFLICT, "Изменение системных настроек поддерживается только для Станции Миди"); }
        match ops::resolve(&context.api, &duid, None).await { Ok(t) => Some(t), Err(r) => return r }
    } else { None };
    let _device_guard = if let Some(target) = &target {
        match ops::exclusive(target).await { Ok(g) => Some(g), Err(e) => return ops::error(StatusCode::CONFLICT, e) }
    } else { None };
    let (bytes, config) = if let Some(target) = &target {
        match firmware::read(target).await { Ok(v) => v, Err(e) => return ops::error(StatusCode::BAD_GATEWAY, e) }
    } else { (Vec::new(), station.quasar_config) };
    let next = match catalog::apply(&config, &scope, &input.group, &input.changes) {
        Ok(v) => v, Err(e) => return ops::error(StatusCode::CONFLICT, e),
    };
    if next == config { return ops::error(StatusCode::UNPROCESSABLE_ENTITY, "Нет изменённых значений"); }
    if scope == "user" {
        let (parsed, issues) = crate::station_config::parse(Some(&next), &[]);
        if !issues.is_empty() || parsed != next { return ops::error(StatusCode::UNPROCESSABLE_ENTITY, "Настройка не поддерживается схемой сервера"); }
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    let fields = catalog::fields(&scope).unwrap();
    let undo = input.changes.iter().filter_map(|c| {
        let field = fields.iter().find(|f| f.id == c.id)?;
        Some(Change { id: c.id.clone(), previous: field.value(&next)?, value: field.value(&config)? })
    }).collect::<Vec<_>>();
    let backup_path = target.as_ref().map(|_| format!("{}/before-{id}.json", firmware::BACKUPS));
    let backup = json!({"id":id,"group":input.group,"created_at":chrono::Utc::now().to_rfc3339(),"undo":undo,"path":backup_path});
    // Record only allowlisted values. Full firmware backups stay on the device, mode 0600.
    if context.api.storage.set_control_value(&format!("preferences-history:{duid}:{id}"), &backup).await.is_err() {
        return ops::error(StatusCode::SERVICE_UNAVAILABLE, "Не удалось сохранить резервные значения; изменения не отправлены");
    }
    let mut warning = None;
    let message;
    if let Some(target) = &target {
        match firmware::write(target, &bytes, &next, &id).await {
            Ok(w) => warning = w,
            Err(e) => return ops::error(StatusCode::BAD_GATEWAY, e),
        }
        message = "Файл сохранён с резервной копией. Для применения системных параметров перезапустите Алису или колонку.";
    } else {
        match context.api.storage.compare_station_config(&duid, &config, &next).await {
            Ok(true) => {},
            Ok(false) => return ops::error(StatusCode::CONFLICT, "Конфигурация уже изменилась. Перечитайте значения и повторите сохранение."),
            Err(_) => return ops::error(StatusCode::SERVICE_UNAVAILABLE, "Не удалось сохранить настройки"),
        }
        let sent = context.api.registry.send(&duid, StationCommand::Directives { directives: vec![crate::api::raw_directive::push_update_config()] });
        message = if sent > 0 { "Сохранено. Колонке отправлено обновление настроек." } else { "Сохранено. Колонка получит настройки при следующем подключении." };
    }
    if context.api.storage.set_control_value(&format!("{}{}", backup_prefix(&duid, &scope), input.group), &backup).await.is_err() {
        warning = Some("Значения сохранены, но быстрый откат недоступен. Резервная копия создана перед изменением.".into());
    }
    let mut result = snapshot(&context, &duid, &scope, &next).await;
    result["message"] = json!(message);
    result["warning"] = json!(warning);
    result["backup_path"] = json!(backup_path);
    Json(result).into_response()
}
