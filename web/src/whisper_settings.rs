use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;
use serde_json::json;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Deserialize)]
struct WhisperView {
    enabled: bool,
    available: bool,
    persistent: bool,
}

#[component]
pub fn WhisperSettings(state: ControlState, duid: String) -> Element {
    let mut view = use_signal(|| None::<WhisperView>);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut feedback = use_signal(|| None::<(bool, String)>);
    let mut revision = use_signal(|| 0_u64);
    let poll_duid = duid.clone();
    use_future(move || {
        let duid = poll_duid.clone();
        async move {
            loop {
                if state.allowed() && !*busy.peek() {
                    let started_revision = *revision.peek();
                    let result = api::get::<WhisperView>(
                        state,
                        &format!("/devices/{}/whisper", api::encoded(&duid)),
                    )
                    .await;
                    // A response started before a PUT must not undo its result.
                    if started_revision == *revision.peek() && !*busy.peek() {
                        match result {
                            Ok(value) => {
                                view.set(Some(value));
                                error.set(None);
                            }
                            Err(message) => error.set(Some(message)),
                        }
                    }
                }
                TimeoutFuture::new(3000).await;
            }
        }
    });
    let snapshot = view();
    let enabled = snapshot.as_ref().is_some_and(|value| value.enabled);
    let available = snapshot.as_ref().is_some_and(|value| value.available);
    let disabled = busy() || snapshot.is_none() || error().is_some() || (!available && !enabled);
    rsx! {
        section { class: "card settings-card whisper-settings", aria_label: "Режим шёпота",
            div { class: "card-heading",
                span { class: "card-kicker", Icon { name: "volume-down", size: 16 } "ГОЛОС АЛИСЫ" }
                span { class: "subtle-pill", if busy() { "Сохраняем…" } else if error().is_some() { "Нет связи" } else if snapshot.is_none() { "Загружаем…" } else if enabled { "Шёпот включён" } else { "Обычный голос" } }
            }
            h2 { "Режим шёпота" }
            p { class: "settings-intro", "Алиса озвучивает ответы шёпотом, пока вы не выключите этот режим. Настройка действует только на выбранную колонку." }
            label { class: "toggle-field",
                span { strong { "Отвечать шёпотом" } small { "Применяется сразу к следующим ответам" } }
                input { r#type: "checkbox", checked: enabled, disabled,
                    onchange: move |event| {
                        if disabled { return; }
                        let next = event.checked();
                        busy.set(true);
                        feedback.set(None);
                        let next_revision = *revision.peek() + 1;
                        revision.set(next_revision);
                        let url = format!("/devices/{}/whisper", api::encoded(&duid));
                        dioxus::core::spawn_forever(async move {
                            let result = match api::mutate(state, "PUT", &url, Some(&json!({ "enabled": next }))).await {
                                Ok(response) => response.json::<WhisperView>().await.map_err(|_| "Сервер вернул непонятный ответ".to_owned()),
                                Err(error) => Err(error),
                            };
                            match result {
                                Ok(value) => {
                                    if let Ok(mut current) = view.try_write() { *current = Some(value); }
                                    if let Ok(mut current) = error.try_write() { *current = None; }
                                    if let Ok(mut current) = feedback.try_write() { *current = Some((true, if next { "Ответы шёпотом включены" } else { "Обычный голос включён" }.to_owned())); }
                                }
                                Err(message) => {
                                    if let Ok(mut current) = feedback.try_write() { *current = Some((false, message)); }
                                }
                            }
                            if let Ok(mut current) = busy.try_write() { *current = false; }
                        });
                    }
                }
            }
            p { class: "form-help", "Голосом: «Алиса, говори шёпотом» или «Алиса, говори обычным голосом». Автоматическое распознавание шёпота пока недоступно." }
            p { class: "form-help", "Шёпот озвучивает ElevenLabs через OpenRouter: текст ответа передаётся этому сервису. Голос может отличаться от обычного; звук активации и музыка сохраняют свои настройки." }
            if snapshot.is_some() && !available {
                p { class: "form-feedback failure", "Для шёпота добавьте ключ OpenRouter в настройках сервера. Основной голос Piper не поддерживает шёпот." }
            }
            if snapshot.as_ref().is_some_and(|value| !value.persistent) {
                p { class: "form-feedback failure", "База недоступна: настройка шёпота сбросится после перезапуска сервера." }
            }
            if let Some(message) = error() { p { class: "form-feedback failure", role: "alert", "{message}" } }
            if let Some((success, message)) = feedback() { p { class: if success { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" } }
        }
    }
}
