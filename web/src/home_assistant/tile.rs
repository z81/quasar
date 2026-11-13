use dioxus::prelude::*;

use super::{controls::EntityControls, model::*, presentation::*, Dashboard};
use crate::icons::Icon;

// Independent channels get independent tiles. Related readings and configuration
// stay with their physical device and remain available in its details.
pub(super) fn tile_entities(device: &Device) -> Vec<&Entity> {
    let channels: Vec<_> = device
        .entities
        .iter()
        .filter(|e| {
            e.category.is_none()
                && (!e.controls.is_empty()
                    || matches!(
                        e.domain.as_str(),
                        "light"
                            | "switch"
                            | "input_boolean"
                            | "climate"
                            | "fan"
                            | "humidifier"
                            | "media_player"
                            | "cover"
                    ))
        })
        .collect();
    if channels.is_empty() {
        device
            .entities
            .iter()
            .find(|e| e.category.is_none())
            .or_else(|| device.entities.first())
            .into_iter()
            .collect()
    } else {
        channels
    }
}

#[component]
pub(super) fn DeviceTile(
    device: Device,
    entity: Entity,
    stale: bool,
    source: String,
    show_source: bool,
    dashboard: Dashboard,
    dispatch: EventHandler<ActionRequest>,
) -> Element {
    let key = entity_key(&source, &entity.id);
    let pending = dashboard.pending.read().get(&key).cloned();
    let error = dashboard.command_errors.read().get(&key).cloned();
    let level = entity.controls.iter().find(|c| c.kind == "range");
    let active = !stale && !entity.unavailable() && entity.active();
    let title = if entity.controls.is_empty() && tile_entities(&device).len() == 1 {
        device.name.clone()
    } else {
        entity.name.clone()
    };
    let companion = if entity.controls.is_empty() {
        device
            .entities
            .iter()
            .find(|e| e.id != entity.id && e.category.is_none() && e.unit.is_some())
            .map(|e| format!("{} · {}", metric_label(e), value(e)))
    } else {
        None
    };
    let popup = format!("ha-details-{source}-{}", entity.id);
    let heading = format!("{popup}-title");
    let room = area(&device).to_owned();
    let mut class = format!("ha-tile ha-domain-{}", entity.domain);
    if active {
        class.push_str(" is-active");
    }
    if stale || entity.unavailable() {
        class.push_str(" is-unavailable");
    }
    if level.is_some() {
        class.push_str(" has-dial");
    }
    let settings: Vec<_> = device
        .entities
        .iter()
        .filter(|e| {
            e.id != entity.id && e.category.as_deref() == Some("config") && !e.controls.is_empty()
        })
        .cloned()
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
        div { class: if level.is_some() { "ha-tile-item has-dial" } else { "ha-tile-item" },
            article { class, aria_label: "{title}", aria_busy: pending.is_some(),
                div { class: "ocean-device-heading", h3 { "{title}" } p { "{room}" } }
                if entity.controls.is_empty() {
                    div { class: "ha-tile-face", TileFace { entity: entity.clone(), title: title.clone(), stale, companion: companion.clone() } }
                } else {
                    EntityControls { entity: entity.clone(), device_name: device.name.clone(), source: source.clone(), stale,
                        show_name: false, pending: pending.clone(), error: None, dispatch }
                }
                button { r#type: "button", class: "ha-tile-more", aria_label: "Настройки — {title}", title: "Настройки", aria_haspopup: "dialog",
                    "popovertarget": popup.clone(), "popovertargetaction": "show",
                    svg { width: "24", height: "24", view_box: "0 0 24 24", fill: "currentColor", "aria-hidden": "true",
                        circle { cx: "5", cy: "12", r: "2" } circle { cx: "12", cy: "12", r: "2" } circle { cx: "19", cy: "12", r: "2" }
                    }
                }
            }
            if let Some(pending) = &pending { p { class: "ha-command-status", role: "status", span { class: "ha-spinner" }
                if pending.accepted { "Ожидаем устройство…" } else { "Отправляем…" }
            } }
            if let Some(error) = error { p { class: "ha-command-error", role: "alert", "{error}" } }
            div { id: popup.clone(), class: "ha-popup", "popover": "auto", role: "dialog", aria_labelledby: heading.clone(),
                div { class: "ha-popup-heading",
                    div { h3 { id: heading, "{title}" } p { "{room}" if show_source { " · {source}" } } }
                    button { r#type: "button", class: "ha-popup-close", aria_label: "Закрыть настройки", "popovertarget": popup, "popovertargetaction": "hide", Icon { name: "close", size: 22 } }
                }
                if !entity.controls.is_empty() {
                    EntityControls { entity: entity.clone(), device_name: device.name.clone(), source: source.clone(), stale,
                        show_name: false, pending: pending.clone(), error: None, dispatch }
                }
                if !settings.is_empty() {
                    div { class: "ha-popup-settings", h4 { "Настройки устройства" }
                        for setting in settings {
                            {let key = entity_key(&source, &setting.id); rsx! {
                                EntityControls { key: "{setting.id}", entity: setting, device_name: device.name.clone(), source: source.clone(), stale,
                                    show_name: true, pending: dashboard.pending.read().get(&key).cloned(), error: dashboard.command_errors.read().get(&key).cloned(), dispatch }
                            }}
                        }
                    }
                }
                details { class: "ha-details", summary { "Все параметры · {device.entities.len()}" Icon { name: "chevron", size: 14 } }
                    EntityList { entities: normal }
                    if !diagnostic.is_empty() { details { class: "ha-diagnostics", summary { "Служебные · {diagnostic.len()}" } EntityList { entities: diagnostic } } }
                }
            }
        }
    }
}

#[component]
fn TileFace(entity: Entity, title: String, stale: bool, companion: Option<String>) -> Element {
    let readings = facts(&entity);
    rsx! {
        span { class: "ha-tile-icon",
            if entity.domain == "light" {
                svg { width: "34", height: "34", view_box: "0 0 24 24", fill: "currentColor", "aria-hidden": "true",
                    path { d: "M12 2a7 7 0 0 0-4 12.75V18h8v-3.25A7 7 0 0 0 12 2ZM9 20v1a1 1 0 0 0 1 1h4a1 1 0 0 0 1-1v-1H9Z" }
                }
            } else { Icon { name: icon(&entity), size: 32 } }
        }
        span { class: "ha-tile-caption",
            strong { title: "{title}", "{title}" }
            span { class: "ha-tile-state", if stale { "Данные устарели" } else { "{value(&entity)}" } }
            if !stale {
                if let Some(companion) = companion { span { class: "ha-tile-reading", "{companion}" } }
                else if let Some(reading) = readings.first() { span { class: "ha-tile-reading", "{reading.name} · {reading.value}" } }
            }
        }
    }
}

#[component]
fn EntityList(entities: Vec<Entity>) -> Element {
    rsx! { dl { class: "ha-entities",
        for entity in entities { div { key: "{entity.id}", class: "ha-entity",
            dt { title: "{entity.id}", "{entity.name}" } dd { "{value(&entity)}" }
            for reading in &entity.readings { div { class: "ha-entity-reading", span { "{reading.name}" } span { "{reading.value}" } } }
        } }
    } }
}
