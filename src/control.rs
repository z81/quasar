//! Persistent command results and saved commands for the management UI.
use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::storage::Storage;

const HISTORY_LIMIT: usize = 1000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    pub status: String,
    pub result: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub id: String,
    pub created_at: String,
    pub duid: String,
    pub station: String,
    pub label: String,
    pub text: String,
    pub path: String,
    pub body: Value,
    pub status: String,
    pub response: Option<String>,
    pub actions: Vec<Action>,
    pub error: Option<String>,
    pub delivered: usize,
}

pub struct Control {
    pub storage: Arc<Storage>,
    records: Mutex<HashMap<String, CommandRecord>>,
    favorites_lock: Mutex<()>,
}

impl Control {
    pub async fn load(storage: Arc<Storage>) -> Result<Arc<Self>> {
        let mut records = HashMap::new();
        for value in storage
            .list_control_values("command:", HISTORY_LIMIT as i64)
            .await?
        {
            let mut record: CommandRecord = serde_json::from_value(value)?;
            if matches!(record.status.as_str(), "queued" | "running") {
                record.status = "failed".into();
                record.error = Some(
                    "Обработка прервана перезапуском сервера. Перед повтором проверьте колонку."
                        .into(),
                );
                storage
                    .set_control_value(
                        &format!("command:{}", record.id),
                        &serde_json::to_value(&record)?,
                    )
                    .await?;
            }
            records.insert(record.id.clone(), record);
        }
        Ok(Arc::new(Self {
            storage,
            records: Mutex::new(records),
            favorites_lock: Mutex::new(()),
        }))
    }

    pub async fn create(
        self: &Arc<Self>,
        duid: &str,
        station: &str,
        action: &str,
        body: Value,
    ) -> Result<CommandFeedback> {
        let id = Uuid::new_v4().to_string();
        let label = match action {
            "push-text" => "Команда",
            "push-raw" => "Озвучить",
            "push" => "Событие",
            "player" => "Плеер",
            _ => "Директива",
        };
        let text = body["eventText"]
            .as_str()
            .or(body["Name"].as_str())
            .or(body["action"].as_str())
            .unwrap_or(label)
            .to_owned();
        let record = CommandRecord {
            id: id.clone(),
            created_at: Utc::now().to_rfc3339(),
            duid: duid.into(),
            station: station.into(),
            label: label.into(),
            text,
            path: format!("/devices/{duid}/{action}"),
            body,
            status: "queued".into(),
            response: None,
            actions: vec![],
            error: None,
            delivered: 0,
        };
        let mut records = self.records.lock().await;
        anyhow::ensure!(
            records
                .values()
                .filter(|record| matches!(record.status.as_str(), "queued" | "running"))
                .count()
                < 128,
            "Too many commands in progress"
        );
        self.storage
            .set_control_value(&format!("command:{id}"), &serde_json::to_value(&record)?)
            .await?;
        records.insert(id.clone(), record);
        if records.len() > HISTORY_LIMIT {
            let oldest = records
                .values()
                .filter(|record| !matches!(record.status.as_str(), "queued" | "running"))
                .min_by_key(|record| &record.created_at)
                .map(|record| record.id.clone());
            if let Some(oldest) = oldest {
                self.storage
                    .delete_control_value(&format!("command:{oldest}"))
                    .await?;
                records.remove(&oldest);
            }
        }
        drop(records);
        let feedback = CommandFeedback {
            control: self.clone(),
            id,
            duid: duid.to_owned(),
        };
        let timeout = feedback.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(180)).await;
            timeout.update(|record| {
                if matches!(record.status.as_str(), "queued" | "running") {
                    record.status = "failed".into();
                    record.error = Some("Сервер не получил результат за 3 минуты. Проверьте колонку перед повтором.".into());
                }
            }).await;
        });
        Ok(feedback)
    }
}

#[derive(Clone)]
pub struct CommandFeedback {
    pub id: String,
    pub duid: String,
    control: Arc<Control>,
}

impl std::fmt::Debug for CommandFeedback {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandFeedback")
            .field("id", &self.id)
            .field("duid", &self.duid)
            .finish()
    }
}

