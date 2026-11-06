use std::collections::BTreeSet;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};

use crate::{device_access::send, icons::Icon};

const EXTRA_SEGMENTS: [&str; 14] = [
    "H1", "H2", "L1", "L2", "M1", "M2", "P1", "P2", "P4", "P5", "Q1", "Q2", "Q4", "Q5",
];
const ALPHABET: &str = "0123456789ABCDEFGH IJLNOPQRSTUY-_";

fn glyph(character: char) -> &'static str {
    match character.to_ascii_uppercase() {
        '0' | 'O' => "ABCDEF",
        '1' | 'I' => "BC",
        '2' => "ABDEG",
        '3' => "ABCDG",
        '4' => "BCFG",
        '5' | 'S' => "ACDFG",
        '6' => "ACDEFG",
        'G' => "ACDEF",
        '7' => "ABC",
        '8' => "ABCDEFG",
        'B' => "CDEFG",
        '9' => "ABCDFG",
        'Q' => "ABCFG",
        'A' => "ABCEFG",
        'C' => "ADEF",
        'D' => "BCDEG",
        'E' => "ADEFG",
        'F' => "AEFG",
        'H' => "BCEFG",
        'J' => "BCDE",
        'L' => "DEF",
        'N' => "CEG",
        'P' => "ABEFG",
        'R' => "EG",
        'T' => "DEFG",
        'U' => "BCDEF",
        'Y' => "BCDFG",
        '-' => "G",
        '_' => "D",
        _ => "",
    }
}

fn text_frame(text: &str, tick: usize) -> BTreeSet<String> {
    let text = text.to_ascii_uppercase();
    let letters: Vec<char> = if text.len() > 4 {
        format!("    {text}    ").chars().collect()
    } else {
        format!("{}{text}", " ".repeat((4 - text.len()) / 2))
            .chars()
            .collect()
    };
    let offset = if letters.len() > 4 {
        tick % (letters.len() - 3)
    } else {
        0
    };
    letters
        .iter()
        .skip(offset)
        .take(4)
        .enumerate()
        .flat_map(|(index, letter)| {
            glyph(*letter)
                .chars()
                .map(move |segment| format!("{segment}{}", index + 1))
        })
        .collect()
}

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
    bridge: Signal<Option<document::Eval>>,
    ready: Signal<bool>,
    mut status: Signal<Option<Value>>,
    mut busy: Signal<bool>,
) -> Element {
    let mut mode = use_signal(|| "text".to_owned());
    let mut text = use_signal(|| "HELLO".to_owned());
    let mut segments = use_signal(BTreeSet::<String>::new);
    let mut animation = use_signal(|| "wave".to_owned());
    let mut brightness = use_signal(|| "60".to_owned());
    let mut duration = use_signal(|| "5".to_owned());
    let mut tick = use_signal(|| 0_usize);
    let mut showing = use_signal(|| false);
    let mut feedback = use_signal(|| None::<(bool, String)>);
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
    let preview = match mode().as_str() {
        "text" => text_frame(&text(), tick()),
        "frame" => segments(),
        _ => {
            let step = tick() % 6;
            if animation() == "pulse" {
                text_frame("8888", 0)
            } else if animation() == "orbit" {
                (1..=4)
                    .map(|digit| format!("{}{}", ["A", "B", "C", "D", "E", "F"][step], digit))
                    .collect()
            } else {
                ('A'..='G')
                    .map(|segment| format!("{segment}{}", tick() % 4 + 1))
                    .collect()
            }
        }
    };
    let editable = mode() == "frame" && !busy();
    let intensity = if mode() == "animation" && animation() == "pulse" {
        [0.0, 0.25, 0.5, 0.75, 1.0, 0.75, 0.5, 0.25, 0.0][tick() % 9]
    } else {
        1.0
    } * f64::from(brightness_value.unwrap_or(0))
        / 100.0;
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
                    for (index, x) in [12, 76, 158, 222].into_iter().enumerate() {
                        g { transform: "translate({x} 8)",
                            for (letter, points) in [
                                ("A", "7,0 35,0 41,6 35,12 7,12 1,6"),
                                ("B", "36,8 42,14 42,31 36,37 30,31 30,14"),
                                ("C", "36,39 42,45 42,62 36,68 30,62 30,45"),
                                ("D", "7,64 35,64 41,70 35,76 7,76 1,70"),
                                ("E", "6,39 12,45 12,62 6,68 0,62 0,45"),
                                ("F", "6,8 12,14 12,31 6,37 0,31 0,14"),
                                ("G", "7,32 35,32 41,38 35,44 7,44 1,38")
                            ] {
                                Segment { id: format!("{letter}{}", index+1), points: points.to_owned(), selected: preview.contains(&format!("{letter}{}", index+1)), editable, segments }
                            }
                        }
                    }
                    Segment { id: "PLUS1".to_owned(), points: "140,26 148,26 148,34 140,34".to_owned(), selected: preview.contains("PLUS1"), editable, segments }
                    Segment { id: "PLUS2".to_owned(), points: "140,58 148,58 148,66 140,66".to_owned(), selected: preview.contains("PLUS2"), editable, segments }
                    Segment { id: "DEGREE".to_owned(), points: "280,8 286,8 289,11 289,17 286,20 280,20 277,17 277,11".to_owned(), selected: preview.contains("DEGREE"), editable, segments }
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
        }
    }
}
