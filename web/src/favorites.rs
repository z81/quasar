use dioxus::prelude::*;
use serde_json::json;

use crate::api;
use crate::icons::Icon;
use crate::state::{ControlState, Mode};

#[component]
pub fn Favorites(mut state: ControlState) -> Element {
    let mut label = use_signal(String::new);
    let mut text = use_signal(String::new);
    let mut mode = use_signal(Mode::default);
    let favorites = (state.favorites)();
    let mut busy = use_signal(|| false);
    rsx! {
        div { class: "settings-grid",
            section { class: "card settings-card",
                div { class: "card-heading", span { class: "card-kicker", Icon { name: "star", size: 16 } "НОВАЯ КОМАНДА" } }
                h2 { "Один раз сохраните — запускайте одним нажатием." }
                form { onsubmit: move |event| {
                    event.prevent_default();
                    if !busy() && !label.read().trim().is_empty() && !text.read().trim().is_empty() && state.device().is_some() {
                        busy.set(true);
                        let draft_label = label(); let draft_text = text(); let duid = (state.selected)(); let path = mode().path().to_string();
                        dioxus::core::spawn_forever(async move {
                            match api::create_favorite(state, draft_label.clone(), duid, path, json!({"eventText":draft_text.trim()})).await {
                                Ok(_) => {
                                    if let Ok(mut value) = label.try_write() { if *value == draft_label { value.clear(); } }
                                    if let Ok(mut value) = text.try_write() { if *value == draft_text { value.clear(); } }
                                }
                                Err(error) => state.notice.set(Some(error)),
                            }
                            if let Ok(mut value) = busy.try_write() { *value = false; }
                        });
                    }
                },
                    label { class: "form-field", span { "Название" } input { required: true, maxlength: "80", placeholder: "Например, доброе утро", value: "{label}", oninput: move |event| label.set(event.value()) } }
                    div { class: "mode-tabs", role: "group", aria_label: "Тип сохранённой команды",
                        for item in Mode::ALL { button { r#type: "button", class: if mode() == item { "mode-tab selected" } else { "mode-tab" }, onclick: move |_| mode.set(item), "{item.label()}" } }
                    }
                    label { class: "form-field", span { "Текст" } textarea { required: true, maxlength: "8000", placeholder: mode().placeholder(), value: "{text}", oninput: move |event| text.set(event.value()) } }
                    p { class: "form-help", if let Some(device) = state.device() { "Для колонки: {device.label()}" } else { "Выберите колонку в боковом меню" } }
                    button { class: "button primary", r#type: "submit", disabled: busy() || label.read().trim().is_empty() || text.read().trim().is_empty() || state.device().is_none(), Icon { name: "star", size: 16 } "Сохранить команду" }
                }
            }
            section { class: "favorites-list", aria_label: "Сохранённые команды",
                if favorites.is_empty() {
                    div { class: "card history-empty", span { class: "empty-icon", Icon { name: "star", size: 24 } } h3 { "Ваши частые просьбы — всегда рядом" } p { "Добавьте команду здесь или сохраните её из истории." } }
                }
                for favorite in favorites {
                    {
                        let run = favorite.clone();
                        let remove = favorite.id.clone();
                        let station = state.devices.read().iter().find(|device| device.duid == favorite.duid).map(|device| device.label().to_owned()).unwrap_or_else(|| favorite.duid.clone());
                        let text = favorite.body["eventText"].as_str().or_else(|| favorite.body["Name"].as_str()).unwrap_or("Директива").to_string();
                        let enabled = !(state.sending)() && state.server_error.read().is_none() && state.devices.read().iter().any(|device| device.duid == favorite.duid && device.can_receive());
                        rsx! {
                            article { key: "{favorite.id}", class: "card favorite-card",
                                div { class: "favorite-heading", h3 { "{favorite.label}" }
                                    button { class: "icon-button", aria_label: "Удалить сохранённую команду", title: "Удалить", onclick: move |_| { let url = format!("/control/favorites/{}", api::encoded(&remove)); dioxus::core::spawn_forever(async move { match api::mutate(state, "DELETE", &url, None).await { Ok(_) => api::refresh_saved(state).await, Err(error) => state.notice.set(Some(error)) } }); }, Icon { name: "trash", size: 15 } }
                                }
                                p { class: "favorite-text", "{text}" }
                                div { class: "favorite-bottom", span { class: "form-help", "{station}" }
                                    button { class: "button secondary", disabled: !enabled, onclick: move |_| api::dispatch(state, run.duid.clone(), run.path.clone(), run.body.clone(), false), "Запустить" Icon { name: "arrow", size: 15 } }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
