//! Device health and Bluetooth management through the existing owner-only ADB transport.

mod bluetooth;
mod bluetooth_output;
mod probe;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::{
    api::ApiState,
    auth::AuthManager,
    device_access::operations::{self, Target},
};

const PREFIX: &str = "device-monitor:";
const MAX_SAMPLES: usize = 1440;

struct Tools {
    api: Arc<ApiState>,
    auth: Arc<AuthManager>,
    records: Mutex<HashMap<String, Arc<Mutex<Record>>>>,
    bluetooth_jobs: tokio::sync::Semaphore,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Record {
    duid: String,
    enabled: bool,
    target: Option<Target>,
    samples: VecDeque<probe::Sample>,
    events: VecDeque<Value>,
    last_error: Option<String>,
    #[serde(skip)]
    consecutive_failures: u32,
    #[serde(skip)]
    retry_at: i64,
}

impl Record {
    fn trim(&mut self) {
        let earliest = Utc::now().timestamp() - 86400;
        self.samples.retain(|sample| sample.at >= earliest);
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
        while self.events.len() > 100 {
            self.events.pop_front();
        }
        self.events
            .retain(|event| event["at"].as_i64().is_some_and(|at| at >= earliest));
    }

    fn event(&mut self, kind: &str, text: &str) {
        self.events
            .push_back(json!({"at":Utc::now().timestamp(),"kind":kind,"text":text}));
        self.trim();
    }

    fn sample(&mut self, sample: probe::Sample) {
        self.consecutive_failures = 0;
        self.retry_at = 0;
        if self.last_error.take().is_some() {
            self.event("connected", "Связь по ADB восстановлена");
        }
        if self
            .samples
            .back()
            .and_then(|sample| sample.uptime_seconds)
            .zip(sample.uptime_seconds)
            .is_some_and(|(previous, current)| current + 30.0 < previous)
        {
            self.event("restart", "Колонка перезагрузилась");
        }
        // Opening the page cannot grow the minute-resolution history without bound.
        if self
            .samples
            .back()
            .is_some_and(|last| last.at / 60 == sample.at / 60)
        {
            self.samples.pop_back();
        }
        self.samples.push_back(sample);
        self.trim();
    }
}

pub(crate) fn router(api: Arc<ApiState>, auth: Arc<AuthManager>) -> Router {
    let state = Arc::new(Tools {
        api,
        auth,
        records: Mutex::new(HashMap::new()),
        bluetooth_jobs: tokio::sync::Semaphore::new(1),
    });
    start_monitor(&state);
    Router::new()
        .route("/devices/{duid}/tools/status", get(status))
        .route("/devices/{duid}/tools/history", get(history))
        .route("/devices/{duid}/tools/monitor", axum::routing::put(monitor))
        .route("/devices/{duid}/tools/logs", get(logs))
        .route("/devices/{duid}/tools/live", get(live))
        .route(
            "/devices/{duid}/tools/bluetooth",
            get(bluetooth_status).post(bluetooth_action),
        )
        .with_state(state)
}

fn failure(message: impl Into<String>) -> Response {
    operations::error(StatusCode::BAD_GATEWAY, message)
}

impl Tools {
    async fn record(&self, duid: &str) -> Result<Arc<Mutex<Record>>, Response> {
        if let Some(record) = self.records.lock().await.get(duid).cloned() {
            return Ok(record);
        }
        let value = self
            .api
            .storage
            .get_control_value(&format!("{PREFIX}{duid}"))
            .await
            .map_err(|_| failure("Не удалось прочитать историю диагностики"))?;
        let mut record: Record = value
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        record.duid = duid.to_owned();
        record.trim();
        let mut records = self.records.lock().await;
        Ok(records
            .entry(duid.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(record)))
            .clone())
    }

    async fn persist(&self, record: &Record) -> Result<(), String> {
        self.api
            .storage
            .set_control_value(&format!("{PREFIX}{}", record.duid), &json!(record))
            .await
            .map_err(|_| "Не удалось сохранить историю диагностики".into())
    }

    async fn target(&self, duid: &str, query: &TargetQuery) -> Result<Target, Response> {
        let saved = self.record(duid).await?.lock().await.target.clone();
        operations::resolve(&self.api, duid, query.target().or(saved)).await
    }
}

#[derive(Default, Deserialize)]
struct TargetQuery {
    host: Option<String>,
    port: Option<u16>,
}

