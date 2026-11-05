use dioxus::core::Task;
use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::json;

use super::Editor;
use crate::{api, state::ControlState};

#[derive(Clone, PartialEq)]
pub(super) enum RevealTarget {
    Setting { key: String, baseline: bool },
    Mcp { prefix: String },
}

impl RevealTarget {
    fn request(&self, revision: u64) -> (&'static str, serde_json::Value) {
        match self {
            Self::Setting { key, baseline } => (
                "/settings/server/reveal",
                json!({"revision":revision,"key":key,"baseline":baseline}),
            ),
            Self::Mcp { prefix } => (
                "/settings/server/mcp/reveal",
                json!({"revision":revision,"prefix":prefix}),
            ),
        }
    }
}

#[derive(Clone)]
struct Revealed {
    draft: Option<String>,
    target: Option<RevealTarget>,
    value: String,
}

#[derive(Deserialize)]
struct SecretResponse {
    token: Option<String>,
}

#[component]
pub(super) fn SecretInput(
    id: String,
    label: String,
    target: Option<RevealTarget>,
    draft: Option<String>,
    is_set: bool,
    state: ControlState,
    mut editor: Editor,
    revision: u64,
    disabled: bool,
    onchange: EventHandler<String>,
) -> Element {
    let mut revealed = use_signal(|| None::<Revealed>);
    let mut pending = use_signal(|| None::<Task>);
    let mut loading = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    // Cancel an in-flight reveal if the row is edited, cleared, or locked.
    // Revealed values live only in this component, never in the settings draft.
    use_effect(use_reactive(
        (&draft, &target, &disabled),
        move |(draft, target, disabled)| {
            if let Some(task) = pending.write().take() {
                task.cancel();
            }
            loading.set(false);
            error.set(None);
            if disabled
                || revealed
                    .peek()
                    .as_ref()
                    .is_some_and(|value| value.draft != draft || value.target != target)
            {
                revealed.set(None);
            }
        },
    ));
    let visible = revealed
        .read()
        .as_ref()
        .filter(|value| !disabled && value.draft == draft && value.target == target)
        .cloned();
    let shown = visible.is_some();
    let value = visible
        .map(|value| value.value)
        .unwrap_or_else(|| draft.clone().unwrap_or_default());
    let has_value = draft.as_ref().map_or(is_set, |value| !value.is_empty());
    let input_target = target.clone();
    rsx! {
        div { class: "setting-secret",
            div { class: "setting-secret-input",
                input { id, r#type: if shown { "text" } else { "password" }, value,
                    placeholder: if is_set && draft.is_none() { "********" } else { "Не задан" },
                    disabled, spellcheck: false, autocomplete: "new-password", autocapitalize: "none",
                    oninput: move |event| {
                        let value = event.value();
                        let draft = (!value.is_empty()).then_some(value.clone());
                        revealed.set((shown && draft.is_some()).then_some(Revealed { draft, target: input_target.clone(), value: value.clone() }));
                        onchange.call(value);
                    }
                }
                button { class: "setting-secret-toggle", r#type: "button", disabled: disabled || !has_value || loading(),
                    aria_label: if shown { "Скрыть: {label}" } else { "Показать: {label}" }, aria_pressed: shown,
                    onclick: move |_| {
                        error.set(None);
                        if shown { revealed.set(None); return; }
                        if let Some(value) = draft.clone() {
                            revealed.set(Some(Revealed { draft: Some(value.clone()), target: target.clone(), value }));
                            return;
                        }
                        let Some(target) = target.clone() else { return; };
                        loading.set(true);
                        pending.set(Some(spawn(async move {
                            let (endpoint, body) = target.request(revision);
                            let result = match api::mutate(state, "POST", endpoint, Some(&body)).await {
                                Ok(response) => response.json::<SecretResponse>().await.map_err(|_| "Не удалось прочитать ключ. Обновите настройки и повторите.".to_owned()),
                                Err(error) => Err(error),
                            };
                            match result {
                                Ok(response) => if let Some(value) = response.token {
                                    revealed.set(Some(Revealed { draft: None, target: Some(target), value }));
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
            if let Some(error) = error() { small { class: "setting-secret-error", role: "alert", "{error}" } }
        }
    }
}
