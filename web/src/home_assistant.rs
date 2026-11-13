mod controls;
mod model;
mod presentation;

use std::collections::{BTreeMap, BTreeSet};

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::json;

use crate::{api, icons::Icon, state::ControlState};
use controls::EntityControls;
use model::{entity_key, same_value, ActionRequest, Device, Entity, Pending, Snapshot};
use presentation::{area, channel_name, facts, icon, kind, metric_label, value};

#[derive(Clone, Copy, PartialEq)]
struct Dashboard {
    snapshots: Signal<Option<Vec<Snapshot>>>,
    error: Signal<Option<String>>,
    generation: Signal<u64>,
    pending: Signal<BTreeMap<String, Pending>>,
    command_errors: Signal<BTreeMap<String, String>>,
}

async fn refresh(state: ControlState, mut dashboard: Dashboard) {
    let generation = *dashboard.generation.peek() + 1;
    dashboard.generation.set(generation);
    let result = api::get::<Vec<Snapshot>>(state, "/home-assistant").await;
    if *dashboard.generation.peek() != generation {
        return;
    }
    match result {
        Ok(snapshots) => {
            dashboard.snapshots.set(Some(snapshots));
            dashboard.error.set(None);
        }
        Err(message) => dashboard.error.set(Some(message)),
    }
    let outcomes: Vec<_> = {
        let data = dashboard.snapshots.peek();
        dashboard
            .pending
            .peek()
            .iter()
            .filter_map(|(key, pending)| {
                let source = data.as_ref().and_then(|sources| {
                    sources.iter().find(|s| s.source == pending.request.source)
                });
                let confirmed = pending.accepted
                    && dashboard.error.peek().is_none()
                    && source.is_some_and(|source| {
                        !source.stale
                            && source
                                .devices
                                .iter()
                                .flat_map(|d| &d.entities)
                                .find(|entity| {
                                    entity.id == pending.request.entity_id && !entity.unavailable()
                                })
                                .and_then(|entity| {
                                    entity
                                        .controls
                                        .iter()
                                        .find(|control| control.id == pending.request.control)
                                })
                                .is_some_and(|control| same_value(control, &pending.request.value))
                    });
                if confirmed {
                    Some((key.clone(), None))
                } else if pending.accepted && js_sys::Date::now() - pending.started >= 30_000.0 {
                    Some((
                        key.clone(),
                        Some(
                            "Не удалось дождаться подтверждения. Проверьте текущее состояние."
                                .to_owned(),
                        ),
                    ))
                } else {
                    None
                }
            })
            .collect()
    };
    for (key, error) in outcomes {
        dashboard.pending.write().remove(&key);
        if let Some(error) = error {
            dashboard.command_errors.write().insert(key, error);
        }
    }
}

