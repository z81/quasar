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
        section { class: "ocean-command", aria_label: "Новая команда",
            h1 { "Спроси у " span { "Алисы" } }
            form { class: "ocean-command-form", onsubmit: move |event| { event.prevent_default(); if enabled { api::submit(state); } },
                label { class: "sr-only", r#for: "command-text", "Текст команды" }
                textarea { id: "command-text", rows: "1", placeholder: mode.placeholder(), value: "{state.text}", maxlength: "8000", spellcheck: "true",
                    oninput: move |event| state.text.set(event.value()),
                    onkeydown: move |event| { if event.key() == Key::Enter && !event.modifiers().shift() { event.prevent_default(); if state.can_send() { api::submit(state); } } }
                }
                label { class: "ocean-command-mode", span { class: "sr-only", "Способ отправки" }
                    select { value: mode.path(), onchange: move |event| { if let Some(mode) = Mode::ALL.into_iter().find(|m| m.path() == event.value()) { state.mode.set(mode); } },
                        for item in Mode::ALL { option { value: item.path(), selected: item == mode, "{item.label()}" } }
                    }
                }
                button { class: "ocean-send", r#type: "submit", disabled: !enabled, aria_label: "Отправить команду", title: "Отправить · Enter",
                    if (state.sending)() { span { class: "spinner" } } else { Icon { name: "send", size: 27 } }
                }
            }
            div { class: "ocean-suggestions",
                for (label, icon, text) in [("Погода", "cloud", "Какая сегодня погода?"), ("Таймер на 5 минут", "timer", "Поставь таймер на 5 минут")] {
                    button { r#type: "button", onclick: move |_| { state.mode.set(Mode::Command); state.text.set(text.to_string()); }, Icon { name: icon, size: 24 } "{label}" }
                }
                button { r#type: "button", onclick: move |_| state.mode.set(Mode::Speak), Icon { name: "text", size: 23 } "Озвучить текст" }
                button { r#type: "button", class: "ocean-save-command", title: "Сохранить в избранное", aria_label: "Сохранить команду в избранное", disabled: state.text.read().trim().is_empty() || state.device().is_none(),
                    onclick: move |_| { let text = state.text.read().trim().to_string(); api::save_favorite(state, text.chars().take(80).collect(), (state.selected)(), mode.path().to_string(), serde_json::json!({"eventText":text})); state.panel.set(crate::state::Panel::Favorites); }, Icon { name: "star", size: 22 }
                }
            }
            if mode != Mode::Command { p { class: "ocean-command-hint", "{mode.hint()}" } }
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
