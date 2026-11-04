use dioxus::prelude::*;

use crate::api;
use crate::icons::Icon;
use crate::state::{ControlState, Delivery, Entry, Panel};

#[component]
pub fn History(mut state: ControlState, compact: bool) -> Element {
    let mut filter = use_signal(String::new);
    let entries = (state.history)();
    let needle = filter().to_lowercase();
    let visible: Vec<_> = entries
        .iter()
        .filter(|entry| {
            compact
                || format!("{} {} {}", entry.text, entry.station, entry.label)
                    .to_lowercase()
                    .contains(&needle)
        })
        .take(if compact { 4 } else { 100 })
        .cloned()
        .collect();
    rsx! {
        section { class: "history-section", aria_label: "История отправки команд",
            div { class: "history-heading",
                div { class: "history-title", h2 { if compact { "Последние команды" } else { "Все отправки" } } span { class: "count-badge", "{entries.len()}" } }
                if compact {
                    button { class: "text-button", onclick: move |_| state.panel.set(Panel::History), "Вся история" Icon { name: "arrow", size: 16 } }
                } else {
                    button { class: "text-button", disabled: entries.is_empty() || (state.sending)(), onclick: move |_| state.history.write().clear(), Icon { name: "trash", size: 15 } "Очистить" }
                }
            }
            if !compact {
                div { class: "history-search", Icon { name: "search", size: 17 }
                    input { r#type: "search", aria_label: "Поиск по истории", placeholder: "Найти команду или колонку…", value: "{filter}", oninput: move |event| filter.set(event.value()) }
                }
            }
            div { class: "card history-card", aria_live: "polite",
                if visible.is_empty() {
                    div { class: "history-empty", span { class: "empty-icon", Icon { name: "history", size: 25 } }
                        h3 { if entries.is_empty() { "У каждой команды будет своя история" } else { "Ничего не нашлось" } }
                        p { if entries.is_empty() { "Отправьте первую — её результат появится здесь." } else { "Попробуйте другой текст для поиска." } }
                    }
                } else {
                    for entry in visible { HistoryRow { key: "{entry.id}", state, entry } }
                }
            }
            p { class: "history-note", "Последние 100 отправок хранятся до перезагрузки страницы. Статус показывает ответ сервера; выполнение команды колонкой не подтверждается." }
        }
    }
}

#[component]
fn HistoryRow(state: ControlState, entry: Entry) -> Element {
    let (class, icon, status) = match entry.delivery {
        Delivery::Sending => ("pending", "refresh", "Отправляем"),
        Delivery::Sent => ("success", "check", "Отправлено"),
        Delivery::Accepted => ("pending", "arrow", "Принято сервером"),
        Delivery::Failed => ("failed", "alert", "Ошибка"),
    };
    let retry = entry.clone();
    let enabled = !(state.sending)()
        && state.server_error.read().is_none()
        && state
            .devices
            .read()
            .iter()
            .any(|device| device.duid == entry.duid && device.can_receive());
    rsx! {
        article { class: "history-row",
            span { class: "entry-icon {class}", Icon { name: icon, size: 17 } }
            div { class: "entry-content",
                p { class: "entry-text", "{entry.text}" }
                div { class: "entry-meta", span { "{entry.label}" } span { "·" } span { "{entry.station}" } span { "·" } time { "{entry.time}" } }
                if entry.delivery == Delivery::Failed { p { class: "entry-error", "{entry.detail}" } }
                if entry.delivery == Delivery::Accepted { p { class: "entry-meta", "{entry.detail}" } }
            }
            span { class: "delivery-status {class}", "{status}" }
            button { class: "icon-button repeat-button", title: "Повторить команду", aria_label: "Повторить команду", disabled: !enabled,
                onclick: move |_| api::repeat(state, retry.clone()), Icon { name: "refresh", size: 16 }
            }
        }
    }
}