impl CommandFeedback {
    async fn update(&self, change: impl FnOnce(&mut CommandRecord)) {
        let mut records = self.control.records.lock().await;
        let Some(record) = records.get_mut(&self.id) else {
            return;
        };
        change(record);
        if let Err(error) = self
            .control
            .storage
            .set_control_value(&format!("command:{}", self.id), &json!(record))
            .await
        {
            tracing::error!("cannot persist management command result: {error}");
        }
    }
    pub async fn delivered(&self, count: usize) {
        self.update(|record| record.delivered = count).await;
    }
    pub async fn running(&self) {
        self.update(|record| {
            if record.status == "queued" {
                record.status = "running".into();
            }
        })
        .await;
    }
    pub async fn answer(&self, text: &str) {
        self.update(|record| {
            let response = record.response.get_or_insert_with(String::new);
            if !response.is_empty() && !text.is_empty() {
                response.push('\n');
            }
            response.extend(
                text.chars()
                    .take(32_000usize.saturating_sub(response.chars().count())),
            );
        })
        .await;
    }
    pub async fn completed(&self) {
        self.update(|record| {
            if record.error.is_none() {
                record.status = "completed".into();
            }
        })
        .await;
    }
    pub async fn failed(&self, error: &str) {
        self.update(|record| {
            record.status = "failed".into();
            record.error = Some(error.chars().take(2000).collect());
        })
        .await;
    }
    async fn interrupted(&self) {
        self.update(|record| {
            if matches!(record.status.as_str(), "queued" | "running") {
                record.status = "failed".into();
                record.error = Some("Обработка прервана. Проверьте колонку перед повтором.".into());
            }
        })
        .await;
    }
    pub async fn action(&self, name: &str, status: &str, result: Option<&str>) {
        self.update(|record| {
            if record.actions.len() < 100 {
                record.actions.push(Action {
                    name: name.into(),
                    status: status.into(),
                    result: result.map(|text| text.chars().take(4000).collect()),
                });
            }
        })
        .await;
    }
}

/// A cancelled turn must not leave a command in the running state forever.
pub struct FeedbackGuard(CommandFeedback);
impl FeedbackGuard {
    pub fn new(feedback: &CommandFeedback) -> Self {
        Self(feedback.clone())
    }
}
impl Drop for FeedbackGuard {
    fn drop(&mut self) {
        let feedback = self.0.clone();
        tokio::spawn(async move {
            feedback.interrupted().await;
        });
    }
}

pub fn router(control: Arc<Control>) -> Router {
    Router::new()
        .route("/control/status", get(status))
        .route("/control/history", get(history).delete(clear_history))
        .route("/control/favorites", get(favorites).post(save_favorite))
        .route(
            "/control/favorites/{id}",
            axum::routing::delete(delete_favorite),
        )
        .with_state(control)
}

async fn status(State(control): State<Arc<Control>>) -> Json<Value> {
    Json(json!({"persistent":control.storage.is_persistent()}))
}
async fn history(State(control): State<Arc<Control>>) -> Json<Vec<CommandRecord>> {
    let mut records: Vec<_> = control.records.lock().await.values().cloned().collect();
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Json(records)
}
async fn clear_history(State(control): State<Arc<Control>>) -> Response {
    let mut records = control.records.lock().await;
    match control.storage.clear_control_values("command:").await {
        Ok(()) => {
            records.clear();
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => unavailable(),
    }
}
async fn favorites(State(control): State<Arc<Control>>) -> Response {
    match control.storage.list_control_values("favorite:", 200).await {
        Ok(values) => Json(values).into_response(),
        Err(_) => unavailable(),
    }
}
#[derive(Deserialize)]
struct NewFavorite {
    label: String,
    duid: String,
    path: String,
    body: Value,
}
async fn save_favorite(
    State(control): State<Arc<Control>>,
    Json(favorite): Json<NewFavorite>,
) -> Response {
    let allowed = ["push", "push-raw", "push-text", "push-directive", "player"]
        .iter()
        .any(|action| favorite.path == format!("/devices/{}/{action}", favorite.duid));
    if !allowed
        || favorite.label.trim().is_empty()
        || favorite.label.chars().count() > 100
        || !favorite.body.is_object()
        || favorite.body.to_string().len() > 32_000
    {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "Проверьте название и команду",
        )
            .into_response();
    }
    let _guard = control.favorites_lock.lock().await;
    if control
        .storage
        .list_control_values("favorite:", 200)
        .await
        .map_or(true, |values| values.len() >= 200)
    {
        return (StatusCode::CONFLICT, "Лимит — 200 избранных команд").into_response();
    }
    let id = Uuid::new_v4().to_string();
    let value = json!({"id":id,"created_at":Utc::now().to_rfc3339(),"label":favorite.label.trim(),"duid":favorite.duid,"path":favorite.path,"body":favorite.body});
    match control
        .storage
        .set_control_value(&format!("favorite:{id}"), &value)
        .await
    {
        Ok(()) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(_) => unavailable(),
    }
}
async fn delete_favorite(State(control): State<Arc<Control>>, Path(id): Path<String>) -> Response {
    match control
        .storage
        .delete_control_value(&format!("favorite:{id}"))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => unavailable(),
    }
}
fn unavailable() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, "Хранилище недоступно").into_response()
}
