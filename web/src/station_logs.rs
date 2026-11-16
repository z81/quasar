use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;
use serde_json::Value;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Deserialize, PartialEq)]
struct Snapshot {
    source: String,
    text: String,
    truncated: bool,
    at: i64,
}

fn selected_log(source: &str, sources: Option<&[String]>) -> String {
    if !source.is_empty() {
        return source.to_owned();
    }
    let sources = sources.unwrap_or_default();
    ["maind.log", "quasar.log"]
        .into_iter()
        .find(|name| sources.iter().any(|source| source == name))
        .map(str::to_owned)
        .or_else(|| sources.iter().find(|name| name.ends_with(".log")).cloned())
        .unwrap_or_else(|| "dmesg".to_owned())
}

#[component]
pub fn StationLogsPage(state: ControlState) -> Element {
    let duid = (state.selected)();
    let ip = state.device().and_then(|device| device.ip_address).unwrap_or_default();
    rsx! {
        if duid.is_empty() {
            section { class: "card empty-state", h2 { "Выберите колонку" }
                p { "Выберите станцию в меню, чтобы читать её журналы по ADB." }
            }
        } else { Connection { key: "{duid}", state, duid, ip } }
    }
}

#[component]
fn Connection(state: ControlState, duid: String, ip: String) -> Element {
    let mut host = use_signal(String::new);
    let mut port = use_signal(|| "5555".to_owned());
    let mut target = use_signal(String::new);
    let valid_port = port().parse::<u16>().is_ok_and(|value| value > 0);
    rsx! {
        div { class: "logs-page",
            details { class: "card station-log-connection",
                summary { "Подключение по ADB" }
                div { class: "station-log-target",
                    label { class: "form-field", span { "Адрес колонки" }
                        input { value: host, placeholder: if ip.is_empty() { "Автоматически".to_owned() } else { ip.clone() },
                            oninput: move |event| host.set(event.value()) }
                    }
                    label { class: "form-field", span { "Порт" }
                        input { value: port, inputmode: "numeric", oninput: move |event| port.set(event.value()) }
                    }
                    button { class: "button secondary", disabled: !valid_port, onclick: move |_| {
                        let address = host().trim().to_owned();
                        target.set(if address.is_empty() { String::new() } else {
                            format!("?host={}&port={}", api::encoded(&address), api::encoded(&port()))
                        });
                    }, "Применить" }
                }
                p { class: "field-help", "Пустой адрес — сохранённое подключение или текущий IP выбранной колонки." }
            }
            StationLogs { state, duid, target: target() }
        }
    }
}

