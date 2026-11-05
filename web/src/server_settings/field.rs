use dioxus::prelude::*;

use super::{Editor, Setting};
use crate::icons::Icon;

#[component]
pub(super) fn SettingField(setting: Setting, mut editor: Editor, disabled: bool) -> Element {
    let changes = (editor.changes)();
    let draft = changes.get(&setting.key);
    let reset = matches!(draft, Some(None));
    let changed = draft.is_some();
    let value = match draft {
        Some(Some(value)) => value.clone(),
        Some(None) => setting.baseline_value.clone().unwrap_or_default(),
        None => setting.value.clone().unwrap_or_default(),
    };
    let is_set = match draft {
        Some(Some(value)) => !value.is_empty(),
        Some(None) => setting.baseline_is_set,
        None => setting.is_set,
    };
    let choice = value.trim().to_ascii_lowercase();
    let invalid = setting.invalid(&changes);
    let source = if reset {
        if setting.env_present {
            "После сохранения: env"
        } else {
            "После сохранения: по умолчанию"
        }
    } else if changed {
        "Не сохранено"
    } else {
        match setting.source.as_str() {
            "override" if setting.env_present => "Переопределяет env",
            "override" => "Из настроек",
            "env" => "Из env",
            _ => "По умолчанию",
        }
    };
    let input_setting = setting.clone();
    let reset_setting = setting.clone();
    let undo_key = setting.key.clone();
    let clear_key = setting.key.clone();
    let secret_placeholder = if is_set {
        "Введите новый ключ"
    } else {
        "Не задан"
    };
    let input_type = if setting.secret { "password" } else { "text" };
    let inputmode = if setting.kind == "integer" {
        "numeric"
    } else if setting.kind == "number" {
        "decimal"
    } else {
        "text"
    };
    let row_class = format!(
        "server-setting-row{}{}",
        if changed { " changed" } else { "" },
        if setting.kind == "textarea" {
            " wide"
        } else {
            ""
        },
    );
    rsx! {
        div { class: row_class,
            div { class: "server-setting-heading",
                label { r#for: "setting-{setting.key}", "{setting.label}" }
                span { class: if reset { "setting-source reset" } else if changed || setting.source == "override" { "setting-source override" } else { "setting-source" }, "{source}" }
            }
            div { class: if invalid { "form-field server-setting-input invalid" } else { "form-field server-setting-input" },
                if setting.kind == "select" || setting.kind == "boolean" {
                    select { id: "setting-{setting.key}", value: "{choice}", disabled, onchange: move |event| editor.change(&input_setting, event.value()),
                        if setting.kind == "boolean" {
                            option { value: "true", selected: choice == "true", "Включено" } option { value: "false", selected: choice == "false", "Выключено" }
                        } else {
                            if !setting.options.iter().any(|option| option == &choice) { option { value: "{choice}", selected: true, if choice.is_empty() { "По умолчанию" } else { "{choice}" } } }
                            for option in &setting.options { option { value: "{option}", selected: option == &choice, if option.is_empty() { "По умолчанию" } else { "{option}" } } }
                        }
                    }
                } else if setting.kind == "textarea" && !setting.secret {
                    textarea { id: "setting-{setting.key}", value: "{value}", disabled, spellcheck: false, oninput: move |event| editor.change(&input_setting, event.value()) }
                } else {
                    input { id: "setting-{setting.key}", r#type: input_type, inputmode, value: "{value}", disabled, spellcheck: false, autocomplete: if setting.secret { "new-password" } else { "off" }, placeholder: if setting.secret { secret_placeholder } else { "Не задано" }, oninput: move |event| editor.change(&input_setting, event.value()) }
                }
                if invalid { small { class: "failure", "Введите корректное число в допустимом диапазоне." } }
                if setting.secret {
                    small { if reset { if is_set { "Вернётся ключ из env" } else { "Ключ будет сброшен" } }
                        else if changed { if is_set { "Будет сохранён новый ключ" } else { "Ключ будет очищен" } }
                        else if is_set { "Ключ задан и скрыт" }
                        else { "Ключ не задан" }
                    }
                }
            }
            div { class: "server-setting-footer",
                code { "{setting.env_name.as_deref().unwrap_or(&setting.key)}" }
                div { class: "server-setting-actions",
                    if changed { button { class: "text-button", disabled, onclick: move |_| { editor.changes.write().remove(&undo_key); }, "Отменить" } }
                    if setting.secret && is_set && !reset {
                        button { class: "text-button", disabled, onclick: move |_| { editor.changes.write().insert(clear_key.clone(), Some(String::new())); }, "Очистить ключ" }
                    }
                    if (setting.source == "override" || changed) && !reset {
                        button { class: "text-button", disabled, title: "Сбросить к {setting.fallback()}", aria_label: "Сбросить {setting.label} к {setting.fallback()}", onclick: move |_| editor.reset(&reset_setting), Icon { name: "refresh", size: 13 } "Сбросить" }
                    }
                    details { class: "server-setting-help",
                        summary { aria_label: "Описание: {setting.label}", title: "Описание настройки", "?" }
                        div { class: "server-setting-help-body",
                            p { "{setting.description}" }
                            if setting.secret { p { "Пустое поле оставляет текущий ключ. Для замены введите новый; «Очистить ключ» сохранит пустое значение." } }
                            p { "Сброс возвращает значение из env, если оно задано, иначе — значение по умолчанию." }
                        }
                    }
                }
            }
        }
    }
}
