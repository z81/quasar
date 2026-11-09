use std::collections::BTreeSet;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};

use crate::{
    device_access::send,
    icons::Icon,
    state::ControlState,
    station_preview::{
        display_frame, DisplayDraft, DISPLAY_DIGIT_X, DISPLAY_EXTRA_POLYGONS,
        DISPLAY_SEGMENT_POLYGONS,
    },
};

const EXTRA_SEGMENTS: [&str; 14] = [
    "H1", "H2", "L1", "L2", "M1", "M2", "P1", "P2", "P4", "P5", "Q1", "Q2", "Q4", "Q5",
];
const ALPHABET: &str = "0123456789ABCDEFGH IJLNOPQRSTUY-_";

fn toggle(mut segments: Signal<BTreeSet<String>>, segment: &str) {
    if !segments.write().remove(segment) {
        segments.write().insert(segment.to_owned());
    }
}

#[component]
fn Segment(
    id: String,
    points: String,
    selected: bool,
    editable: bool,
    segments: Signal<BTreeSet<String>>,
) -> Element {
    let key_id = id.clone();
    rsx! {
        polygon { points, class: if selected { "midi-segment lit" } else { "midi-segment" },
            role: if editable { "button" } else { "img" }, tabindex: if editable { "0" } else { "-1" },
            "aria-label": "Сегмент {id}", "aria-pressed": selected, "data-segment": "{id}",
            onclick: move |_| { if editable { toggle(segments, &id); } },
            onkeydown: move |event| { if editable && (event.key() == Key::Enter || event.key() == Key::Character(" ".into())) { event.prevent_default(); toggle(segments, &key_id); } }
        }
    }
}

