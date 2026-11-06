use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;
use serde_json::json;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Deserialize)]
struct SoundStatus {
    supported: bool,
    connected: bool,
    persistent: bool,
    configured: bool,
    reason: Option<String>,
    enabled: bool,
    room_correction: bool,
    gains: Vec<f64>,
    equalizer_confirmed: Option<bool>,
}

#[derive(Clone, PartialEq)]
struct SoundForm {
    enabled: bool,
    room: bool,
    gains: Vec<String>,
}

impl SoundForm {
    fn from_status(status: &SoundStatus) -> Self {
        Self {
            enabled: status.enabled,
            room: status.room_correction,
            gains: status.gains.iter().map(ToString::to_string).collect(),
        }
    }

    fn gains(&self) -> Option<[i8; 5]> {
        self.gains
            .iter()
            .map(|value| {
                let value = value.trim().parse::<f64>().ok()?;
                (value.is_finite() && value.fract() == 0.0 && (-12.0..=12.0).contains(&value))
                    .then_some(value as i8)
            })
            .collect::<Option<Vec<_>>>()?
            .try_into()
            .ok()
    }
}

#[derive(Clone, Copy)]
struct Editor {
    status: Signal<Option<SoundStatus>>,
    draft: Signal<Option<SoundForm>>,
    original: Signal<Option<SoundForm>>,
    loading: Signal<bool>,
    error: Signal<Option<String>>,
    busy: Signal<bool>,
    feedback: Signal<Option<(bool, String)>>,
    revision: Signal<u64>,
}

async fn refresh(state: ControlState, duid: &str, mut editor: Editor) {
    if !state.allowed() || *editor.loading.peek() || *editor.busy.peek() {
        return;
    }
    editor.loading.set(true);
    let revision = *editor.revision.peek();
    let url = format!("/devices/{}/sound", api::encoded(duid));
    let response = api::get::<SoundStatus>(state, &url).await;
    if *editor.revision.peek() != revision || *editor.busy.peek() {
        editor.loading.set(false);
        return;
    }
    match response {
        Ok(status) => {
            let pristine = *editor.draft.peek() == *editor.original.peek();
            if pristine {
                let form = SoundForm::from_status(&status);
                editor.draft.set(Some(form.clone()));
                editor.original.set(Some(form));
            }
            editor.status.set(Some(status));
            editor.error.set(None);
        }
        Err(error) => editor.error.set(Some(error)),
    }
    editor.loading.set(false);
}

async fn request_refresh(state: ControlState, duid: &str, mut editor: Editor) {
    if !state.allowed() || *editor.loading.peek() || *editor.busy.peek() {
        return;
    }
    editor.loading.set(true);
    let revision = *editor.revision.peek();
    let url = format!("/devices/{}/sound", api::encoded(duid));
    let result = api::mutate(state, "PUT", &url, Some(&json!({"action":"refresh"}))).await;
    editor.loading.set(false);
    if *editor.revision.peek() != revision || *editor.busy.peek() {
        return;
    }
    if let Err(error) = result {
        editor.feedback.set(Some((false, error)));
    }
    refresh(state, duid, editor).await;
}

async fn save(state: ControlState, duid: String, body: serde_json::Value, mut editor: Editor) {
    if *editor.busy.peek() {
        return;
    }
    editor.busy.set(true);
    let revision = *editor.revision.peek();
    editor.revision.set(revision.wrapping_add(1));
    editor.feedback.set(None);
    let url = format!("/devices/{}/sound", api::encoded(&duid));
    match api::mutate(state, "PUT", &url, Some(&body)).await {
        Ok(response) => {
            #[derive(Deserialize)]
            struct Saved {
                notified: bool,
                sound: SoundStatus,
            }
            match response.json::<Saved>().await {
                Ok(saved) => {
                    let form = SoundForm::from_status(&saved.sound);
                    editor.draft.set(Some(form.clone()));
                    editor.original.set(Some(form));
                    editor.status.set(Some(saved.sound));
                    editor.feedback.set(Some((
                        true,
                        if saved.notified {
                            "Сохранено. Колонке отправлено обновление настроек."
                        } else {
                            "Сохранено. Колонка получит настройки после подключения."
                        }
                        .to_owned(),
                    )));
                    api::refresh(state).await;
                }
                Err(_) => editor.feedback.set(Some((
                    false,
                    "Ответ не удалось прочитать. Обновите настройки, чтобы проверить результат."
                        .into(),
                ))),
            }
        }
        Err(error) => editor.feedback.set(Some((false, error))),
    }
    editor.busy.set(false);
}

