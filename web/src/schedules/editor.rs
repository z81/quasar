use dioxus::prelude::*;
use serde_json::json;

use super::{mutate, time, PanelData, Schedule, WEEKDAYS};
use crate::api;
use crate::icons::Icon;
use crate::state::ControlState;

#[derive(Clone, PartialEq)]
struct Draft {
    kind: String,
    title: String,
    text: String,
    time: String,
    date: String,
    weekdays: Vec<u8>,
    timezone: String,
    repeat: bool,
    enabled: bool,
}

impl Draft {
    fn new(item: Option<&Schedule>, timezone: &str, now: &str) -> Self {
        if let Some(item) = item {
            return Self {
                kind: item.kind.clone(),
                title: item.title.clone(),
                text: item.text.clone(),
                time: item.time.clone(),
                date: item
                    .date
                    .clone()
                    .unwrap_or_else(|| time::tomorrow(now, &item.timezone)),
                weekdays: if item.weekdays.is_empty() {
                    vec![1, 2, 3, 4, 5]
                } else {
                    item.weekdays.clone()
                },
                timezone: item.timezone.clone(),
                repeat: !item.weekdays.is_empty(),
                enabled: item.enabled,
            };
        }
        let timezone = if timezone.is_empty() {
            time::browser_timezone()
        } else {
            timezone.to_string()
        };
        Self {
            kind: "alarm".to_string(),
            title: String::new(),
            text: String::new(),
            time: "07:00".to_string(),
            date: time::tomorrow(now, &timezone),
            weekdays: vec![1, 2, 3, 4, 5],
            timezone,
            repeat: false,
            enabled: true,
        }
    }

    fn valid(&self) -> bool {
        !self.title.trim().is_empty()
            && self.title.trim().chars().count() <= 120
            && !self.time.is_empty()
            && !self.timezone.trim().is_empty()
            && (self.kind != "reminder" || !self.text.trim().is_empty())
            && if self.repeat {
                !self.weekdays.is_empty()
            } else {
                !self.date.is_empty()
            }
    }
}

