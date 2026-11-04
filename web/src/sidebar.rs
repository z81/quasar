use dioxus::prelude::*;

use crate::icons::Icon;
use crate::state::{ControlState, Panel, Theme};

#[component]
pub fn Sidebar(mut state: ControlState) -> Element {
    let devices = (state.devices)();
    let selected = (state.selected)();
    let history_count = (state.history)().len();
    let server_ok =
        state.server_error.read().is_none() && (!(state.loading)() || !devices.is_empty());
    rsx! {
        aside { class: "sidebar",
            a { class: "brand", href: "/ui/", aria_label: "Quasar — главная",
                span { class: "brand-mark", Icon { name: "spark", size: 24 } }
                span { "quasar" span { class: "brand-dot", "." } }
            }
            div { class: "workspace-tag", span { class: "workspace-avatar", "Q" } div { strong { "Моё пространство" } span { "Пульт управления" } } }
            div { class: "nav-label", "ПРОСТРАНСТВО" }
            nav { class: "navigation", aria_label: "Разделы пульта",
                for (panel, icon) in [(Panel::Control, "grid"), (Panel::Favorites, "star"), (Panel::Schedules, "alarm"), (Panel::History, "history"), (Panel::Settings, "settings"), (Panel::Access, "lock"), (Panel::Advanced, "code")] {
                    button { class: if (state.panel)() == panel { "nav-item active" } else { "nav-item" }, aria_current: if (state.panel)() == panel { "page" } else { "false" },
                        onclick: move |_| state.panel.set(panel),
                        Icon { name: icon, size: 18 } "{panel.label()}"
                        if panel == Panel::History && history_count > 0 { span { class: "nav-count", "{history_count}" } }
                    }
                }
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
