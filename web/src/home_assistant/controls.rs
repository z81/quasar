use dioxus::prelude::*;
use serde_json::{json, Value};

use super::dial::Dial;
use super::model::{ActionRequest, Control, Entity, Pending};
use super::presentation::{channel_name, facts, value};
use crate::icons::Icon;

#[component]
pub(super) fn EntityControls(
    entity: Entity,
    device_name: String,
    source: String,
    stale: bool,
    show_name: bool,
    pending: Option<Pending>,
    error: Option<String>,
    dispatch: EventHandler<ActionRequest>,
) -> Element {
    let disabled = stale || entity.unavailable() || pending.is_some();
    let action_entity = entity.id.clone();
    let send = EventHandler::new(move |(control, value): (String, Value)| {
        if !disabled {
            dispatch.call(ActionRequest {
                source: source.clone(),
                entity_id: action_entity.clone(),
                control,
                value,
            });
        }
    });
    let toggle = entity.controls.iter().find(|c| c.kind == "toggle");
    let actual = value(&entity);
    let readings = facts(&entity);
    let label = channel_name(&entity.name, &device_name);
    let status_class = if stale || entity.unavailable() {
        "ha-channel-state is-unavailable"
    } else if entity.active() {
        "ha-channel-state is-on"
    } else {
        "ha-channel-state"
    };
    rsx! {
        div { class: "ha-channel", aria_label: "{entity.name}", aria_busy: pending.is_some(),
            div { class: "ha-channel-heading",
                div { class: "ha-channel-label",
                    if show_name { strong { title: "{entity.name}", "{label}" } }
                    span { class: status_class, span { class: "ha-status-dot" } "{actual}" }
                }
                if let Some(toggle) = toggle {
                    {let on = toggle.value.as_bool().unwrap_or(false); let control_id = toggle.id.clone(); rsx! {
                        button { r#type: "button", class: if on { "ha-power is-on" } else { "ha-power" },
                            disabled: disabled || toggle.value.is_null(), aria_pressed: on,
                            aria_label: format!("{} {}", if on { "Выключить" } else { "Включить" }, entity.name),
                            title: if on { "Выключить" } else { "Включить" },
                            onclick: move |_| send.call((control_id.clone(), json!(!on))),
                            Icon { name: "power", size: 18 }
                            span { if on { "Выкл" } else { "Вкл" } }
                        }
                    }}
                }
            }
            if !readings.is_empty() {
                div { class: "ha-channel-facts",
                    for reading in readings { span { title: "{reading.name}", small { "{reading.name}" } strong { "{reading.value}" } } }
                }
            }
            for control in entity.controls.iter().filter(|c| c.kind != "toggle") {
                if control.kind == "range" {
                    Dial { key: "{entity.id}:{control.id}", control: control.clone(), entity_name: entity.name.clone(), disabled, onchange: send }
                } else if control.kind == "select" {
                    ChoiceControl { key: "{entity.id}:{control.id}", control: control.clone(), entity_name: entity.name.clone(), disabled, onchange: send }
                }
            }
            if let Some(pending) = &pending {
                p { class: "ha-command-status", role: "status", span { class: "ha-spinner" }
                    if pending.accepted { "Ожидаем устройство…" } else { "Отправляем…" }
                }
            }
            if let Some(error) = error { p { class: "ha-command-error", role: "alert", "{error}" } }
        }
    }
}

#[component]
fn ChoiceControl(
    control: Control,
    entity_name: String,
    disabled: bool,
    onchange: EventHandler<(String, Value)>,
) -> Element {
    let current = control.value.as_str().unwrap_or_default();
    let known = control.options.iter().any(|option| option.value == current);
    let control_id = control.id.clone();
    rsx! {
        label { class: "ha-choice", span { "{control.label}" }
            select { value: current, disabled, aria_label: format!("{} — {}", control.label, entity_name),
                onchange: move |e| onchange.call((control_id.clone(), json!(e.value()))),
                if !known { option { value: current, disabled: true, if current.is_empty() { "Выберите…" } else { "{current}" } } }
                for option in control.options { option { value: "{option.value}", "{option.label}" } }
            }
        }
    }
}
