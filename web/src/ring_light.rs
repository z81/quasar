use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::{device_access::send, icons::Icon, state::ControlState, station_preview::LightDraft};

fn valid_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

#[component]
pub fn RingLight(
    mut state: ControlState,
    duid: String,
    bridge: Signal<Option<document::Eval>>,
    ready: Signal<bool>,
    mut status: Signal<Option<Value>>,
    mut busy: Signal<bool>,
    #[props(default = false)] logo: bool,
) -> Element {
    let initial = use_hook(|| {
        state
            .station_previews
            .peek()
            .get(&duid)
            .map(|preview| {
                if logo {
                    preview.logo.clone()
                } else {
                    preview.ring.clone()
                }
            })
            .unwrap_or_default()
    });
    let mut animation = use_signal(|| initial.animation.clone());
    let mut color = use_signal(|| initial.color.clone());
    let mut brightness = use_signal(|| initial.brightness.clone());
    let mut duration = use_signal(|| initial.duration.clone());
    let mut showing = use_signal(|| false);
    let mut feedback = use_signal(|| None::<(bool, String)>);

    use_effect(move || {
        let draft = LightDraft {
            animation: animation(),
            color: color(),
            brightness: brightness(),
            duration: duration(),
        };
        let changed = state
            .station_previews
            .peek()
            .get(&duid)
            .map(|preview| {
                if logo {
                    preview.logo != draft
                } else {
                    preview.ring != draft
                }
            })
            .unwrap_or(true);
        if changed {
            state.station_previews.with_mut(|previews| {
                let preview = previews.entry(duid.clone()).or_default();
                if logo {
                    preview.logo = draft;
                } else {
                    preview.ring = draft;
                }
            });
        }
    });

    use_effect(move || {
        if !ready() {
            showing.set(false);
            feedback.set(None);
            return;
        }
        let Some(event) = status() else { return };
        if !*showing.peek() {
            return;
        }
        match event["type"].as_str().unwrap_or_default() {
            "ring_done" => {
                showing.set(false);
                busy.set(false);
                let result = match event["outcome"].as_str() {
                    Some("completed") => (
                        true,
                        "Анимация завершена. Предыдущее состояние подсветки восстановлено.",
                    ),
                    Some("interrupted") => (
                        true,
                        "Анимация завершена раньше: колонка включила штатную подсветку.",
                    ),
                    _ => (false, "Колонка не подтвердила результат показа."),
                };
                feedback.set(Some((result.0, result.1.into())));
            }
            "ring_error" | "error" => {
                showing.set(false);
                busy.set(false);
                feedback.set(Some((
                    false,
                    event["message"]
                        .as_str()
                        .unwrap_or("Не удалось запустить подсветку")
                        .to_owned(),
                )));
            }
            _ => {}
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
    let color_valid = valid_color(&color());
    let valid = brightness_value.is_some() && duration_value.is_some() && color_valid;
    let preview_color = if color_valid {
        color()
    } else {
        "#a855f7".to_owned()
    };
    let level = f64::from(brightness_value.unwrap_or(0)) / 100.0;
    let preview_class = format!(
        "ring-preview ring-preview-{}{}{}",
        animation(),
        if busy() && !showing() { " paused" } else { "" },
        if logo { " ring-preview-logo" } else { "" },
    );
    let target = if logo { "logo" } else { "ring" };
    let title = if logo {
        "Подсветка логотипа"
    } else {
        "Верхняя подсветка"
    };
    let pixels = if logo { 6 } else { 24 };

    rsx! {
        document::Stylesheet { href: asset!("/assets/ring-light.css") }
        section { class: "ring-editor", aria_label: title,
            h3 { "{title}" }
            p { class: "ring-help", if logo { "Отдельные анимации шести RGB-светодиодов логотипа Алисы." } else { "Короткие световые анимации на верхней панели колонки." } }
            div { class: "ring-layout",
                figure { class: preview_class, style: "--ring-color:{preview_color};--ring-level:{level}",
                    svg { view_box: "0 0 200 200", role: "img", "aria-label": "Предпросмотр: {title}", "focusable": "false",
                        rect { class: "ring-face", x: "12", y: "12", width: "176", height: "176", rx: "58" }
                        circle { class: "ring-center", cx: "100", cy: "100", r: "14" }
                        path { class: "ring-center-mark", d: "M94 103 L100 94 L106 103 Z" }
                        for index in 0..pixels {
                            {
                                let angle = (f64::from(index) * 360.0 / f64::from(pixels) - 90.0).to_radians();
                                let radius = if logo { 30.0 } else { 72.0 };
                                let x = 100.0 + radius * angle.cos();
                                let y = 100.0 + radius * angle.sin();
                                let tint = if animation() == "rainbow" {
                                    format!("hsl({} 85% 55%)", index * 360 / pixels)
                                } else {
                                    "var(--ring-color)".to_owned()
                                };
                                let delay = if animation() == "wave" { index * 100 } else { 0 };
                                rsx! {
                                    circle { class: "ring-led-base", cx: "{x}", cy: "{y}", r: "4" }
                                    circle { class: "ring-led", cx: "{x}", cy: "{y}", r: "4", style: "fill:{tint};animation-delay:-{delay}ms" }
                                }
                            }
                        }
                    }
                    figcaption { "Предпросмотр" }
                }
                div { class: "ring-controls",
                    div { class: "ring-effects", role: "group", aria_label: "Анимация подсветки",
                        for (value, label) in [("wave", "Волна"), ("pulse", "Пульс"), ("rainbow", "Радуга")] {
                            button { r#type: "button", class: if animation() == value { "ring-effect selected" } else { "ring-effect" }, aria_pressed: animation() == value, disabled: busy(),
                                onclick: move |_| { animation.set(value.into()); feedback.set(None); }, "{label}"
                            }
                        }
                    }
                    if animation() != "rainbow" {
                        label { class: "form-field ring-color", r#for: "{target}-color", span { "Цвет" }
                            span { class: "ring-color-value",
                                input { id: "{target}-color", r#type: "color", value: "{color}", disabled: busy(), oninput: move |event| { color.set(event.value()); feedback.set(None); } }
                                span { "{color}" }
                            }
                        }
                    }
                    div { class: "ring-options",
                        label { class: "form-field", r#for: "{target}-brightness", span { "Яркость, %" }
                            input { id: "{target}-brightness", r#type: "text", inputmode: "numeric", value: "{brightness}", disabled: busy(), aria_invalid: brightness_value.is_none(), oninput: move |event| { brightness.set(event.value()); feedback.set(None); } }
                        }
                        label { class: "form-field", r#for: "{target}-duration", span { "Длительность, с" }
                            input { id: "{target}-duration", r#type: "text", inputmode: "numeric", value: "{duration}", disabled: busy(), aria_invalid: duration_value.is_none(), oninput: move |event| { duration.set(event.value()); feedback.set(None); } }
                        }
                    }
                    button { class: "button primary ring-show", disabled: !ready() || busy() || !valid, onclick: move |_| {
                        if !ready() || busy() || !valid { return; }
                        let command = json!({
                            "type":"ring_animation", "target":target, "animation":animation(), "color":color(),
                            "brightness":brightness_value.unwrap(), "duration_ms":duration_value.unwrap() * 1_000,
                        });
                        status.set(None); feedback.set(None); showing.set(true); busy.set(true); send(bridge, command);
                    },
                        if showing() { span { class: "spinner" } "Показываем…" }
                        else { Icon { name: "play", size: 15 } "Показать на колонке" }
                    }
                }
            }
            if brightness_value.is_none() { p { class: "form-feedback failure", "Яркость — целое число от 0 до 100." } }
            if duration_value.is_none() { p { class: "form-feedback failure", "Длительность — от 1 до 15 целых секунд." } }
            if !color_valid { p { class: "form-feedback failure", "Выберите цвет подсветки." } }
            p { class: "ring-help", "После показа подсветка вернётся в прежнее состояние. Если колонке понадобится штатная индикация, анимация завершится раньше." }
            if let Some((success, message)) = feedback() {
                p { class: if success { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" }
            }
        }
    }
}
