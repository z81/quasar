use std::collections::HashMap;

use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{api, state::ControlState};

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct Track {
    uri: String,
    name: String,
    artists: String,
    album: String,
    duration_ms: Option<u64>,
    url: Option<String>,
}

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct Authorization {
    configured: bool,
    connected: bool,
    reconnect_required: bool,
    missing_scopes: Vec<String>,
    authorize_url: Option<String>,
    redirect_uri: Option<String>,
}

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct MusicStatus {
    track: Option<Track>,
    position_ms: Option<u64>,
    duration_ms: Option<u64>,
    playing: bool,
    shuffle: Option<bool>,
    repeat: Option<String>,
    capabilities: HashMap<String, bool>,
    authorization: Authorization,
    message: Option<String>,
}

#[derive(Clone, Default, Deserialize, PartialEq)]
struct LyricLine {
    at_ms: u64,
    text: String,
}

#[derive(Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct Content {
    tracks: Vec<Track>,
    lines: Vec<LyricLine>,
    plain: Option<String>,
    message: Option<String>,
    name: Option<String>,
    genres: Vec<String>,
    followers: Option<u64>,
    url: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
struct Editor {
    status: Signal<Option<MusicStatus>>,
    received_at: Signal<f64>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    notice: Signal<Option<String>>,
    tab: Signal<String>,
    query: Signal<String>,
    content: Signal<Option<Content>>,
    content_error: Signal<Option<String>>,
    generation: Signal<u64>,
    saved: Signal<Option<bool>>,
}

fn endpoint(duid: &str) -> String {
    format!("/devices/{}/player/music", api::encoded(duid))
}

async fn refresh(state: ControlState, duid: &str, mut editor: Editor) {
    match api::get::<MusicStatus>(state, &endpoint(duid)).await {
        Ok(status) => {
            editor.received_at.set(js_sys::Date::now());
            editor.status.set(Some(status));
            editor.error.set(None);
        }
        Err(error) => {
            editor.status.set(None);
            editor.error.set(Some(error));
        }
    }
}

fn section(state: ControlState, duid: String, mut editor: Editor, tab: String) {
    let generation = *editor.generation.peek() + 1;
    editor.generation.set(generation);
    editor.content.set(None);
    editor.content_error.set(None);
    if tab.is_empty() || tab == "search" {
        return;
    }
    spawn(async move {
        let result = api::get::<Content>(state, &format!("{}?op={}", endpoint(&duid), tab)).await;
        if *editor.generation.peek() != generation {
            return;
        }
        match result {
            Ok(content) => editor.content.set(Some(content)),
            Err(error) => editor.content_error.set(Some(error)),
        }
    });
}

fn perform(state: ControlState, duid: String, mut editor: Editor, body: Value) {
    if *editor.busy.peek() {
        return;
    }
    editor.busy.set(true);
    editor.error.set(None);
    editor.notice.set(None);
    spawn(async move {
        match api::mutate(state, "POST", &endpoint(&duid), Some(&body)).await {
            Ok(_) => {
                editor.notice.set(Some("Spotify принял запрос".into()));
                if body["action"] == "save" {
                    editor.saved.set(body["saved"].as_bool());
                }
                refresh(state, &duid, editor).await;
                let tab = editor.tab.peek().clone();
                section(state, duid, editor, tab);
                api::refresh_player(state).await;
            }
            Err(error) => editor.error.set(Some(error)),
        }
        editor.busy.set(false);
    });
}

