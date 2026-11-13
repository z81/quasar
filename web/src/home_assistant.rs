use std::collections::BTreeSet;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Deserialize, PartialEq)]
struct Snapshot {
    source: String,
    devices: Vec<Device>,
    updated_at: Option<String>,
    stale: bool,
    error: Option<String>,
    grouping: String,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Device {
    id: String,
    name: String,
    area: Option<String>,
    entities: Vec<Entity>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Entity {
    id: String,
    name: String,
    domain: String,
    state: String,
    device_class: Option<String>,
    unit: Option<String>,
    category: Option<String>,
    readings: Vec<Reading>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Reading {
    name: String,
    value: String,
}

fn kind(domain: &str) -> &'static str {
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

fn icon(entity: &Entity) -> &'static str {
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

fn value(entity: &Entity) -> String {
    if entity.state == "unavailable" {
        return "Недоступно".into();
    }
    if entity.state == "unknown" {
        return "Нет данных".into();
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
        return format!("{} {unit}", entity.state);
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

fn area(device: &Device) -> &str {
    device.area.as_deref().unwrap_or("Без комнаты")
}

#[component]
pub fn HomeAssistant(state: ControlState) -> Element {
    let mut snapshots = use_signal(|| None::<Vec<Snapshot>>);
    let mut error = use_signal(|| None::<String>);
    let mut room = use_signal(|| None::<String>);
    let mut category = use_signal(String::new);
    let mut search = use_signal(String::new);
    let mut expanded = use_signal(|| false);
    use_future(move || async move {
        loop {
            if state.allowed()
                && !document::eval("return document.hidden")
                    .join::<bool>()
                    .await
                    .unwrap_or(false)
            {
                match api::get::<Vec<Snapshot>>(state, "/home-assistant").await {
                    Ok(data) => {
                        snapshots.set(Some(data));
                        error.set(None);
                    }
                    Err(message) => error.set(Some(message)),
                }
            }
            TimeoutFuture::new(15_000).await;
        }
    });
    let data = snapshots().unwrap_or_default();
    if snapshots().is_some() && data.is_empty() && error().is_none() {
        return rsx! {};
    }
    let devices: Vec<_> = data
        .iter()
        .flat_map(|s| s.devices.iter().map(move |d| (s, d)))
        .collect();
    let rooms: BTreeSet<_> = devices.iter().map(|(_, d)| area(d).to_owned()).collect();
    let categories: BTreeSet<_> = devices
        .iter()
        .flat_map(|(_, d)| {
            d.entities
                .iter()
                .filter(|e| e.category.is_none())
                .map(|e| kind(&e.domain))
        })
        .collect();
    let chosen_room = room().filter(|r| rooms.contains(r));
    let chosen_category = category();
    let query = search().trim().to_lowercase();
    let filtered: Vec<_> = devices
        .iter()
        .filter(|(_, d)| {
            chosen_room.as_ref().is_none_or(|r| r == area(d))
                && (chosen_category.is_empty()
                    || !categories.contains(chosen_category.as_str())
                    || d.entities
                        .iter()
                        .any(|e| e.category.is_none() && kind(&e.domain) == chosen_category))
                && (query.is_empty()
                    || d.name.to_lowercase().contains(&query)
                    || area(d).to_lowercase().contains(&query)
                    || d.entities.iter().any(|e| {
                        e.name.to_lowercase().contains(&query)
                            || e.id.to_lowercase().contains(&query)
                    }))
        })
        .collect();
    let visible = if expanded() { filtered.len() } else { 8 };
    let stale = data.iter().any(|s| s.stale) || error().is_some();
    rsx! {
        document::Stylesheet { href: asset!("/assets/home-assistant.css") }
        section { class: "ha-dashboard", aria_label: "Умный дом Home Assistant",
            div { class: "ha-heading",
                div { class: "ha-title", Icon { name: "home", size: 22 } h2 { "Умный дом" }
                    span { class: "ha-total", "{devices.len()}" }
                    if stale { span { class: "ha-stale", "Данные устарели" } }
                }
                span { class: "ha-source", "Home Assistant" }
            }
            if let Some(message) = error() { p { class: "ha-error", role: "status", "{message}" } }
            for source in &data {
                if let Some(message) = &source.error { p { class: "ha-error", role: "status", "{source.source}: {message}" } }
                if source.grouping == "context" { p { class: "ha-note", "Показаны отдельные сущности. Реестр устройств недоступен." } }
            }
            if snapshots().is_none() && error().is_none() { p { class: "ha-note", role: "status", "Загружаем устройства…" } }
            if !devices.is_empty() {
                div { class: "ha-toolbar",
                    label { class: "ha-search", Icon { name: "search", size: 18 }
                        input { r#type: "search", aria_label: "Поиск устройств Home Assistant", placeholder: "Найти устройство", value: search,
                            oninput: move |e| { search.set(e.value()); expanded.set(false); }
                        }
                    }
                    select { class: "ha-category", aria_label: "Тип устройств", value: chosen_category,
                        onchange: move |e| { category.set(e.value()); expanded.set(false); },
                        option { value: "", "Все типы" }
                        for category in &categories { option { value: "{category}", "{category}" } }
                    }
                }
                div { class: "ha-rooms", role: "group", aria_label: "Комнаты",
                    button { r#type: "button", class: if chosen_room.is_none() { "ha-room selected" } else { "ha-room" }, aria_pressed: chosen_room.is_none(),
                        onclick: move |_| { room.set(None); expanded.set(false); }, "Все" }
                    for name in &rooms {
                        {let target = name.clone(); rsx! {
                            button { key: "{name}", r#type: "button", class: if chosen_room.as_ref() == Some(name) { "ha-room selected" } else { "ha-room" }, aria_pressed: chosen_room.as_ref() == Some(name),
                                onclick: move |_| { room.set(Some(target.clone())); expanded.set(false); }, "{name}" }
                        }}
                    }
                }
                if filtered.is_empty() { p { class: "ha-note", "Ничего не найдено" } }
                div { class: "ha-grid",
                    for (source, device) in filtered.iter().take(visible) {
                        DeviceCard { key: "{source.source}:{device.id}", device: (*device).clone(), stale: source.stale || error().is_some(), source: if data.len() > 1 { Some(source.source.clone()) } else { None } }
                    }
                }
                if filtered.len() > 8 {
                    button { r#type: "button", class: "ha-show-more", onclick: move |_| expanded.set(!expanded()),
                        if expanded() { "Свернуть" } else { "Показать все · {filtered.len()}" }
                    }
                }
            } else if snapshots().is_some() && !stale {
                p { class: "ha-note", "Нет доступных устройств. Откройте нужные сущности для Assist в Home Assistant." }
            }
        }
    }
}

#[component]
fn DeviceCard(device: Device, stale: bool, source: Option<String>) -> Element {
    let Some(primary) = device.entities.first() else {
        return rsx! {};
    };
    let active = !stale
        && device.entities.iter().any(|entity| {
            entity.category.is_none()
                && matches!(
                    entity.domain.as_str(),
                    "light" | "switch" | "climate" | "fan" | "media_player" | "vacuum"
                )
                && matches!(
                    entity.state.as_str(),
                    "on" | "playing" | "heat" | "cool" | "cleaning"
                )
        });
    let unavailable = device.entities.iter().all(|e| e.state == "unavailable");
    let channels: Vec<_> = device
        .entities
        .iter()
        .filter(|e| e.category.is_none() && e.domain == primary.domain)
        .collect();
    let state_value = if matches!(primary.domain.as_str(), "light" | "switch")
        && channels.len() > 1
        && channels
            .iter()
            .all(|e| matches!(e.state.as_str(), "on" | "off"))
    {
        format!(
            "{} из {} включено",
            channels.iter().filter(|e| e.state == "on").count(),
            channels.len()
        )
    } else {
        value(primary)
    };
    let metrics: Vec<_> = device
        .entities
        .iter()
        .filter(|e| {
            e.id != primary.id
                && e.category.is_none()
                && e.unit.is_some()
                && !matches!(e.state.as_str(), "unavailable" | "unknown")
        })
        .take(3)
        .collect();
    let normal: Vec<_> = device
        .entities
        .iter()
        .filter(|e| e.category.is_none())
        .cloned()
        .collect();
    let diagnostic: Vec<_> = device
        .entities
        .iter()
        .filter(|e| e.category.is_some())
        .cloned()
        .collect();
    rsx! {
        article { class: if active { "ha-device is-active" } else if unavailable { "ha-device is-unavailable" } else { "ha-device" },
            div { class: "ha-device-heading",
                span { class: "ha-device-icon", Icon { name: icon(primary), size: 22 } }
                div { class: "ha-device-title", h3 { "{device.name}" }
                    span { "{area(&device)}" if let Some(source) = source { " · {source}" } }
                }
            }
            div { class: "ha-device-state", title: "{state_value}", "{state_value}" }
            div { class: "ha-readings",
                for metric in metrics { span { title: "{metric.name}", Icon { name: icon(metric), size: 14 } "{value(metric)}" } }
                for reading in primary.readings.iter().take(2) { span { title: "{reading.name}", "{reading.value}" } }
            }
            details { class: "ha-details",
                summary { "Параметры · {device.entities.len()}" Icon { name: "chevron", size: 14 } }
                EntityList { entities: normal }
                if !diagnostic.is_empty() {
                    details { class: "ha-diagnostics", summary { "Служебные · {diagnostic.len()}" }
                        EntityList { entities: diagnostic }
                    }
                }
            }
        }
    }
}

#[component]
fn EntityList(entities: Vec<Entity>) -> Element {
    rsx! {
        dl { class: "ha-entities",
            for entity in entities { div { key: "{entity.id}", class: "ha-entity",
                dt { title: "{entity.id}", "{entity.name}" }
                dd { "{value(&entity)}" }
                for reading in &entity.readings { div { class: "ha-entity-reading", span { "{reading.name}" } span { "{reading.value}" } } }
            } }
        }
    }
}
