//! HA entity capabilities, not device-name heuristics. The same descriptors
//! render controls and validate every command against a fresh registry snapshot.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::registry::Registry;

#[derive(Clone, Serialize)]
pub struct Control {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub value: Value,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub unit: Option<String>,
    pub options: Vec<OptionValue>,
    #[serde(skip)]
    service: &'static str,
    #[serde(skip)]
    parameter: &'static str,
}

#[derive(Clone, Serialize)]
pub struct OptionValue {
    value: String,
    label: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub source: String,
    pub entity_id: String,
    pub control: String,
    pub value: Value,
}

#[derive(Debug)]
pub enum ActionError {
    Invalid(&'static str),
    Unavailable,
    Unconfirmed,
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| v.is_finite())
}

impl Control {
    fn new(
        id: &str,
        label: &str,
        kind: &'static str,
        value: Value,
        service: &'static str,
        parameter: &'static str,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            value,
            min: None,
            max: None,
            step: None,
            unit: None,
            options: Vec::new(),
            service,
            parameter,
        }
    }
}

struct Builder<'a> {
    domain: &'a str,
    attrs: &'a Value,
    services: &'a Value,
    controls: Vec<Control>,
}

impl Builder<'_> {
    fn has(&self, service: &str) -> bool {
        self.services[self.domain][service].is_object()
    }
    fn feature(&self, flag: u64) -> bool {
        self.attrs["supported_features"].as_u64().unwrap_or(0) & flag == flag
    }

    fn power(&mut self, state: &str, supported: bool) {
        if supported && self.has("turn_on") && self.has("turn_off") {
            let value = if matches!(state, "unavailable" | "unknown") {
                Value::Null
            } else {
                json!(state != "off")
            };
            self.controls.push(Control::new(
                "power",
                "Питание",
                "toggle",
                value,
                "turn_on",
                "",
            ));
        }
    }

    fn range(
        &mut self,
        id: &str,
        label: &str,
        current: Option<f64>,
        bounds: (Option<f64>, Option<f64>, Option<f64>),
        unit: Option<&str>,
        service: &'static str,
        parameter: &'static str,
    ) {
        let (Some(min), Some(max), Some(step)) = bounds else {
            return;
        };
        if !self.has(service)
            || !min.is_finite()
            || !max.is_finite()
            || !step.is_finite()
            || min >= max
            || step <= 0.0
        {
            return;
        }
        let mut control = Control::new(
            id,
            label,
            "range",
            current
                .filter(|v| v.is_finite() && *v >= min && *v <= max)
                .map_or(Value::Null, |v| json!(v)),
            service,
            parameter,
        );
        control.min = Some(min);
        control.max = Some(max);
        control.step = Some(step);
        control.unit = unit.map(str::to_owned);
        self.controls.push(control);
    }

    fn select(
        &mut self,
        id: &str,
        label: &str,
        current: Value,
        options: &Value,
        service: &'static str,
        parameter: &'static str,
    ) {
        if !self.has(service) {
            return;
        }
        let options: Vec<_> = options
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|v| {
                let value = v.as_str()?;
                Some(OptionValue {
                    value: value.into(),
                    label: option_label(value).into(),
                })
            })
            .collect();
        if options.is_empty() {
            return;
        }
        let mut control = Control::new(id, label, "select", current, service, parameter);
        control.options = options;
        self.controls.push(control);
    }
}

fn option_label(value: &str) -> &str {
    match value {
        "off" => "Выключен",
        "heat" => "Обогрев",
        "cool" => "Охлаждение",
        "heat_cool" => "Обогрев / охлаждение",
        "auto" | "Auto" => "Авто",
        "dry" => "Осушение",
        "fan_only" => "Вентиляция",
        "sleep" | "Sleep" => "Сон",
        "eco" => "Экономичный",
        "comfort" => "Комфорт",
        "away" => "Нет дома",
        "home" => "Дома",
        "boost" => "Усиленный",
        _ => value,
    }
}

