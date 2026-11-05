use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::api;
use crate::icons::Icon;
use crate::state::{ControlState, Device};

#[component]
pub fn Settings(state: ControlState) -> Element {
    let mut server = use_signal(|| true);
    let device = state.device();
    rsx! {
        div { class: "settings-tabs", role: "tablist", aria_label: "Область настроек",
            button { class: if server() { "settings-tab active" } else { "settings-tab" }, role: "tab", aria_selected: server(), onclick: move |_| server.set(true), Icon { name: "settings", size: 16 } "Сервер" }
            button { class: if !server() { "settings-tab active" } else { "settings-tab" }, role: "tab", aria_selected: !server(), onclick: move |_| server.set(false), Icon { name: "speaker", size: 16 } "Колонка" }
        }
        if server() {
            crate::server_settings::ServerSettings { state }
        } else if device.is_none() {
            div { class: "card history-empty", Icon { name: "speaker", size: 28 } h3 { "Выберите колонку" } p { "Настройки появятся после её подключения к серверу." } }
        } else {
            // The keyed list also resets station-scoped form drafts on selection.
            for device in device {
                div { class: "station-settings-stack", key: "{device.duid}",
                    DeviceSettings { state, device: device.clone() }
                    crate::display_settings::DisplaySettings { state, device: device.clone() }
                    crate::display_studio::DisplayStudio { state, duid: device.duid.clone() }
                    crate::weather::WeatherSettings { state, duid: device.duid }
                }
            }
        }
    }
}

#[component]
fn DeviceSettings(mut state: ControlState, device: Device) -> Element {
    let mut name = use_signal(|| device.name.clone());
    let mut timezone = use_signal(|| device.timezone.clone().unwrap_or_default());
    let mut wake = use_signal(|| {
        device.quasar_config["accountConfig"]["spotter"]
            .as_str()
            .unwrap_or("alisa")
            .to_string()
    });
    let mut jingle = use_signal(|| {
        device.quasar_config["accountConfig"]["jingle"]
            .as_bool()
            .unwrap_or(false)
    });
    let mut smart = use_signal(|| {
        device.quasar_config["accountConfig"]["smartActivation"]
            .as_bool()
            .unwrap_or(true)
    });
    let mut adaptive = use_signal(|| {
        device.quasar_config["accountConfig"]["aliceAdaptiveVolume"]["enabled"]
            .as_bool()
            .unwrap_or(true)
    });
    let mut busy = use_signal(|| false);
    let mut feedback = use_signal(|| None::<(bool, String)>);
    let submit_device = device.clone();
    rsx! {
        section { class: "card settings-card station-settings",
            div { class: "card-heading", span { class: "card-kicker", Icon { name: "settings", size: 16 } "НАСТРОЙКИ КОЛОНКИ" } span { class: "subtle-pill", "{device.duid}" } }
            h2 { "Как к вам обращаться?" }
            p { class: "settings-intro", "Настройки сохраняются на сервере. Колонка получит обновление при доступном соединении." }
            form { onsubmit: move |event| {
                event.prevent_default();
                if busy() || name.read().trim().is_empty() { return; }
                busy.set(true); feedback.set(None);
                // Start from the latest complete config: PATCH replaces it, it is not a partial merge.
                let mut config = state.devices.read().iter().find(|current| current.duid == submit_device.duid).map(|current| current.quasar_config.clone()).unwrap_or_else(|| submit_device.quasar_config.clone());
                config["accountConfig"]["spotter"] = json!(wake());
                config["accountConfig"]["jingle"] = json!(jingle());
                config["accountConfig"]["smartActivation"] = json!(smart());
                config["accountConfig"]["aliceAdaptiveVolume"]["enabled"] = json!(adaptive());
                let zone = timezone.read().trim().to_string();
                let body = json!({"name":name.read().trim(),"quasarConfig":config,"timezone":if zone.is_empty(){Value::Null}else{json!(zone)}});
                let url = format!("/devices/{}", api::encoded(&submit_device.duid));
                dioxus::core::spawn_forever(async move {
                    match api::mutate(state, "PATCH", &url, Some(&body)).await {
                        Ok(_) => { if let Ok(mut value) = feedback.try_write() { *value = Some((true, "Настройки сохранены".to_string())); } api::refresh(state).await; }
                        Err(error) => { if let Ok(mut value) = feedback.try_write() { *value = Some((false,error)); } },
                    }
                    if let Ok(mut value) = busy.try_write() { *value = false; }
                });
            },
                div { class: "form-columns",
                    label { class: "form-field", span { "Название колонки" } input { required: true, maxlength: "160", value: "{name}", oninput: move |event| name.set(event.value()), placeholder: "Станция в гостиной" } }
                    label { class: "form-field", span { "Часовой пояс ответов Алисы" } input { list: "timezones", value: "{timezone}", oninput: move |event| timezone.set(event.value()), placeholder: "Автоматически" } small { "IANA, например Europe/Moscow. Пустое поле — автоматически." } }
                    datalist { id: "timezones", for zone in ["Europe/Moscow","Europe/Kaliningrad","Europe/Samara","Asia/Yekaterinburg","Asia/Novosibirsk","Asia/Krasnoyarsk","Asia/Irkutsk","Asia/Yakutsk","Asia/Vladivostok","Asia/Kamchatka","Europe/Berlin","Asia/Tbilisi","UTC"] { option { value: zone } } }
                }
                div { class: "settings-divider" }
                h3 { "Голос и активация" }
                label { class: "form-field wake-field", span { "Слово активации" }
                    select { value: "{wake}", onchange: move |event| wake.set(event.value()), option { value: "alisa", "Алиса" } option { value: "yandex", "Яндекс" } option { value: "yasmina", "Ясмина" } }
                }
                for (key, title, hint) in [("jingle", "Звук активации", "Короткий сигнал, когда колонка вас услышала"), ("smart", "Умная активация", "Колонка определяет, обращаетесь ли вы к Алисе"), ("adaptive", "Адаптивная громкость", "Громкость ответа подстраивается под окружение")] {
                    label { class: "toggle-field", span { strong { "{title}" } small { "{hint}" } }
                        input { r#type: "checkbox", checked: match key { "jingle" => jingle(), "smart" => smart(), _ => adaptive() }, onchange: move |event| match key { "jingle" => jingle.set(event.checked()), "smart" => smart.set(event.checked()), _ => adaptive.set(event.checked()) } }
                    }
                }
                div { class: "settings-actions",
                    button { class: "button primary", r#type: "submit", disabled: busy() || name.read().trim().is_empty(), if busy() { span { class: "spinner" } "Сохраняем…" } else { Icon { name: "check", size: 16 } "Сохранить настройки" } }
                    if let Some((success,message)) = feedback() { p { class: if success { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" } }
                }
            }
        }
    }
}