#[component]
pub fn MidiDisplay(
    mut state: ControlState,
    duid: String,
    bridge: Signal<Option<document::Eval>>,
    ready: Signal<bool>,
    mut status: Signal<Option<Value>>,
    mut busy: Signal<bool>,
) -> Element {
    let initial = use_hook(|| {
        state
            .station_previews
            .peek()
            .get(&duid)
            .map(|preview| preview.display.clone())
            .unwrap_or_default()
    });
    let mut mode = use_signal(|| initial.mode.clone());
    let mut text = use_signal(|| initial.text.clone());
    let mut segments = use_signal(|| initial.segments.clone());
    let mut animation = use_signal(|| initial.animation.clone());
    let mut brightness = use_signal(|| initial.brightness.clone());
    let mut duration = use_signal(|| initial.duration.clone());
    let mut tick = use_signal(|| 0_usize);
    let mut showing = use_signal(|| false);
    let mut feedback = use_signal(|| None::<(bool, String)>);
    let preview_duid = duid.clone();
    use_effect(move || {
        let draft = DisplayDraft {
            mode: mode(),
            text: text(),
            segments: segments(),
            animation: animation(),
            brightness: brightness(),
            duration: duration(),
        };
        let changed = state
            .station_previews
            .peek()
            .get(&preview_duid)
            .map(|preview| preview.display != draft)
            .unwrap_or(true);
        if changed {
            state.station_previews.with_mut(|previews| {
                previews.entry(preview_duid.clone()).or_default().display = draft;
            });
        }
    });
    use_future(move || async move {
        loop {
            TimeoutFuture::new(350).await;
            let next = tick.peek().wrapping_add(1);
            tick.set(next);
        }
    });
    use_effect(move || {
        if !ready() {
            showing.set(false);
            feedback.set(None);
            return;
        }
        let Some(event) = status() else {
            return;
        };
        let kind = event["type"].as_str().unwrap_or_default();
        if kind == "display_done" {
            showing.set(false);
            busy.set(false);
            let message = if event["outcome"] == "interrupted" {
                "Показ завершён: штатный экран колонки сменил содержимое."
            } else {
                "Показ завершён. Предыдущее состояние сегментов восстановлено."
            };
            feedback.set(Some((true, message.into())));
        } else if kind == "display_error" || (kind == "error" && *showing.peek()) {
            showing.set(false);
            busy.set(false);
            feedback.set(Some((
                false,
                event["message"]
                    .as_str()
                    .unwrap_or("Не удалось показать на дисплее")
                    .to_owned(),
            )));
        }
    });
    let brightness_value = brightness()
        .parse::<u8>()
        .ok()
        .filter(|value| *value <= 100);
    let duration_value = duration()
        .parse::<u64>()
        .ok()
        .filter(|value| (1..=15).contains(value));
    let text_valid = !text().trim().is_empty()
        && text().len() <= 32
        && text()
            .to_ascii_uppercase()
            .chars()
            .all(|letter| ALPHABET.contains(letter));
    let duration_valid = duration_value.is_some_and(|seconds| {
        mode() != "text" || text().len() <= 4 || seconds * 1_000 >= (text().len() as u64 + 5) * 100
    });
    let valid = brightness_value.is_some() && duration_valid && (mode() != "text" || text_valid);
    let frame = display_frame(
        &DisplayDraft {
            mode: mode(),
            text: text(),
            segments: segments(),
            animation: animation(),
            brightness: brightness(),
            duration: duration(),
        },
        tick(),
    );
    let preview = frame.segments;
    let intensity = frame.intensity;
    let editable = mode() == "frame" && !busy();
    rsx! {
        document::Stylesheet { href: asset!("/assets/midi-display.css") }
        section { class: "midi-editor", aria_label: "Редактор сегментного дисплея Миди",
            h3 { "Своя Миди" }
            p { class: "midi-editor-help", "Надпись, рисунок из сегментов или короткая анимация. Для сегментного экрана Миди; нужны права записи в дисплей." }
            div { class: "mode-tabs midi-modes", role: "tablist", aria_label: "Содержимое дисплея",
                for (value, label) in [("text", "Надпись"), ("frame", "Рисунок"), ("animation", "Анимация")] {
                    button { class: if mode() == value { "mode-tab selected" } else { "mode-tab" }, role: "tab", aria_selected: mode() == value, disabled: busy(), onclick: move |_| { mode.set(value.into()); feedback.set(None); }, "{label}" }
                }
            }
            div { class: "midi-preview", style: "--midi-preview-level:{intensity}",
                svg { view_box: "0 0 300 92", role: "group", "aria-label": if editable { "Нажмите сегменты, чтобы нарисовать" } else { "Предпросмотр дисплея" },
                    for (index, x) in DISPLAY_DIGIT_X.into_iter().enumerate() {
                        g { transform: "translate({x} 8)",
                            for (letter, points) in DISPLAY_SEGMENT_POLYGONS {
                                Segment { id: format!("{letter}{}", index+1), points: points.to_owned(), selected: preview.contains(&format!("{letter}{}", index+1)), editable, segments }
                            }
                        }
                    }
                    for (id, points) in DISPLAY_EXTRA_POLYGONS {
                        Segment { id: id.to_owned(), points: points.to_owned(), selected: preview.contains(id), editable, segments }
                    }
                }
            }
            if mode() == "text" {
                label { class: "form-field", span { "Текст" } input { id: "midi-text", value: "{text}", maxlength: "32", spellcheck: "false", disabled: busy(), aria_invalid: !text_valid, oninput: move |event| { text.set(event.value()); tick.set(0); feedback.set(None); } }
                    small { "До 32 символов. Латиница и цифры; длинная надпись прокручивается. Некоторые буквы выглядят условно." }
                }
                if !text_valid { p { class: "form-feedback failure", "Доступны цифры, A B C D E F G H I J L N O P Q R S T U Y, пробел, - и _." } }
            } else if mode() == "frame" {
                div { class: "midi-frame-actions", p { class: "midi-editor-help", "Нажимайте на сегменты в предпросмотре." }
                    button { class: "text-button", disabled: busy(), onclick: move |_| segments.set(BTreeSet::new()), "Очистить рисунок" }
                }
                details { class: "midi-extra", summary { "Дополнительные сегменты" }
                    p { "Положение этих сегментов в предпросмотре не отображается." }
                    div { for segment in EXTRA_SEGMENTS { button { class: if segments().contains(segment) { "selected" } else { "" }, disabled: busy(), aria_pressed: segments().contains(segment), onclick: move |_| toggle(segments, segment), "{segment}" } } }
                }
            } else {
                label { class: "form-field", span { "Анимация" } select { id: "midi-animation", value: "{animation}", disabled: busy(), onchange: move |event| animation.set(event.value()),
                    for (value, label) in [("wave", "Волна"), ("pulse", "Пульс"), ("orbit", "Орбита")] { option { value, selected: animation() == value, "{label}" } }
                } }
            }
            div { class: "midi-editor-options",
                label { class: "form-field", span { "Яркость, %" } input { id: "midi-brightness", r#type: "text", inputmode: "numeric", value: "{brightness}", disabled: busy(), aria_invalid: brightness_value.is_none(), oninput: move |event| brightness.set(event.value()) } }
                label { class: "form-field", span { "Длительность, с" } input { id: "midi-duration", r#type: "text", inputmode: "numeric", value: "{duration}", disabled: busy(), aria_invalid: !duration_valid, oninput: move |event| duration.set(event.value()) } }
                button { class: "button primary", disabled: !ready() || busy() || !valid, onclick: move |_| {
                    if !ready() || busy() || !valid { return; }
                    let mut command = json!({"type":format!("display_{}", mode()),"brightness":brightness_value.unwrap(),"duration_ms":duration_value.unwrap()*1000});
                    match mode().as_str() {
                        "text" => command["text"] = json!(text()),
                        "frame" => command["segments"] = json!(segments()),
                        _ => command["animation"] = json!(animation()),
                    }
                    status.set(None); feedback.set(None); showing.set(true); busy.set(true); send(bridge, command);
                }, if showing() { span { class: "spinner" } "Показываем…" } else { Icon { name: "play", size: 15 } "Показать" } }
            }
            if brightness_value.is_none() { p { class: "form-feedback failure", "Яркость — целое число от 0 до 100." } }
            if !duration_valid { p { class: "form-feedback failure", "Длительность — от 1 до 15 целых секунд. Для длинной надписи увеличьте время показа." } }
            p { class: "midi-editor-help", "После показа предыдущее состояние восстанавливается. Если штатный экран сменит содержимое, показ завершится раньше. Растровые картинки и GIF этот дисплей не поддерживает." }
            if let Some((success, message)) = feedback() { p { class: if success { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" } }
            crate::animation_timeline::AnimationTimeline { state, duid, bridge, ready, busy, status }
        }
    }
}