#[component]
pub fn SoundSettings(state: ControlState, duid: String) -> Element {
    let mut editor = Editor {
        status: use_signal(|| None),
        draft: use_signal(|| None),
        original: use_signal(|| None),
        loading: use_signal(|| false),
        error: use_signal(|| None),
        busy: use_signal(|| false),
        feedback: use_signal(|| None),
        revision: use_signal(|| 0),
    };
    let polling_duid = duid.clone();
    use_future(move || {
        let duid = polling_duid.clone();
        async move {
            let mut requested = false;
            loop {
                refresh(state, &duid, editor).await;
                if !requested
                    && editor
                        .status
                        .peek()
                        .as_ref()
                        .is_some_and(|status| status.supported && status.connected)
                {
                    requested = true;
                    request_refresh(state, &duid, editor).await;
                }
                TimeoutFuture::new(6_000).await;
            }
        }
    });
    let status = (editor.status)();
    let draft = (editor.draft)();
    let changed = draft != (editor.original)();
    let busy = (editor.busy)();
    let allowed = state.allowed() && !busy;
    let refresh_duid = duid.clone();
    let save_duid = duid.clone();
    rsx! {
        document::Stylesheet { href: asset!("/assets/sound-settings.css") }
        section { class: "card settings-card sound-settings", aria_label: "Настройки звука",
            div { class: "card-heading",
                span { class: "card-kicker", Icon { name: "speaker", size: 16 } "ЗВУК" }
                button { class: "text-button", disabled: busy || (editor.loading)(), onclick: move |_| {
                    let duid = refresh_duid.clone(); spawn(async move { request_refresh(state, &duid, editor).await; });
                }, Icon { name: "refresh", size: 14 } "Обновить" }
            }
            h2 { "Эквалайзер и Room Correction" }
            if let Some(error) = (editor.error)() { p { class: "form-feedback failure", role: "alert", "{error}" } }
            if let Some(status) = status {
                if !status.supported {
                    p { class: "settings-intro", "{status.reason.as_deref().unwrap_or_default()}" }
                } else if let Some(form) = draft {
                    p { class: "settings-intro", "Настройте звучание музыки и адаптацию к акустике комнаты. Изменения применяются после сохранения." }
                    form { onsubmit: move |event| {
                        event.prevent_default();
                        if !allowed || !changed { return; }
                        let Some(form) = (editor.draft)() else { return; };
                        let Some(gains) = form.gains() else { return; };
                        let body = json!({"action":"apply","enabled":form.enabled,"room_correction":form.room,"gains":gains});
                        let duid = save_duid.clone();
                        spawn(async move { save(state, duid, body, editor).await; });
                    },
                        label { class: "toggle-field sound-first-toggle",
                            span { strong { "Эквалайзер" } small { "Пять полос: от баса до высоких частот" } }
                            input { id: "sound-equalizer-enabled", r#type: "checkbox", checked: form.enabled, disabled: !allowed, onchange: move |event| {
                                if let Some(form) = editor.draft.write().as_mut() { form.enabled = event.checked(); } editor.feedback.set(None);
                            } }
                        }
                        div { class: "sound-bands", role: "group", aria_label: "Полосы эквалайзера",
                            for (index, label) in ["60 Гц", "230 Гц", "910 Гц", "3,6 кГц", "14 кГц"].into_iter().enumerate() {
                                {
                                    let value = form.gains.get(index).cloned().unwrap_or_default();
                                    let valid = value.parse::<f64>().is_ok_and(|value| value.is_finite() && value.fract() == 0.0 && (-12.0..=12.0).contains(&value));
                                    let slider = if valid { value.clone() } else { "0".to_owned() };
                                    rsx! {
                                        div { class: "sound-band",
                                            label { r#for: "sound-gain-{index}", "{label}" }
                                            input { r#type: "range", min: "-12", max: "12", step: "1", value: slider, disabled: !allowed || !form.enabled, aria_label: "{label}", oninput: move |event| {
                                                if let Some(form) = editor.draft.write().as_mut() { if let Some(gain) = form.gains.get_mut(index) { *gain = event.value(); } } editor.feedback.set(None);
                                            } }
                                            div { class: "form-field sound-gain-number",
                                                input { id: "sound-gain-{index}", r#type: "text", inputmode: "decimal", value, maxlength: "7", disabled: !allowed || !form.enabled, aria_invalid: !valid, aria_label: "{label}, дБ", oninput: move |event| {
                                                    if let Some(form) = editor.draft.write().as_mut() { if let Some(gain) = form.gains.get_mut(index) { *gain = event.value(); } } editor.feedback.set(None);
                                                } }
                                                span { "дБ" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "sound-band-actions",
                            p { class: if form.gains().is_none() { "form-help failure" } else { "form-help" }, "От −12 до +12 дБ, шаг 1 дБ." }
                            button { class: "text-button", r#type: "button", disabled: !allowed, onclick: move |_| {
                                if let Some(form) = editor.draft.write().as_mut() { form.gains = vec!["0".to_owned();5]; } editor.feedback.set(None);
                            }, "Сбросить полосы" }
                        }
                        label { class: "toggle-field",
                            span { strong { "Room Correction" } small { "Миди учитывает отражения звука от стен и мебели. По умолчанию включено." } }
                            input { id: "sound-room-correction", r#type: "checkbox", checked: form.room, disabled: !allowed, onchange: move |event| {
                                if let Some(form) = editor.draft.write().as_mut() { form.room = event.checked(); } editor.feedback.set(None);
                            } }
                        }
                        div { class: "sound-readback", role: "status",
                            span { class: "subtle-pill", if !status.connected { "Колонка не подключена" }
                                else if status.equalizer_confirmed == Some(true) { "Эквалайзер подтверждён колонкой" }
                                else if status.equalizer_confirmed == Some(false) { "Эквалайзер: ожидаем применение" }
                                else { "Эквалайзер: ждём состояние колонки" }
                            }
                            p { class: "form-help", "Для Room Correction показана сохранённая настройка: отдельного подтверждения состояния колонка не передаёт." }
                            if !status.persistent { p { class: "form-feedback failure", "Сервер работает без базы данных. Настройки сохранятся только до его перезапуска." } }
                        }
                        div { class: "settings-actions",
                            button { class: "button primary", r#type: "submit", disabled: !allowed || !changed || form.gains().is_none(),
                                if busy { span { class: "spinner" } "Сохраняем…" } else { Icon { name: "check", size: 16 } "Сохранить звук" }
                            }
                            button { class: "text-button", r#type: "button", disabled: !allowed || !changed, onclick: move |_| {
                                let original = (editor.original)(); editor.draft.set(original); editor.feedback.set(None);
                            }, "Отменить изменения" }
                            if status.configured {
                                button { class: "text-button", r#type: "button", disabled: !allowed, onclick: move |_| {
                                    let duid = duid.clone(); spawn(async move { save(state, duid, json!({"action":"reset"}), editor).await; });
                                }, "Настройки по умолчанию" }
                            }
                        }
                    }
                }
            } else {
                p { class: "settings-intro", if !state.allowed() { "Войдите, чтобы открыть настройки звука." } else { "Загружаем настройки звука…" } }
            }
            if let Some((success,message)) = (editor.feedback)() { p { class: if success { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" } }
        }
    }
}
