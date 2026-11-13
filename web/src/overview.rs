use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};

use crate::{
    api,
    icons::Icon,
    schedules::ScheduleList,
    state::{ControlState, Panel},
    timers::TimerList,
};

#[derive(Clone, Copy, PartialEq)]
struct TodayData {
    schedules: Signal<Option<ScheduleList>>,
    timers: Signal<Option<TimerList>>,
    schedule_error: Signal<Option<String>>,
    timer_error: Signal<Option<String>>,
    action_error: Signal<Option<String>>,
    busy: Signal<bool>,
    revision: Signal<u64>,
    clock_offset: Signal<i64>,
}

async fn refresh(state: ControlState, duid: &str, mut data: TodayData) {
    if !state.allowed() || data.busy.try_peek().is_err() || *data.busy.peek() {
        return;
    }
    let revision = *data.revision.peek();
    let base = format!("/devices/{}", api::encoded(duid));
    let schedules = api::get::<ScheduleList>(state, &format!("{base}/schedules")).await;
    if data.revision.try_peek().is_err() || *data.revision.peek() != revision {
        return;
    }
    match schedules {
        Ok(value) => {
            data.schedules.set(Some(value));
            data.schedule_error.set(None);
        }
        Err(error) => data.schedule_error.set(Some(error)),
    }
    let started = js_sys::Date::now() as i64;
    let timers = api::get::<TimerList>(state, &format!("{base}/timers")).await;
    if data.revision.try_peek().is_err() || *data.revision.peek() != revision {
        return;
    }
    match timers {
        Ok(value) => {
            if let Some(server_time) = crate::timers::timestamp(&value.server_time) {
                data.clock_offset
                    .set(server_time - (started + js_sys::Date::now() as i64) / 2);
            }
            data.timers.set(Some(value));
            data.timer_error.set(None);
        }
        Err(error) => data.timer_error.set(Some(error)),
    }
}

fn change(
    state: ControlState,
    duid: String,
    mut data: TodayData,
    method: &'static str,
    path: String,
    body: Option<Value>,
) {
    if !state.allowed() || *data.busy.peek() {
        return;
    }
    data.busy.set(true);
    data.action_error.set(None);
    let revision = *data.revision.peek() + 1;
    data.revision.set(revision);
    dioxus::core::spawn_forever(async move {
        let result = api::mutate(
            state,
            method,
            &format!("/devices/{}/{path}", api::encoded(&duid)),
            body.as_ref(),
        )
        .await;
        if data.busy.try_peek().is_err() {
            return;
        }
        if let Err(error) = result {
            data.action_error.set(Some(error));
        }
        data.busy.set(false);
        refresh(state, &duid, data).await;
    });
}

#[component]
pub fn Today(state: ControlState) -> Element {
    rsx! {
        document::Stylesheet { href: asset!("/assets/ocean-overview.css") }
        for device in state.device() {
            StationToday { key: "{device.duid}", state, duid: device.duid }
        }
    }
}

