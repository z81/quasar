use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct Snapshot {
    pub source: String,
    pub devices: Vec<Device>,
    pub updated_at: Option<String>,
    pub stale: bool,
    pub error: Option<String>,
    pub grouping: String,
}

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct Device {
    pub id: String,
    pub name: String,
    pub area: Option<String>,
    pub entities: Vec<Entity>,
}

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct Entity {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub state: String,
    pub device_class: Option<String>,
    pub unit: Option<String>,
    pub category: Option<String>,
    pub readings: Vec<Reading>,
    #[serde(default)]
    pub controls: Vec<Control>,
}

impl Entity {
    pub fn unavailable(&self) -> bool {
        matches!(self.state.as_str(), "unavailable" | "unknown")
    }

    pub fn active(&self) -> bool {
        self.controls
            .iter()
            .find(|c| c.id == "power")
            .and_then(|c| c.value.as_bool())
            .unwrap_or_else(|| {
                matches!(
                    self.domain.as_str(),
                    "light"
                        | "switch"
                        | "climate"
                        | "fan"
                        | "humidifier"
                        | "media_player"
                        | "vacuum"
                ) && matches!(
                    self.state.as_str(),
                    "on" | "playing"
                        | "heat"
                        | "cool"
                        | "auto"
                        | "heat_cool"
                        | "dry"
                        | "fan_only"
                        | "cleaning"
                )
            })
    }
}

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct Reading {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub value: String,
}

#[derive(Clone, Deserialize, PartialEq)]
pub(crate) struct Control {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub value: Value,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub unit: Option<String>,
    #[serde(default)]
    pub options: Vec<Choice>,
}

#[derive(Clone, Deserialize, PartialEq)]
pub(crate) struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Clone, PartialEq)]
pub(super) struct ActionRequest {
    pub source: String,
    pub entity_id: String,
    pub control: String,
    pub value: Value,
}

#[derive(Clone, PartialEq)]
pub(super) struct Pending {
    pub request: ActionRequest,
    pub started: f64,
    pub accepted: bool,
}

pub(super) fn entity_key(source: &str, id: &str) -> String {
    format!("{source}\0{id}")
}

pub(super) fn same_value(control: &Control, desired: &Value) -> bool {
    match (control.value.as_f64(), desired.as_f64()) {
        (Some(actual), Some(desired)) => {
            (actual - desired).abs() <= control.step.unwrap_or(0.01).abs() / 2.0 + 0.000001
        }
        _ => control.value == *desired,
    }
}
