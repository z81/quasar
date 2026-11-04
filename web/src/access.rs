use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::api;
use crate::icons::Icon;
use crate::state::{AuthStatus, ControlState};

#[component]
pub fn LoginGate(state: ControlState) -> Element {
    let loaded = (state.auth_loaded)();
    let available = (state.auth)().available;
    rsx! {
        div { class: "auth-shell",
            a { class: "brand", href: "/ui/", span { class: "brand-mark", Icon { name: "spark", size: 24 } } "quasar." }
            section { class: "card auth-card",
                if !loaded {
                    span { class: "empty-icon", Icon { name: "refresh", size: 24 } }
                    h1 { "Подключаемся." } p { class: "settings-intro", "Проверяем доступ к вашему пространству." }
                    if let Some(error) = (state.auth_error)() { p { class: "form-feedback failure", role: "alert", "{error}" } }
                } else if !available {
                    span { class: "empty-icon", Icon { name: "alert", size: 24 } }
                    h1 { "Хранилище недоступно." }
                    p { class: "settings-intro", "Проверьте подключение к PostgreSQL. После восстановления базы перезапустите сервер — пульт подключится автоматически." }
                } else {
                    span { class: "empty-icon", Icon { name: "lock", size: 24 } }
                    h1 { "С возвращением." }
                    p { class: "settings-intro", "Войдите в пульт управления вашей Алисой." }
                    PasswordForm { state, setup: false }
                }
            }
            p { class: "auth-footer", "Ваш дом. Ваши команды. Quasar Control." }
        }
    }
}

