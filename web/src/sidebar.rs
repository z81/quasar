use dioxus::prelude::*;

use crate::icons::Icon;
use crate::state::{ControlState, Panel, Theme};

const NAVIGATION_GROUPS: [(&str, &[(Panel, &str)]); 3] = [
    (
        "Каждый день",
        &[
            (Panel::Control, "grid"),
            (Panel::Favorites, "star"),
            (Panel::Schedules, "alarm"),
            (Panel::Timers, "timer"),
            (Panel::History, "history"),
        ],
    ),
    (
        "Колонка",
        &[
            (Panel::MidiDisplay, "spark"),
            (Panel::SmartHome, "home"),
            (Panel::Settings, "settings"),
        ],
    ),
    (
        "Инструменты",
        &[
            (Panel::DeviceAccess, "terminal"),
            (Panel::Advanced, "code"),
            (Panel::Access, "lock"),
        ],
    ),
];

#[component]
pub fn Sidebar(mut state: ControlState) -> Element {
    let devices = (state.devices)();
    let selected = (state.selected)();
    let history_count = (state.history)().len();
    let server_ok =
        state.server_error.read().is_none() && (!(state.loading)() || !devices.is_empty());
    use_effect(move || {
        let _ = (state.panel)();
        document::eval(
            "if (matchMedia('(max-width: 760px)').matches) document.querySelector('.navigation [aria-current=\"page\"]')?.scrollIntoView({block: 'nearest', inline: 'nearest'});",
        );
    });
    rsx! {
        aside { class: "sidebar",
            a { class: "brand", href: "/ui/", aria_label: "Quasar — главная",
                span { class: "brand-mark", Icon { name: "spark", size: 24 } }
                span { "quasar" span { class: "brand-dot", "." } }
            }
            section { class: "devices-section", aria_label: "Выбор колонки",
                div { class: "nav-label device-list-heading", span { "КОЛОНКИ" } span { "{devices.len()}" } }
                if (state.loading)() && devices.is_empty() {
                    div { class: "station-skeleton", aria_label: "Загружаем колонки" }
                } else if devices.is_empty() {
                    p { class: "sidebar-empty", "Здесь появятся ваши подключённые колонки." }
                }
                div { class: "device-list",
                    for device in devices {
                        {
                            let id = device.duid.clone();
                            let connected = device.connected == Some(true) && server_ok;
                            rsx! {
                                button { key: "{id}", class: if id == selected { "device-item selected" } else { "device-item" },
                                    aria_pressed: id == selected, onclick: move |_| { state.player.set(None); state.selected.set(id.clone()); },
                                    span { class: "device-icon", Icon { name: "speaker", size: 20 } }
                                    span { class: "device-copy", strong { "{device.label()}" } span { if server_ok { "{device.connection_label()}" } else { "Нет связи с сервером" } } }
                                    span { class: if connected { "status-dot connected" } else { "status-dot" } }
                                }
                            }
                        }
                    }
                }
            }
            nav { class: "navigation", aria_label: "Разделы пульта",
                for (label, panels) in NAVIGATION_GROUPS {
                    div { class: "nav-group",
                        div { class: "nav-label", "{label}" }
                        for &(panel, icon) in panels {
                            button { class: if (state.panel)() == panel { "nav-item active" } else { "nav-item" }, aria_current: if (state.panel)() == panel { "page" } else { "false" },
                                onclick: move |_| state.panel.set(panel),
                                Icon { name: icon, size: 18 } "{panel.label()}"
                                if panel == Panel::History && history_count > 0 { span { class: "nav-count", "{history_count}" } }
                            }
                        }
                    }
                }
            }
            div { class: "sidebar-bottom",
                div { class: "server-status", span { class: if server_ok { "status-dot connected" } else { "status-dot" } }
                    span { if (state.loading)() { "Проверяем сервер…" } else if server_ok { "Сервер доступен" } else { "Сервер недоступен" } }
                }
                div { class: "theme-switch", role: "group", aria_label: "Тема оформления",
                    for (theme, icon, label) in [(Theme::Light, "sun", "Светлая"), (Theme::Dark, "moon", "Тёмная")] {
                        button { class: if (state.theme)() == theme { "theme-option chosen" } else { "theme-option" }, aria_pressed: (state.theme)() == theme,
                            onclick: move |_| { theme.save(); state.theme.set(theme); }, Icon { name: icon, size: 15 } "{label}"
                        }
                    }
                }
            }
        }
    }
}