fn time_label(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[component]
pub(super) fn MusicPanel(state: ControlState) -> Element {
    let duid = (state.selected)();
    rsx! {
        if !duid.is_empty() { MusicForStation { key: "{duid}", state, duid } }
    }
}

#[component]
fn MusicForStation(state: ControlState, duid: String) -> Element {
    let editor = Editor {
        status: use_signal(|| None::<MusicStatus>),
        received_at: use_signal(js_sys::Date::now),
        busy: use_signal(|| false),
        error: use_signal(|| None::<String>),
        notice: use_signal(|| None::<String>),
        tab: use_signal(String::new),
        query: use_signal(String::new),
        content: use_signal(|| None::<Content>),
        content_error: use_signal(|| None::<String>),
        generation: use_signal(|| 0),
        saved: use_signal(|| None::<bool>),
    };
    let mut clock = use_signal(js_sys::Date::now);
    let mut seek = use_signal(|| None::<u64>);
    let mut redirect = use_signal(String::new);
    let poll_duid = duid.clone();
    use_future(move || {
        let duid = poll_duid.clone();
        async move {
            if duid.is_empty() {
                return;
            }
            loop {
                refresh(state, &duid, editor).await;
                gloo_timers::future::TimeoutFuture::new(10000).await;
            }
        }
    });
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(500).await;
            clock.set(js_sys::Date::now());
        }
    });
    let track_uri = use_memo(move || {
        editor
            .status
            .read()
            .as_ref()
            .and_then(|value| value.track.as_ref())
            .map(|track| track.uri.clone())
            .unwrap_or_default()
    });
    let section_duid = duid.clone();
    use_effect(move || {
        let _uri = track_uri();
        let tab = (editor.tab)();
        section(state, section_duid.clone(), editor, tab);
    });
    let saved_duid = duid.clone();
    let library_ready = use_memo(move || {
        editor.status.read().as_ref().is_some_and(|status| {
            status.authorization.connected
                && !status
                    .authorization
                    .missing_scopes
                    .iter()
                    .any(|scope| scope == "user-library-read")
        })
    });
    use_effect(move || {
        let uri = track_uri();
        let ready = library_ready();
        let mut editor = editor;
        seek.set(None);
        editor.saved.set(None);
        if uri.is_empty() || !ready {
            return;
        }
        let duid = saved_duid.clone();
        spawn(async move {
            let result = api::get::<Value>(
                state,
                &format!("{}?op=saved&uri={}", endpoint(&duid), api::encoded(&uri)),
            )
            .await;
            if *track_uri.peek() != uri {
                return;
            }
            if let Ok(value) = result {
                editor.saved.set(value["saved"].as_bool());
            }
        });
    });
    let snapshot = (editor.status)().unwrap_or_default();
    let uri = track_uri();
    let can_send = state.can_send() && !(editor.busy)();
    let can = |name: &str| can_send && snapshot.capabilities.get(name).copied().unwrap_or(false);
    let elapsed = if snapshot.playing {
        (clock() - (editor.received_at)()).clamp(0.0, 15000.0) as u64
    } else {
        0
    };
    let position = snapshot
        .position_ms
        .map(|value| (value + elapsed).min(snapshot.duration_ms.unwrap_or(u64::MAX)));
    let duration = snapshot.duration_ms.unwrap_or(0);
    let can_seek = can("seek") && duration > 0;
    let can_shuffle = can("shuffle");
    let can_repeat = can("repeat");
    let can_queue = can("queue");
    let can_save = snapshot.authorization.connected
        && snapshot
            .authorization
            .missing_scopes
            .iter()
            .all(|scope| scope != "user-library-modify")
        && !(editor.busy)();
    let shuffle = snapshot.shuffle.unwrap_or(false);
    let repeat = snapshot.repeat.as_deref().unwrap_or("off");
    let saved = (editor.saved)();
    let tab = (editor.tab)();
    let content = (editor.content)();
    let current_line = content.as_ref().and_then(|value| {
        value
            .lines
            .iter()
            .rposition(|line| position.is_some_and(|position| position >= line.at_ms))
    });
    let seek_duid = duid.clone();
    let seek_uri = uri.clone();
    let shuffle_duid = duid.clone();
    let repeat_duid = duid.clone();
    let save_duid = duid.clone();
    let save_uri = uri.clone();
    let search_duid = duid.clone();
    let auth_duid = duid.clone();
    rsx! {
        document::Stylesheet { href: asset!("/assets/music.css") }
        div { class: "music-panel",
            if let Some(position) = position {
                div { class: "music-progress",
                    span { "{time_label(seek().unwrap_or(position))}" }
                    input { r#type: "range", min: "0", max: "{duration}", step: "1000", value: "{seek().unwrap_or(position)}", disabled: !can_seek,
                        aria_label: "Позиция в треке",
                        oninput: move |event| seek.set(event.value().parse::<u64>().ok()),
                        onchange: move |_| {
                            let selected_position = *seek.peek();
                            if let Some(position) = selected_position {
                                perform(state, seek_duid.clone(), editor, json!({"action":"seek","position_ms":position,"expected_uri":seek_uri}));
                                seek.set(None);
                            }
                        }
                    }
                    span { "{time_label(duration)}" }
                }
            }
            div { class: "music-actions",
                button { class: "button secondary", disabled: !can_shuffle, aria_pressed: "{shuffle}", onclick: move |_| perform(state, shuffle_duid.clone(), editor, json!({"action":"shuffle","enabled":!shuffle})), if shuffle { "Вперемешку: вкл." } else { "Вперемешку" } }
                label { "Повтор"
                    select { value: "{repeat}", disabled: !can_repeat, aria_label: "Режим повтора", onchange: move |event| perform(state, repeat_duid.clone(), editor, json!({"action":"repeat","mode":event.value()})),
                        option { value: "off", "Выключен" } option { value: "context", "Очередь" } option { value: "track", "Трек" }
                    }
                }
                if !uri.is_empty() {
                    button { class: "button secondary", disabled: !can_save || saved.is_none(), aria_pressed: "{saved.unwrap_or(false)}", onclick: move |_| perform(state, save_duid.clone(), editor, json!({"action":"save","uri":save_uri,"saved":!saved.unwrap_or(false)})), if saved == Some(true) { "♥ Любимый трек" } else { "♡ В любимые" } }
                }
            }
            if let Some(message) = snapshot.message { p { class: "player-note", "{message}" } }
            if let Some(error) = (editor.error)() { p { class: "form-feedback failure", role: "alert", "{error}" } }
            if let Some(notice) = (editor.notice)() { p { class: "player-note", role: "status", "{notice}" } }
            div { class: "music-tabs", role: "group", aria_label: "Музыка",
                for (name, label) in [("queue", "Очередь"), ("search", "Найти трек"), ("lyrics", "Текст песни"), ("artist", "Исполнитель"), ("related", "Похожие треки")] {
                    button { class: if tab == name { "button secondary active" } else { "button secondary" }, aria_pressed: "{tab == name}", onclick: move |_| { let mut editor = editor; editor.tab.set(if *editor.tab.peek() == name { String::new() } else { name.into() }); }, "{label}" }
                }
            }
            if tab == "search" {
                form { class: "music-search", onsubmit: move |event| {
                    event.prevent_default();
                    let mut editor = editor;
                    let query = editor.query.peek().trim().to_owned();
                    if query.is_empty() { return; }
                    let duid = search_duid.clone();
                    let generation = *editor.generation.peek() + 1;
                    editor.generation.set(generation); editor.content.set(None); editor.content_error.set(None);
                    spawn(async move {
                        let result = api::get::<Content>(state, &format!("{}?op=search&q={}", endpoint(&duid), api::encoded(&query))).await;
                        if *editor.generation.peek() != generation { return; }
                        match result { Ok(value) => editor.content.set(Some(value)), Err(error) => editor.content_error.set(Some(error)) }
                    });
                },
                    input { r#type: "search", maxlength: "200", aria_label: "Название трека или исполнитель", placeholder: "Трек или исполнитель", value: "{editor.query}", oninput: move |event| { let mut editor = editor; editor.query.set(event.value()); } }
                    button { class: "button secondary", r#type: "submit", disabled: !snapshot.authorization.connected, "Найти" }
                }
            }
            if !tab.is_empty() {
                div { class: "music-content",
                    if tab != "search" {
                        button { class: "text-button", onclick: {
                            let duid = duid.clone(); let tab = tab.clone();
                            move |_| section(state, duid.clone(), editor, tab.clone())
                        }, "Обновить" }
                    }
                    if let Some(error) = (editor.content_error)() { p { class: "form-feedback failure", role: "status", "{error}" } }
                    else if let Some(content) = content {
                        if let Some(message) = content.message { p { class: "player-note", "{message}" } }
                        if matches!(tab.as_str(), "queue" | "search" | "related") {
                            if content.tracks.is_empty() { p { class: "player-note", "Треков нет" } }
                            ol { class: "music-track-list",
                                for (index, track) in content.tracks.into_iter().enumerate() {
                                    MusicTrack { key: "{index}:{track.uri}", state, duid: duid.clone(), editor, track, can_play: can_send && snapshot.authorization.connected, can_queue }
                                }
                            }
                        }
                        if tab == "lyrics" {
                            p { class: "player-note", a { href: "https://lrclib.net", target: "_blank", rel: "noopener noreferrer", "Текст: LRCLIB" } }
                            if !content.lines.is_empty() {
                                if let Some(index) = current_line { p { class: "music-current-lyric", "{content.lines[index].text}" } }
                                div { class: "music-lyrics",
                                    for (index, line) in content.lines.into_iter().enumerate() { p { class: if current_line == Some(index) { "current" } else { "" }, "{line.text}" } }
                                }
                            } else if let Some(plain) = content.plain { p { class: "music-lyrics plain", "{plain}" } }
                        }
                        if tab == "artist" {
                            if let Some(name) = content.name { h3 { "{name}" } }
                            if !content.genres.is_empty() { p { {content.genres.join(", ")} } }
                            if let Some(count) = content.followers { p { "Подписчики: {count}" } }
                            if let Some(url) = content.url { a { href: url, target: "_blank", rel: "noopener noreferrer", "Исполнитель в Spotify" } }
                        }
                    } else if tab != "search" { p { class: "player-note", "Загрузка…" } }
                }
            }
            if snapshot.authorization.configured && (!snapshot.authorization.connected || snapshot.authorization.reconnect_required) {
                details { class: "music-auth",
                    summary { if snapshot.authorization.connected { "Разрешить доступ к любимым трекам" } else { "Подключить управление Spotify" } }
                    p { "Откройте ссылку, разрешите доступ и вставьте полный адрес, на который Spotify перенаправит браузер. Страница по этому адресу может не открыться — нужен сам адрес." }
                    if let Some(url) = snapshot.authorization.authorize_url { a { href: url, target: "_blank", rel: "noopener noreferrer", "Войти в Spotify" } }
                    form { class: "music-search", onsubmit: move |event| { event.prevent_default(); perform(state, auth_duid.clone(), editor, json!({"action":"authorize","url":redirect()})); redirect.set(String::new()); },
                        input { r#type: "text", autocomplete: "off", spellcheck: "false", maxlength: "8192", aria_label: "Адрес перенаправления Spotify", placeholder: snapshot.authorization.redirect_uri.unwrap_or_default(), value: "{redirect}", oninput: move |event| redirect.set(event.value()) }
                        button { class: "button secondary", r#type: "submit", disabled: (editor.busy)() || redirect().is_empty(), "Подключить" }
                    }
                }
            }
        }
    }
}

#[component]
fn MusicTrack(
    state: ControlState,
    duid: String,
    editor: Editor,
    track: Track,
    can_play: bool,
    can_queue: bool,
) -> Element {
    let add_duid = duid.clone();
    let add_uri = track.uri.clone();
    rsx! {
        li {
            div { class: "music-track-copy", strong { "{track.name}" } span { "{track.artists}" } small { "{track.album}" } }
            div { class: "music-track-actions",
                button { class: "button secondary", disabled: !can_play, aria_label: "Включить {track.name}", onclick: move |_| perform(state, duid.clone(), editor, json!({"action":"play","uri":track.uri})), "Играть" }
                button { class: "button secondary", disabled: !can_queue, aria_label: "Добавить в очередь {track.name}", onclick: move |_| perform(state, add_duid.clone(), editor, json!({"action":"queue_add","uri":add_uri})), "+ В очередь" }
                if let Some(url) = track.url { a { href: url, target: "_blank", rel: "noopener noreferrer", "Spotify ↗" } }
            }
        }
    }
}
