use crate::{api, icons::Icon, state::ControlState};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};

#[component]
pub fn SmartHomePage(state: ControlState) -> Element {
    rsx! {
        document::Stylesheet { href: asset!("/assets/smart-home.css") }
        if let Some(device) = state.device() {
            SmartHome { key: "{device.duid}", state, duid: device.duid }
        } else {
            div { class: "card history-empty", Icon { name: "speaker", size: 28 } h3 { "Выберите колонку" } p { "Здесь появятся встроенный Zigbee-хаб и Matter." } }
        }
    }
}

#[component]
fn SmartHome(state: ControlState, duid: String) -> Element {
    let mut status = use_signal(|| None::<Value>);
    let mut error = use_signal(|| None::<String>);
    let mut notice = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let mut protocol = use_signal(|| "zigbee".to_owned());
    let mut setup_code = use_signal(String::new);
    let mut ssid = use_signal(String::new);
    let mut password = use_signal(String::new);
    let url = format!("/devices/{}/smart-home", api::encoded(&duid));
    let poll_url = url.clone();
    use_future(move || {
        let url = poll_url.clone();
        async move {
            let mut snapshot_requested = false;
            loop {
                if state.allowed() {
                    if !snapshot_requested {
                        snapshot_requested = true;
                        if let Err(message) =
                            api::mutate(state, "POST", &url, Some(&json!({"action":"refresh"})))
                                .await
                        {
                            error.set(Some(message));
                        }
                    }
                    match api::get::<Value>(state, &url).await {
                        Ok(value) => {
                            status.set(Some(value));
                            error.set(None);
                        }
                        Err(message) => error.set(Some(message)),
                    }
                }
                TimeoutFuture::new(3_000).await;
            }
        }
    });
    let dispatch = EventHandler::new(move |body: Value| {
        if busy() || !state.allowed() {
            return;
        }
        let url = url.clone();
        busy.set(true);
        notice.set(None);
        spawn(async move {
            match api::mutate(state, "POST", &url, Some(&body)).await {
                Ok(_) => {
                    notice.set(Some(
                        "Команда отправлена. Ожидаем подтверждения состоянием станции.".into(),
                    ));
                    if body["action"] == "start" {
                        password.set(String::new());
                        setup_code.set(String::new());
                    }
                    match api::get::<Value>(state, &url).await {
                        Ok(value) => {
                            status.set(Some(value));
                            error.set(None);
                        }
                        Err(message) => error.set(Some(message)),
                    }
                }
                Err(message) => error.set(Some(message)),
            }
            busy.set(false);
        });
    });
    let data = status().unwrap_or_default();
    let connected = data["connected"].as_bool().unwrap_or(false);
    let received = data["capability_received"].as_bool().unwrap_or(false);
    let protocols = data["protocols"].as_array().cloned().unwrap_or_default();
    let selected = protocols
        .iter()
        .find(|p| p["id"] == protocol())
        .cloned()
        .unwrap_or_default();
    let session = data["session"].clone();
    let active = matches!(
        session["status"].as_str(),
        Some("queued" | "searching" | "awaiting_confirmation" | "confirming" | "cancelling")
    );
    let pairing_allowed = connected
        && selected["can_pair"] == true
        && !active
        && !busy()
        && error().is_none()
        && state.allowed();
    let endpoints = data["endpoints"].as_array().cloned().unwrap_or_default();
    rsx! {
        div { class: "smart-home-page",
            section { class: "card smart-home-card",
                span { class: "card-kicker", "ВСТРОЕННЫЙ ХАБ" }
                h2 { "Умный дом рядом" }
                button { class: "button secondary", disabled: busy() || !state.allowed(), onclick: move |_| dispatch.call(json!({"action":"refresh"})), "Обновить со станции" }
                p { class: "smart-home-help", "Подключайте устройства к колонке по Zigbee или Matter over Wi-Fi и управляйте ими в локальной сети." }
                if !state.allowed() { p { class: "form-error", "Войдите в пульт Quasar для управления умным домом." } }
                if let Some(message) = error() { p { class: "form-error", role: "alert", "{message}" } }
                if status().is_none() && state.allowed() && error().is_none() { p { class: "smart-home-help", "Получаем состояние хаба…" } }
                if !connected && status().is_some() { p { class: "smart-home-help", "Колонка не подключена к серверу." } }
                if connected && !received { p { class: "smart-home-help", "Ждём данные встроенного хаба от колонки. Сопряжение станет доступно после подтверждения поддержки протокола." } }
                div { class: "smart-home-protocols",
                    for item in protocols {
                        div { class: "smart-home-protocol",
                            strong { if item["id"] == "zigbee" { "Zigbee" } else { "Matter over Wi-Fi" } }
                            span { if item["supported"] != true { "Поддержка пока не подтверждена" } else { {provider_status(item["status"].as_str().unwrap_or_default())} } }
                        }
                    }
                }
            }
            section { class: "card smart-home-card",
                h2 { "Подключить устройство" }
                div { class: "smart-home-tabs", role: "tablist", aria_label: "Протокол подключения",
                    for (value,label) in [("zigbee","Zigbee"),("matter","Matter over Wi-Fi")] {
                        button { r#type: "button", role: "tab", aria_selected: protocol() == value, class: if protocol() == value { "button secondary active" } else { "button secondary" }, disabled: active || busy(), onclick: move |_| protocol.set(value.into()), "{label}" }
                    }
                }
                form { onsubmit: move |event| {
                    event.prevent_default();
                    if !pairing_allowed { return; }
                    dispatch.call(json!({"action":"start","protocol":protocol(),"setup_code":setup_code(),"ssid":ssid(),"password":password()}));
                },
                    if protocol() == "matter" {
                        p { class: "smart-home-help", "Используйте Matter over Wi-Fi, сеть 2,4 ГГц и код с устройства. Включите на устройстве режим подключения. Для добавления из другого дома нужен новый код общего доступа." }
                        label { class: "form-field", span { "Код Matter или содержимое QR-кода" } input { value: "{setup_code}", required: true, maxlength: "256", autocomplete: "off", spellcheck: "false", placeholder: "11 или 21 цифра, либо MT:…", disabled: busy() || active, oninput: move |event| setup_code.set(event.value()) } }
                        div { class: "smart-home-network",
                            label { class: "form-field", span { "Сеть Wi-Fi (SSID)" } input { value: "{ssid}", required: true, maxlength: "32", autocomplete: "off", spellcheck: "false", disabled: busy() || active, oninput: move |event| ssid.set(event.value()) } }
                            label { class: "form-field", span { "Пароль Wi-Fi" } input { r#type: "password", value: "{password}", maxlength: "64", autocomplete: "new-password", disabled: busy() || active, oninput: move |event| password.set(event.value()) } }
                        }
                        p { class: "smart-home-help", "Код и пароль передаются только в команду подключения, без сохранения в настройках и истории." }
                    } else {
                        p { class: "smart-home-help", "Запустите поиск на 120 секунд, затем переведите Zigbee-устройство в режим сопряжения по его инструкции. Найденное устройство появится здесь для подтверждения." }
                    }
                    button { class: "button primary", r#type: "submit", disabled: !pairing_allowed, "Начать поиск" }
                }
                if !session.is_null() {
                    div { class: "smart-home-session", role: "status", aria_live: "polite",
                        strong { {session_status(session["status"].as_str().unwrap_or_default())} }
                        p { {session["message"].as_str().unwrap_or_default()} }
                        for candidate in session["candidates"].as_array().cloned().unwrap_or_default() {
                            Candidate { candidate, session:session.clone(), busy:busy(), dispatch }
                        }
                        if active {
                            button { class: "button secondary", disabled: busy() || session["status"] == "cancelling", onclick: move |_| dispatch.call(json!({"action":"cancel","session_id":session["id"]})), "Отменить поиск" }
                        }
                    }
                }
                if let Some(message) = notice() { p { class: "smart-home-help", role:"status", "{message}" } }
            }
            section { class: "card smart-home-card",
                h2 { "Устройства" }
                if endpoints.is_empty() { p { class: "smart-home-help", "Станция пока не передала список Zigbee- и Matter-устройств. Здесь появятся устройства и доступные им команды после получения данных." } }
                for endpoint in endpoints {
                    EndpointCard { key:"{endpoint_id(&endpoint)}", endpoint, disabled:busy() || !connected || error().is_some() || !state.allowed(), dispatch }
                }
            }
        }
    }
}

#[component]
fn Candidate(
    candidate: Value,
    session: Value,
    busy: bool,
    dispatch: EventHandler<Value>,
) -> Element {
    let label = label(&candidate);
    rsx! { div { class: "smart-home-device",
        strong { "{label}" }
        small { {candidate["id"].as_str().unwrap_or_default()} }
        button { class: "button primary", disabled: busy || session["status"] != "awaiting_confirmation", onclick: move |_| dispatch.call(json!({"action":"finish","session_id":session["id"],"callback_id":session["callback_id"],"endpoint_id":candidate["id"]})), "Подключить" }
    }}
}

#[component]
fn EndpointCard(endpoint: Value, disabled: bool, dispatch: EventHandler<Value>) -> Element {
    let mut draft = use_signal(String::new);
    let label = label(&endpoint);
    let id_on = endpoint["id"].clone();
    let id_off = id_on.clone();
    let id_level = id_on.clone();
    let brightness = endpoint["brightness"].clone();
    let value = draft()
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite());
    rsx! { article { class: "smart-home-device",
        div { class: "smart-home-device-title", strong { "{label}" } span { if endpoint["protocol"] == "zigbee" { "Zigbee" } else { "Matter" } } }
        small { {endpoint["id"].as_str().unwrap_or_default()} }
        p { class: "smart-home-help", match endpoint["online"].as_bool() { Some(true) => "В сети", Some(false) => "Не в сети", None => "Состояние связи неизвестно" }
            if let Some(on) = endpoint["on"].as_bool() { if on { " · Включено" } else { " · Выключено" } }
        }
        if endpoint["can_toggle"] == true {
            div { class: "smart-home-actions",
                button { class: "button secondary", disabled, onclick: move |_| dispatch.call(json!({"action":"on_off","endpoint_id":id_on,"on":true})), "Включить" }
                button { class: "button secondary", disabled, onclick: move |_| dispatch.call(json!({"action":"on_off","endpoint_id":id_off,"on":false})), "Выключить" }
            }
        }
        if !brightness.is_null() {
            p { class: "smart-home-help", {format!("Яркость: {}. Диапазон: {}–{}.",brightness["value"],brightness["min"],brightness["max"])} }
            if brightness["writable"] == true {
                form { class: "smart-home-actions", onsubmit:move |event| { event.prevent_default(); if let Some(value) = value.filter(|_|!disabled) { dispatch.call(json!({"action":"brightness","endpoint_id":id_level,"value":value})); } },
                    label { class: "form-field", span { "Новая яркость" } input { r#type:"text", inputmode:"decimal", value:"{draft}", disabled, oninput:move |event|draft.set(event.value()) } }
                    button { class:"button secondary", r#type:"submit", disabled:disabled || value.is_none(), "Применить" }
                }
            }
        }
    }}
}

fn label(endpoint: &Value) -> String {
    let model = endpoint["model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("Устройство");
    let manufacturer = endpoint["manufacturer"].as_str().unwrap_or_default();
    format!("{manufacturer} {model}").trim().to_owned()
}
fn provider_status(status: &str) -> &'static str {
    match status {
        "READY" => "Готов к подключению",
        "INITIALIZING" => "Запускается",
        "DISCOVERING" | "DISCOVERING_IN_BACKGROUND" => "Идёт поиск",
        "DISABLED" => "Отключён в прошивке",
        "FAILURE" => "Ошибка хаба",
        "WIPED" => "Сеть не настроена",
        _ => "Ждём состояние станции",
    }
}
fn session_status(status: &str) -> &'static str {
    match status {
        "queued" => "Поиск запрошен",
        "searching" => "Поиск устройств",
        "awaiting_confirmation" => "Ответ станции",
        "confirming" => "Подключение запрошено",
        "cancelling" => "Остановка поиска",
        "cancelled" => "Поиск завершён",
        "complete" => "Получен результат",
        "expired" => "Время ожидания истекло",
        "failed" => "Не удалось подключить",
        _ => "Подключение",
    }
}

fn endpoint_id(endpoint: &Value) -> &str {
    endpoint["id"].as_str().unwrap_or_default()
}
