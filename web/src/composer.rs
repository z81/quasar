use dioxus::prelude::*;
use serde_json::Value;

use crate::api;
use crate::icons::Icon;
use crate::state::{ControlState, Mode};

#[component]
pub fn Composer(mut state: ControlState) -> Element {
    let mode = (state.mode)();
    let enabled = state.can_send() && !state.text.read().trim().is_empty();
    rsx! {
        section { class: "card composer", aria_label: "Новая команда",
            div { class: "card-heading", span { class: "card-kicker", Icon { name: "spark", size: 16 } "НОВАЯ КОМАНДА" } span { class: "subtle-pill", "Текст → действие" } }
            h2 { "Попросить Алису" }
            div { class: "mode-tabs", role: "group", aria_label: "Способ отправки",
                for item in Mode::ALL {
                    button { class: if item == mode { "mode-tab selected" } else { "mode-tab" }, aria_pressed: item == mode,
                        onclick: move |_| state.mode.set(item), "{item.label()}"
                    }
                }
            }
            label { class: "sr-only", r#for: "command-text", "Текст команды" }
            div { class: "command-field",
                textarea { id: "command-text", placeholder: mode.placeholder(), value: "{state.text}", maxlength: "8000", spellcheck: "true",
                    oninput: move |event| state.text.set(event.value()),
                    onkeydown: move |event| {
                        if event.key() == Key::Enter && (event.modifiers().ctrl() || event.modifiers().meta()) {
                            event.prevent_default();
                            if state.can_send() { api::submit(state); }
                        }
                    }
                }
                div { class: "field-footer", span { "⌘ / Ctrl + Enter" } span { "{state.text.read().chars().count()} / 8000" } }
            }
            p { class: "mode-hint", "{mode.hint()}" }
            div { class: "composer-bottom",
                span { class: "send-target", Icon { name: "speaker", size: 15 }
                    if let Some(device) = state.device() { span { "{device.label()}" } } else { span { "Колонка не выбрана" } }
                }
                button { class: "text-button save-draft", disabled: state.text.read().trim().is_empty() || state.device().is_none(), onclick: move |_| {
                    let text = state.text.read().trim().to_string();
                    api::save_favorite(state, text.chars().take(80).collect(), (state.selected)(), mode.path().to_string(), serde_json::json!({"eventText":text}));
                    state.panel.set(crate::state::Panel::Favorites);
                }, Icon { name: "star", size: 16 } "В избранное" }
                button { class: "button primary", disabled: !enabled, onclick: move |_| api::submit(state),
                    if (state.sending)() { span { class: "spinner" } "Отправляем" } else { "Отправить" Icon { name: "arrow", size: 18 } }
                }
            }
            div { class: "suggestions", span { "Попробуйте" }
                for (label, text) in [("Погода", "Какая сегодня погода?"), ("Время", "Который час?"), ("Возможности", "Что ты умеешь?")] {
                    button { class: "suggestion", onclick: move |_| { state.mode.set(Mode::Command); state.text.set(text.to_string()); }, "{label}" Icon { name: "chevron", size: 11 } }
                }
            }
        }
    }
}

const DEFAULT_DIRECTIVE: &str = "{\n  \"Name\": \"sound_louder\",\n  \"Type\": \"client_action\",\n  \"IsLedSilent\": true,\n  \"Payload\": { \"fields\": [] }\n}";

#[component]
pub fn Advanced(state: ControlState) -> Element {
    let mut draft = use_signal(|| DEFAULT_DIRECTIVE.to_string());
    let parsed = serde_json::from_str::<Value>(&draft());
    let validation = match &parsed {
        Ok(Value::Object(object))
            if object
                .get("Name")
                .and_then(Value::as_str)
                .is_some_and(|name| !name.trim().is_empty()) =>
        {
            None
        }
        Ok(_) => Some("Нужен JSON-объект с непустым строковым полем Name.".to_string()),
        Err(error) => Some(format!("Проверьте JSON: {error}")),
    };
    let enabled = state.can_send() && validation.is_none();
    rsx! {
        section { class: "card composer advanced", aria_label: "Редактор директив",
            div { class: "card-heading", span { class: "card-kicker", Icon { name: "code", size: 16 } "ДИРЕКТИВА" } span { class: "subtle-pill", "JSON" } }
            h2 { "Команда без посредников" }
            p { class: "mode-hint", "Директива отправится напрямую в выбранную колонку." }
            div { class: "directive-presets",
                for (name, label) in [("sound_louder", "Громче"), ("sound_quiter", "Тише"), ("audio_stop", "Остановить")] {
                    button { class: "suggestion", onclick: move |_| draft.set(DEFAULT_DIRECTIVE.replace("sound_louder", name)), "{label}" }
                }
            }
            label { class: "sr-only", r#for: "directive-json", "JSON директивы" }
            textarea { id: "directive-json", class: "json-editor", value: "{draft}", spellcheck: "false", oninput: move |event| draft.set(event.value()) }
            if let Some(error) = validation { p { class: "validation-error", role: "status", "{error}" } }
            div { class: "composer-bottom",
                span { class: "send-target", Icon { name: "speaker", size: 15 } span { "Прямое управление" } }
                button { class: "button primary", disabled: !enabled, onclick: move |_| {
                    if let Ok(body) = serde_json::from_str::<Value>(&draft()) {
                        api::dispatch(state, (state.selected)(), "push-directive".to_string(), body, false);
                    }
                }, "Отправить" Icon { name: "arrow", size: 18 } }
            }
        }
    }
}
