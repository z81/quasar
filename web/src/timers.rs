mod card;
mod editor;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;
use serde_json::Value;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Timer {
    id: String,
    title: String,
    duration_seconds: u32,
    remaining_ms: Option<i64>,
    next_run_at: Option<String>,
    enabled: bool,
    status: String,
    last_error: Option<String>,
    active_run_id: Option<String>,
}

impl Timer {
    fn active(&self) -> bool {
        self.active_run_id.is_some()
    }

    fn running(&self) -> bool {
        self.enabled && self.next_run_at.is_some() && !self.active()
    }

    fn remaining(&self, now: i64) -> i64 {
        if self.status == "paused" {
            return self.remaining_ms.unwrap_or_default().max(0);
        }
        if self.running() {
            return self
                .next_run_at
                .as_deref()
                .and_then(timestamp)
                .map(|deadline| (deadline - now).clamp(0, i64::from(self.duration_seconds) * 1000))
                .unwrap_or_default();
        }
        0
    }

    fn status_label(&self, remaining: i64) -> &'static str {
        match self.status.as_str() {
            "paused" => "На паузе",
            "sent" => "Отправлен на колонку",
            "ringing" => "Звонит",
            "stopped" => "Звонок остановлен",
            "expired" => "Время звонка истекло",
            "cancelled" => "Отменён",
            "missed" => "Пропущен",
            "failed" => "Не удалось запустить",
            _ if self.active() => "Запускаем сигнал",
            _ if self.running() && remaining == 0 => "Ждём срабатывания",
            _ if self.running() => "Идёт отсчёт",
            _ => "Состояние неизвестно",
        }
    }
}

#[derive(Clone, Deserialize, PartialEq)]
struct TimerList {
    items: Vec<Timer>,
    persistent: bool,
    server_time: String,
    #[serde(default)]
    native_alarm_supported: Option<bool>,
}

#[derive(Clone, Copy, PartialEq)]
struct PanelData {
    response: Signal<Option<TimerList>>,
    loading: Signal<bool>,
    error: Signal<Option<String>>,
    action_error: Signal<Option<String>>,
    feedback: Signal<Option<String>>,
    busy: Signal<bool>,
    editor_revision: Signal<u64>,
    request: Signal<u64>,
    clock_offset: Signal<i64>,
}

fn timestamp(value: &str) -> Option<i64> {
    let millis = js_sys::Date::parse(value);
    millis.is_finite().then_some(millis as i64)
}

fn clock() -> i64 {
    js_sys::Date::now() as i64
}

