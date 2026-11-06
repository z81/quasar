use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{api, icons::Icon, schedules::time::timestamp, state::ControlState};

#[derive(Clone, PartialEq, Deserialize, Serialize)]
struct Schedule {
    enabled: bool,
    start: String,
    end: String,
    weekdays: Vec<u8>,
    allow_incoming_calls: bool,
}

impl Schedule {
    fn valid(&self) -> bool {
        !self.start.is_empty()
            && !self.end.is_empty()
            && self.start != self.end
            && !self.weekdays.is_empty()
    }
}

#[derive(Clone, Deserialize)]
struct Status {
    schedule: Schedule,
    timezone: String,
    current_enabled: bool,
    #[serde(default)]
    forced_target: Option<bool>,
    next_transition_at: Option<String>,
    next_enabled: Option<bool>,
    manual_override: Option<Override>,
    connected: bool,
    persistent: bool,
    delivery: Delivery,
}

#[derive(Clone, Deserialize)]
struct Override {
    enabled: bool,
    until: String,
}

#[derive(Clone, Deserialize)]
struct Delivery {
    status: String,
    confirmed_enabled: Option<bool>,
    error: Option<String>,
}

#[derive(Clone, Copy)]
struct Editor {
    status: Signal<Option<Status>>,
    draft: Signal<Option<Schedule>>,
    original: Signal<Option<Schedule>>,
    loading: Signal<bool>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    feedback: Signal<Option<(bool, String)>>,
    revision: Signal<u64>,
}

async fn refresh(state: ControlState, duid: &str, mut editor: Editor) {
    if !state.allowed() || *editor.busy.peek() || *editor.loading.peek() {
        return;
    }
    editor.loading.set(true);
    let revision = *editor.revision.peek();
    let result = api::get::<Status>(state, &format!("/devices/{}/dnd", api::encoded(duid))).await;
    if revision == *editor.revision.peek() && !*editor.busy.peek() {
        match result {
            Ok(status) => {
                if *editor.draft.peek() == *editor.original.peek() {
                    editor.draft.set(Some(status.schedule.clone()));
                    editor.original.set(Some(status.schedule.clone()));
                }
                editor.status.set(Some(status));
                editor.error.set(None);
            }
            Err(error) => editor.error.set(Some(error)),
        }
    }
    editor.loading.set(false);
}

async fn save(state: ControlState, duid: String, manual: Option<bool>, mut editor: Editor) {
    if *editor.busy.peek() || !state.allowed() {
        return;
    }
    let Some(draft) = editor.draft.peek().clone() else {
        return;
    };
    if manual.is_none() && !draft.valid() {
        return;
    }
    editor.busy.set(true);
    let revision = *editor.revision.peek();
    editor.revision.set(revision.wrapping_add(1));
    editor.feedback.set(None);
    let url = format!(
        "/devices/{}/dnd{}",
        api::encoded(&duid),
        if manual.is_some() { "/manual" } else { "" }
    );
    let body = manual.map_or_else(|| json!(draft), |enabled| json!({"enabled":enabled}));
    match api::mutate(
        state,
        if manual.is_some() { "POST" } else { "PUT" },
        &url,
        Some(&body),
    )
    .await
    {
        Ok(response) => match response.json::<Status>().await {
            Ok(status) => {
                if manual.is_none() {
                    editor.draft.set(Some(status.schedule.clone()));
                    editor.original.set(Some(status.schedule.clone()));
                }
                editor.status.set(Some(status));
                editor.error.set(None);
                editor.feedback.set(Some((
                    true,
                    if manual.is_some() {
                        "Режим сохранён"
                    } else {
                        "Расписание сохранено"
                    }
                    .into(),
                )));
                api::refresh(state).await;
            }
            Err(_) => editor.feedback.set(Some((
                false,
                "Не удалось прочитать ответ. Обновите статус перед повтором.".into(),
            ))),
        },
        Err(error) => editor.feedback.set(Some((false, error))),
    }
    editor.busy.set(false);
}