impl TargetQuery {
    fn target(&self) -> Option<Target> {
        self.host.as_ref().map(|host| Target {
            host: host.clone(),
            port: self.port.unwrap_or(5555),
        })
    }
}

async fn status(
    State(state): State<Arc<Tools>>,
    Path(duid): Path<String>,
    Query(query): Query<TargetQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) =
        operations::authorize(&state.api, &state.auth, &duid, &headers, false).await
    {
        return response;
    }
    let target = match state.target(&duid, &query).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let (sample, info) = match probe::collect(&target).await {
        Ok(result) => result,
        Err(error) => return failure(error),
    };
    let record = match state.record(&duid).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let mut record = record.lock().await;
    let mut storage_error = None;
    if record.enabled {
        record.sample(sample.clone());
        storage_error = state.persist(&record).await.err();
    }
    Json(json!({"sample":sample,"info":info,"monitoring":record.enabled,"persistent":state.api.storage.is_persistent(),"storage_error":storage_error})).into_response()
}

async fn history(
    State(state): State<Arc<Tools>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) =
        operations::authorize(&state.api, &state.auth, &duid, &headers, false).await
    {
        return response;
    }
    let record = match state.record(&duid).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let mut record = record.lock().await;
    record.trim();
    Json(json!({"enabled":record.enabled,"samples":record.samples,"events":record.events,"last_error":record.last_error,"interval_seconds":60,"retention_hours":24,"persistent":state.api.storage.is_persistent()})).into_response()
}

#[derive(Deserialize)]
struct MonitorRequest {
    enabled: bool,
    target: Option<Target>,
}

async fn monitor(
    State(state): State<Arc<Tools>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(body): Json<MonitorRequest>,
) -> Response {
    if let Err(response) =
        operations::authorize(&state.api, &state.auth, &duid, &headers, true).await
    {
        return response;
    }
    let query = body
        .target
        .map(|target| TargetQuery {
            host: Some(target.host),
            port: Some(target.port),
        })
        .unwrap_or_default();
    let target = match state.target(&duid, &query).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let record = match state.record(&duid).await {
        Ok(record) => record,
        Err(response) => return response,
    };
    let mut record = record.lock().await;
    let previous = record.clone();
    record.enabled = body.enabled;
    record.retry_at = 0;
    record.consecutive_failures = 0;
    record.target = Some(target);
    record.event(
        "monitor",
        if body.enabled {
            "Сбор локальной истории включён"
        } else {
            "Сбор истории выключен"
        },
    );
    if let Err(error) = state.persist(&record).await {
        *record = previous;
        return failure(error);
    }
    Json(json!({"enabled":record.enabled,"persistent":state.api.storage.is_persistent()}))
        .into_response()
}

fn start_monitor(state: &Arc<Tools>) {
    let weak = Arc::downgrade(state);
    tokio::spawn(async move {
        if let Some(state) = weak.upgrade() {
            if let Ok(values) = state.api.storage.list_control_values(PREFIX, 64).await {
                for value in values {
                    if let Ok(mut record) = serde_json::from_value::<Record>(value) {
                        if record.duid.is_empty() {
                            continue;
                        }
                        record.trim();
                        state
                            .records
                            .lock()
                            .await
                            .entry(record.duid.clone())
                            .or_insert_with(|| Arc::new(Mutex::new(record)));
                    }
                }
            }
        }
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let records = state
                .records
                .lock()
                .await
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for record in records {
                let mut record = record.lock().await;
                if !record.enabled || record.retry_at > Utc::now().timestamp() {
                    continue;
                }
                // Resolving again catches a station removed from the saved device list.
                let target =
                    operations::resolve(&state.api, &record.duid, record.target.clone()).await;
                let result = match target {
                    Ok(target) => probe::collect(&target).await,
                    Err(_) => Err("Адрес станции больше недоступен".into()),
                };
                match result {
                    Ok((sample, _)) => record.sample(sample),
                    Err(error) => {
                        if record.last_error.is_none() {
                            record.event("disconnected", "Нет связи по ADB");
                        }
                        record.last_error = Some(error);
                        record.consecutive_failures = record.consecutive_failures.saturating_add(1);
                        let delay =
                            (60_i64 * (1_i64 << record.consecutive_failures.min(3))).min(300);
                        record.retry_at = Utc::now().timestamp() + delay;
                        record.trim();
                    }
                }
                if state.persist(&record).await.is_err() {
                    tracing::warn!("device monitor history could not be saved");
                }
            }
        }
    });
}

