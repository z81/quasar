use dioxus::prelude::*;
use serde_json::{json, Value};

use super::model::{same_value, ActionRequest, Control, Entity, Pending};
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
                    LevelControl { key: "{entity.id}:{control.id}", control: control.clone(), entity_name: entity.name.clone(), disabled, onchange: send }
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

// Range fields never expose intermediate numeric text. Quantize to the API step
// before storing a draft so the displayed level and the submitted value agree.
fn normalize(value: f64, min: f64, max: f64, step: f64) -> f64 {
    let value = (min + ((value - min) / step).round() * step).clamp(min, max);
    (value * 1_000_000.0).round() / 1_000_000.0
}

#[component]
fn LevelControl(
    control: Control,
    entity_name: String,
    disabled: bool,
    onchange: EventHandler<(String, Value)>,
) -> Element {
    let mut draft = use_signal(|| None::<f64>);
    use_effect(use_reactive((&control,), move |(control,)| {
        if draft().is_some_and(|value| same_value(&control, &json!(value))) {
            draft.set(None);
        }
    }));
    let Some((min, max, step)) = control
        .min
        .zip(control.max)
        .zip(control.step)
        .map(|((a, b), c)| (a, b, c))
        .filter(|(min, max, step)| {
            min.is_finite() && max.is_finite() && step.is_finite() && min < max && *step > 0.0
        })
    else {
        return rsx! {};
    };
    let actual = control.value.as_f64().filter(|n| n.is_finite());
    let selected = draft().unwrap_or_else(|| actual.unwrap_or(min));
    let selected = normalize(selected, min, max, step);
    let changed = draft().is_some() && !same_value(&control, &json!(selected));
    let label = format!("{} — {}", control.label, entity_name);
    let apply_label = format!(
        "Применить {} — {}",
        control.label.to_lowercase(),
        entity_name
    );
    let number = if actual.is_none() && draft().is_none() {
        "—".into()
    } else {
        format!("{selected:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .replace('.', ",")
    };
    let unit = control.unit.as_deref().unwrap_or_default();
    let progress = ((selected - min) / (max - min) * 100.0).clamp(0.0, 100.0);
    let control_id = control.id.clone();
    rsx! {
        div { class: if changed { "ha-level is-dirty" } else { "ha-level" },
            div { class: "ha-level-heading", span { "{control.label}" }
                span { class: "ha-level-value", "{number}" if !unit.is_empty() { " {unit}" } }
            }
            div { class: "ha-level-input",
                input { r#type: "range", min: "{min}", max: "{max}", step: "{step}", value: "{selected}", disabled,
                    aria_label: label, aria_valuetext: format!("{number} {unit}"), style: "--ha-progress: {progress}%;",
                    oninput: move |e| { if let Ok(value) = e.value().parse::<f64>() { if value.is_finite() { draft.set(Some(normalize(value,min,max,step))); } } },
                }
                button { r#type: "button", class: "ha-apply", title: "Применить", aria_label: apply_label,
                    disabled: disabled || !changed,
                    onclick: move |_| onchange.call((control_id.clone(), json!(selected))),
                    Icon { name: "check", size: 17 }
                }
            }
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