#[component]
pub fn StationLogs(state: ControlState, duid: String, target: String) -> Element {
    let connection = (duid.clone(), target.clone());
    let mut sources = use_signal(|| None::<Vec<String>>);
    let mut source = use_signal(String::new);
    let mut search = use_signal(String::new);
    let mut limit = use_signal(|| "200".to_owned());
    let mut automatic = use_signal(|| true);
    let mut follow = use_signal(|| true);
    let mut data = use_signal(|| None::<Snapshot>);
    let mut error = use_signal(|| None::<String>);
    let mut loading = use_signal(|| false);
    let mut task = use_future(move || {
        let base = format!("/devices/{}/tools", api::encoded(&duid));
        let target = target.clone();
        async move {
            TimeoutFuture::new(250).await;
            loop {
                let hidden = js_sys::eval("document.hidden").ok().and_then(|value| value.as_bool()).unwrap_or(false);
                if state.allowed() && !hidden {
                    loading.set(true);
                    // Discover once per connection; all journal reads reuse the existing ADB API.
                    if sources.peek().is_none() {
                        match api::get::<Value>(state, &format!("{base}/status{target}")).await {
                            Ok(value) => sources.set(Some(value["info"]["logs"].as_array().into_iter().flatten()
                                .filter_map(Value::as_str).map(str::to_owned).collect())),
                            Err(message) => error.set(Some(message)),
                        }
                    }
                    if sources.peek().is_some() {
                        let name = selected_log(&source.peek(), sources.peek().as_deref());
                        let join = if target.is_empty() { "?" } else { "&" };
                        let url = format!("{base}/logs{target}{join}source={}&lines={}&filter={}",
                            api::encoded(&name), api::encoded(&limit.peek()), api::encoded(&search.peek()));
                        match api::get::<Snapshot>(state, &url).await {
                            Ok(value) => {
                                if data.peek().as_ref() != Some(&value) { data.set(Some(value)); }
                                error.set(None);
                            }
                            Err(message) => error.set(Some(message)),
                        }
                    }
                    loading.set(false);
                }
                if !*automatic.peek() { break; }
                TimeoutFuture::new(5000).await;
            }
        }
    });
    use_effect(move || {
        let _ = (source(), search(), limit());
        data.set(None);
        error.set(None);
        task.restart();
    });
    use_effect(use_reactive((&connection,), move |_| {
        sources.set(None);
        source.set(String::new());
        data.set(None);
        error.set(None);
        task.restart();
    }));
    use_effect(move || {
        if automatic() { task.restart(); } else { task.cancel(); loading.set(false); }
    });
    let contents = use_memo(move || data.read().as_ref().map(|value| value.text.clone()));
    use_effect(move || {
        let _ = contents();
        if follow() {
            document::eval("requestAnimationFrame(() => { const view = document.getElementById('station-log-output'); if (view) view.scrollTop = view.scrollHeight; });");
        }
    });
    let names = sources().unwrap_or_default();
    let selected = selected_log(&source(), Some(&names));
    let snapshot = data();
    let toggle_icon = if automatic() { "pause" } else { "play" };
    rsx! {
        document::Stylesheet { href: asset!("/assets/server-logs.css") }
        section { class: "card server-logs station-logs", aria_label: "Журналы колонки",
            div { class: "server-log-toolbar",
                label { class: "form-field station-log-source", span { "Журнал" }
                    select { value: selected, onchange: move |event| source.set(event.value()),
                        for name in &names { option { value: name.clone(), "{name}" } }
                        option { value: "dmesg", "Ядро (dmesg)" }
                        option { value: "system", "Система" }
                    }
                }
                label { class: "form-field server-log-search", span { "Поиск" }
                    input { r#type: "search", value: search, maxlength: "128", placeholder: "Ошибка, имя службы…", oninput: move |event| search.set(event.value()) }
                }
                label { class: "form-field", span { "Строк" }
                    select { value: limit, onchange: move |event| limit.set(event.value()),
                        for count in [100, 200, 500, 1000] { option { value: "{count}", "{count}" } }
                    }
                }
                button { class: "button secondary", aria_pressed: !automatic(), onclick: move |_| automatic.set(!automatic()),
                    Icon { name: toggle_icon, size: 16 }
                    if automatic() { "Пауза" } else { "Продолжить" }
                }
                button { class: "button secondary", disabled: loading(), onclick: move |_| { sources.set(None); task.restart(); },
                    Icon { name: "refresh", size: 16 } "Обновить"
                }
            }
            div { class: "server-log-summary",
                span { role: "status", if automatic() { "Автообновление каждые 5 секунд" } else { "Автообновление на паузе" } }
                label { input { r#type: "checkbox", checked: follow(), onchange: move |event| follow.set(event.checked()) } "К новым записям" }
                if let Some(value) = &snapshot {
                    span { "Прочитано: " {String::from(js_sys::Date::new(&((value.at as f64 * 1000.0).into())).to_locale_time_string("ru-RU"))} }
                }
            }
            if let Some(message) = error() { p { class: "notice error", role: "alert", "{message}" } }
            div { id: "station-log-output", class: "server-log-output", tabindex: "0", role: "log", aria_label: "Записи журнала колонки", aria_live: "off", onwheel: move |_| follow.set(false),
                if let Some(value) = &snapshot {
                    if value.text.is_empty() { p { class: "server-log-empty", "Подходящих строк нет." } }
                    else { pre { class: "station-log-text", "{value.text}" } }
                } else if loading() { p { class: "server-log-empty", "Читаем журнал с колонки…" } }
            }
            if snapshot.as_ref().is_some_and(|value| value.truncated) { p { class: "notice", "Вывод ограничен 256 КБ. Уменьшите число строк." } }
            p { class: "field-help server-log-retention", "Журналы читаются с выбранной колонки по ADB. Поиск применяется к последним выбранным строкам." }
        }
    }
}
