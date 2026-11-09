//! Owner-managed server scenarios and a separate, explicitly installed station alarm runtime.
mod engine;
mod model;
mod offline;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{atomic::AtomicBool, Arc},
};
use tokio::sync::Mutex;

use crate::{api::ApiState, auth::AuthManager, device_access::operations};
use model::{Document, Run};

struct AutomationState {
    api: Arc<ApiState>,
    auth: Arc<AuthManager>,
    edits: Mutex<()>,
    histories: Mutex<()>,
    running: Mutex<HashMap<String, (String, Arc<AtomicBool>)>>,
    devices: Mutex<()>,
}

pub(crate) fn router(api: Arc<ApiState>, auth: Arc<AuthManager>) -> Router {
    let state = Arc::new(AutomationState {
        api,
        auth,
        edits: Mutex::new(()),
        histories: Mutex::new(()),
        running: Mutex::new(HashMap::new()),
        devices: Mutex::new(()),
    });
    engine::start_scheduler(state.clone());
    Router::new()
        .route("/devices/{duid}/automations", get(read).put(save))
        .route("/devices/{duid}/automations/run", post(run))
        .route("/devices/{duid}/automations/stop", post(stop))
        .route(
            "/devices/{duid}/automations/offline",
            get(offline::status).post(offline::operate),
        )
        .with_state(state)
}

pub(crate) async fn stop_offline(target: &operations::Target) -> Result<(), String> {
    let script = r#"D=/data/quasar/alice-automations
[ -f "$D/control.sh" ] || exit 0
sh "$D/control.sh" shutdown || exit 2
i=0
while sh "$D/control.sh" status | grep -q '^running=1$'; do
    [ "$i" -lt 8 ] || { echo 'Локальный планировщик не остановился'; exit 3; }
    sleep 1; i=$((i+1))
done
"#;
    let (code, output) = operations::execute(target, script).await?;
    if code == 0 {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output).trim().to_owned())
    }
}

pub(crate) async fn uninstall_offline(target: &operations::Target) -> Result<(), String> {
    stop_offline(target).await?;
    let (code, output) = operations::execute(target, &offline::uninstall()).await?;
    if code == 0 {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output).trim().to_owned())
    }
}

impl AutomationState {
    async fn document(&self, duid: &str) -> Result<Document, String> {
        self.api
            .storage
            .get_control_value(&format!("automations:v1:{duid}"))
            .await
            .map_err(|e| e.to_string())?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| e.to_string())
            .map(Option::unwrap_or_default)
    }
    async fn history(&self, duid: &str) -> Result<Vec<Run>, String> {
        self.api
            .storage
            .get_control_value(&format!("automation-history:v1:{duid}"))
            .await
            .map_err(|e| e.to_string())?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| e.to_string())
            .map(Option::unwrap_or_default)
    }
    async fn record(&self, duid: &str, run: &Run) -> Result<(), String> {
        let _guard = self.histories.lock().await;
        let mut history = self.history(duid).await?;
        history.retain(|r| r.id != run.id);
        history.insert(0, run.clone());
        history.truncate(50);
        self.api
            .storage
            .set_control_value(&format!("automation-history:v1:{duid}"), &json!(history))
            .await
            .map_err(|e| e.to_string())
    }
    async fn authorize(
        &self,
        duid: &str,
        headers: &HeaderMap,
        mutation: bool,
    ) -> Result<(), Response> {
        operations::authorize(&self.api, &self.auth, duid, headers, mutation).await
    }
}

async fn read(
    State(state): State<Arc<AutomationState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = state.authorize(&duid, &headers, false).await {
        return r;
    }
    match (state.document(&duid).await, state.history(&duid).await) {
        (Ok(document), Ok(history)) => Json(json!({"document": document, "history": history, "persistent": state.api.storage.is_persistent(), "running": state.running.lock().await.contains_key(&duid)})).into_response(),
        _ => operations::error(StatusCode::SERVICE_UNAVAILABLE, "Не удалось прочитать сценарии"),
    }
}

async fn save(
    State(state): State<Arc<AutomationState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(mut document): Json<Document>,
) -> Response {
    if let Err(r) = state.authorize(&duid, &headers, true).await {
        return r;
    }
    if !state.api.storage.is_persistent() {
        return operations::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Для сценариев и сохранения локальных будильников требуется постоянное хранилище",
        );
    }
    if let Err(e) = document.validate() {
        return operations::error(StatusCode::UNPROCESSABLE_ENTITY, e);
    }
    let _guard = state.edits.lock().await;
    let current = match state.document(&duid).await {
        Ok(d) => d,
        Err(e) => return operations::error(StatusCode::SERVICE_UNAVAILABLE, e),
    };
    if current.revision != document.revision {
        return operations::error(
            StatusCode::CONFLICT,
            "Настройки изменены в другом окне. Обновите страницу перед сохранением",
        );
    }
    document.revision = current.revision.saturating_add(1);
    match state.api.storage.set_control_value(&format!("automations:v1:{duid}"), &json!(document)).await {
        Ok(()) => Json(json!({"document":document, "message":"Сохранено. Локальные будильники нужно отдельно передать на колонку."})).into_response(),
        Err(_) => operations::error(StatusCode::SERVICE_UNAVAILABLE, "Не удалось сохранить настройки"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    scenario_id: String,
}

async fn run(
    State(state): State<Arc<AutomationState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Start>,
) -> Response {
    if let Err(r) = state.authorize(&duid, &headers, true).await {
        return r;
    }
    match engine::launch(state, duid, input.scenario_id).await {
        Ok(id) => (StatusCode::ACCEPTED, Json(json!({"run_id":id}))).into_response(),
        Err(e) => operations::error(StatusCode::CONFLICT, e),
    }
}

async fn stop(
    State(state): State<Arc<AutomationState>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = state.authorize(&duid, &headers, true).await {
        return r;
    }
    if let Some((_, cancel)) = state.running.lock().await.get(&duid) {
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Json(json!({"message":"Запрошена остановка. Уже переданные колонке команды не отменяются."}))
        .into_response()
}
