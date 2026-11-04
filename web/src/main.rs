mod access;
mod api;
mod composer;
mod favorites;
mod history;
mod icons;
mod player;
mod schedules;
mod settings;
mod sidebar;
mod state;
mod station;
mod weather;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use icons::Icon;
use state::{ControlState, Panel, Theme};

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut state = ControlState {
        devices: use_signal(Vec::new),
        selected: use_signal(String::new),
        loading: use_signal(|| true),
        server_error: use_signal(|| None),
        sending: use_signal(|| false),
        history: use_signal(Vec::new),
        history_error: use_signal(|| None),
        favorites: use_signal(Vec::new),
        persistent: use_signal(|| None),
        player: use_signal(|| None),
        player_error: use_signal(|| None),
        weather: use_signal(|| None),
        weather_error: use_signal(|| None),
        weather_loading: use_signal(|| false),
        weather_request: use_signal(|| 0),
        auth: use_signal(Default::default),
        auth_loaded: use_signal(|| false),
        auth_error: use_signal(|| None),
        mode: use_signal(Default::default),
        text: use_signal(String::new),
        panel: use_signal(Default::default),
        theme: use_signal(Theme::load),
        notice: use_signal(|| None),
    };
    use_future(move || async move {
        loop {
            api::refresh_auth(state).await;
            if state.allowed() {
                api::refresh(state).await;
                api::refresh_saved(state).await;
            }
            TimeoutFuture::new(10_000).await;
        }
    });
    use_future(move || async move {
        loop {
            if state.allowed() {
                api::refresh_history(state).await;
            }
            let active = state
                .history
                .read()
                .iter()
                .any(|entry| matches!(entry.status.as_str(), "queued" | "running"));
            TimeoutFuture::new(if active { 2_000 } else { 10_000 }).await;
        }
    });
    use_future(move || async move {
        loop {
            if state.allowed() {
                api::refresh_player(state).await;
            }
            TimeoutFuture::new(5_000).await;
        }
    });
    use_effect(move || {
        let _ = (state.selected)();
        state.player.set(None);
        if state.allowed() {
            spawn(api::refresh_player(state));
        }
    });
    use_future(move || async move {
        loop {
            if state.allowed() {
                weather::refresh(state).await;
            }
            TimeoutFuture::new(60_000).await;
        }
    });
    use_effect(move || {
        let _ = (state.selected)();
        state.weather.set(None);
        state.weather_error.set(None);
        if state.allowed() {
            spawn(weather::refresh(state));
        }
    });
    let panel = (state.panel)();
    rsx! {
        document::Title { "Quasar — пульт управления" }
        document::Stylesheet { href: asset!("/assets/control.css") }
        div { class: if state.allowed() { "app" } else { "app auth-app" }, "data-theme": (state.theme)().name(),
            if !state.allowed() { access::LoginGate { state } } else {
            sidebar::Sidebar { state }
            div { class: "workspace",
                header { class: "topbar",
                    div { class: "breadcrumbs", span { "Рабочее пространство" } Icon { name: "chevron", size: 13 } strong { "{panel.label()}" } }
                    span { class: "topbar-label", span { class: "tiny-orbit" } "QUASAR CONTROL" }
                }
                main { class: "main",
                    div { class: "page-heading",
                        div {
                            div { class: "eyebrow", "ВАША АЛИСА. ВАШИ ПРАВИЛА." }
                            h1 { match panel { Panel::Control => "Всё под контролем.", Panel::History => "История команд.", Panel::Advanced => "Прямой доступ.", Panel::Favorites => "Всегда под рукой.", Panel::Schedules => "Будильники и напоминания.", Panel::Settings => "На ваш лад.", Panel::Access => "Только для своих." } }
                            p { match panel {
                                Panel::Control => "Скажите, что нужно сделать. Остальное — за Алисой.",
                                Panel::History => "Запросы, ответы Алисы и результаты обработки.",
                                Panel::Advanced => "Отправляйте колонке команды её собственного протокола.",
                                Panel::Favorites => "Любимые команды для каждого дня. Сохраните и запускайте одним нажатием.",
                                Panel::Schedules => "Планы на утро, важные дела и время для себя.",
                                Panel::Settings => "Имя, погода и голосовые настройки выбранной колонки.",
                                Panel::Access => "Вход в пульт и ключи для ваших интеграций.",
                            } }
                        }
                        button { class: "button secondary refresh-button", disabled: (state.loading)(), onclick: move |_| { state.loading.set(true); spawn(api::refresh(state)); spawn(weather::refresh(state)); },
                            Icon { name: "refresh", size: 16 } "Обновить"
                        }
                    }
                    if let Some(error) = (state.server_error)() {
                        div { class: "notice error", role: "alert", Icon { name: "alert" } span { "{error}. Повторяем подключение автоматически." } }
                    }
                    if let Some(notice) = (state.notice)() {
                        div { class: "notice error", role: "alert", Icon { name: "alert" } span { "{notice}" }
                            button { class: "dismiss", aria_label: "Закрыть уведомление", onclick: move |_| state.notice.set(None), "×" }
                        }
                    }
                    match panel {
                        Panel::Control => rsx! {
                            div { class: "control-grid",
                                composer::Composer { state }
                                station::StationCard { state }
                            }
                            weather::WeatherCard { state }
                            player::PlayerCard { state }
                            history::History { state, compact: true }
                        },
                        Panel::History => rsx! { history::History { state, compact: false } },
                        Panel::Favorites => rsx! { favorites::Favorites { state } },
                        Panel::Schedules => rsx! { schedules::Schedules { state } },
                        Panel::Settings => rsx! { settings::Settings { state } },
                        Panel::Access => rsx! { access::Access { state } },
                        Panel::Advanced => rsx! {
                            div { class: "control-grid", composer::Advanced { state } station::StationCard { state } }
                        },
                    }
                    footer { class: "page-footer",
                        span { "Команды и ответы — в одном пространстве." }
                        span { "Quasar Control" }
                    }
                }
            }
            }
        }
    }
}
