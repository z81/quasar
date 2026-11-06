use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::Deserialize;
use serde_json::json;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Deserialize, PartialEq)]
struct Profile {
    id: String,
    name: String,
    preferences: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Enrollment {
    device: String,
    name: String,
    samples: usize,
    required: usize,
    seconds_remaining: u64,
    message: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct Decision {
    status: String,
    name: Option<String>,
    message: String,
}
#[derive(Clone, Deserialize, PartialEq)]
struct View {
    persistent: bool,
    backend: String,
    message: String,
    profiles: Vec<Profile>,
    enrollment: Option<Enrollment>,
    last_decision: Option<Decision>,
}

#[component]
pub fn SpeakerProfiles(state: ControlState) -> Element {
    let duid = (state.selected)();
    if duid.is_empty() {
        return rsx! { div { class: "card history-empty", Icon { name: "speaker", size: 28 } h3 { "Выберите колонку" } p { "Для обучения голосов нужна подключённая колонка." } } };
    }
    rsx! { SpeakerPanel { key: "{duid}", state, duid } }
}

#[component]
fn SpeakerPanel(state: ControlState, duid: String) -> Element {
    let mut view = use_signal(|| None::<View>);
    let mut error = use_signal(|| None::<String>);
    let mut feedback = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    let mut name = use_signal(String::new);
    let mut preferences = use_signal(String::new);
    let mut consent = use_signal(|| false);
    let poll_duid = duid.clone();
    use_future(move || {
        let duid = poll_duid.clone();
        async move {
            loop {
                let result = api::get::<View>(
                    state,
                    &format!("/settings/speakers/{}", api::encoded(&duid)),
                )
                .await;
                match result {
                    Ok(value) => {
                        view.set(Some(value));
                        error.set(None);
                    }
                    Err(message) => error.set(Some(message)),
                }
                TimeoutFuture::new(2500).await;
            }
        }
    });
    let snapshot = view();
    let enrollment = snapshot.as_ref().and_then(|v| v.enrollment.clone());
    let available = error().is_none() && snapshot.as_ref().is_some_and(|v| v.backend == "ready");
    let connected = state
        .devices
        .read()
        .iter()
        .any(|d| d.duid == duid && d.connected == Some(true));
    let start_duid = duid.clone();
    rsx! {
        div { class: "station-settings-stack",
            section { class: "card settings-card",
                div { class: "card-heading", span { class: "card-kicker", Icon { name: "speaker", size: 16 } "ДОМАШНИЕ ГОЛОСА" }
                    span { class: "subtle-pill", if available { "Сервис доступен" } else if snapshot.is_none() { "Проверяем…" } else { "Без распознавания" } }
                }
                h2 { "Для каждого — свой профиль" }
                p { class: "settings-intro", "Алиса сопоставляет голос с обученными профилями и учитывает имя и предпочтения. Если совпадение неуверенное или фраза короткая, она использует общий профиль. Распознавание голоса не даёт прав доступа." }
                p { class: "form-help", "При совпадении имя и предпочтения передаются настроенной языковой модели; имя и ID профиля — подключённым серверам функций и состояния. Голосовые отпечатки им не передаются." }
                if let Some(error) = error() { p { class: "form-feedback failure", role: "alert", "{error}" } }
                if let Some(value) = &snapshot {
                    p { class: "form-help", "{value.message}" }
                    if !value.persistent { p { class: "form-feedback failure", "База недоступна: профили исчезнут после перезапуска сервера." } }
                    if let Some(decision) = &value.last_decision {
                        div { class: "settings-actions", span { class: "subtle-pill", if decision.status == "recognized" { "Последний голос: {decision.name.clone().unwrap_or_default()}" } else { "Последний голос: не определён" } }
                            p { class: "form-help", "{decision.message}" }
                        }
                    }
                }
                div { class: "settings-divider" }
                if let Some(pending) = enrollment {
                    h3 { "Обучение: {pending.name}" }
                    p { class: "settings-intro", "Принято {pending.samples} из {pending.required} образцов. Осталось {pending.seconds_remaining} с. {pending.message}" }
                    if pending.device != duid { p { class: "form-help", "Обучение начато на другой колонке." } }
                    p { class: "form-help", "Во время обучения обращения к выбранной колонке становятся образцами голоса и не выполняются как команды. Говорите один, без музыки и чужих голосов." }
                    div { class: "settings-actions", button { class: "button secondary", disabled: busy(), onclick: move |_| {
                        busy.set(true); spawn(async move {
                            match api::mutate(state,"DELETE","/settings/speakers/enrollment",None).await { Ok(_) => feedback.set(Some("Обучение отменено. Образцы удалены.".into())), Err(e) => feedback.set(Some(e)) }
                            busy.set(false);
                        });
                    }, "Отменить обучение" } }
                } else {
                    h3 { "Добавить голос" }
                    form { onsubmit: move |event| {
                        event.prevent_default();
                        if busy() || !consent() || !available || !connected || name.read().trim().is_empty() { return; }
                        let body = json!({"duid":start_duid,"name":name.read().trim(),"preferences":preferences(),"consent":consent()});
                        busy.set(true); feedback.set(None);
                        spawn(async move {
                            match api::mutate(state,"POST","/settings/speakers/enrollment",Some(&body)).await {
                                Ok(_) => { feedback.set(Some("Обучение начато. Трижды обратитесь к выбранной колонке.".into())); consent.set(false); },
                                Err(e) => feedback.set(Some(e)),
                            }
                            busy.set(false);
                        });
                    },
                        label { class: "form-field", span { "Имя" } input { value: "{name}", maxlength: "80", required: true, placeholder: "Как к вам обращаться", oninput: move |e| name.set(e.value()) } }
                        label { class: "form-field", span { "Предпочтения" } textarea { value: "{preferences}", maxlength: "2000", placeholder: "Например: короткие ответы, любимые жанры музыки", oninput: move |e| preferences.set(e.value()) } }
                        label { class: "toggle-field", span { strong { "Согласие на обучение голоса" } small { "Человек согласен сохранить голосовой отпечаток. Аудиозаписи не сохраняются. Профиль можно удалить в любой момент." } } input { r#type: "checkbox", checked: consent(), onchange: move |e| consent.set(e.checked()) } }
                        p { class: "form-help", "Нужны три разных фразы, каждая не короче трёх секунд речи. После старта произносите их по очереди, начиная со слова активации. Отпечатки остаются на вашем сервере; обычное распознавание речи работает с настроенным провайдером." }
                        div { class: "settings-actions", button { class: "button primary", r#type: "submit", disabled: busy() || !consent() || !available || !connected || name.read().trim().is_empty(), "Начать обучение · 2 минуты" } }
                        if !connected { p { class: "form-feedback failure", "Выбранная колонка сейчас не подключена." } }
                    }
                }
                if let Some(message) = feedback() { p { class: "form-feedback", role: "status", "{message}" } }
            }
            if let Some(value) = snapshot {
                for profile in value.profiles { ProfileEditor { key: "{profile.id}", state, profile } }
            }
        }
    }
}

#[component]
fn ProfileEditor(state: ControlState, profile: Profile) -> Element {
    let mut name = use_signal(|| profile.name.clone());
    let mut preferences = use_signal(|| profile.preferences.clone());
    let mut busy = use_signal(|| false);
    let mut deleting = use_signal(|| false);
    let mut feedback = use_signal(|| None::<String>);
    let save_id = profile.id.clone();
    let delete_id = profile.id.clone();
    rsx! {
        section { class: "card settings-card",
            h3 { "{profile.name}" }
            form { onsubmit: move |event| {
                event.prevent_default(); if busy() { return; } busy.set(true);
                let body = json!({"name": name(), "preferences": preferences()});
                let url = format!("/settings/speakers/profiles/{}", api::encoded(&save_id));
                spawn(async move {
                    match api::mutate(state,"PATCH",&url,Some(&body)).await { Ok(_) => feedback.set(Some("Профиль сохранён.".into())), Err(e) => feedback.set(Some(e)) }
                    busy.set(false);
                });
            },
                label { class: "form-field", span { "Имя" } input { value: "{name}", maxlength: "80", required: true, oninput: move |e| name.set(e.value()) } }
                label { class: "form-field", span { "Предпочтения" } textarea { value: "{preferences}", maxlength: "2000", oninput: move |e| preferences.set(e.value()) } }
                div { class: "settings-actions", button { class: "button primary", r#type: "submit", disabled: busy(), "Сохранить" }
                    button { class: "button secondary", r#type: "button", disabled: busy(), onclick: move |_| deleting.set(!deleting()), "Удалить профиль" }
                }
            }
            if deleting() {
                p { class: "form-help", "Имя, предпочтения и все голосовые отпечатки будут удалены. Для нового распознавания потребуется обучение." }
                div { class: "settings-actions", button { class: "button danger", disabled: busy(), onclick: move |_| {
                    busy.set(true); let url = format!("/settings/speakers/profiles/{}", api::encoded(&delete_id));
                    spawn(async move {
                        match api::mutate(state,"DELETE",&url,None).await { Ok(_) => feedback.set(Some("Профиль удалён.".into())), Err(e) => feedback.set(Some(e)) }
                        busy.set(false);
                    });
                }, "Да, удалить отпечатки" } button { class: "button secondary", onclick: move |_| deleting.set(false), "Оставить" } }
            }
            if let Some(message) = feedback() { p { class: "form-feedback", role: "status", "{message}" } }
        }
    }
}