fn duration(milliseconds: i64) -> String {
    let seconds = (milliseconds.max(0) + 999) / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn endpoint(duid: &str) -> String {
    format!("/devices/{}/timers", api::encoded(duid))
}

async fn refresh(state: ControlState, duid: &str, mut data: PanelData) {
    if !state.allowed() || data.response.try_read().is_err() {
        return;
    }
    let request = *data.request.peek() + 1;
    data.request.set(request);
    data.loading.set(true);
    let started = clock();
    let result = api::get::<TimerList>(state, &endpoint(duid)).await;
    if data.request.try_peek().is_err() || *data.request.peek() != request {
        return;
    }
    match result {
        Ok(response) => {
            if let Some(server_time) = timestamp(&response.server_time) {
                data.clock_offset.set(server_time - (started + clock()) / 2);
            }
            data.response.set(Some(response));
            data.error.set(None);
        }
        Err(error) => data.error.set(Some(error)),
    }
    data.loading.set(false);
}

fn mutate(
    state: ControlState,
    duid: String,
    mut data: PanelData,
    method: &'static str,
    suffix: String,
    body: Option<Value>,
) {
    if !state.allowed() || *data.busy.peek() {
        return;
    }
    data.busy.set(true);
    data.action_error.set(None);
    data.feedback.set(None);
    // Invalidate a list request started before the mutation.
    let revision = *data.request.peek() + 1;
    data.request.set(revision);
    dioxus::core::spawn_forever(async move {
        let result = api::mutate(
            state,
            method,
            &format!("{}{suffix}", endpoint(&duid)),
            body.as_ref(),
        )
        .await;
        if data.busy.try_read().is_err() {
            return;
        }
        match result {
            Ok(_) => {
                let feedback = if method == "DELETE" {
                    "Таймер удалён"
                } else if suffix.ends_with("/stop") {
                    "Остановка звонка запрошена"
                } else if suffix.ends_with("/pause") {
                    "Таймер на паузе"
                } else if suffix.ends_with("/resume") {
                    "Отсчёт продолжен"
                } else if suffix.ends_with("/cancel") {
                    "Таймер отменён"
                } else {
                    "Таймер запущен"
                };
                data.feedback.set(Some(feedback.to_string()));
                if suffix.is_empty() {
                    let revision = *data.editor_revision.peek() + 1;
                    data.editor_revision.set(revision);
                }
                refresh(state, &duid, data).await;
            }
            Err(error) => data.action_error.set(Some(error)),
        }
        if let Ok(mut busy) = data.busy.try_write() {
            *busy = false;
        }
    });
}

#[component]
pub fn Timers(state: ControlState) -> Element {
    let device = state.device();
    rsx! {
        if device.is_none() {
            div { class: "card history-empty", Icon { name: "timer", size: 30 } h3 { "Выберите колонку" } p { "У каждой колонки — свои таймеры." } }
        }
        // A keyed list remounts station-scoped signals and polling on selection changes.
        for device in device {
            StationTimers { key: "{device.duid}", state, duid: device.duid }
        }
    }
}

#[component]
fn StationTimers(state: ControlState, duid: String) -> Element {
    let data = PanelData {
        response: use_signal(|| None),
        loading: use_signal(|| true),
        error: use_signal(|| None),
        action_error: use_signal(|| None),
        feedback: use_signal(|| None),
        busy: use_signal(|| false),
        editor_revision: use_signal(|| 0),
        request: use_signal(|| 0),
        clock_offset: use_signal(|| 0),
    };
    let mut now = use_signal(clock);
    let polling_duid = duid.clone();
    use_future(move || {
        let duid = polling_duid.clone();
        async move {
            loop {
                if !*data.busy.peek() {
                    refresh(state, &duid, data).await;
                }
                TimeoutFuture::new(5_000).await;
            }
        }
    });
    use_future(move || async move {
        loop {
            TimeoutFuture::new(1_000).await;
            now.set(clock());
        }
    });
    let response = (data.response)();
    let items = response
        .as_ref()
        .map(|response| response.items.clone())
        .unwrap_or_default();
    let refresh_duid = duid.clone();
    // Track the local tick, but use the current clock on renders caused by requests too.
    let _ = now();
    let server_now = clock() + (data.clock_offset)();
    rsx! {
        section { class: "schedule-intro", aria_label: "Как работают таймеры",
            Icon { name: "speaker", size: 18 }
            p { "Таймеры сохраняются на сервере: вкладку можно закрыть. Для сигнала нужны включённый сервер и подключённая колонка." }
        }
        if let Some(error) = (data.action_error)().or_else(|| (data.error)()) {
            div { class: "notice error", role: "alert", Icon { name: "alert", size: 18 } span { "{error}" } }
        }
        if let Some(feedback) = (data.feedback)() {
            p { class: "form-feedback success schedule-feedback", role: "status", "{feedback}" }
        }
        if response.as_ref().is_some_and(|response| !response.persistent) {
            div { class: "notice error", role: "alert", Icon { name: "alert", size: 18 } span { "Хранилище недоступно. Таймеры могут не сохраниться после перезапуска." } }
        }
        div { class: "timers-grid",
            if let Some(response) = response.as_ref() {
                for revision in [(data.editor_revision)()] {
                    editor::TimerEditor { key: "{revision}", state, duid: duid.clone(), data, unsupported: response.native_alarm_supported == Some(false) }
                }
            } else {
                div { class: "card schedule-form-placeholder", role: "status",
                    if (data.loading)() { span { class: "spinner" } "Загружаем таймеры…" }
                    else { Icon { name: "alert", size: 24 } p { "Не удалось загрузить таймеры. Попробуйте обновить." } }
                }
            }
            section { class: "schedule-list-section", aria_label: "Ваши таймеры",
                div { class: "schedule-list-heading",
                    div { h2 { "Ваши таймеры" } span { "{items.len()} сохранено" } }
                    button { class: "text-button", disabled: (data.loading)() || (data.busy)(), onclick: move |_| { let duid = refresh_duid.clone(); spawn(async move { refresh(state, &duid, data).await; }); }, Icon { name: "refresh", size: 15 } "Обновить" }
                }
                if items.is_empty() {
                    div { class: "card schedule-empty", Icon { name: "timer", size: 34 } h3 { "Всему своё время" } p { "Запустите таймер — колонка подаст сигнал, когда время выйдет." } }
                } else {
                    div { class: "schedule-list",
                        for item in items {
                            card::TimerCard { key: "{item.id}", state, duid: duid.clone(), data, item, now: server_now }
                        }
                    }
                }
            }
        }
    }
}