#[component]
fn PasswordForm(mut state: ControlState, setup: bool) -> Element {
    let mut password = use_signal(String::new);
    let mut confirmation = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    rsx! {
        form { class: "password-form", onsubmit: move |event| {
            event.prevent_default();
            if busy() { return; }
            if setup && password.read().chars().count() < 12 { error.set(Some("Используйте не меньше 12 символов.".to_string())); return; }
            if setup && password() != confirmation() { error.set(Some("Пароли не совпадают.".to_string())); return; }
            busy.set(true); error.set(None);
            let body = json!({"password":password()});
            dioxus::core::spawn_forever(async move {
                match api::mutate(state, "POST", if setup { "/auth/setup" } else { "/auth/login" }, Some(&body)).await {
                    Ok(response) => match response.json::<AuthStatus>().await {
                        Ok(auth) => { state.auth.set(auth); state.auth_error.set(None); api::refresh(state).await; api::refresh_saved(state).await; api::refresh_history(state).await; }
                        Err(_) => { if let Ok(mut value) = error.try_write() { *value = Some("Не удалось прочитать ответ сервера".to_string()); } }
                    },
                    Err(message) => { if let Ok(mut value) = error.try_write() { *value = Some(message); } }
                }
                if let Ok(mut value) = busy.try_write() { *value = false; }
                if let Ok(mut value) = password.try_write() { value.clear(); }
                if let Ok(mut value) = confirmation.try_write() { value.clear(); }
            });
        },
            input { class: "sr-only", r#type: "text", autocomplete: "username", name: "username", value: "owner", readonly: true, aria_label: "Владелец" }
            label { class: "form-field", span { if setup { "Новый пароль" } else { "Пароль" } }
                input { r#type: "password", name: "password", autocomplete: if setup { "new-password" } else { "current-password" }, required: true, minlength: if setup { "12" } else { "1" }, maxlength: "256", value: "{password}", oninput: move |event| password.set(event.value()), placeholder: if setup { "Не менее 12 символов" } else { "Введите пароль" } }
            }
            if setup { label { class: "form-field", span { "Повторите пароль" } input { r#type: "password", autocomplete: "new-password", required: true, maxlength: "256", value: "{confirmation}", oninput: move |event| confirmation.set(event.value()), placeholder: "Ещё раз" } } }
            if let Some(message) = error() { p { class: "form-feedback failure", role: "alert", "{message}" } }
            button { class: "button primary", r#type: "submit", disabled: busy() || password.read().is_empty(), if busy() { span { class: "spinner" } "Подождите…" } else if setup { Icon { name: "lock", size: 16 } "Включить защиту" } else { "Войти" Icon { name: "arrow", size: 16 } } }
        }
    }
}

#[derive(Clone, Deserialize, PartialEq)]
struct Token {
    id: String,
    name: String,
    #[serde(rename = "createdAt")]
    created_at: String,
}

#[derive(Deserialize)]
struct CreatedToken {
    token: String,
}

#[component]
pub fn Access(mut state: ControlState) -> Element {
    let configured = (state.auth)().configured;
    rsx! {
        div { class: "settings-grid",
            section { class: "card settings-card",
                div { class: "card-heading", span { class: "card-kicker", Icon { name: "lock", size: 16 } "ВХОД В ПУЛЬТ" } }
                if configured {
                    h2 { "Пространство под защитой." }
                    p { class: "settings-intro", "Вход по паролю включён. Браузер хранит сессию, интеграции используют отдельные токены." }
                    button { class: "button secondary", onclick: move |_| {
                        dioxus::core::spawn_forever(async move {
                            match api::mutate(state, "POST", "/auth/logout", None).await {
                                Ok(_) => { state.auth.write().authenticated = false; state.history.set(Vec::new()); state.favorites.set(Vec::new()); state.text.set(String::new()); }
                                Err(error) => state.notice.set(Some(error)),
                            }
                        });
                    }, Icon { name: "logout", size: 16 } "Выйти из пульта" }
                } else {
                    h2 { "Только для своих." }
                    p { class: "settings-intro", "Сейчас пульт и API открыты в вашей сети. Задайте пароль, чтобы включить вход." }
                    p { class: "security-note", "После включения интеграциям потребуется API-токен в заголовке Authorization: Bearer. Создайте токен здесь и обновите их настройки." }
                    PasswordForm { state, setup: true }
                }
            }
            if configured { Tokens { state } }
            else { section { class: "card settings-card", div { class: "card-kicker", Icon { name: "key", size: 16 } "ТОКЕНЫ ДЛЯ ИНТЕГРАЦИЙ" } h2 { "Каждому сервису — свой ключ." } p { class: "settings-intro", "После настройки входа здесь можно создать токены для автоматизаций и отозвать их в любой момент." } } }
        }
    }
}

#[component]
fn Tokens(state: ControlState) -> Element {
    let mut tokens = use_signal(Vec::<Token>::new);
    let mut name = use_signal(String::new);
    let mut created = use_signal(|| None::<String>);
    let mut error = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);
    use_future(move || async move {
        match api::get::<Vec<Token>>(state, "/auth/tokens").await {
            Ok(values) => tokens.set(values),
            Err(message) => error.set(Some(message)),
        }
    });
    rsx! {
        section { class: "card settings-card",
            div { class: "card-kicker", Icon { name: "key", size: 16 } "ТОКЕНЫ ДЛЯ ИНТЕГРАЦИЙ" }
            h2 { "Подключайте свои сервисы." }
            p { class: "settings-intro", "Токен даёт доступ к командам и данным колонок. Создавайте отдельный для каждой интеграции." }
            form { class: "token-create", onsubmit: move |event| {
                event.prevent_default(); if busy() || name.read().trim().is_empty() { return; }
                busy.set(true); error.set(None); created.set(None);
                let body = json!({"name":name.read().trim()});
                spawn(async move {
                    match api::mutate(state, "POST", "/auth/tokens", Some(&body)).await {
                        Ok(response) => match response.json::<CreatedToken>().await {
                            Ok(value) => { created.set(Some(value.token)); name.set(String::new()); match api::get::<Vec<Token>>(state, "/auth/tokens").await { Ok(values) => tokens.set(values), Err(message) => error.set(Some(message)) } }
                            Err(_) => error.set(Some("Не удалось прочитать токен. Отзовите его и создайте заново.".to_string())),
                        },
                        Err(message) => error.set(Some(message)),
                    }
                    busy.set(false);
                });
            },
                label { class: "form-field", span { "Название интеграции" } input { required: true, maxlength: "80", value: "{name}", placeholder: "Например, Home Assistant", oninput: move |event| name.set(event.value()) } }
                button { class: "button secondary", r#type: "submit", disabled: busy() || name.read().trim().is_empty(), Icon { name: "plus", size: 16 } "Создать токен" }
            }
            if let Some(token) = created() {
                div { class: "created-token", role: "status", strong { "Сохраните токен сейчас" } p { "Он показывается один раз. Скопируйте целиком и храните как пароль." }
                    textarea { readonly: true, value: "{token}", aria_label: "Новый API токен", spellcheck: "false" }
                    button { class: "text-button", onclick: move |_| created.set(None), "Я сохранил токен — скрыть" }
                }
            }
            if let Some(message) = error() { p { class: "form-feedback failure", role: "alert", "{message}" } }
            div { class: "token-list",
                if tokens.read().is_empty() { p { class: "form-help", "Активных токенов пока нет." } }
                for token in tokens() {
                    {
                        let id = token.id.clone();
                        rsx! { div { key: "{token.id}", class: "token-row", div { strong { "{token.name}" } small { "Создан: {token.created_at}" } }
                            button { class: "text-button", disabled: busy(), onclick: move |_| {
                                let url = format!("/auth/tokens/{}", api::encoded(&id)); busy.set(true); created.set(None);
                                spawn(async move { match api::mutate(state, "DELETE", &url, None).await { Ok(_) => match api::get::<Vec<Token>>(state, "/auth/tokens").await { Ok(values) => tokens.set(values), Err(message) => error.set(Some(message)) }, Err(message) => error.set(Some(message)) }; busy.set(false); });
                            }, Icon { name: "trash", size: 14 } "Отозвать" }
                        } }
                    }
                }
            }
            p { class: "form-help", "Заголовок запроса: Authorization: Bearer ВАШ_ТОКЕН" }
        }
    }
}
