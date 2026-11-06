use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::json;

use crate::{icons::Icon, state::ControlState, station_preview::*};

#[component]
pub(crate) fn StationModel(state: ControlState, duid: String) -> Element {
    let mut tick = use_signal(|| 0_usize);
    let clock_duid = duid.clone();
    use_future(move || {
        let duid = clock_duid.clone();
        async move {
            loop {
                TimeoutFuture::new(350).await;
                let reduced = web_sys::window()
                    .and_then(|w| {
                        w.match_media("(prefers-reduced-motion: reduce)")
                            .ok()
                            .flatten()
                    })
                    .is_some_and(|query| query.matches());
                let animated = state
                    .station_previews
                    .peek()
                    .get(&duid)
                    .is_none_or(|preview| {
                        preview.display.mode == "animation"
                            || (preview.display.mode == "text" && preview.display.text.len() > 4)
                    });
                if reduced {
                    if *tick.peek() != 4 {
                        tick.set(4);
                    }
                } else if animated {
                    let next = tick.peek().wrapping_add(1);
                    tick.set(next);
                }
            }
        }
    });
    let preview = state
        .station_previews
        .read()
        .get(&duid)
        .cloned()
        .unwrap_or_default();
    let frame = display_frame(&preview.display, tick());
    let layout = use_hook(|| {
        let mut segments = Vec::new();
        for (index, x) in DISPLAY_DIGIT_X.into_iter().enumerate() {
            for (letter, points) in DISPLAY_SEGMENT_POLYGONS {
                segments
                    .push(json!({"id":format!("{letter}{}", index+1),"x":x,"y":8,"points":points}));
            }
        }
        for (id, points) in DISPLAY_EXTRA_POLYGONS {
            segments.push(json!({"id":id,"x":0,"y":0,"points":points}));
        }
        serde_json::to_string(&segments).unwrap_or_default()
    });
    let preview_json = serde_json::to_string(&preview).unwrap_or_default();
    let frame_json = serde_json::to_string(&frame).unwrap_or_default();
    rsx! {
        document::Stylesheet { href: asset!("/assets/station-model.css") }
        document::Script { r#type: "module", src: asset!("/assets/station-model.js") }
        div { class: "station-model", "data-engine": asset!("/assets/vendor/three-quasar-0.187.0.min.js").to_string(),
            "data-preview": preview_json, "data-display": frame_json, "data-layout": layout,
            div { class: "station-model-stage",
                canvas { class: "station-model-canvas", tabindex: "0", role: "img", aria_label: "3D-модель Миди. Вращение перетаскиванием или клавишами со стрелками." }
                div { class: "station-model-fallback", super::illustration::Midi {} }
            }
            div { class: "station-model-toolbar",
                span { class: "station-model-label", "3D · Предпросмотр" }
                div { class: "station-model-controls", role: "group", aria_label: "Положение 3D-колонки",
                    button { class: "icon-button", "data-turn": "-1", aria_label: "Повернуть колонку влево", Icon { name: "previous", size: 14 } }
                    button { class: "icon-button", "data-reset-model": "", aria_label: "Вернуть исходный ракурс", Icon { name: "refresh", size: 14 } }
                    button { class: "icon-button", "data-turn": "1", aria_label: "Повернуть колонку вправо", Icon { name: "next", size: 14 } }
                }
            }
            p { class: "station-model-hint", "Потяните, чтобы повернуть" }
            p { class: "station-model-unavailable", role: "status", "3D недоступно в этом браузере. Показана иллюстрация." }
        }
    }
}
