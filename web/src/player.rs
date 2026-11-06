use dioxus::prelude::*;

use crate::api;
use crate::icons::Icon;
use crate::state::ControlState;

#[component]
pub fn PlayerCard(state: ControlState) -> Element {
    rsx! { PlayerControls { key: "{state.selected}", state } }
}

#[component]
fn PlayerControls(state: ControlState) -> Element {
    let mut volume = use_signal(String::new);
    let mut dirty = use_signal(|| false);
    use_effect(move || {
        let snapshot = (state.player)()
            .and_then(|player| player.volume)
            .map(|value| value.to_string())
            .unwrap_or_default();
        if !dirty() && *volume.peek() != snapshot {
            volume.set(snapshot);
        } else if dirty() && *volume.peek() == snapshot {
            dirty.set(false);
        }
    });
    let player = (state.player)();
    let enabled = state.can_send();
    let level = volume()
        .parse::<f64>()
        .ok()
        .filter(|level| {
            level.is_finite() && *level >= 0.0 && *level <= 100.0 && level.fract() == 0.0
        })
        .map(|level| level as u8);
    let can_volume = enabled
        && player
            .as_ref()
            .is_some_and(|player| player.capabilities.set_volume);
    let status = player
        .as_ref()
        .map(|player| match player.state.as_str() {
            "playing" => "Играет",
            "paused" => "На паузе",
            "buffering" => "Загрузка",
            "stopped" | "idle" | "finished" => "Остановлено",
            _ => "Состояние неизвестно",
        })
        .unwrap_or("Состояние неизвестно");
    let can_pause = player
        .as_ref()
        .is_some_and(|player| player.capabilities.pause);
    let can_resume = player
        .as_ref()
        .is_some_and(|player| player.capabilities.resume);
    rsx! {
        section { class: "card player-card", aria_label: "Плеер выбранной колонки",
            div { class: "player-now",
                span { class: "player-art", Icon { name: "music", size: 28 } }
                div { class: "player-track",
                    div { class: "card-kicker", "СЕЙЧАС НА КОЛОНКЕ" if let Some(source) = player.as_ref().and_then(|player| player.source.as_ref()) { span { class: "source-label", "{source}" } } }
                    h2 { if let Some(title) = player.as_ref().and_then(|player| player.title.as_ref()) { "{title}" } else { "{status}" } }
                    p { if let Some(artist) = player.as_ref().and_then(|player| player.artist.as_ref()) { "{artist} · " } "{status}" if player.as_ref().is_some_and(|player| player.muted == Some(true)) { " · Звук выключен" } }
                }
            }
            div { class: "player-controls", role: "group", aria_label: "Управление воспроизведением",
                for (action,icon,label,available) in [
                    ("previous","previous","Предыдущий трек",player.as_ref().is_some_and(|player| player.capabilities.previous)),
                    if can_pause { ("pause", "pause", "Пауза", true) } else { ("resume", "play", "Продолжить", can_resume) },
                    ("next","next","Следующий трек",player.as_ref().is_some_and(|player| player.capabilities.next)),
                ] {
                    button { class: if matches!(action, "pause" | "resume") { "icon-button player-button player-toggle" } else { "icon-button player-button" }, disabled: !enabled || !available, aria_label: label, title: label, onclick: move |_| api::player_action(state, action, None), Icon { name: icon, size: 18 } }
                }
                button { class: "icon-button player-button", disabled: !enabled, aria_label: "Остановить воспроизведение", title: "Остановить", onclick: move |_| api::player_action(state,"stop",None), Icon { name: "stop", size: 17 } }
            }
            form { class: "volume-form", onsubmit: move |event| {
                event.prevent_default(); if let Some(level) = level.filter(|_| can_volume) { volume.set(level.to_string()); dirty.set(true); api::player_action(state,"set_volume",Some(level)); }
            },
                div { class: "volume-label", label { r#for: "volume-value", Icon { name: "volume-up", size: 15 } "Громкость" }
                    span { if let Some(current) = player.as_ref().and_then(|player| player.volume) { "Сейчас: {current}%" } else { "Нет данных" } }
                }
                div { class: "volume-inputs",
                    input { r#type: "range", min: "0", max: "100", step: "1", aria_label: "Желаемая громкость", value: level.unwrap_or(0), disabled: !can_volume, oninput: move |event| { dirty.set(true); volume.set(event.value()); } }
                    input { id: "volume-value", class: "volume-number", r#type: "text", inputmode: "numeric", pattern: "[0-9]+([.][0-9]*)?", maxlength: "6", aria_label: "Громкость в процентах", placeholder: "—", value: "{volume}", disabled: !can_volume, oninput: move |event| { dirty.set(true); volume.set(event.value()); } }
                    span { class: "form-help", "%" }
                    button { class: "button secondary", r#type: "submit", disabled: !can_volume || level.is_none(), "Применить" }
                }
            }
            if dirty() && level.is_none() { p { class: "player-note failure", "Введите целое число от 0 до 100." } }
            if let Some(error) = (state.player_error)() { p { class: "player-note failure", "Плеер недоступен: {error}" } }
            else if player.as_ref().is_none_or(|player| player.updated_at.is_none()) { p { class: "player-note", "Ждём состояние от колонки. Недоступные для текущего источника команды отключены." } }
        }
    }
}