#[component]
fn StationToday(mut state: ControlState, duid: String) -> Element {
    let data = TodayData {
        schedules: use_signal(|| None),
        timers: use_signal(|| None),
        schedule_error: use_signal(|| None),
        timer_error: use_signal(|| None),
        action_error: use_signal(|| None),
        busy: use_signal(|| false),
        revision: use_signal(|| 0),
        clock_offset: use_signal(|| 0),
    };
    let mut now = use_signal(|| js_sys::Date::now() as i64);
    let polling_duid = duid.clone();
    use_future(move || {
        let duid = polling_duid.clone();
        async move {
            loop {
                refresh(state, &duid, data).await;
                TimeoutFuture::new(5_000).await;
            }
        }
    });
    use_future(move || async move {
        loop {
            now.set(js_sys::Date::now() as i64 + *data.clock_offset.peek());
            TimeoutFuture::new(250).await;
        }
    });
    let schedule_list = (data.schedules)();
    let timer_list = (data.timers)();
    let alarm = schedule_list
        .as_ref()
        .and_then(|list| {
            list.items
                .iter()
                .filter(|item| item.alarm())
                .min_by_key(|item| {
                    (
                        !item.active(),
                        !item.enabled,
                        item.next_run_at
                            .as_deref()
                            .and_then(crate::timers::timestamp)
                            .unwrap_or(i64::MAX),
                    )
                })
        })
        .cloned();
    let timer = timer_list
        .as_ref()
        .and_then(|list| {
            list.items.iter().min_by_key(|item| {
                (
                    !item.active(),
                    !item.running(),
                    item.status != "paused",
                    item.next_run_at
                        .as_deref()
                        .and_then(crate::timers::timestamp)
                        .unwrap_or(i64::MAX),
                )
            })
        })
        .cloned();
    let alarm_disabled = (data.busy)() || (data.schedule_error)().is_some() || !state.allowed();
    let timer_disabled = (data.busy)() || (data.timer_error)().is_some() || !state.allowed();
    let toggle_duid = duid.clone();
    rsx! {
        section { class: "ocean-today", aria_label: "На сегодня",
            div { class: "ocean-section-heading", h2 { "На сегодня" }
                button { class: "text-button", onclick: move |_| state.panel.set(Panel::Schedules), "Все" Icon { name: "chevron", size: 16 } }
            }
            div { class: "ocean-today-grid",
                article { class: "ocean-alarm-card",
                    div { class: "ocean-card-heading", h3 { Icon { name: "alarm", size: 27 } "Будильник" }
                        if let Some(item) = &alarm {
                            {let id = item.id.clone(); let enabled = item.enabled; let active = item.active(); rsx! {
                                button { r#type: "button", class: if enabled { "ocean-switch is-on" } else { "ocean-switch" }, role: "switch", aria_checked: enabled,
                                    aria_label: "Включить будильник: {item.title}", disabled: alarm_disabled || active,
                                    onclick: move |_| change(state,toggle_duid.clone(),data,"PATCH",format!("schedules/{}",api::encoded(&id)),Some(json!({"enabled":!enabled}))),
                                    span {} }
                            }}
                        }
                    }
                    if let Some(item) = &alarm {
                        button { r#type: "button", class: "ocean-alarm-time", aria_label: "Открыть будильники", onclick: move |_| state.panel.set(Panel::Schedules), "{item.time}" }
                        p { class: "ocean-alarm-caption", "{item.repetition()}" }
                        if item.active() || !item.enabled || matches!(item.status.as_str(), "failed" | "missed") {
                            p { class: "ocean-alarm-status", "{item.status_label()}" }
                        } else if !item.title.is_empty() { p { class: "ocean-alarm-status", title: "{item.title}", "{item.title}" } }
                    } else if (data.schedule_error)().is_some() {
                        p { class: "ocean-card-empty", "Не удалось загрузить будильники" }
                    } else if schedule_list.is_none() {
                        p { class: "ocean-card-empty", role: "status", "Загружаем…" }
                    } else {
                        button { r#type: "button", class: "ocean-card-create", onclick: move |_| state.panel.set(Panel::Schedules), Icon { name: "plus", size: 28 } "Добавить" }
                        p { class: "ocean-alarm-caption", "Утро начнётся вовремя" }
                    }
                    div { class: "ocean-alarm-rings", aria_hidden: "true" }
                }
                article { class: "ocean-timer-card",
                    div { class: "ocean-card-heading", h3 { Icon { name: "timer", size: 26 } "Таймер" }
                        if let Some(item) = &timer { span { class: "ocean-timer-status", title: "{item.status_label(item.remaining(now()))}", span {} "{item.status_label(item.remaining(now()))}" } }
                    }
                    if let Some(item) = timer {
                        {let remaining = item.remaining(now()); let active = item.active(); let running = item.running(); let paused = item.status == "paused";
                        let displayed = if !running && !paused && !active { i64::from(item.duration_seconds)*1000 } else { remaining };
                        let full_duration = crate::timers::duration(displayed);
                        let duration = full_duration.strip_prefix("00:").unwrap_or(&full_duration).to_owned();
                        let progress = if item.duration_seconds == 0 { 0.0 } else { (remaining as f64 / (f64::from(item.duration_seconds)*1000.0)).clamp(0.0,1.0) *100.0 };
                        let (action,label,icon) = if active { ("stop","Остановить звонок","stop") } else if running { ("pause","Приостановить таймер","pause") } else if paused { ("resume","Продолжить таймер","play") } else { ("restart","Запустить таймер снова","play") };
                        let play_duid = duid.clone(); let play_id = item.id.clone(); let stop_duid = duid.clone(); let stop_id = item.id.clone(); rsx! {
                            button { r#type: "button", class: if duration.len() > 5 { "ocean-timer-time is-long" } else { "ocean-timer-time" }, aria_label: "Открыть таймеры", onclick: move |_| state.panel.set(Panel::Timers),
                                span { role: "timer", aria_label: "Осталось времени", "{duration}" }
                            }
                            p { class: "ocean-timer-unit", if duration.len() > 5 { "ч : мин : сек" } else { "мин : сек" } }
                            div { class: "ocean-timer-ring",
                                svg { view_box: "0 0 112 112", "aria-hidden": "true", circle { class: "ocean-countdown-track", cx: "56", cy: "56", r: "48" }
                                    circle { class: "ocean-countdown-progress", cx: "56", cy: "56", r: "48", path_length: "100", stroke_dasharray: "{progress} 100" }
                                }
                                button { r#type: "button", class: "ocean-timer-play", disabled: timer_disabled, aria_label: label, title: label,
                                    onclick: move |_| change(state,play_duid.clone(),data,"POST",format!("timers/{}/{action}",api::encoded(&play_id)),None), Icon { name: icon, size: 24 } }
                            }
                            if running || paused {
                                button { r#type: "button", class: "ocean-timer-stop", disabled: timer_disabled, aria_label: "Отменить таймер", title: "Отменить таймер",
                                    onclick: move |_| change(state,stop_duid.clone(),data,"POST",format!("timers/{}/cancel",api::encoded(&stop_id)),None), Icon { name: "stop", size: 18 } }
                            }
                        }}
                    } else if (data.timer_error)().is_some() {
                        p { class: "ocean-card-empty", "Не удалось загрузить таймеры" }
                    } else if timer_list.is_none() {
                        p { class: "ocean-card-empty", role: "status", "Загружаем…" }
                    } else {
                        button { r#type: "button", class: "ocean-card-create", onclick: move |_| state.panel.set(Panel::Timers), Icon { name: "plus", size: 28 } "Запустить" }
                        p { class: "ocean-timer-unit", "Время для короткой паузы" }
                    }
                }
            }
            if let Some(error) = (data.action_error)().or_else(|| (data.schedule_error)()).or_else(|| (data.timer_error)()) {
                p { class: "ocean-overview-error", role: "alert", "{error}" }
            }
            if schedule_list.as_ref().is_some_and(|list| !list.persistent) || timer_list.as_ref().is_some_and(|list| !list.persistent) {
                p { class: "ocean-overview-error", role: "status", "Хранилище недоступно: расписание может не сохраниться после перезапуска." }
            }
        }
    }
}

#[component]
pub fn StationOverview(mut state: ControlState) -> Element {
    let device = state.device();
    let connected = device
        .as_ref()
        .is_some_and(|device| device.connected == Some(true))
        && state.server_error.read().is_none();
    let enabled = state.can_send();
    rsx! {
        document::Stylesheet { href: asset!("/assets/ocean-overview.css") }
        section { class: "ocean-station", aria_label: "Ваша колонка",
            div { class: "ocean-station-heading",
                h2 { if let Some(device) = &device { "{device.label()}" } else { "Ваша колонка" } }
                span { class: if connected { "ocean-station-status connected" } else { "ocean-station-status" }, span { class: "status-dot" }
                    if state.server_error.read().is_some() { "Нет связи с сервером" }
                    else if let Some(device) = &device { "{device.connection_label()}" }
                    else { "Выберите колонку" }
                }
            }
            crate::station::StationModel { state, duid: (state.selected)(), show_details: false }
            div { class: "ocean-station-controls", role: "group", aria_label: "Быстрое управление станцией",
                for (panel,icon,label) in [(Panel::StationSettings,"volume-down","Настройки шёпота"),(Panel::StationSettings,"moon","Настройки «Не беспокоить»"),(Panel::DeviceTools,"bluetooth","Bluetooth")] {
                    button { r#type: "button", class: "ocean-station-button", aria_label: label, title: label, disabled: device.is_none(), onclick: move |_| state.panel.set(panel), Icon { name: icon, size: 26 } }
                }
                for (action,icon,label) in [("sound_quiter","minus","Тише"),("audio_stop","stop","Остановить воспроизведение"),("sound_louder","plus","Громче")] {
                    button { r#type: "button", class: "ocean-station-button", aria_label: label, title: label, disabled: !enabled, onclick: move |_| api::quick_action(state,action,label), Icon { name: icon, size: 26 } }
                }
            }
        }
    }
}