#[component]
pub fn HomeAssistant(state: ControlState) -> Element {
    let dashboard = Dashboard {
        snapshots: use_signal(|| None::<Vec<Snapshot>>),
        error: use_signal(|| None::<String>),
        generation: use_signal(|| 0),
        pending: use_signal(BTreeMap::new),
        command_errors: use_signal(BTreeMap::new),
    };
    let snapshots = dashboard.snapshots;
    let error = dashboard.error;
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
                refresh(state, dashboard).await;
            }
            TimeoutFuture::new(5_000).await;
        }
    });
    let dispatch = EventHandler::new(move |request: ActionRequest| {
        let key = entity_key(&request.source, &request.entity_id);
        if !state.allowed() || dashboard.pending.peek().contains_key(&key) {
            return;
        }
        let mut dashboard = dashboard;
        dashboard.command_errors.write().remove(&key);
        dashboard.pending.write().insert(
            key.clone(),
            Pending {
                request: request.clone(),
                started: js_sys::Date::now(),
                accepted: false,
            },
        );
        spawn(async move {
            let payload = json!({"source": request.source, "entity_id": request.entity_id, "control": request.control, "value": request.value});
            match api::mutate(state, "POST", "/home-assistant/action", Some(&payload)).await {
                Ok(_) => {
                    if let Some(pending) = dashboard.pending.write().get_mut(&key) {
                        pending.accepted = true;
                        pending.started = js_sys::Date::now();
                    }
                }
                Err(message) => {
                    dashboard.pending.write().remove(&key);
                    dashboard.command_errors.write().insert(key, message);
                }
            }
            refresh(state, dashboard).await;
        });
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
                        DeviceCard { key: "{source.source}:{device.id}", device: (*device).clone(), stale: source.stale || error().is_some(), source: source.source.clone(), show_source: data.len() > 1, dashboard, dispatch }
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
fn DeviceCard(
    device: Device,
    stale: bool,
    source: String,
    show_source: bool,
    dashboard: Dashboard,
    dispatch: EventHandler<ActionRequest>,
) -> Element {
    let Some(primary) = device.entities.first() else {
        return rsx! {};
    };
    let channels: Vec<_> = device
        .entities
        .iter()
        .filter(|e| e.category.is_none() && !e.controls.is_empty())
        .cloned()
        .collect();
    let settings: Vec<_> = device
        .entities
        .iter()
        .filter(|e| e.category.as_deref() == Some("config") && !e.controls.is_empty())
        .cloned()
        .collect();
    let active = !stale
        && device
            .entities
            .iter()
            .any(|e| e.category.is_none() && e.active());
    let unavailable = device.entities.iter().all(Entity::unavailable);
    let metrics: Vec<_> = device
        .entities
        .iter()
        .filter(|e| {
            e.category.is_none()
                && e.unit.is_some()
                && !e.unavailable()
                && (channels.is_empty() || e.controls.is_empty())
        })
        .take(4)
        .collect();
    let main_value = value(primary);
    let readings = if channels.is_empty() {
        facts(primary)
    } else {
        Vec::new()
    };
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
                div { class: "ha-device-title", h3 { title: "{device.name}", "{device.name}" }
                    span { "{area(&device)}" if show_source { " · {source}" } }
                }
            }
            if !channels.is_empty() {
                div { class: "ha-channels",
                    for entity in &channels {
                        {let key = entity_key(&source, &entity.id); rsx! {
                            EntityControls { key: "{entity.id}", entity: entity.clone(), device_name: device.name.clone(), source: source.clone(), stale,
                                show_name: channels.len() > 1, pending: dashboard.pending.read().get(&key).cloned(), error: dashboard.command_errors.read().get(&key).cloned(), dispatch }
                        }}
                    }
                }
            } else if metrics.is_empty() {
                div { class: "ha-device-state", title: "{main_value}", "{main_value}" }
            }
            if !metrics.is_empty() || !readings.is_empty() {
                div { class: "ha-metrics",
                    for metric in metrics {
                        div { class: "ha-metric", title: "{metric.name}",
                            span { class: "ha-metric-label", Icon { name: icon(metric), size: 14 }
                                if device.entities.iter().filter(|e| e.category.is_none() && e.device_class == metric.device_class).count() > 1 {
                                    "{channel_name(&metric.name, &device.name)}"
                                } else { "{metric_label(metric)}" }
                            }
                            strong { "{value(metric)}" }
                        }
                    }
                    for reading in readings.iter().take(2) {
                        div { class: "ha-metric", span { class: "ha-metric-label", "{reading.name}" } strong { "{reading.value}" } }
                    }
                }
            }
            if !settings.is_empty() {
                details { class: "ha-details ha-settings",
                    summary { "Настройки" Icon { name: "settings", size: 14 } }
                    for entity in settings {
                        {let key = entity_key(&source, &entity.id); rsx! {
                            EntityControls { key: "{entity.id}", entity: entity.clone(), device_name: device.name.clone(), source: source.clone(), stale,
                                show_name: true, pending: dashboard.pending.read().get(&key).cloned(), error: dashboard.command_errors.read().get(&key).cloned(), dispatch }
                        }}
                    }
                }
            }
            details { class: "ha-details",
                summary { "Все параметры · {device.entities.len()}" Icon { name: "chevron", size: 14 } }
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
