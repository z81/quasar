use dioxus::prelude::*;

use super::{duration, mutate, PanelData, Timer};
use crate::{api, icons::Icon, state::ControlState};

#[component]
pub(super) fn TimerCard(
    state: ControlState,
    duid: String,
    data: PanelData,
    item: Timer,
    now: i64,
) -> Element {
    let mut confirm_delete = use_signal(|| false);
    let busy = (data.busy)();
    let active = item.active();
    let running = item.running();
    let paused = item.status == "paused";
    let remaining = item.remaining(now);
    let percentage = if item.duration_seconds == 0 {
        0
    } else {
        (remaining * 100 / (i64::from(item.duration_seconds) * 1000)).clamp(0, 100)
    };
    let stop_duid = duid.clone();
    let stop_id = item.id.clone();
    let delete_duid = duid.clone();
    let delete_id = item.id.clone();
    let actions = if running {
        vec![("pause", "Пауза", "pause"), ("cancel", "Отменить", "stop")]
    } else if paused {
        vec![
            ("resume", "Продолжить", "play"),
            ("cancel", "Отменить", "stop"),
        ]
    } else if !active {
        vec![("restart", "Запустить снова", "refresh")]
    } else {
        Vec::new()
    };
    rsx! {
        article { class: if active { "card timer-item active-timer" } else { "card timer-item" },
            div { class: "timer-item-heading",
                span { class: "card-kicker", Icon { name: "timer", size: 15 } "ТАЙМЕР" }
                span { class: match item.status.as_str() {
                    "ringing" | "paused" => "schedule-status highlighted",
                    "missed" | "failed" => "schedule-status failure",
                    _ => "schedule-status",
                }, "{item.status_label(remaining)}" }
            }
            h3 { "{item.title}" }
            div { class: "timer-countdown", role: "timer", aria_label: "Осталось времени", "{duration(remaining)}" }
            p { class: "timer-total", "Длительность: {duration(i64::from(item.duration_seconds) * 1000)}" }
            if running || paused {
                div { class: if paused { "timer-track paused" } else { "timer-track" }, "aria-hidden": "true", span { style: "width: {percentage}%" } }
            }
            if running && remaining == 0 {
                p { class: "timer-waiting", "Отсчёт закончился. Ждём результат от сервера." }
            }
            if active {
                div { class: "schedule-ringing",
                    p { match item.status.as_str() {
                        "ringing" => "Колонка сообщила, что сигнал звучит.",
                        "sent" => "Сигнал отправлен. Ждём подтверждение колонки.",
                        _ => "Передаём сигнал колонке.",
                    } }
                    div { button { class: "button primary", disabled: busy, onclick: move |_| mutate(state, stop_duid.clone(), data, "POST", format!("/{}/stop", api::encoded(&stop_id)), None), Icon { name: "stop", size: 15 } "Остановить звонок" } }
                    small { "Остановка завершит все активные сигналы таймеров и будильников этой колонки." }
                }
            }
            if let Some(error) = item.last_error.as_ref() {
                p { class: "schedule-error", role: "status", Icon { name: "alert", size: 14 } "{error}" }
            }
            div { class: "timer-item-actions",
                div { class: "timer-playback-actions",
                    for (action, label, icon) in actions {
                        TimerAction { state, duid: duid.clone(), data, id: item.id.clone(), action, label, icon }
                    }
                }
                if confirm_delete() {
                    span { class: "schedule-delete-confirm", "Удалить?"
                        button { class: "text-button danger", disabled: busy || active, onclick: move |_| { mutate(state, delete_duid.clone(), data, "DELETE", format!("/{}", api::encoded(&delete_id)), None); confirm_delete.set(false); }, "Да" }
                        button { class: "text-button", disabled: busy, onclick: move |_| confirm_delete.set(false), "Нет" }
                    }
                } else {
                    button { class: "text-button", disabled: busy || active, onclick: move |_| confirm_delete.set(true), Icon { name: "trash", size: 14 } "Удалить" }
                }
            }
        }
    }
}

#[component]
fn TimerAction(
    state: ControlState,
    duid: String,
    data: PanelData,
    id: String,
    action: &'static str,
    label: &'static str,
    icon: &'static str,
) -> Element {
    rsx! {
        button { class: "text-button", disabled: (data.busy)(), onclick: move |_| mutate(state, duid.clone(), data, "POST", format!("/{}/{action}", api::encoded(&id)), None), Icon { name: icon, size: 14 } "{label}" }
    }
}
