mod editor;
mod time;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;
use serde_json::Value;

use crate::api;
use crate::icons::Icon;
use crate::state::ControlState;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Schedule {
    id: String,
    kind: String,
    title: String,
    text: String,
    time: String,
    date: Option<String>,
    weekdays: Vec<u8>,
    timezone: String,
    enabled: bool,
    next_run_at: Option<String>,
    last_run_at: Option<String>,
    status: String,
    last_error: Option<String>,
    active_run_id: Option<String>,
    ringing_until: Option<String>,
    snoozed_until: Option<String>,
}

impl Schedule {
    fn alarm(&self) -> bool {
        self.kind == "alarm"
    }

    fn active(&self) -> bool {
        self.alarm() && self.active_run_id.is_some()
    }

    fn label(&self) -> &'static str {
        if self.alarm() {
            "Будильник"
        } else {
            "Напоминание"
        }
    }

    fn status_label(&self) -> &'static str {
        match self.status.as_str() {
            "ringing" => "Звонит",
            "sent" if self.alarm() && self.active() => "Отправлен на колонку",
            "sent" => "Отправлено на колонку",
            "snoozed" => "Отложен",
            "disabled" => "Выключен",
            "missed" => "Пропущен",
            "failed" => "Не удалось запустить",
            "stopped" => "Остановлен",
            "expired" => "Время звонка истекло",
            _ if !self.enabled => "Выключен",
            _ => "Запланирован",
        }
    }

    fn repetition(&self) -> String {
        if let Some(date) = &self.date {
            return time::calendar_date(date);
        }
        if self.weekdays.len() == 7 {
            return "Каждый день".to_string();
        }
        if self.weekdays == [1, 2, 3, 4, 5] {
            return "По будням".to_string();
        }
        self.weekdays
            .iter()
            .filter_map(|day| WEEKDAYS.get(usize::from(*day).wrapping_sub(1)))
            .map(|(_, label)| *label)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

const WEEKDAYS: [(u8, &str); 7] = [
    (1, "Пн"),
    (2, "Вт"),
    (3, "Ср"),
    (4, "Чт"),
    (5, "Пт"),
    (6, "Сб"),
    (7, "Вс"),
];

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ScheduleList {
    items: Vec<Schedule>,
    persistent: bool,
    timezone: String,
    server_time: String,
    #[serde(default)]
    native_alarm_supported: Option<bool>,
}

#[derive(Clone, Copy, PartialEq)]
pub struct PanelData {
    response: Signal<Option<ScheduleList>>,
    loading: Signal<bool>,
    error: Signal<Option<String>>,
    action_error: Signal<Option<String>>,
    feedback: Signal<Option<String>>,
    busy: Signal<bool>,
    editing: Signal<Option<Schedule>>,
    editor_revision: Signal<u64>,
    request: Signal<u64>,
}

fn endpoint(duid: &str) -> String {
    format!("/devices/{}/schedules", api::encoded(duid))
}

async fn refresh(state: ControlState, duid: &str, mut data: PanelData) {
    if !state.allowed() || data.response.try_read().is_err() {
        return;
    }
    let request = *data.request.peek() + 1;
    data.request.set(request);
    data.loading.set(true);
    let result = api::get::<ScheduleList>(state, &endpoint(duid)).await;
    if data.request.try_peek().is_err() || *data.request.peek() != request {
        return;
    }
    match result {
        Ok(response) => {
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
    reset_editor: bool,
) {
    if !state.allowed() || *data.busy.peek() {
        return;
    }
    data.busy.set(true);
    data.action_error.set(None);
    data.feedback.set(None);
    dioxus::core::spawn_forever(async move {
        let result = api::mutate(
            state,
            method,
            &format!("{}{suffix}", endpoint(&duid)),
            body.as_ref(),
        )
        .await;
        // The selected station/panel may have been unmounted while the request completed.
        if data.busy.try_read().is_err() {
            return;
        }
        match result {
            Ok(_) => {
                data.feedback.set(Some(
                    if method == "DELETE" {
                        "Удалено"
                    } else if suffix.ends_with("/stop") {
                        "Остановка запрошена"
                    } else if suffix.ends_with("/snooze") {
                        "Отложено на 10 минут"
                    } else {
                        "Сохранено"
                    }
                    .to_string(),
                ));
                if reset_editor {
                    data.editing.set(None);
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
pub fn Schedules(state: ControlState) -> Element {
    let device = state.device();
    rsx! {
        if device.is_none() {
            div { class: "card history-empty", Icon { name: "alarm", size: 30 } h3 { "Выберите колонку" } p { "Для каждой колонки — свои будильники и напоминания." } }
        }
        // Keys participate in reconciliation inside a dynamic list. A key on
        // one static component does not restart its station-scoped hooks.
        for device in device {
            StationSchedules { key: "{device.duid}", state, duid: device.duid }
        }
    }
}

#[component]
fn StationSchedules(state: ControlState, duid: String) -> Element {
    let data = PanelData {
        response: use_signal(|| None),
        loading: use_signal(|| true),
        error: use_signal(|| None),
        action_error: use_signal(|| None),
        feedback: use_signal(|| None),
        busy: use_signal(|| false),
        editing: use_signal(|| None),
        editor_revision: use_signal(|| 0),
        request: use_signal(|| 0),
    };
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
    let response = (data.response)();
    let items = response
        .as_ref()
        .map(|response| response.items.clone())
        .unwrap_or_default();
    let refresh_duid = duid.clone();
    rsx! {
        section { class: "schedule-intro", aria_label: "Работа расписания",
            Icon { name: "speaker", size: 18 }
            p { "Для срабатывания нужны включённый сервер и подключённая колонка. Если связи нет, попробуем доставить сигнал в течение 5 минут." }
        }
        if let Some(error) = (data.action_error)().or_else(|| (data.error)()) {
            div { class: "notice error", role: "alert", Icon { name: "alert", size: 18 } span { "{error}" } }
        }
        if let Some(feedback) = (data.feedback)() {
            p { class: "form-feedback success schedule-feedback", role: "status", "{feedback}" }
        }
        if response.as_ref().is_some_and(|response| !response.persistent) {
            div { class: "notice error", role: "alert", Icon { name: "alert", size: 18 } span { "Постоянное хранилище недоступно. Расписание может не сохраниться после перезапуска." } }
        }
        div { class: "schedules-grid",
            if let Some(response) = response.as_ref() {
                for revision in [(data.editor_revision)()] {
                    editor::ScheduleEditor { key: "{revision}", state, duid: duid.clone(), data, initial: (data.editing)(), timezone: response.timezone.clone(), server_time: response.server_time.clone(), native_alarm_supported: response.native_alarm_supported }
                }
            } else {
                div { class: "card schedule-form-placeholder", role: "status",
                    if (data.loading)() { span { class: "spinner" } "Загружаем расписание…" }
                    else { Icon { name: "alert", size: 24 } p { "Не удалось загрузить расписание. Попробуйте обновить." } }
                }
            }
            section { class: "schedule-list-section", aria_label: "Сохранённые будильники и напоминания",
                div { class: "schedule-list-heading",
                    div { h2 { "Ваше расписание" } span { "{items.len()} сохранено" } }
                    button { class: "text-button", disabled: (data.loading)(), onclick: move |_| { let duid = refresh_duid.clone(); spawn(async move { refresh(state,&duid,data).await; }); }, Icon { name: "refresh", size: 15 } "Обновить" }
                }
                if items.is_empty() {
                    div { class: "card schedule-empty", Icon { name: "alarm", size: 34 } h3 { "Не пропустите важное" } p { "Добавьте будильник на утро или напоминание о ваших планах." } }
                } else {
                    div { class: "schedule-list",
                        for item in items {
                            ScheduleCard { key: "{item.id}", state, duid: duid.clone(), data, item }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ScheduleCard(state: ControlState, duid: String, mut data: PanelData, item: Schedule) -> Element {
    let mut confirm_delete = use_signal(|| false);
    let busy = (data.busy)();
    let active = item.active();
    let kind_icon = if item.alarm() { "alarm" } else { "bell" };
    let toggle_duid = duid.clone();
    let toggle_id = item.id.clone();
    let edit_item = item.clone();
    let delete_duid = duid.clone();
    let delete_id = item.id.clone();
    let stop_duid = duid.clone();
    let stop_id = item.id.clone();
    let snooze_duid = duid;
    let snooze_id = item.id.clone();
    let next_label = item
        .snoozed_until
        .as_deref()
        .or_else(|| {
            item.enabled
                .then_some(item.next_run_at.as_deref())
                .flatten()
        })
        .map(|value| time::timestamp(value, &item.timezone));
    let last_label = item
        .last_run_at
        .as_deref()
        .map(|value| time::timestamp(value, &item.timezone));
    let until_label = item
        .ringing_until
        .as_deref()
        .map(|value| time::timestamp(value, &item.timezone));
    rsx! {
        article { class: if active { "card schedule-item active-alarm" } else { "card schedule-item" },
            div { class: "schedule-item-heading",
                span { class: "card-kicker", Icon { name: kind_icon, size: 15 } "{item.label()}" }
                label { class: "schedule-toggle",
                    span { if item.enabled { "Включён" } else { "Выключен" } }
                    input { r#type: "checkbox", aria_label: "Включить: {item.title}", checked: item.enabled, disabled: busy || active,
                        onchange: move |event| mutate(state,toggle_duid.clone(),data,"PATCH",format!("/{}",api::encoded(&toggle_id)),Some(serde_json::json!({"enabled":event.checked()})),false)
                    }
                }
            }
            div { class: "schedule-time-row", strong { class: "schedule-time", "{item.time}" } span { "{item.repetition()}" } }
            h3 { "{item.title}" }
            if !item.alarm() { p { class: "schedule-message", "{item.text}" } }
            div { class: "schedule-meta",
                span { Icon { name: "globe", size: 13 } "{item.timezone}" }
                span { class: match item.status.as_str() { "ringing" | "snoozed" => "schedule-status highlighted", "failed" | "missed" => "schedule-status failure", _ => "schedule-status" }, "{item.status_label()}" }
            }
            if let Some(next) = next_label {
                p { class: "schedule-next", "Следующий запуск: {next}" }
            } else if let Some(last) = last_label {
                p { class: "schedule-next", "Последний запуск: {last}" }
            }
            if active {
                div { class: "schedule-ringing",
                    p { if item.status == "ringing" { "Колонка сообщила, что будильник звонит." } else { "Сигнал отправлен. Ждём подтверждение колонки." } }
                    if let Some(until) = until_label { small { "Автоостановка: {until}" } }
                    div {
                        button { class: "button primary", disabled: busy, onclick: move |_| mutate(state,stop_duid.clone(),data,"POST",format!("/{}/stop",api::encoded(&stop_id)),None,false), Icon { name: "stop", size: 15 } "Остановить звонок" }
                        button { class: "button secondary", disabled: busy, onclick: move |_| mutate(state,snooze_duid.clone(),data,"POST",format!("/{}/snooze",api::encoded(&snooze_id)),None,false), Icon { name: "history", size: 15 } "Ещё 10 минут" }
                    }
                    small { "Остановка завершит звонок всех активных будильников этой колонки. «Ещё 10 минут» перенесёт только этот будильник и также остановит остальные." }
                }
            }
            if let Some(error) = item.last_error.as_ref() { p { class: "schedule-error", role: "status", Icon { name: "alert", size: 14 } "{error}" } }
            div { class: "schedule-item-actions",
                button { class: "text-button", disabled: busy || active, onclick: move |_| { data.editing.set(Some(edit_item.clone())); let revision=*data.editor_revision.peek()+1; data.editor_revision.set(revision); data.feedback.set(None); }, Icon { name: "edit", size: 14 } "Изменить" }
                if confirm_delete() {
                    span { class: "schedule-delete-confirm", "Удалить?"
                        button { class: "text-button danger", disabled: busy || active, onclick: move |_| { let reset=data.editing.peek().as_ref().is_some_and(|editing| editing.id==delete_id); mutate(state,delete_duid.clone(),data,"DELETE",format!("/{}",api::encoded(&delete_id)),None,reset); confirm_delete.set(false); }, "Да" }
                        button { class: "text-button", disabled: busy, onclick: move |_| confirm_delete.set(false), "Отмена" }
                    }
                } else {
                    button { class: "text-button", disabled: busy || active, onclick: move |_| confirm_delete.set(true), Icon { name: "trash", size: 14 } "Удалить" }
                }
            }
        }
    }
}
