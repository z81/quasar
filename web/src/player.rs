use dioxus::prelude::*;
use serde_json::json;

use crate::{
    api,
    home_assistant::{dial::Dial, model::Control},
    icons::Icon,
    state::ControlState,
};

mod music;

#[component]
pub fn PlayerCard(state: ControlState) -> Element {
    rsx! { PlayerControls { key: "{state.selected}", state, expanded: false } }
}

#[component]
pub fn MusicPage(state: ControlState) -> Element {
    rsx! { PlayerControls { key: "{state.selected}", state, expanded: true } }
}

#[component]
fn PlayerControls(state: ControlState, expanded: bool) -> Element {
    let external = use_signal(|| None::<music::ExternalPlayback>);
    let other = external();
    let player = (state.player)();
    let title = other
        .as_ref()
        .map(|e| e.track.name.clone())
        .or_else(|| player.as_ref().and_then(|p| p.title.clone()));
    let artist = other
        .as_ref()
        .map(|e| e.track.artists.clone())
        .or_else(|| player.as_ref().and_then(|p| p.artist.clone()));
    let enabled = state.can_send();
    let can_volume = enabled && player.as_ref().is_some_and(|p| p.capabilities.set_volume);
    let album = other
        .as_ref()
        .map(|e| e.track.album.clone())
        .or_else(|| player.as_ref().and_then(|p| p.album.clone()));
    let cover_url = other
        .as_ref()
        .and_then(|e| e.cover_url.clone())
        .or_else(|| {
            if other.is_none() {
                player.as_ref().and_then(|p| p.cover_url.clone())
            } else {
                None
            }
        });
    let device_name = state
        .device()
        .map(|device| device.label().to_owned())
        .unwrap_or_else(|| "Колонка".into());
    let volume = Control {
        id: "volume".into(),
        label: if other.is_some() {
            "Громкость колонки"
        } else {
            "Громкость"
        }
        .into(),
        kind: "range".into(),
        value: player
            .as_ref()
            .and_then(|p| p.volume)
            .map_or(serde_json::Value::Null, |value| json!(value)),
        min: Some(0.0),
        max: Some(100.0),
        step: Some(1.0),
        unit: Some("%".into()),
        options: vec![],
    };
    rsx! {
        document::Stylesheet { href: asset!("/assets/home-assistant-dial.css") }
        document::Stylesheet { href: asset!("/assets/ocean-player.css") }
        div { class: "ocean-player-wrap",
            section { class: if expanded { "ocean-player is-expanded" } else { "ocean-player" }, aria_label: "Плеер выбранной колонки",
                AlbumArt { cover_url, album: album.clone() }
                div { class: "player-track",
                    h2 { title: title.clone().unwrap_or_else(|| "Музыка на колонке".into()),
                        if let Some(title) = title.as_ref() { "{title}" } else { "Музыка на колонке" }
                    }
                    p { class: "player-artist",
                        if let Some(artist) = artist.as_ref() { "{artist}" }
                        if let Some(album) = album { if artist.is_some() { " · " } "{album}" }
                    }
                    if let Some(other) = &other { p { class: "player-external-device", "Играет на {other.device_name}" } }
                    if other.is_none() && player.as_ref().is_some_and(|p| p.muted == Some(true)) { p { class: "player-muted", "Звук выключен" } }
                }
                div { class: "player-volume-dial",
                    Dial { control: volume, entity_name: device_name, disabled: !can_volume,
                        onchange: move |(_, value): (String, serde_json::Value)| {
                            if let Some(level) = value.as_f64().filter(|v| v.is_finite() && (0.0..=100.0).contains(v) && v.fract() == 0.0) {
                                api::player_action(state, "set_volume", Some(level as u8));
                            }
                        }
                    }
                }
                music::MusicPanel { state, expanded, external }
                if let Some(error) = (state.player_error)() { p { class: "player-note failure", "Плеер недоступен: {error}" } }
                else if other.is_none() && player.as_ref().is_none_or(|p| p.updated_at.is_none()) { p { class: "player-note", "Ждём состояние от колонки…" } }
            }
        }
    }
}

#[component]
fn AlbumArt(cover_url: Option<String>, album: Option<String>) -> Element {
    let mut failed = use_signal(|| None::<String>);
    let cover_url = cover_url.filter(|url| failed.read().as_ref() != Some(url));
    rsx! {
        span { class: "player-art",
            if let Some(url) = cover_url {
                img {
                    key: "{url}",
                    src: url.clone(),
                    alt: album.map(|name| format!("Обложка альбома «{name}»")).unwrap_or_else(|| "Обложка альбома".into()),
                    width: "160", height: "160", decoding: "async", loading: "eager", referrerpolicy: "no-referrer",
                    onerror: move |_| failed.set(Some(url.clone())),
                    onload: move |_| failed.set(None),
                }
            } else {
                Icon { name: "music", size: 28 }
            }
        }
    }
}
