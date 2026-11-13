use crate::{overview, schedules, settings, state::ControlState, timers};
use dioxus::prelude::*;

#[component]
pub fn AutomationPage(state: ControlState) -> Element {
    rsx! { div { class: "ocean-automation-page",
        section { class: "ocean-page-surface", h2 { "Будильники и напоминания" } schedules::Schedules { state } }
        section { class: "ocean-page-surface", h2 { "Таймеры" } timers::Timers { state } }
    } }
}

#[component]
pub fn StationPage(state: ControlState) -> Element {
    rsx! { div { class: "ocean-station-page",
        aside { class: "ocean-page-surface ocean-station-profile", overview::StationOverview { state }
            if let Some(device) = state.device() { dl { dt { "Часовой пояс" } dd { "{device.timezone.as_deref().unwrap_or(\"Не задан\")}" } } }
        }
        div { class: "ocean-station-preferences", settings::Settings { state, station: true } }
    } }
}