#[component]
pub fn DndSettings(state: ControlState, duid: String) -> Element {
    let mut editor = Editor {
        status: use_signal(|| None),
        draft: use_signal(|| None),
        original: use_signal(|| None),
        loading: use_signal(|| false),
        busy: use_signal(|| false),
        error: use_signal(|| None),
        feedback: use_signal(|| None),
        revision: use_signal(|| 0),
    };
    let polling_duid = duid.clone();
    use_future(move || {
        let duid = polling_duid.clone();
        async move {
            loop {
                refresh(state, &duid, editor).await;
                TimeoutFuture::new(5_000).await;
            }
        }
    });
    let status = (editor.status)();
    let draft = (editor.draft)();
    let changed = draft != (editor.original)();
    let busy = (editor.busy)();
    let editable =
        state.allowed() && !busy && status.as_ref().is_some_and(|status| status.persistent);
    let save_duid = duid.clone();
    let manual_duid = duid.clone();
    rsx! {
        document::Stylesheet { href: asset!("/assets/quiet-settings.css") }
        section { class: "card settings-card dnd-settings", aria_label: "Не беспокоить по расписанию",
            div { class: "card-heading",
                span { class: "card-kicker", Icon { name: "moon", size: 16 } "ТИХИЕ ЧАСЫ" }
                button { class: "text-button", disabled: busy || (editor.loading)(), onclick: move |_| {
                    let duid = duid.clone(); spawn(async move { refresh(state, &duid, editor).await; });
                }, Icon { name: "refresh", size: 14 } "Обновить" }
            }
            h2 { "Не беспокоить по расписанию" }
            p { class: "settings-intro", "Колонка сама включает и выключает штатный режим «Не беспокоить». Будильники, таймеры и ответы Алисы остаются доступны." }
            if let Some(error) = (editor.error)() { p { class: "form-feedback failure", role: "alert", "{error}" } }
            if let (Some(status), Some(form)) = (status, draft) {
                div { class: "quiet-status", role: "status",
                    strong {
                        if let Some(enabled) = status.delivery.confirmed_enabled.filter(|_| status.connected) {
                            if enabled { "Сейчас: не беспокоить" } else { "Сейчас: обычный режим" }
                        } else if let Some(enabled) = status.forced_target {
                            if enabled { "Задано: не беспокоить" } else { "Задано: обычный режим" }
                        } else { "Режим колонки пока неизвестен" }
                    }
                    span { class: "subtle-pill", match status.delivery.status.as_str() {
                        "confirmed" => "Подтверждено колонкой", "offline" => "Ждём подключения",
                        "pending" => "Ожидает отправки", "sent" => "Ждём подтверждения колонки",
                        "error" => "Не удалось применить", _ if !status.connected => "Колонка не в сети", _ => "Ручной режим",
                    } }
                    p { class: "form-help", "Часовой пояс: {status.timezone}" }
                    if let Some(next) = &status.next_transition_at {
                        p { class: "form-help", "Следующее переключение: {timestamp(next, &status.timezone)} — " if status.next_enabled == Some(true) { "не беспокоить" } else { "обычный режим" } }
                    }
                    if let Some(value) = &status.manual_override {
                        p { class: "form-help", "Вручную " if value.enabled { "включено" } else { "выключено" } " до {timestamp(&value.until, &status.timezone)}. Затем продолжится расписание." }
                    }
                    if let Some(error) = &status.delivery.error { p { class: "form-feedback failure", "{error}" } }
                }
                if !status.persistent { p { class: "form-feedback failure", "Для расписания подключите постоянное хранилище на сервере." } }
                form { onsubmit: move |event| {
                    event.prevent_default();
                    if !editable || !changed { return; }
                    let duid = save_duid.clone(); spawn(save(state, duid, None, editor));
                },
                    label { class: "toggle-field",
                        span { strong { "Включать по расписанию" } small { "Изменения применятся после сохранения" } }
                        input { r#type: "checkbox", checked: form.enabled, disabled: !editable, onchange: move |event| {
                            if let Some(form) = editor.draft.write().as_mut() { form.enabled = event.checked(); }
                        } }
                    }
                    div { class: "form-columns quiet-times",
                        label { class: "form-field", span { "Начало" }
                            input { r#type: "time", required: true, value: "{form.start}", disabled: !editable, oninput: move |event| { if let Some(form) = editor.draft.write().as_mut() { form.start = event.value(); } } }
                        }
                        label { class: "form-field", span { "Окончание" }
                            input { r#type: "time", required: true, value: "{form.end}", disabled: !editable, oninput: move |event| { if let Some(form) = editor.draft.write().as_mut() { form.end = event.value(); } } }
                        }
                    }
                    div { class: "form-field",
                        span { "Дни начала тихих часов" }
                        div { class: "schedule-weekdays", role: "group", aria_label: "Дни начала тихих часов",
                            for (day, label) in [(1,"Пн"),(2,"Вт"),(3,"Ср"),(4,"Чт"),(5,"Пт"),(6,"Сб"),(7,"Вс")] {
                                button { r#type: "button", class: if form.weekdays.contains(&day) { "schedule-weekday chosen" } else { "schedule-weekday" }, aria_pressed: form.weekdays.contains(&day), disabled: !editable,
                                    onclick: move |_| { if let Some(form) = editor.draft.write().as_mut() {
                                        if form.weekdays.contains(&day) { form.weekdays.retain(|value| *value != day); } else { form.weekdays.push(day); form.weekdays.sort(); }
                                    } }, "{label}"
                                }
                            }
                        }
                        small { "Например, понедельник 22:00–08:00 — с вечера понедельника до утра вторника." }
                        if form.weekdays.is_empty() { small { class: "failure", "Выберите хотя бы один день" } }
                        if form.start == form.end { small { class: "failure", "Начало и окончание должны отличаться" } }
                    }
                    label { class: "toggle-field",
                        span { strong { "Разрешать входящие звонки" } small { "Исключение для режима «Не беспокоить» по расписанию" } }
                        input { r#type: "checkbox", checked: form.allow_incoming_calls, disabled: !editable, onchange: move |event| { if let Some(form) = editor.draft.write().as_mut() { form.allow_incoming_calls = event.checked(); } } }
                    }
                    p { class: "form-help", "При отключении расписания восстановится режим, который был до его включения. Перед первым изменением сохраняется резервная копия настроек." }
                    div { class: "settings-actions",
                        button { class: "button primary", r#type: "submit", disabled: !editable || !changed || !form.valid(), if busy { span { class: "spinner" } "Сохраняем…" } else { Icon { name: "check", size: 16 } "Сохранить расписание" } }
                        if changed { button { class: "button secondary", r#type: "button", disabled: busy, onclick: move |_| {
                            let saved = editor.original.peek().clone(); editor.draft.set(saved); editor.feedback.set(None);
                        }, "Отменить изменения" } }
                    }
                }
                div { class: "quiet-manual",
                    button { class: "button secondary", disabled: !editable, onclick: move |_| {
                        let duid = manual_duid.clone(); spawn(save(state, duid, Some(!status.current_enabled), editor));
                    }, if status.current_enabled { "Выключить сейчас" } else { "Включить сейчас" } }
                    p { class: "form-help", if status.schedule.enabled { "Ручной режим действует до следующего переключения расписания." } else { "Ручной режим действует до следующего изменения." } }
                }
            } else if (editor.error)().is_none() { p { class: "settings-intro", "Загружаем расписание…" } }
            if let Some((success, message)) = (editor.feedback)() { p { class: if success { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" } }
        }
    }
}