#[component]
pub(super) fn ScheduleEditor(
    state: ControlState,
    duid: String,
    mut data: PanelData,
    initial: Option<Schedule>,
    timezone: String,
    server_time: String,
    native_alarm_supported: Option<bool>,
) -> Element {
    let mut draft = use_signal(|| {
        let mut draft = Draft::new(initial.as_ref(), &timezone, &server_time);
        if initial.is_none() && native_alarm_supported == Some(false) {
            draft.kind = "reminder".to_string();
        }
        draft
    });
    let initial_id = initial.as_ref().map(|item| item.id.clone());
    let editing = initial.is_some();
    let value = draft();
    let busy = (data.busy)();
    let alarm_unavailable = native_alarm_supported == Some(false);
    rsx! {
        section { class: "card settings-card schedule-editor", aria_label: "Создание или изменение расписания", onmounted: move |event| async move {
            if editing { let _ = event.scroll_to(dioxus::html::ScrollBehavior::Instant).await; }
        },
            div { class: "schedule-editor-heading",
                span { class: "card-kicker", Icon { name: "alarm", size: 16 } if editing { "ИЗМЕНИТЬ РАСПИСАНИЕ" } else { "НОВОЕ РАСПИСАНИЕ" } }
                if editing { button { class: "text-button", disabled: busy, onclick: move |_| { data.editing.set(None); let revision=*data.editor_revision.peek()+1; data.editor_revision.set(revision); }, "Отмена" } }
            }
            h2 { if editing { "Уточним планы." } else { "Всё вовремя." } }
            p { class: "settings-intro", "Выберите время — колонка подаст сигнал или произнесёт ваше напоминание." }
            if alarm_unavailable { p { class: "schedule-error", role: "status", "Колонка не поддерживает сигнал будильника. Голосовые напоминания доступны." } }
            form { onsubmit: move |event| {
                event.prevent_default();
                let value=draft();
                if !value.valid() || busy || (alarm_unavailable && value.kind=="alarm") { return; }
                let body=json!({"kind":value.kind,"title":value.title.trim(),"text":value.text.trim(),"time":value.time,"date":if value.repeat {None}else{Some(value.date)},"weekdays":if value.repeat{value.weekdays}else{Vec::new()},"timezone":value.timezone.trim(),"enabled":value.enabled});
                let (method,suffix)=initial_id.as_ref().map(|id|("PUT",format!("/{}",api::encoded(id)))).unwrap_or(("POST",String::new()));
                mutate(state,duid.clone(),data,method,suffix,Some(body),true);
            },
                div { class: "schedule-kind-tabs", role: "group", aria_label: "Тип расписания",
                    for (kind,label,icon) in [("alarm","Будильник","alarm"),("reminder","Напоминание","bell")] {
                        button { r#type: "button", class: if value.kind==kind { "schedule-kind chosen" }else{"schedule-kind"}, aria_pressed: value.kind==kind, disabled: busy || (alarm_unavailable && kind=="alarm"), onclick: move |_| draft.write().kind=kind.to_string(), Icon { name: icon, size: 16 } "{label}" }
                    }
                }
                label { class: "form-field", span { "Название" }
                    input { required: true, maxlength: "120", value: "{value.title}", placeholder: if value.kind=="alarm" {"Доброе утро"}else{"Позвонить родителям"}, disabled: busy, oninput: move |event| draft.write().title=event.value() }
                }
                if value.kind=="reminder" {
                    label { class: "form-field", span { "Что сказать" }
                        textarea { required: true, maxlength: "1000", value: "{value.text}", placeholder: "Пора выходить на встречу", disabled: busy, oninput: move |event| draft.write().text=event.value() }
                    }
                }
                div { class: "form-columns schedule-time-inputs",
                    label { class: "form-field", span { "Время" } input { r#type: "time", required: true, step: "60", value: "{value.time}", disabled: busy, oninput: move |event| draft.write().time=event.value() } }
                    label { class: "form-field", span { "Повтор" }
                        select { value: if value.repeat {"weekly"}else{"once"}, disabled: busy, onchange: move |event| draft.write().repeat=event.value()=="weekly", option { value: "once", "Один раз" } option { value: "weekly", "По дням недели" } }
                    }
                }
                if value.repeat {
                    div { class: "schedule-days-field",
                        span { "Повторять в эти дни" }
                        div { class: "schedule-weekdays", role: "group", aria_label: "Дни недели",
                            for (day,label) in WEEKDAYS {
                                button { r#type: "button", class: if value.weekdays.contains(&day){"schedule-weekday chosen"}else{"schedule-weekday"}, aria_pressed: value.weekdays.contains(&day), disabled: busy,
                                    onclick: move |_| { let mut value=draft.write(); if value.weekdays.contains(&day) { value.weekdays.retain(|value| *value!=day); } else { value.weekdays.push(day); value.weekdays.sort(); } }, "{label}"
                                }
                            }
                        }
                        if value.weekdays.is_empty() { small { class: "failure", "Выберите хотя бы один день" } }
                    }
                } else {
                    label { class: "form-field", span { "Дата" } input { r#type: "date", required: true, value: "{value.date}", disabled: busy, oninput: move |event| draft.write().date=event.value() } }
                }
                label { class: "form-field", span { "Часовой пояс" }
                    input { required: true, list: "schedule-timezones", value: "{value.timezone}", placeholder: "Europe/Moscow", disabled: busy, oninput: move |event| draft.write().timezone=event.value() }
                    small { "Время выше относится к этому часовому поясу." }
                }
                datalist { id: "schedule-timezones", for zone in ["Europe/Moscow","Europe/Kaliningrad","Europe/Samara","Asia/Yekaterinburg","Asia/Novosibirsk","Asia/Krasnoyarsk","Asia/Irkutsk","Asia/Yakutsk","Asia/Vladivostok","Asia/Kamchatka","Europe/Berlin","Asia/Tbilisi","UTC"] { option { value: zone } } }
                label { class: "toggle-field", span { strong { "Включено" } small { "Выключенное расписание сохранится без запуска." } } input { r#type: "checkbox", checked: value.enabled, disabled: busy, onchange: move |event| draft.write().enabled=event.checked() } }
                div { class: "settings-actions", button { class: "button primary", r#type: "submit", disabled: busy || !value.valid() || (alarm_unavailable && value.kind=="alarm"),
                    if busy { span { class: "spinner" } "Сохраняем…" } else { Icon { name: "check", size: 16 } if editing { "Сохранить изменения" } else { "Добавить" } }
                } }
            }
        }
    }
}
