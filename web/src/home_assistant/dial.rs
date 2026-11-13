use dioxus::{prelude::*, web::WebEventExt};
use js_sys::wasm_bindgen::JsCast;
use serde_json::{json, Value};

use super::model::{same_value, Control};

#[derive(Clone, Copy)]
struct Range {
    min: f64,
    max: f64,
    step: f64,
    precision: usize,
}

impl Range {
    fn from_control(control: &Control) -> Option<Self> {
        let (min, max, step) = (control.min?, control.max?, control.step?);
        if !min.is_finite() || !max.is_finite() || !step.is_finite() || min >= max || step <= 0.0 {
            return None;
        }
        Some(Self {
            min,
            max,
            step,
            precision: [min, max, step]
                .into_iter()
                .map(decimal_places)
                .max()
                .unwrap_or(0),
        })
    }

    fn normalize(self, value: f64) -> f64 {
        let value = if value >= self.max {
            self.max
        } else {
            (self.min + ((value - self.min) / self.step).round() * self.step)
                .clamp(self.min, self.max)
        };
        let scale = 10_f64.powi(self.precision as i32);
        if (value * scale).is_finite() {
            ((value * scale).round() / scale).clamp(self.min, self.max)
        } else {
            value
        }
    }

    fn fraction(self, value: f64) -> f64 {
        ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    fn format(self, value: f64) -> String {
        let text = format!("{:.*}", self.precision, value);
        let text = if self.precision > 0 {
            text.trim_end_matches('0').trim_end_matches('.')
        } else {
            &text
        };
        if text == "-0" {
            "0".into()
        } else {
            text.replace('.', ",")
        }
    }
}

fn decimal_places(value: f64) -> usize {
    let text = value.to_string();
    let (mantissa, exponent) = text.split_once('e').map_or((text.as_str(), 0), |(m, e)| {
        (m, e.parse::<i32>().unwrap_or(0))
    });
    let decimals = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len() as i32);
    (decimals - exponent).clamp(0, 15) as usize
}

#[derive(Clone)]
struct Drag {
    pointer: i32,
    element: web_sys::Element,
    center_x: f64,
    center_y: f64,
    previous: Option<f64>,
}

impl Drag {
    fn value(&self, event: &PointerEvent, range: Range, previous: Option<f64>) -> f64 {
        let point = event.client_coordinates();
        let angle = ((point.y - self.center_y)
            .atan2(point.x - self.center_x)
            .to_degrees()
            - 135.0)
            .rem_euclid(360.0);
        // Keep an endpoint while crossing the bottom gap instead of jumping
        // from maximum to minimum halfway through a continuous gesture.
        let fraction = if angle <= 270.0 {
            angle / 270.0
        } else if previous.map_or(angle < 315.0, |value| range.fraction(value) >= 0.5) {
            1.0
        } else {
            0.0
        };
        range.normalize(range.min + fraction * (range.max - range.min))
    }
}

fn adjustment_key(key: &Key) -> bool {
    matches!(
        key,
        Key::ArrowUp
            | Key::ArrowRight
            | Key::ArrowDown
            | Key::ArrowLeft
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
    )
}

