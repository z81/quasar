use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    pub enabled: bool,
    pub start: String,
    pub end: String,
    /// ISO weekday on which the interval starts, including overnight intervals.
    pub weekdays: Vec<u8>,
    pub allow_incoming_calls: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            enabled: false,
            start: "22:00".into(),
            end: "08:00".into(),
            weekdays: (1..=7).collect(),
            allow_incoming_calls: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManualOverride {
    pub enabled: bool,
    pub until: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct NativeMode {
    pub enabled: bool,
    pub allow_incoming_calls: bool,
}

impl NativeMode {
    pub fn from_config(config: &Value) -> Self {
        let mode = &config["deviceConfig"]["dndMode"];
        Self {
            enabled: mode["enabled"].as_bool().unwrap_or(false),
            allow_incoming_calls: mode["features"]["allowIncomingCalls"]
                .as_bool()
                .unwrap_or(false),
        }
    }

    pub fn value(&self) -> Value {
        json!({"enabled": self.enabled, "features": {"allowIncomingCalls": self.allow_incoming_calls}})
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Document {
    pub schedule: Schedule,
    pub baseline: NativeMode,
    pub manual_override: Option<ManualOverride>,
    /// A disabled schedule relinquishes control after its final manual value is
    /// confirmed. Subsequent direct/native manual changes remain independent.
    pub manual_pending: bool,
    pub station_timezone: Option<String>,
    pub updated_at: String,
    pub last_sent_at: Option<String>,
}

#[derive(Serialize)]
pub struct Status {
    pub schedule: Schedule,
    pub timezone: String,
    /// The native value saved in the server config, independently of confirmation.
    pub current_enabled: bool,
    pub scheduled_active: bool,
    pub next_transition_at: Option<String>,
    pub next_enabled: Option<bool>,
    pub manual_override: Option<ManualOverride>,
    pub connected: bool,
    pub persistent: bool,
    pub delivery: Delivery,
    pub server_time: String,
}

#[derive(Serialize)]
pub struct Delivery {
    pub status: &'static str,
    pub last_sent_at: Option<String>,
    pub confirmed_enabled: Option<bool>,
    pub confirmed_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DndError {
    #[error("{0}")]
    Invalid(String),
    #[error("Колонка не найдена")]
    NotFound,
    #[error("Хранилище временно недоступно")]
    Unavailable,
    #[error("Для расписания требуется постоянное хранилище")]
    NotPersistent,
}
