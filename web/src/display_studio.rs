use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;
use serde_json::json;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Deserialize, PartialEq)]
struct Animation {
    id: String,
    label: String,
}

#[derive(Clone, Deserialize, PartialEq)]
struct CustomDisplay {
    supported: bool,
    reason: String,
}

#[derive(Clone, Deserialize, PartialEq)]
struct DisplayStatus {
    connected: bool,
    capability_received: bool,
    supported: bool,
    reason: Option<String>,
    animations: Vec<Animation>,
    custom: CustomDisplay,
}

#[derive(Clone, Copy)]
struct Editor {
    status: Signal<Option<DisplayStatus>>,
    selected: Signal<String>,
    loading: Signal<bool>,
    error: Signal<Option<String>>,
}

async fn refresh(state: ControlState, duid: &str, mut editor: Editor) {
    if !state.allowed() || *editor.loading.peek() {
        return;
    }
    editor.loading.set(true);
    let url = format!("/devices/{}/display", api::encoded(duid));
    match api::get::<DisplayStatus>(state, &url).await {
        Ok(status) => {
            if !status
                .animations
                .iter()
                .any(|animation| animation.id == *editor.selected.peek())
            {
                editor.selected.set(
                    status
                        .animations
                        .first()
                        .map(|animation| animation.id.clone())
                        .unwrap_or_default(),
                );
            }
            editor.status.set(Some(status));
            editor.error.set(None);
        }
        Err(error) => editor.error.set(Some(error)),
    }
    editor.loading.set(false);
}

fn duration_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    let fraction = value
        .split_once('.')
        .map(|(_, fraction)| fraction.len())
        .unwrap_or(0);
    if fraction > 3
        || value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    let seconds = value.parse::<f64>().ok()?;
    (seconds.is_finite() && (0.5..=30.0).contains(&seconds))
        .then(|| (seconds * 1000.0).round() as u64)
}

fn symbol(id: &str) -> &'static str {
    match id {
        "smile" => "☺",
        "sadness" => "☹",
        "joy" => "✦",
        "angry" => "!",
        "wow" => "◎",
        "wink" => ";)",
        "love" => "♥",
        "pause" => "Ⅱ",
        "next" => "▷",
        "battery" => "▰",
        "co2" => "CO₂",
        "humidity" => "%",
        "temperature" => "°",
        _ => "✧",
    }
}

#[component]
pub fn DisplayStudio(state: ControlState, duid: String) -> Element {
    let mut editor = Editor {
        status: use_signal(|| None),
        selected: use_signal(String::new),
        loading: use_signal(|| false),
        error: use_signal(|| None),
    };
    let mut duration = use_signal(|| "5".to_owned());
    let mut sending = use_signal(|| false);
    let mut feedback = use_signal(|| None::<(bool, String)>);
    let polling_duid = duid.clone();
    use_future(move || {
        let duid = polling_duid.clone();
        async move {
            loop {
                refresh(state, &duid, editor).await;
                TimeoutFuture::new(10_000).await;
            }
        }
    });
    let status = (editor.status)();
    let duration_value = duration_ms(&duration());
    let supported = status
        .as_ref()
        .is_some_and(|status| status.supported && status.connected);
    let can_send = supported
        && state.can_send()
        && (editor.error)().is_none()
        && duration_value.is_some()
        && !sending()
        && !(editor.selected)().is_empty();
    let refresh_duid = duid.clone();
    rsx! {
        document::Stylesheet { href: asset!("/assets/display-studio.css") }
        section { class: "card display-studio", aria_label: "Анимации на экране колонки",
            div { class: "display-studio-heading",
                div { span { class: "card-kicker", "ЭКРАН КОЛОНКИ" } h2 { "Анимации" } }
                button { class: "text-button", disabled: (editor.loading)(), onclick: move |_| { let duid = refresh_duid.clone(); spawn(async move { refresh(state, &duid, editor).await; }); }, Icon { name: "refresh", size: 14 } "Обновить" }
            }
            if let Some(error) = (editor.error)() { p { class: "form-feedback failure", role: "alert", "{error}" } }
            if let Some(status) = status {
                if !status.animations.is_empty() {
                    div { class: "display-animation-list", role: "group", aria_label: "Выбор анимации",
                        for animation in status.animations {
                            {
                                let id = animation.id.clone();
                                rsx! { button { class: if (editor.selected)() == animation.id { "display-animation active" } else { "display-animation" }, disabled: !supported || sending(), aria_pressed: (editor.selected)() == animation.id,
                                    onclick: move |_| { editor.selected.set(id.clone()); feedback.set(None); },
                                    span { class: "display-animation-symbol", aria_hidden: "true", "{symbol(&animation.id)}" }
                                    span { "{animation.label}" }
                                } }
                            }
                        }
                    }
                }
                if !supported {
                    p { class: "display-studio-hint", role: "status",
                        if !status.connected { "Колонка не подключена. Анимации станут доступны после подключения." }
                        else if let Some(reason) = status.reason { "{reason}" }
                        else if !status.capability_received { "Колонка пока не сообщила, какие анимации поддерживает." }
                        else { "Эта колонка не сообщила о поддержке анимаций." }
                    }
                }
                if status.supported {
                    div { class: "display-studio-actions",
                        label { class: "form-field display-duration", r#for: "display-animation-duration", span { "Не дольше, с" }
                            input { id: "display-animation-duration", r#type: "text", inputmode: "decimal", value: "{duration}", disabled: sending(), aria_invalid: duration_value.is_none(), oninput: move |event| { duration.set(event.value()); feedback.set(None); } }
                        }
                        button { class: "button primary", disabled: !can_send, onclick: move |_| {
                            let Some(duration_ms) = duration_ms(&duration()) else { return; };
                            if !can_send { return; }
                            sending.set(true); feedback.set(None);
                            let url = format!("/devices/{}/display", api::encoded(&duid));
                            let body = json!({"action":"animation","animation":(editor.selected)(),"duration_ms":duration_ms});
                            dioxus::core::spawn_forever(async move {
                                let result = match api::mutate(state, "POST", &url, Some(&body)).await {
                                    Ok(_) => (true, "Анимация поставлена в очередь. Подтверждение показа колонка не присылает.".into()),
                                    Err(error) => (false, error),
                                };
                                if let Ok(mut feedback) = feedback.try_write() { *feedback = Some(result); }
                                if let Ok(mut sending) = sending.try_write() { *sending = false; }
                            });
                        },
                            if sending() { span { class: "spinner" } "Отправляем…" } else { Icon { name: "play", size: 15 } "Показать на колонке" }
                        }
                    }
                    if duration_value.is_none() { p { class: "form-feedback failure", "Укажите от 0,5 до 30 секунд, не более трёх знаков после точки." } }
                }
                if !status.custom.supported { p { class: "display-custom-note", "{status.custom.reason}" } }
            } else if (editor.loading)() { p { class: "display-studio-hint", role: "status", "Проверяем возможности экрана…" } }
            if let Some((success, message)) = feedback() { p { class: if success { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" } }
        }
    }
}