pub fn for_entity(
    domain: &str,
    state: &Value,
    services: &Value,
    temperature_unit: Option<&str>,
) -> Vec<Control> {
    let attrs = &state["attributes"];
    let actual = state["state"].as_str().unwrap_or("unknown");
    let mut b = Builder {
        domain,
        attrs,
        services,
        controls: Vec::new(),
    };
    let percent = (Some(0.0), Some(100.0), Some(1.0));
    match domain {
        "light" => {
            b.power(actual, true);
            let modes = attrs["supported_color_modes"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            if modes.iter().any(|v| {
                matches!(
                    v.as_str(),
                    Some("brightness" | "color_temp" | "hs" | "xy" | "rgb" | "rgbw" | "rgbww")
                )
            }) {
                b.range(
                    "brightness",
                    "Яркость",
                    if actual == "off" {
                        Some(0.0)
                    } else {
                        number(&attrs["brightness"]).map(|v| (v * 100.0 / 255.0).round())
                    },
                    percent,
                    Some("%"),
                    "turn_on",
                    "brightness_pct",
                );
            }
            if modes.iter().any(|v| v == "color_temp") {
                b.range(
                    "color_temperature",
                    "Температура света",
                    number(&attrs["color_temp_kelvin"]),
                    (
                        number(&attrs["min_color_temp_kelvin"]),
                        number(&attrs["max_color_temp_kelvin"]),
                        Some(1.0),
                    ),
                    Some("K"),
                    "turn_on",
                    "color_temp_kelvin",
                );
            }
        }
        "switch" | "input_boolean" => b.power(actual, true),
        "climate" => {
            b.power(actual, b.feature(128 | 256));
            b.select(
                "mode",
                "Режим",
                json!(actual),
                &attrs["hvac_modes"],
                "set_hvac_mode",
                "hvac_mode",
            );
            if b.feature(1) {
                b.range(
                    "temperature",
                    "Температура",
                    number(&attrs["temperature"]),
                    (
                        number(&attrs["min_temp"]),
                        number(&attrs["max_temp"]),
                        number(&attrs["target_temp_step"]),
                    ),
                    temperature_unit,
                    "set_temperature",
                    "temperature",
                );
            }
            if b.feature(4) {
                b.range(
                    "humidity",
                    "Влажность",
                    number(&attrs["humidity"]),
                    (
                        number(&attrs["min_humidity"]),
                        number(&attrs["max_humidity"]),
                        Some(1.0),
                    ),
                    Some("%"),
                    "set_humidity",
                    "humidity",
                );
            }
            if b.feature(8) {
                b.select(
                    "fan_mode",
                    "Вентилятор",
                    attrs["fan_mode"].clone(),
                    &attrs["fan_modes"],
                    "set_fan_mode",
                    "fan_mode",
                );
            }
            if b.feature(16) {
                b.select(
                    "preset",
                    "Профиль",
                    attrs["preset_mode"].clone(),
                    &attrs["preset_modes"],
                    "set_preset_mode",
                    "preset_mode",
                );
            }
        }
        "fan" => {
            b.power(actual, b.feature(16 | 32));
            if b.feature(1) {
                b.range(
                    "speed",
                    "Скорость",
                    if actual == "off" {
                        Some(0.0)
                    } else {
                        number(&attrs["percentage"])
                    },
                    (
                        Some(0.0),
                        Some(100.0),
                        number(&attrs["percentage_step"]).or(Some(1.0)),
                    ),
                    Some("%"),
                    "set_percentage",
                    "percentage",
                );
            }
            if b.feature(8) {
                b.select(
                    "preset",
                    "Режим",
                    attrs["preset_mode"].clone(),
                    &attrs["preset_modes"],
                    "set_preset_mode",
                    "preset_mode",
                );
            }
        }
        "humidifier" => {
            b.power(actual, true);
            b.range(
                "humidity",
                "Влажность",
                number(&attrs["humidity"]),
                (
                    number(&attrs["min_humidity"]),
                    number(&attrs["max_humidity"]),
                    Some(1.0),
                ),
                Some("%"),
                "set_humidity",
                "humidity",
            );
            if b.feature(1) {
                b.select(
                    "mode",
                    "Режим",
                    attrs["mode"].clone(),
                    &attrs["available_modes"],
                    "set_mode",
                    "mode",
                );
            }
        }
        "media_player" => {
            b.power(actual, b.feature(128 | 256));
            if b.feature(4) {
                b.range(
                    "volume",
                    "Громкость",
                    number(&attrs["volume_level"]).map(|v| (v * 100.0).round()),
                    percent,
                    Some("%"),
                    "volume_set",
                    "volume_level",
                );
            }
        }
        "cover" => {
            if b.feature(4) {
                b.range(
                    "position",
                    "Открытие",
                    number(&attrs["current_position"]),
                    percent,
                    Some("%"),
                    "set_cover_position",
                    "position",
                );
            }
        }
        "number" | "input_number" => {
            b.range(
                "value",
                "Значение",
                number(&state["state"]),
                (
                    number(&attrs["min"]),
                    number(&attrs["max"]),
                    number(&attrs["step"]),
                ),
                attrs["unit_of_measurement"].as_str(),
                "set_value",
                "value",
            );
        }
        "select" | "input_select" => b.select(
            "option",
            "Значение",
            json!(actual),
            &attrs["options"],
            "select_option",
            "option",
        ),
        _ => {}
    }
    b.controls
}

/// Never accept service names, targets, or raw service_data from the browser.
pub fn prepare(registry: &Registry, action: &Action) -> Result<Value, ActionError> {
    use ActionError::Invalid;
    let devices = super::from_registry(registry, true);
    let entity = devices
        .iter()
        .flat_map(|d| &d.entities)
        .find(|e| e.id == action.entity_id)
        .ok_or(Invalid(
            "Устройство больше недоступно для Assist. Обновите список.",
        ))?;
    if matches!(entity.state.as_str(), "unavailable" | "unknown") {
        return Err(Invalid(
            "Устройство недоступно или ещё не передало состояние.",
        ));
    }
    let control = entity
        .controls
        .iter()
        .find(|c| c.id == action.control)
        .ok_or(Invalid("Этот параметр не поддерживается устройством."))?;
    let mut data = json!({});
    let mut service = control.service;
    match control.kind {
        "toggle" => {
            service = match action.value.as_bool() {
                Some(true) => "turn_on",
                Some(false) => "turn_off",
                None => return Err(Invalid("Для переключателя нужно значение true или false.")),
            };
        }
        "range" => {
            let value = action
                .value
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or(Invalid("Нужно числовое значение."))?;
            let (min, max, step) = (
                control.min.unwrap(),
                control.max.unwrap(),
                control.step.unwrap(),
            );
            if value < min || value > max {
                return Err(Invalid("Значение вне диапазона устройства."));
            }
            let steps = (value - min) / step;
            // The maximum is a valid endpoint even for fans whose percentage step is 100/n.
            if (value - max).abs() > 1e-7 && (steps - steps.round()).abs() > 1e-5 {
                return Err(Invalid("Значение не соответствует шагу устройства."));
            }
            let value = if control.id == "volume" {
                value / 100.0
            } else if control.id == "speed" {
                // HA's fan service takes an integer percentage, while its
                // advertised interval can be fractional (e.g. three speeds).
                value.round()
            } else {
                value
            };
            data[control.parameter] =
                if value.fract() == 0.0 && (i64::MIN as f64..i64::MAX as f64).contains(&value) {
                    json!(value as i64)
                } else {
                    json!(value)
                };
        }
        "select" => {
            let value = action
                .value
                .as_str()
                .ok_or(Invalid("Выберите значение из списка."))?;
            if !control.options.iter().any(|o| o.value == value) {
                return Err(Invalid("Такого режима нет у устройства."));
            }
            data[control.parameter] = json!(value);
        }
        _ => return Err(Invalid("Неизвестный тип управления.")),
    }
    Ok(
        json!({"type": "call_service", "domain": entity.domain, "service": service,
        "service_data": data, "target": {"entity_id": entity.id}}),
    )
}
