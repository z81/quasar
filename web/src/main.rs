mod api;
mod composer;
mod history;
mod icons;
mod sidebar;
mod state;
mod station;

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
        mode: use_signal(Default::default),
        text: use_signal(String::new),
        panel: use_signal(Default::default),
        theme: use_signal(Theme::load),
        notice: use_signal(|| None),
    };
    use_future(move || async move {
        loop {
            api::refresh(state).await;
            TimeoutFuture::new(10_000).await;
        }
    });
    let panel = (state.panel)();
    rsx! {
        document::Title { "Quasar — пульт управления" }
        document::Stylesheet { href: asset!("/assets/control.css") }
        div { class: "app", "data-theme": (state.theme)().name(),
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
                            h1 { match panel { Panel::Control => "Всё под контролем.", Panel::History => "История команд.", Panel::Advanced => "Прямой доступ." } }
                            p { match panel {
                                Panel::Control => "Скажите, что нужно сделать. Остальное — за Алисой.",
                                Panel::History => "Ваши запросы и результаты отправки в этой сессии.",
                                Panel::Advanced => "Отправляйте колонке команды её собственного протокола.",
                            } }
                        }
                        button { class: "button secondary refresh-button", disabled: (state.loading)(), onclick: move |_| { state.loading.set(true); spawn(api::refresh(state)); },
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
                            history::History { state, compact: true }
                        },
                        Panel::History => rsx! { history::History { state, compact: false } },
                        Panel::Advanced => rsx! {
                            div { class: "control-grid", composer::Advanced { state } station::StationCard { state } }
                        },
                    }
                    footer { class: "page-footer",
                        span { "Ответы Алисы звучат на выбранной колонке." }
                        span { "Quasar Control" }
                    }
                }
            }
        }
    }
}
