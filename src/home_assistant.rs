//! Dashboard data derived from HA registries, with an Assist MCP snapshot fallback.
pub mod controls;
pub mod registry;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    pub source: String,
    pub devices: Vec<Device>,
    pub updated_at: Option<String>,
    pub stale: bool,
    pub error: Option<String>,
    pub grouping: String,
}

#[derive(Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub area: Option<String>,
    pub entities: Vec<Entity>,
}

#[derive(Clone, Serialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub state: String,
    pub device_class: Option<String>,
    pub unit: Option<String>,
    pub category: Option<String>,
    pub readings: Vec<Reading>,
    pub controls: Vec<controls::Control>,
}

#[derive(Clone, Serialize)]
pub struct Reading {
    pub id: String,
    pub name: String,
    pub value: String,
}

pub async fn dashboard(State(state): State<Arc<crate::api::ApiState>>) -> Json<Vec<Snapshot>> {
    Json(state.runtime.snapshot().mcp.dashboard().await)
}

pub async fn action(
    State(state): State<Arc<crate::api::ApiState>>,
    Json(action): Json<controls::Action>,
) -> Response {
    use controls::ActionError;
    match state.runtime.snapshot().mcp.dashboard_action(&action).await {
        Ok(()) => (StatusCode::ACCEPTED, Json(json!({"status": "accepted"}))).into_response(),
        Err(error) => {
            let (status, message) = match error {
                ActionError::Invalid(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),
                ActionError::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "Home Assistant недоступен. Команда не отправлена."),
                ActionError::Unconfirmed => (StatusCode::BAD_GATEWAY, "Не удалось подтвердить команду. Проверьте состояние устройства перед повтором."),
            };
            (status, Json(json!({"error": message}))).into_response()
        }
    }
}

fn string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn entity(
    id: String,
    name: String,
    domain: String,
    state: &Value,
    category: Option<String>,
) -> Entity {
    let attrs = &state["attributes"];
    let mut readings = Vec::new();
    let temperature_unit = string(&attrs["temperature_unit"])
        .or_else(|| string(&attrs["unit_of_measurement"]))
        .unwrap_or_default();
    for (key, label, unit) in [
        (
            "current_temperature",
            "Температура",
            temperature_unit.as_str(),
        ),
        (
            "temperature",
            if domain == "climate" {
                "Задано"
            } else {
                "Температура"
            },
            temperature_unit.as_str(),
        ),
        ("humidity", "Влажность", "%"),
        ("current_humidity", "Влажность", "%"),
        ("percentage", "Скорость", "%"),
        ("current_position", "Открытие", "%"),
        ("media_title", "Трек", ""),
        ("media_artist", "Исполнитель", ""),
        ("hvac_action", "Режим", ""),
    ] {
        if let Some(value) = string(&attrs[key]) {
            readings.push(Reading {
                id: key.into(),
                name: label.into(),
                value: format!("{value} {unit}").trim().into(),
            });
        }
    }
    if let Some(brightness) = attrs["brightness"]
        .as_f64()
        .filter(|n| (0.0..=255.0).contains(n))
    {
        readings.push(Reading {
            id: "brightness".into(),
            name: "Яркость".into(),
            value: format!("{} %", (brightness * 100.0 / 255.0).round()),
        });
    }
    Entity {
        id,
        name,
        domain,
        state: string(&state["state"]).unwrap_or_else(|| "unknown".into()),
        device_class: string(&attrs["device_class"]),
        unit: string(&attrs["unit_of_measurement"]),
        category,
        readings,
        controls: Vec::new(),
    }
}