#[derive(Deserialize)]
struct LogQuery {
    #[serde(flatten)]
    target: TargetQuery,
    #[serde(default = "default_source")]
    source: String,
    #[serde(default = "default_lines")]
    lines: usize,
    #[serde(default)]
    filter: String,
}
fn default_source() -> String {
    "dmesg".into()
}
fn default_lines() -> usize {
    200
}

async fn logs(
    State(state): State<Arc<Tools>>,
    Path(duid): Path<String>,
    Query(query): Query<LogQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) =
        operations::authorize(&state.api, &state.auth, &duid, &headers, false).await
    {
        return response;
    }
    if !(1..=1000).contains(&query.lines)
        || query.filter.len() > 128
        || !probe::valid_log_name(&query.source)
    {
        return operations::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "Выберите журнал, до 1000 строк и фильтр до 128 символов",
        );
    }
    let target = match state.target(&duid, &query.target).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let command = if query.source == "dmesg" {
        format!("dmesg | tail -n {}", query.lines)
    } else if query.source == "system" {
        format!("tail -n {} /var/log/messages", query.lines)
    } else {
        format!(
            "tail -n {} {}",
            query.lines,
            operations::quote(&format!("/data/quasar/daemons_logs/{}", query.source))
        )
    };
    let (code, output) = match operations::execute(&target, &command).await {
        Ok(result) => result,
        Err(error) => return failure(error),
    };
    if code != 0 {
        return failure("Журнал недоступен на этой прошивке");
    }
    let filter = query.filter.to_lowercase();
    let source = String::from_utf8_lossy(&output);
    let text = source
        .lines()
        .filter(|line| filter.is_empty() || line.to_lowercase().contains(&filter))
        .map(|line| {
            line.chars()
                .filter(|character| !character.is_control() || *character == '\t')
                .take(4096)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = text.len() > 256 * 1024;
    let mut text = text;
    if truncated {
        let mut end = 256 * 1024;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Json(json!({"source":query.source,"text":text,"truncated":truncated,"at":Utc::now().timestamp()})).into_response()
}

async fn live(
    State(state): State<Arc<Tools>>,
    Path(duid): Path<String>,
    Query(query): Query<TargetQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) =
        operations::authorize(&state.api, &state.auth, &duid, &headers, false).await
    {
        return response;
    }
    let target = match state.target(&duid, &query).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    match probe::live(&target).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => failure(error),
    }
}

async fn bluetooth_status(
    State(state): State<Arc<Tools>>,
    Path(duid): Path<String>,
    Query(query): Query<TargetQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) =
        operations::authorize(&state.api, &state.auth, &duid, &headers, false).await
    {
        return response;
    }
    let target = match state.target(&duid, &query).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    match bluetooth::status(&target).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
struct BluetoothRequest {
    action: String,
    mac: Option<String>,
    target: Option<Target>,
}

async fn bluetooth_action(
    State(state): State<Arc<Tools>>,
    Path(duid): Path<String>,
    headers: HeaderMap,
    Json(body): Json<BluetoothRequest>,
) -> Response {
    if let Err(response) =
        operations::authorize(&state.api, &state.auth, &duid, &headers, true).await
    {
        return response;
    }
    let query = body
        .target
        .map(|target| TargetQuery {
            host: Some(target.host),
            port: Some(target.port),
        })
        .unwrap_or_default();
    let target = match state.target(&duid, &query).await {
        Ok(target) => target,
        Err(response) => return response,
    };
    let Ok(_permit) = state.bluetooth_jobs.try_acquire() else {
        return operations::error(
            StatusCode::CONFLICT,
            "Дождитесь завершения текущего действия Bluetooth",
        );
    };
    let _device_guard = match operations::exclusive(&target).await {
        Ok(guard) => guard,
        Err(error) => return operations::error(StatusCode::CONFLICT, error),
    };
    match bluetooth::action(&target, &body.action, body.mac.as_deref()).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => failure(error),
    }
}

pub(crate) async fn restore_bluetooth(target: &Target) -> Result<(), String> {
    bluetooth_output::stop(target).await
}

/// Call under the caller's exclusive device guard before backup or managed-file restore.
pub(crate) async fn ensure_bluetooth_idle(target: &Target) -> Result<(), String> {
    bluetooth_output::ensure_idle(target).await
}
