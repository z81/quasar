use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Deserialize, PartialEq)]
struct Entry {
    id: u64,
    at: String,
    level: String,
    target: String,
    message: String,
    truncated: bool,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Snapshot {
    started_at: String,
    capacity: usize,
    retained: usize,
    matched: usize,
    entries: Vec<Entry>,
}

fn timestamp(value: &str) -> String {
    js_sys::Date::new(&value.into()).to_locale_string("ru-RU", &js_sys::wasm_bindgen::JsValue::UNDEFINED).into()
}

#[component]
pub fn Logs(state: ControlState) -> Element {
    let mut station = use_signal(|| false);
    rsx! {
        document::Stylesheet { href: asset!("/assets/server-logs.css") }
        div { class: "logs-page",
            div { class: "mode-tabs", role: "tablist", aria_label: "Источник журнала",
                for (value, label) in [(false, "Сервер"), (true, "Колонка")] {
                    button {
                        class: if station() == value { "mode-tab selected" } else { "mode-tab" },
                        role: "tab", aria_selected: station() == value,
                        onclick: move |_| station.set(value), "{label}"
                    }
                }
            }
            if station() { crate::station_logs::StationLogsPage { state } }
            else { ServerLogs { state } }
        }
    }
}

#[component]
fn ServerLogs(state: ControlState) -> Element {
    let mut level = use_signal(String::new);
    let mut search = use_signal(String::new);
    let mut limit = use_signal(|| "200".to_owned());
    let mut automatic = use_signal(|| true);
    let mut follow = use_signal(|| true);
    let mut refresh = use_signal(|| 0_u64);
    let mut data = use_signal(|| None::<Snapshot>);
    let mut error = use_signal(|| None::<String>);
    let mut loading = use_signal(|| false);
    let mut task = use_future(move || async move {
        TimeoutFuture::new(250).await;
        loop {
            let hidden = js_sys::eval("document.hidden").ok().and_then(|value| value.as_bool()).unwrap_or(false);
            if state.allowed() && !hidden {
                loading.set(true);
                let url = format!("/logs/server?level={}&search={}&limit={}", api::encoded(&level.peek()), api::encoded(&search.peek()), api::encoded(&limit.peek()));
                match api::get::<Snapshot>(state, &url).await {
                    Ok(value) => {
                        if data.peek().as_ref() != Some(&value) { data.set(Some(value)); }
                        error.set(None);
                    }
                    Err(message) => error.set(Some(message)),
                }
                loading.set(false);
            }
            if !*automatic.peek() { break; }
            TimeoutFuture::new(2000).await;
        }
    });
    use_effect(move || {
        let _ = (level(), search(), limit(), refresh());
        task.restart();
    });
    use_effect(move || {
        if automatic() { task.restart(); } else { task.cancel(); loading.set(false); }
    });
    let last_entry = use_memo(move || data.read().as_ref().map(|value| (value.started_at.clone(), value.entries.last().map(|entry| entry.id))));
    use_effect(move || {
        let _ = last_entry();
        if follow() {
            document::eval("requestAnimationFrame(() => { const view = document.getElementById('server-log-output'); if (view) view.scrollTop = view.scrollHeight; });");
        }
    });
    let snapshot = data();
    let toggle_icon = if automatic() { "pause" } else { "play" };
    rsx! {
        document::Stylesheet { href: asset!("/assets/server-logs.css") }
        section { class: "card server-logs", aria_label: "Журнал сервера Quasar",
            div { class: "server-log-toolbar",
                label { class: "form-field", span { "Уровень" }
                    select { value: level, onchange: move |event| level.set(event.value()),
                        for (value, label) in [("", "Все уровни"), ("ERROR", "Ошибки"), ("WARN", "Предупреждения"), ("INFO", "Информация"), ("DEBUG", "Отладка"), ("TRACE", "Трассировка")] {
                            option { value, "{label}" }
                        }
                    }
                }
                label { class: "form-field server-log-search", span { "Поиск" }
                    input { r#type: "search", value: search, maxlength: "128", placeholder: "Модель, ошибка, подключение…", oninput: move |event| search.set(event.value()) }
                }
                label { class: "form-field", span { "Записей" }
                    select { value: limit, onchange: move |event| limit.set(event.value()),
                        for count in [100, 200, 500, 1000] { option { value: "{count}", "{count}" } }
                    }
                }
                button { class: "button secondary", aria_pressed: !automatic(), onclick: move |_| automatic.set(!automatic()),
                    Icon { name: toggle_icon, size: 16 }
                    if automatic() { "Пауза" } else { "Продолжить" }
                }
                button { class: "button secondary", disabled: loading(), onclick: move |_| refresh += 1,
                    Icon { name: "refresh", size: 16 } "Обновить"
                }
            }
            div { class: "server-log-summary",
                span { role: "status", if automatic() { "Автообновление каждые 2 секунды" } else { "Автообновление на паузе" } }
                label { input { r#type: "checkbox", checked: follow(), onchange: move |event| follow.set(event.checked()) } "К новым записям" }
                if let Some(value) = &snapshot { span { "Показано {value.entries.len()} из {value.matched}" } }
            }
            if let Some(message) = error() { p { class: "notice error", role: "alert", "{message}" } }
            div { id: "server-log-output", class: "server-log-output", tabindex: "0", role: "log", aria_label: "Записи журнала сервера", aria_live: "off", onwheel: move |_| follow.set(false),
                if let Some(value) = &snapshot {
                    if value.entries.is_empty() { p { class: "server-log-empty", if search().is_empty() && level().is_empty() { "Записей пока нет. Новые события появятся здесь." } else { "Нет записей по выбранным условиям." } } }
                    for entry in &value.entries {
                        article { class: "server-log-entry", key: "{value.started_at}:{entry.id}", "data-level": entry.level.clone(),
                            div { class: "server-log-meta",
                                time { datetime: entry.at.clone(), "{timestamp(&entry.at)}" }
                                strong { class: "server-log-level", "{entry.level}" }
                                span { class: "server-log-target", "{entry.target}" }
                            }
                            pre { "{entry.message}" }
                            if entry.truncated { small { "Длинная запись сокращена." } }
                        }
                    }
                } else if loading() { p { class: "server-log-empty", "Загружаем журнал…" } }
            }
            p { class: "field-help server-log-retention",
                if let Some(value) = &snapshot { "Журнал с {timestamp(&value.started_at)}. В памяти {value.retained} записей, не более {value.capacity}. " }
                "После перезапуска сервера журнал начинается заново. Уровни записи задаются в настройках сервера."
            }
        }
    }
}