pub fn from_registry(registry: &registry::Registry, writable: bool) -> Vec<Device> {
    let areas: HashMap<_, _> = registry
        .areas
        .iter()
        .filter_map(|a| Some((a["area_id"].as_str()?, a["name"].as_str()?)))
        .collect();
    let devices: HashMap<_, _> = registry
        .devices
        .iter()
        .filter_map(|d| Some((d["id"].as_str()?, d)))
        .collect();
    let entries: HashMap<_, _> = registry
        .entities
        .iter()
        .filter_map(|e| Some((e["entity_id"].as_str()?, e)))
        .collect();
    let mut groups = BTreeMap::<String, Device>::new();
    for state in &registry.states {
        let Some(id) = state["entity_id"].as_str() else {
            continue;
        };
        if registry.exposed[id]["conversation"] != true {
            continue;
        }
        let entry = entries.get(id).copied().unwrap_or(&Value::Null);
        if !entry["disabled_by"].is_null() || !entry["hidden_by"].is_null() {
            continue;
        }
        let device_id = entry["device_id"].as_str();
        let device = device_id
            .and_then(|id| devices.get(id).copied())
            .unwrap_or(&Value::Null);
        if !device["disabled_by"].is_null() {
            continue;
        }
        let area_id = entry["area_id"]
            .as_str()
            .or_else(|| device["area_id"].as_str());
        let area = area_id
            .and_then(|id| areas.get(id))
            .map(|s| (*s).to_string());
        let name = string(&state["attributes"]["friendly_name"])
            .or_else(|| string(&entry["name"]))
            .unwrap_or_else(|| id.to_owned());
        let group_id = format!(
            "{}:{}",
            device_id.unwrap_or(id),
            area_id.unwrap_or_default()
        );
        let group = groups.entry(group_id.clone()).or_insert_with(|| Device {
            id: group_id,
            name: string(&device["name_by_user"])
                .or_else(|| string(&device["name"]))
                .unwrap_or_else(|| name.clone()),
            area,
            entities: Vec::new(),
        });
        let domain = id.split('.').next().unwrap_or_default();
        let mut display_state = state.clone();
        if matches!(domain, "climate" | "water_heater")
            && display_state["attributes"]["temperature_unit"].is_null()
        {
            if let Some(unit) = &registry.temperature_unit {
                display_state["attributes"]["temperature_unit"] = json!(unit);
            }
        }
        let mut view = entity(
            id.into(),
            name,
            domain.into(),
            &display_state,
            string(&entry["entity_category"]),
        );
        if writable && view.category.as_deref() != Some("diagnostic") {
            view.controls = controls::for_entity(
                domain,
                state,
                &registry.services,
                registry.temperature_unit.as_deref(),
            );
        }
        group.entities.push(view);
    }
    sorted(groups.into_values().collect())
}

/// HA returns a text block containing JSON {result: "Live Context: …\n<YAML>"}.
/// Never use the LLM's truncated result or infer device relationships from names.
pub fn from_context(result: &Value) -> Result<Vec<Device>> {
    ensure!(result["isError"] != true, "MCP context failed");
    let structured = result.get("structuredContent").cloned();
    let text = result["content"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let wrapper = structured.or_else(|| serde_json::from_str::<Value>(&text).ok());
    if let Some(wrapper) = &wrapper {
        ensure!(wrapper["success"] != false, "context rejected");
    }
    let text = wrapper
        .as_ref()
        .and_then(|v| v["result"].as_str())
        .unwrap_or(&text);
    ensure!(text.len() <= 2_000_000, "context too large");
    let text = text.trim();
    let yaml = if text.starts_with("Live Context:") {
        text.split_once('\n').map(|(_, yaml)| yaml).unwrap_or("[]")
    } else {
        text
    };
    let values: Value = serde_yaml::from_str(yaml).context("unsupported context format")?;
    let values = values.as_array().context("context is not an entity list")?;
    let mut occurrences = HashMap::<String, usize>::new();
    let mut devices = Vec::new();
    for value in values {
        let name = string(&value["names"])
            .or_else(|| string(&value["name"]))
            .context("missing entity name")?;
        let domain = string(&value["domain"]).context("missing entity domain")?;
        let area = string(&value["areas"]);
        let identity = format!("{name}\0{domain}\0{}", area.as_deref().unwrap_or_default());
        let occurrence = occurrences.entry(identity.clone()).or_default();
        *occurrence += 1;
        let id = format!("{:x}-{}", Sha256::digest(identity.as_bytes()), occurrence);
        devices.push(Device {
            id: id.clone(),
            name: name.clone(),
            area,
            entities: vec![entity(id, name, domain, value, None)],
        });
    }
    Ok(sorted(devices))
}

fn sorted(mut devices: Vec<Device>) -> Vec<Device> {
    for device in &mut devices {
        device.entities.sort_by(|a, b| {
            priority(a)
                .cmp(&priority(b))
                .then(a.name.cmp(&b.name))
                .then(a.id.cmp(&b.id))
        });
    }
    devices.sort_by(|a, b| {
        let offline = |device: &Device| {
            device
                .entities
                .iter()
                .all(|e| matches!(e.state.as_str(), "unavailable" | "unknown"))
        };
        offline(a)
            .cmp(&offline(b))
            .then(
                a.area
                    .is_none()
                    .cmp(&b.area.is_none())
                    .then(a.area.cmp(&b.area)),
            )
            .then(
                a.entities
                    .first()
                    .map(priority)
                    .cmp(&b.entities.first().map(priority)),
            )
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then(a.id.cmp(&b.id))
    });
    devices
}

fn priority(entity: &Entity) -> (bool, u8, u8) {
    (
        entity.category.is_some(),
        match entity.domain.as_str() {
            "light" => 0,
            "climate" | "fan" | "humidifier" => 1,
            "cover" | "lock" | "vacuum" => 2,
            "switch" | "input_boolean" => 3,
            "media_player" => 4,
            "binary_sensor" => 5,
            "sensor" => 6,
            _ => 7,
        },
        match entity.device_class.as_deref() {
            Some(
                "temperature" | "humidity" | "weight" | "power" | "energy" | "occupancy" | "motion",
            ) => 0,
            Some("battery" | "voltage" | "current") => 2,
            _ => 1,
        },
    )
}
