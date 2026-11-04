use dioxus::prelude::*;

use crate::api;
use crate::icons::Icon;
use crate::state::{ControlState, Entry, Panel};

#[component]
pub fn History(mut state: ControlState, compact: bool) -> Element {
    let mut filter = use_signal(String::new);
    let mut confirm_clear = use_signal(|| false);
    let entries = (state.history)();
    let needle = filter().to_lowercase();
    let visible: Vec<_> = entries
        .iter()
        .filter(|entry| {
            compact
                || format!(
                    "{} {} {} {}",
                    entry.text,
                    entry.station,
                    entry.label,
                    entry.response.as_deref().unwrap_or("")
                )
                .to_lowercase()
                .contains(&needle)
        })
        .take(if compact { 4 } else { 500 })
        .cloned()
        .collect();
    rsx! {
        section { class: "history-section", aria_label: "История команд",
            div { class: "history-heading",
                div { class: "history-title", h2 { if compact { "Последние команды" } else { "Все команды" } } span { class: "count-badge", "{entries.len()}" } }
                if compact {
                    button { class: "text-button", onclick: move |_| state.panel.set(Panel::History), "Вся история" Icon { name: "arrow", size: 16 } }
                } else {
                    button { class: "text-button", disabled: entries.is_empty() || (state.sending)(), onclick: move |_| {
                        if !confirm_clear() { confirm_clear.set(true); return; }
                        dioxus::core::spawn_forever(async move {
                            match api::mutate(state, "DELETE", "/control/history", None).await {
                                Ok(_) => { state.history.set(Vec::new()); confirm_clear.set(false); }
                                Err(error) => state.notice.set(Some(error)),
                            }
                        });
                    }, Icon { name: "trash", size: 15 } if confirm_clear() { "Подтвердить удаление" } else { "Очистить" } }
                }
            }
            if !compact {
                div { class: "history-search", Icon { name: "search", size: 17 }
                    input { r#type: "search", aria_label: "Поиск по истории", placeholder: "Найти команду, ответ или колонку…", value: "{filter}", oninput: move |event| filter.set(event.value()) }
                }
            }
            if let Some(error) = (state.history_error)() { p { class: "validation-error", role: "status", "История недоступна: {error}" } }
            div { class: "card history-card", aria_live: "polite",
                if visible.is_empty() {
                    div { class: "history-empty", span { class: "empty-icon", Icon { name: "history", size: 25 } }
                        h3 { if entries.is_empty() { "У каждой команды будет своя история" } else { "Ничего не нашлось" } }
                        p { if entries.is_empty() { "Запрос, ответ Алисы и действия появятся здесь." } else { "Попробуйте другой текст для поиска." } }
                    }
                } else {
                    for entry in visible { HistoryRow { key: "{entry.id}", state, entry } }
                }
            }
            p { class: "history-note", if (state.persistent)() == Some(false) { "База данных недоступна: история и избранное сохраняются только до перезапуска сервера." } else { "История хранится на сервере. Результат обработки не подтверждает воспроизведение звука или физическое действие колонки." } }
        }
    }
}

#[component]
fn HistoryRow(state: ControlState, entry: Entry) -> Element {
    let (class, icon, status) = match entry.status.as_str() {
        "queued" => ("pending", "refresh", "В очереди"),
        "running" => ("pending", "refresh", "Обрабатываем"),
        "completed" => ("success", "check", "Обработано сервером"),
        "failed" => ("failed", "alert", "Ошибка"),
        _ => ("pending", "arrow", "Принято сервером"),
    };
    let retry = entry.clone();
    let saved = entry.clone();
    let enabled = !(state.sending)()
        && state.server_error.read().is_none()
        && state
            .devices
            .read()
            .iter()
            .any(|device| device.duid == entry.duid && device.can_receive());
    let can_save = !entry.path.ends_with("/player") && entry.path != "player";
    rsx! {
        article { class: "history-row",
            span { class: "entry-icon {class}", Icon { name: icon, size: 17 } }
            div { class: "entry-content",
                p { class: "entry-text", "{entry.text}" }
                div { class: "entry-meta", span { "{entry.label}" } span { "·" } span { "{entry.station}" } span { "·" } time { datetime: "{entry.created_at}", "{entry.time()}" } }
                if let Some(error) = &entry.error { p { class: "entry-error", "{error}" } }
                if let Some(response) = &entry.response {
                    if !response.trim().is_empty() { div { class: "alice-answer", span { class: "answer-label", Icon { name: "spark", size: 13 } "АЛИСА" } p { "{response}" } } }
                }
                if !entry.actions.is_empty() {
                    div { class: "entry-actions",
                        for action in &entry.actions {
                            div { class: "action-result",
                                span { class: "action-name", "{action.name}" }
                                span { class: "action-state", match action.status.as_str() { "completed" => "Выполнено", "failed" => "Ошибка", "sent" => "Передано колонке", "scheduled" => "Запланировано", _ => "Обрабатывается" } }
                                if let Some(result) = &action.result { p { "{result}" } }
                            }
                        }
                    }
                }
            }
            div { class: "history-row-tools",
                span { class: "delivery-status {class}", "{status}" }
                if can_save { button { class: "icon-button", title: "Сохранить в избранное", aria_label: "Сохранить в избранное", onclick: move |_| api::save_favorite(state, saved.text.chars().take(80).collect(), saved.duid.clone(), saved.path.clone(), saved.body.clone()), Icon { name: "star", size: 16 } } }
                button { class: "icon-button repeat-button", title: "Повторить команду", aria_label: "Повторить команду", disabled: !enabled, onclick: move |_| api::repeat(state, retry.clone()), Icon { name: "refresh", size: 16 } }
            }
        }
    }
}
