use dioxus::core::Task;
use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::json;

use super::{update, ControlState, Editor};
use crate::api;

#[derive(Clone)]
struct Revealed {
    draft: Option<String>,
    prefix: Option<String>,
    value: String,
}

#[derive(Deserialize)]
struct SecretResponse {
    token: Option<String>,
}

#[component]
pub(super) fn McpToken(
    id: usize,
    original_prefix: Option<String>,
    token: Option<String>,
    token_set: bool,
    state: ControlState,
    mut editor: Editor,
    revision: u64,
    disabled: bool,
) -> Element {
    let mut revealed = use_signal(|| None::<Revealed>);
    let mut pending = use_signal(|| None::<Task>);
    let mut loading = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    // Cancel an in-flight reveal if the row is edited, cleared, or locked.
    // Revealed values live only in this component, never in the settings draft.
    use_effect(use_reactive(
        (&token, &original_prefix, &disabled),
        move |(token, prefix, disabled)| {
            if let Some(task) = pending.write().take() {
                task.cancel();
            }
            loading.set(false);
            error.set(None);
            if disabled
                || revealed
                    .peek()
                    .as_ref()
                    .is_some_and(|value| value.draft != token || value.prefix != prefix)
            {
                revealed.set(None);
            }
        },
    ));
    let visible = revealed
        .read()
        .as_ref()
        .filter(|value| !disabled && value.draft == token && value.prefix == original_prefix)
        .cloned();
    let shown = visible.is_some();
    let value = visible
        .map(|value| value.value)
        .unwrap_or_else(|| token.clone().unwrap_or_default());
    let has_value = token.as_ref().map_or(token_set, |value| !value.is_empty());
    let changed = token.is_some();
    let label = if changed {
        if has_value {
            "Новый ключ"
        } else {
            "Ключ будет удалён"
        }
    } else if token_set {
        "Ключ сохранён"
    } else {
        "Без ключа"
    };
    let input_prefix = original_prefix.clone();
    rsx! {
        div { class: "form-field server-setting-input mcp-token",
            label { r#for: "mcp-token-{id}", "Ключ доступа" small { class: if changed { "mcp-token-changed" } else { "" }, "{label}" } }
            div { class: "mcp-secret-input",
                input { id: "mcp-token-{id}", r#type: if shown { "text" } else { "password" }, value,
                    placeholder: if token_set && token.is_none() { "********" } else { "Необязательно" },
                    disabled, spellcheck: false, autocomplete: "new-password", autocapitalize: "none",
                    oninput: move |event| {
                        let value = event.value();
                        let draft = (!value.is_empty()).then_some(value.clone());
                        revealed.set((shown && draft.is_some()).then_some(Revealed { draft: draft.clone(), prefix: input_prefix.clone(), value }));
                        update(editor, |rows| { if let Some(row) = rows.iter_mut().find(|row| row.id == id) { row.token = draft; } });
                    }
                }
                button { class: "mcp-secret-toggle", r#type: "button", disabled: disabled || !has_value || loading(),
                    aria_label: if shown { "Скрыть ключ" } else { "Показать ключ" }, aria_pressed: shown,
                    onclick: move |_| {
                        error.set(None);
                        if shown { revealed.set(None); return; }
                        if let Some(value) = token.clone() {
                            revealed.set(Some(Revealed { draft: Some(value.clone()), prefix: original_prefix.clone(), value }));
                            return;
                        }
                        let Some(prefix) = original_prefix.clone() else { return; };
                        loading.set(true);
                        pending.set(Some(spawn(async move {
                            let body = json!({"revision":revision,"prefix":prefix});
                            let result = match api::mutate(state, "POST", "/settings/server/mcp/reveal", Some(&body)).await {
                                Ok(response) => response.json::<SecretResponse>().await.map_err(|_| "Не удалось прочитать ключ. Обновите настройки и повторите.".to_owned()),
                                Err(error) => Err(error),
                            };
                            match result {
                                Ok(response) => if let Some(value) = response.token {
                                    revealed.set(Some(Revealed { draft: None, prefix: Some(prefix), value }));
                                } else { error.set(Some("Ключ не задан. Обновите настройки.".into())); },
                                Err(cause) => {
                                    if cause.starts_with("HTTP 409:") { editor.conflict.set(true); editor.error.set(Some(cause.clone())); }
                                    error.set(Some(cause));
                                }
                            }
                            loading.set(false);
                        })));
                    },
                    if loading() { span { class: "spinner" } } else if shown { "Скрыть" } else { "Показать" }
                }
            }
            if let Some(error) = error() { small { class: "failure", role: "alert", "{error}" } }
        }
    }
}
