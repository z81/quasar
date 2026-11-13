use super::model::{Device, Entity, Reading};

pub(super) fn kind(domain: &str) -> &'static str {
    match domain {
        "light" => "Свет",
        "climate" | "fan" | "humidifier" | "water_heater" | "weather" => "Климат",
        "sensor" | "binary_sensor" => "Датчики",
        "switch" | "input_boolean" | "remote" => "Переключатели",
        "media_player" => "Медиа",
        "cover" | "lock" | "alarm_control_panel" | "camera" => "Дом и безопасность",
        "person" | "device_tracker" => "Присутствие",
        "scene" | "script" | "automation" => "Сценарии",
        _ => "Другое",
    }
}

pub(super) fn icon(entity: &Entity) -> &'static str {
    match entity.domain.as_str() {
        "light" => "bulb",
        "climate" | "water_heater" => "thermometer",
        "fan" => "wind",
        "humidifier" => "drop",
        "weather" => "cloud",
        "media_player" => "music",
        "person" | "device_tracker" => "user",
        "lock" => "lock",
        "cover" => "window",
        "switch" | "input_boolean" => "power",
        "sensor" | "binary_sensor" => match entity.device_class.as_deref() {
            Some("temperature") => "thermometer",
            Some("humidity" | "moisture") => "drop",
            Some("battery") => "battery",
            Some("occupancy" | "motion" | "presence") => "user",
            Some("power" | "energy" | "voltage" | "current") => "power",
            Some("door" | "window" | "opening") => "window",
            _ => "activity",
        },
        "scene" | "script" | "automation" => "spark",
        _ => "grid",
    }
}

pub(super) fn value(entity: &Entity) -> String {
    if entity.state == "unavailable" {
        return "Недоступно".into();
    }
    if entity.state == "unknown" {
        return "Нет данных".into();
    }
    if entity.domain == "climate" && entity.state != "off" {
        if let Some(action) = entity.readings.iter().find(|r| r.id == "hvac_action") {
            match action.value.as_str() {
                "heating" => return "Нагревает".into(),
                "cooling" => return "Охлаждает".into(),
                "drying" => return "Осушает".into(),
                "idle" => return "Ожидание".into(),
                _ => {}
            }
        }
    }
    if matches!(
        entity.domain.as_str(),
        "scene" | "script" | "button" | "input_button"
    ) && entity.state.contains('T')
    {
        let date = js_sys::Date::new(&entity.state.clone().into());
        if !date.get_time().is_nan() {
            let options = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&options, &"day".into(), &"numeric".into());
            let _ = js_sys::Reflect::set(&options, &"month".into(), &"short".into());
            return format!("Запуск: {}", date.to_locale_date_string("ru-RU", &options));
        }
    }
    if let Some(unit) = &entity.unit {
        let value = entity
            .state
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(numeric)
            .unwrap_or_else(|| entity.state.clone());
        return format!("{value} {unit}");
    }
    if entity.domain == "binary_sensor" && matches!(entity.state.as_str(), "on" | "off") {
        let on = entity.state == "on";
        return match entity.device_class.as_deref() {
            Some("door" | "window" | "opening" | "garage_door") => {
                if on {
                    "Открыто"
                } else {
                    "Закрыто"
                }
            }
            Some("motion" | "occupancy" | "presence") => {
                if on {
                    "Обнаружено"
                } else {
                    "Не обнаружено"
                }
            }
            Some("connectivity") => {
                if on {
                    "Подключено"
                } else {
                    "Отключено"
                }
            }
            Some("problem" | "safety" | "tamper" | "smoke" | "gas" | "moisture") => {
                if on {
                    "Тревога"
                } else {
                    "В норме"
                }
            }
            Some("battery") => {
                if on {
                    "Низкий заряд"
                } else {
                    "В норме"
                }
            }
            _ => {
                if on {
                    "Активно"
                } else {
                    "Неактивно"
                }
            }
        }
        .into();
    }
    let label = match entity.state.as_str() {
        "on" => "Включено",
        "off" => "Выключено",
        "home" => "Дома",
        "not_home" => "Вне дома",
        "playing" => "Воспроизведение",
        "paused" => "На паузе",
        "idle" => "Ожидание",
        "standby" => "Ожидание",
        "open" => "Открыто",
        "closed" => "Закрыто",
        "opening" => "Открывается",
        "closing" => "Закрывается",
        "locked" => "Закрыт",
        "unlocked" => "Открыт",
        "heat" | "heating" => "Обогрев",
        "cool" | "cooling" => "Охлаждение",
        "auto" => "Авто",
        "heat_cool" => "Обогрев / охлаждение",
        "dry" => "Осушение",
        "fan_only" => "Вентиляция",
        "cleaning" => "Уборка",
        "docked" => "На базе",
        "returning" => "Возвращается",
        "cloudy" => "Облачно",
        "sunny" => "Ясно",
        "partlycloudy" => "Переменная облачность",
        "rainy" => "Дождь",
        _ => &entity.state,
    };
    label.into()
}

pub(super) fn area(device: &Device) -> &str {
    device.area.as_deref().unwrap_or("Без комнаты")
}

pub(super) fn channel_name(name: &str, device: &str) -> String {
    name.strip_prefix(device)
        .map(|rest| rest.trim_start_matches([' ', '—', '-', ':']))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(name)
        .to_owned()
}

pub(super) fn metric_label(entity: &Entity) -> String {
    match entity.device_class.as_deref() {
        Some("temperature") => "Температура",
        Some("humidity") => "Влажность",
        Some("power") => "Мощность",
        Some("energy") => "Энергия",
        Some("battery") => "Заряд",
        Some("voltage") => "Напряжение",
        Some("current") => "Ток",
        Some("weight") => "Вес",
        Some("illuminance") => "Освещённость",
        Some("pm25") => "PM2.5",
        Some("carbon_dioxide") => "CO₂",
        _ => &entity.name,
    }
    .to_owned()
}

pub(super) fn numeric(value: f64) -> String {
    if value != 0.0 && value.abs() < 0.0001 {
        return value.to_string().replace('.', ",");
    }
    let precision = if value != 0.0 && value.abs() < 0.01 {
        4
    } else {
        2
    };
    format!("{value:.precision$}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .replace('.', ",")
}

pub(super) fn facts(entity: &Entity) -> Vec<Reading> {
    entity
        .readings
        .iter()
        .filter(|reading| {
            let control = match reading.id.as_str() {
                "brightness" => "brightness",
                "percentage" => "speed",
                "current_position" => "position",
                "temperature" if entity.domain == "climate" => "temperature",
                "humidity" if matches!(entity.domain.as_str(), "climate" | "humidifier") => {
                    "humidity"
                }
                "hvac_action" if entity.domain == "climate" => return false,
                _ => return true,
            };
            !entity.controls.iter().any(|c| c.id == control)
        })
        .cloned()
        .map(|mut reading| {
            if reading.id == "current_temperature" {
                reading.name = "Сейчас".into();
            }
            reading
        })
        .collect()
}
