use dioxus::prelude::*;

use crate::api;
use crate::icons::Icon;
use crate::state::{ControlState, Panel};

mod illustration;
mod model;
pub(crate) use model::StationModel;

#[component]
pub fn StationCard(mut state: ControlState) -> Element {
    let device = state.device();
    let connected = device
        .as_ref()
        .is_some_and(|device| device.connected == Some(true))
        && state.server_error.read().is_none();
    let unknown = device
        .as_ref()
        .is_some_and(|device| device.connected.is_none())
        && state.server_error.read().is_none();
    let enabled = state.can_send();
    rsx! {
        section { class: "card station-card", aria_label: "Выбранная колонка",
            div { class: "card-heading", span { class: "card-kicker", "ВАША КОЛОНКА" }
                span { class: if connected { "connection-badge online" } else { "connection-badge" }, span { class: "status-dot" } if connected { "На связи" } else if unknown { "Статус неизвестен" } else { "Не в сети" } }
            }
            StationModel { key: "{(state.selected)()}", state, duid: (state.selected)() }
            button { class: "text-button station-customize", onclick: move |_| state.panel.set(Panel::MidiDisplay),
                Icon { name: "spark", size: 14 } "Текст и подсветка" Icon { name: "chevron", size: 12 }
            }
            div { class: "station-title",
                h2 { if let Some(device) = &device { "{device.label()}" } else { "Ждём вашу колонку" } }
                p { if let Some(device) = &device { "{device.duid}" } else { "Она появится здесь после подключения к серверу" } }
            }
            div { class: "quick-controls", role: "group", aria_label: "Быстрое управление колонкой",
                for (name, icon, label) in [("sound_quiter", "volume-down", "Тише"), ("sound_louder", "volume-up", "Громче"), ("audio_stop", "stop", "Стоп")] {
                    button { class: "quick-control", disabled: !enabled, onclick: move |_| api::quick_action(state, name, label),
                        Icon { name: icon, size: 20 } span { "{label}" }
                    }
                }
            }
            p { class: "station-footnote", if connected { "Всегда готова к вашей следующей идее" } else if unknown { "Сервер не сообщает статус соединения колонки" } else { "Для отправки команд нужно соединение с колонкой" } }
        }
    }
}