#[component]
pub(super) fn Dial(
    control: Control,
    entity_name: String,
    disabled: bool,
    onchange: EventHandler<(String, Value)>,
) -> Element {
    let mut draft = use_signal(|| None::<f64>);
    let mut drag = use_signal(|| None::<Drag>);
    let mut keyboard_dirty = use_signal(|| false);
    let mut was_disabled = use_signal(|| disabled);
    use_effect(use_reactive(
        (&control, &disabled),
        move |(control, disabled)| {
            if was_disabled() != disabled {
                // The parent disables controls while a command is pending.
                // Reconcile with the device when it settles, including failure
                // and timeout, so a rejected draft never looks confirmed.
                if !disabled {
                    draft.set(None);
                }
                was_disabled.set(disabled);
            }
            if disabled {
                let active = drag.peek().clone();
                if let Some(active) = active {
                    drag.set(None);
                    draft.set(active.previous);
                    let _ = active.element.release_pointer_capture(active.pointer);
                }
                if *keyboard_dirty.peek() {
                    keyboard_dirty.set(false);
                    draft.set(None);
                }
            }
            // Polling must not overwrite an in-progress gesture or an unconfirmed
            // command. Clear a submitted draft only when the device reports it.
            if drag.read().is_none() && !keyboard_dirty() {
                let selected = *draft.read();
                if selected.is_some_and(|value| same_value(&control, &json!(value))) {
                    draft.set(None);
                }
            }
        },
    ));

    let Some(range) = Range::from_control(&control) else {
        return rsx! {};
    };
    let actual = control.value.as_f64().filter(|value| value.is_finite());
    let selected = draft().or(actual).map(|value| range.normalize(value));
    let fraction = selected.map_or(0.0, |value| range.fraction(value));
    let angle = (135.0 + fraction * 270.0).to_radians();
    let thumb_x = 64.0 + angle.cos() * 52.0;
    let thumb_y = 64.0 + angle.sin() * 52.0;
    let display = selected.map_or_else(|| "—".into(), |value| range.format(value));
    let unit = control.unit.clone().unwrap_or_default();
    let value_text = if selected.is_some() {
        format!("{display} {unit}")
    } else {
        "Нет данных".into()
    };
    let label = format!("{} — {}", control.label, entity_name);
    let font_size = (100.0 / (display.chars().count() as f64 * 0.62)).clamp(10.0, 30.0);
    let is_draft = draft().is_some();
    let commit_control = control.clone();
    let commit = EventHandler::new(move |_: ()| {
        keyboard_dirty.set(false);
        if disabled {
            return;
        }
        let selected = *draft.peek();
        if let Some(value) = selected {
            if same_value(&commit_control, &json!(value)) {
                draft.set(None);
            } else {
                onchange.call((commit_control.id.clone(), json!(value)));
            }
        }
    });
    let cancel = EventHandler::new(move |_: ()| {
        let active = drag.peek().clone();
        if let Some(active) = active {
            drag.set(None);
            draft.set(active.previous);
            let _ = active.element.release_pointer_capture(active.pointer);
        }
    });

    rsx! {
        div {
            class: if is_draft { "ha-dial is-draft" } else { "ha-dial" },
            role: "slider", tabindex: if disabled { "-1" } else { "0" },
            aria_label: label, aria_disabled: disabled,
            aria_valuemin: range.min, aria_valuemax: range.max,
            aria_valuenow: selected.map(|value| value.to_string()), aria_valuetext: value_text,
            "data-control": control.id.clone(),
            title: format!("{} · стрелки — изменить, Esc — сбросить", control.label),
            onpointerdown: move |event| {
                if disabled || drag.peek().is_some() { return; }
                let Some(pointer) = event.data().try_as_web_event() else { return; };
                if pointer.button() != 0 || !pointer.is_primary() { return; }
                let Some(element) = pointer.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    .and_then(|target| target.closest(".ha-dial").ok().flatten()) else { return; };
                let bounds = element.get_bounding_client_rect();
                let point = event.client_coordinates();
                let center_x = bounds.x() + bounds.width() / 2.0;
                let center_y = bounds.y() + bounds.height() / 2.0;
                event.prevent_default();
                event.stop_propagation();
                if let Some(element) = element.dyn_ref::<web_sys::HtmlElement>() { let _ = element.focus(); }
                if (point.x - center_x).hypot(point.y - center_y) < bounds.width() * 0.22 { return; }
                if element.set_pointer_capture(pointer.pointer_id()).is_err() { return; }
                let active = Drag { pointer: pointer.pointer_id(), element, center_x, center_y, previous: *draft.peek() };
                let value = active.value(&event, range, None);
                keyboard_dirty.set(false);
                drag.set(Some(active));
                draft.set(Some(value));
            },
            onpointermove: move |event| {
                if disabled { return; }
                let active = drag.peek().clone();
                if let Some(active) = active.filter(|active| active.pointer == event.pointer_id()) {
                    event.prevent_default();
                    let previous = *draft.peek();
                    draft.set(Some(active.value(&event, range, previous)));
                }
            },
            onpointerup: move |event| {
                let active = drag.peek().clone();
                if let Some(active) = active.filter(|active| active.pointer == event.pointer_id()) {
                    event.prevent_default();
                    event.stop_propagation();
                    if !disabled {
                        let previous = *draft.peek();
                        draft.set(Some(active.value(&event, range, previous)));
                    }
                    drag.set(None);
                    let _ = active.element.release_pointer_capture(active.pointer);
                    commit.call(());
                }
            },
            onpointercancel: move |event| { if drag.peek().as_ref().is_some_and(|active| active.pointer == event.pointer_id()) { cancel.call(()); } },
            onlostpointercapture: move |event| { if drag.peek().as_ref().is_some_and(|active| active.pointer == event.pointer_id()) { cancel.call(()); } },
            onclick: move |event| event.stop_propagation(),
            onkeydown: move |event| {
                if disabled { return; }
                let key = event.key();
                if key == Key::Escape {
                    event.prevent_default();
                    cancel.call(());
                    keyboard_dirty.set(false);
                    draft.set(None);
                    return;
                }
                if drag.peek().is_some() { return; }
                if key == Key::Enter {
                    event.prevent_default();
                    if !event.is_auto_repeating() && keyboard_dirty() { commit.call(()); }
                    return;
                }
                let value = draft().or(actual).unwrap_or(range.min);
                let next = match key {
                    Key::ArrowUp | Key::ArrowRight => value + range.step,
                    Key::ArrowDown | Key::ArrowLeft => value - range.step,
                    Key::PageUp => value + range.step * 10.0,
                    Key::PageDown => value - range.step * 10.0,
                    Key::Home => range.min,
                    Key::End => range.max,
                    _ => return,
                };
                event.prevent_default();
                event.stop_propagation();
                keyboard_dirty.set(true);
                draft.set(Some(range.normalize(next)));
            },
            onkeyup: move |event| {
                if adjustment_key(&event.key()) {
                    event.prevent_default();
                    if keyboard_dirty() { commit.call(()); }
                }
            },
            onblur: move |_| { cancel.call(()); if keyboard_dirty() { commit.call(()); } },
            svg { class: "ha-dial-ring", view_box: "0 0 128 128", "aria-hidden": "true",
                path { class: "ha-dial-track", d: "M 27.23 100.77 A 52 52 0 1 1 100.77 100.77", path_length: "100" }
                if selected.is_some() {
                    path { class: "ha-dial-fill", d: "M 27.23 100.77 A 52 52 0 1 1 100.77 100.77", path_length: "100", stroke_dasharray: format!("{} 100", fraction * 100.0) }
                    circle { class: "ha-dial-thumb", cx: thumb_x, cy: thumb_y, r: "5" }
                }
            }
            span { class: "ha-dial-center", aria_hidden: "true",
                strong { class: "ha-dial-value", style: "font-size: {font_size}px", "{display}" }
                if !unit.is_empty() { span { class: "ha-dial-unit", "{unit}" } }
            }
            span { class: "ha-dial-label", aria_hidden: "true", "{control.label}" }
        }
    }
}
