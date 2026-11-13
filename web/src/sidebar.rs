use crate::{
    icons::Icon,
    state::{ControlState, Panel, Theme},
};
use dioxus::prelude::*;

const ALL: &[(Panel, &str)] = &[
    (Panel::Control, "grid"),
    (Panel::Music, "music"),
    (Panel::Favorites, "star"),
    (Panel::Schedules, "alarm"),
    (Panel::Timers, "timer"),
    (Panel::AutomationOverview, "alarm"),
    (Panel::StationOverview, "speaker"),
    (Panel::Automations, "code"),
    (Panel::History, "history"),
    (Panel::Home, "home"),
    (Panel::SmartHome, "home"),
    (Panel::MidiDisplay, "spark"),
    (Panel::StationSettings, "settings"),
    (Panel::DeviceAccess, "terminal"),
    (Panel::DeviceTools, "settings"),
    (Panel::Maintenance, "settings"),
    (Panel::Advanced, "code"),
    (Panel::Settings, "settings"),
    (Panel::Access, "lock"),
];

fn section(panel: Panel) -> u8 {
    match panel {
        Panel::Control | Panel::History => 0,
        Panel::Music | Panel::Favorites => 1,
        Panel::AutomationOverview | Panel::Automations | Panel::Schedules | Panel::Timers => 2,
        Panel::Home | Panel::SmartHome => 3,
        Panel::StationOverview
        | Panel::StationSettings
        | Panel::MidiDisplay
        | Panel::DeviceAccess
        | Panel::DeviceTools
        | Panel::Maintenance
        | Panel::Advanced => 4,
        Panel::Settings | Panel::Access => 5,
    }
}

#[component]
pub fn Sidebar(mut state: ControlState) -> Element {
    let mut query = use_signal(String::new);
    let panel = (state.panel)();
    let devices = (state.devices)();
    let selected = (state.selected)();
    let target_theme = if (state.theme)() == Theme::Dark {
        Theme::Light
    } else {
        Theme::Dark
    };
    let search = query().trim().to_lowercase();
    let theme_icon = if target_theme == Theme::Light {
        "sun"
    } else {
        "moon"
    };
    rsx! {
        document::Script { r#type: "module", src: asset!("/assets/mobile-menu.js") }
        header { class: "ocean-header",
            a { class: "ocean-brand", href: "/ui/", aria_label: "Quasar — главная", "quasar." Icon { name: "spark", size: 24 } }
            nav { class: "ocean-primary-nav navigation", aria_label: "Основные разделы",
                for (group, target, label) in [(0,Panel::Control,"Пульт"),(1,Panel::Music,"Музыка"),(2,Panel::AutomationOverview,"Автоматизация"),(3,Panel::Home,"Дом"),(4,Panel::StationOverview,"Станция")] {
                    button { r#type: "button", class: if section(panel) == group { "ocean-nav-tab active" } else { "ocean-nav-tab" }, aria_current: if section(panel) == group { "page" } else { "false" },
                        onclick: move |_| state.panel.set(target), "{label}" }
                }
            }
            div { class: "ocean-header-actions",
                label { class: "ocean-device-picker", title: state.device().map(|d| d.label().to_owned()).unwrap_or_else(|| "Выбор колонки".into()),
                    span { class: "sr-only", "Выбор колонки" }
                    select { aria_label: "Выбор колонки", value: selected,
                        onchange: move |event| { state.player.set(None); state.selected.set(event.value()); },
                        if devices.is_empty() { option { value: "", if (state.loading)() { "Загружаем…" } else { "Нет колонок" } } }
                        for device in &devices { option { key: "{device.duid}", value: "{device.duid}", "{device.label()}" } }
                    }
                }
                button { r#type: "button", class: "ocean-header-icon", aria_label: "Поиск разделов", title: "Поиск разделов", "popovertarget": "ocean-search", Icon { name: "search", size: 24 } }
                button { r#type: "button", class: "ocean-header-icon", aria_label: "Настройки", title: "Настройки", onclick: move |_| state.panel.set(Panel::Settings), Icon { name: "settings", size: 24 } }
                button { r#type: "button", class: "ocean-header-icon ocean-theme", aria_label: if target_theme == Theme::Light { "Светлая тема" } else { "Тёмная тема" }, title: if target_theme == Theme::Light { "Светлая тема" } else { "Тёмная тема" },
                    onclick: move |_| { target_theme.save(); state.theme.set(target_theme); }, Icon { name: theme_icon, size: 22 } }
                button { r#type: "button", class: "mobile-menu-toggle ocean-header-icon", aria_label: "Открыть меню разделов", aria_haspopup: "dialog", aria_controls: "mobile-menu", Icon { name: "menu", size: 24 } }
            }
        }
        div { id: "ocean-search", class: "ocean-search-panel", "popover": "auto", role: "dialog", aria_labelledby: "ocean-search-title",
            header { h2 { id: "ocean-search-title", "Найти в Quasar" }
                button { r#type: "button", class: "ocean-header-icon", aria_label: "Закрыть поиск", "popovertarget": "ocean-search", "popovertargetaction": "hide", Icon { name: "close", size: 22 } }
            }
            input { r#type: "search", aria_label: "Раздел или действие", placeholder: "Таймер, музыка, файлы…", value: query, oninput: move |event| query.set(event.value()) }
            nav { class: "ocean-search-results", aria_label: "Результаты поиска",
                for &(target, icon) in ALL.iter().filter(|(target,_)| search.is_empty() || target.label().to_lowercase().contains(&search)) {
                    button { r#type: "button", onclick: move |_| { state.panel.set(target); document::eval("document.getElementById('ocean-search')?.hidePopover()"); }, Icon { name: icon, size: 22 } "{target.label()}" Icon { name: "chevron", size: 18 } }
                }
                if !ALL.iter().any(|(target,_)| search.is_empty() || target.label().to_lowercase().contains(&search)) { p { "Ничего не найдено" } }
            }
        }
        dialog { id: "mobile-menu", class: "mobile-menu", aria_labelledby: "mobile-menu-title",
            div { class: "mobile-menu-panel",
                header { class: "mobile-menu-heading", h2 { id: "mobile-menu-title", "Разделы пульта" }
                    button { class: "mobile-menu-close button secondary", aria_label: "Закрыть меню", autofocus: true, Icon { name: "close", size: 22 } }
                }
                nav { class: "mobile-menu-navigation", aria_label: "Все разделы пульта",
                    for &(target, icon) in ALL { button { class: if target == panel { "nav-item active" } else { "nav-item" }, onclick: move |_| state.panel.set(target), Icon { name: icon, size: 20 } "{target.label()}" } }
                }
            }
        }
    }
}

#[component]
pub fn SectionNavigation(mut state: ControlState) -> Element {
    let panel = (state.panel)();
    let group = section(panel);
    let panels: Vec<_> = ALL
        .iter()
        .copied()
        .filter(|(target, _)| section(*target) == group)
        .collect();
    if panel == Panel::Control || panels.len() < 2 {
        return rsx! {};
    }
    rsx! { nav { class: "ocean-section-nav", aria_label: "Разделы категории",
        for (target, icon) in panels { button { r#type: "button", class: if target == panel { "active" } else { "" }, aria_current: if target == panel { "page" } else { "false" }, onclick: move |_| state.panel.set(target), Icon { name: icon, size: 17 } "{target.label()}" } }
    } }
}
