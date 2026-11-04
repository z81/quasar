use dioxus::prelude::*;

use super::{mutate, PanelData};
use crate::{icons::Icon, state::ControlState};

#[derive(Clone, PartialEq)]
struct Draft {
    title: String,
    hours: String,
    minutes: String,
    seconds: String,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            title: String::new(),
            hours: "0".to_string(),
            minutes: "5".to_string(),
            seconds: "0".to_string(),
        }
    }
}

impl Draft {
    fn duration(&self) -> Option<u32> {
        fn field(value: &str, max: u32) -> Option<u32> {
            if value.is_empty() {
                return Some(0);
            }
            if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            value.parse::<u32>().ok().filter(|number| *number <= max)
        }
        let seconds = field(&self.hours, 168)? * 3600
            + field(&self.minutes, 59)? * 60
            + field(&self.seconds, 59)?;
        (1..=604_800).contains(&seconds).then_some(seconds)
    }
}

#[component]
pub(super) fn TimerEditor(
    state: ControlState,
    duid: String,
    data: PanelData,
    unsupported: bool,
) -> Element {
    let mut draft = use_signal(Draft::default);
    let value = draft();
    let busy = (data.busy)();
    let valid = value.duration().is_some() && value.title.trim().chars().count() <= 120;
    rsx! {
        section { class: "card settings-card timer-editor", aria_label: "Новый таймер",
            span { class: "card-kicker", Icon { name: "timer", size: 16 } "НОВЫЙ ТАЙМЕР" }
            h2 { "Немного времени." }
            p { class: "settings-intro", "Укажите длительность или выберите готовый интервал." }
            if unsupported { p { class: "schedule-error", role: "status", "Эта колонка не поддерживает звуковой сигнал таймера." } }
            form { onsubmit: move |event| {
                event.prevent_default();
                let value = draft();
                if busy || unsupported || value.title.trim().chars().count() > 120 { return; }
                if let Some(seconds) = value.duration() {
                    mutate(state, duid.clone(), data, "POST", String::new(), Some(serde_json::json!({"title":value.title.trim(), "duration_seconds":seconds})));
                }
            },
                label { class: "form-field", span { "Название" }
                    input { maxlength: "120", value: "{value.title}", placeholder: "Например, заварить чай", disabled: busy || unsupported, oninput: move |event| draft.write().title = event.value() }
                    small { "Необязательно — можно оставить «Таймер»." }
                }
                div { class: "timer-duration", role: "group", aria_label: "Длительность таймера",
                    label { class: "form-field", span { "Часы" }
                        input { r#type: "text", inputmode: "numeric", value: "{value.hours}", placeholder: "0", aria_label: "Часы таймера", disabled: busy || unsupported, oninput: move |event| draft.write().hours = event.value() }
                    }
                    span { class: "timer-colon", "∶" }
                    label { class: "form-field", span { "Минуты" }
                        input { r#type: "text", inputmode: "numeric", value: "{value.minutes}", placeholder: "0", aria_label: "Минуты таймера", disabled: busy || unsupported, oninput: move |event| draft.write().minutes = event.value() }
                    }
                    span { class: "timer-colon", "∶" }
                    label { class: "form-field", span { "Секунды" }
                        input { r#type: "text", inputmode: "numeric", value: "{value.seconds}", placeholder: "0", aria_label: "Секунды таймера", disabled: busy || unsupported, oninput: move |event| draft.write().seconds = event.value() }
                    }
                }
                p { class: if value.duration().is_some() { "timer-duration-help" } else { "timer-duration-help failure" },
                    if value.duration().is_some() { "От 1 секунды до 7 дней." }
                    else { "Введите целые числа: часы 0–168, минуты и секунды 0–59. Всего — от 1 секунды до 7 дней." }
                }
                div { class: "timer-presets", role: "group", aria_label: "Быстрые интервалы",
                    for minutes in [1, 5, 10, 15, 30] {
                        button { class: if value.duration() == Some(minutes * 60) { "timer-preset chosen" } else { "timer-preset" }, r#type: "button", aria_pressed: value.duration() == Some(minutes * 60), disabled: busy || unsupported,
                            onclick: move |_| { let mut value = draft.write(); value.hours = "0".to_string(); value.minutes = minutes.to_string(); value.seconds = "0".to_string(); }, "{minutes} мин"
                        }
                    }
                }
                div { class: "settings-actions", button { class: "button primary", r#type: "submit", disabled: busy || !valid || unsupported,
                    if busy { span { class: "spinner" } "Сохраняем…" } else { Icon { name: "play", size: 16 } "Запустить таймер" }
                } }
            }
        }
    }
}
