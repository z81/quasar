use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::{json, Value};

use crate::{api, icons::Icon, state::ControlState, station_preview::*};

#[component]
pub(crate) fn StationModel(
    state: ControlState,
    duid: String,
    #[props(default = true)] show_details: bool,
) -> Element {
    rsx! {
        for duid in [duid] {
            StationModelView { key: "{duid}", state, duid, show_details }
        }
    }
}

#[component]
fn StationModelView(state: ControlState, duid: String, show_details: bool) -> Element {
    let mut live_frame = use_signal(|| None::<Value>);
    let mut live_error = use_signal(|| None::<String>);
    let mut received_at = use_signal(|| 0.0_f64);
    let mut now = use_signal(js_sys::Date::now);
    let live_duid = duid.clone();
    // Poll automatically while mounted; keyed removal cancels the task when
    // switching stations or leaving the dashboard.
    use_future(move || {
        let duid = live_duid.clone();
        async move {
            if duid.is_empty() {
                return;
            }
            loop {
                let hidden = js_sys::eval("document.hidden")
                    .ok()
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                let mut delay = 2_000;
                if !hidden && state.allowed() {
                    let result = api::get::<Value>(
                        state,
                        &format!("/devices/{}/tools/live", api::encoded(&duid)),
                    )
                    .await;
                    match result {
                        Ok(value) if value["available"] == true => {
                            live_frame.set(Some(value));
                            received_at.set(js_sys::Date::now());
                            live_error.set(None);
                        }
                        Ok(value) => {
                            live_error.set(Some(
                                value["reason"]
                                    .as_str()
                                    .unwrap_or("Чтение подсветки этой модели недоступно")
                                    .to_owned(),
                            ));
                            live_frame.set(None);
                            delay = 5_000;
                        }
                        Err(error) => {
                            live_error.set(Some(error));
                            live_frame.set(None);
                            delay = 5_000;
                        }
                    }
                }
                TimeoutFuture::new(delay).await;
            }
        }
    });
    let mut tick = use_signal(|| 0_usize);
    let clock_duid = duid.clone();
    use_future(move || {
        let duid = clock_duid.clone();
        async move {
            loop {
                TimeoutFuture::new(350).await;
                now.set(js_sys::Date::now());
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
        for (id, x, y, points) in DISPLAY_AUXILIARY_POLYGONS {
            segments.push(json!({"id":id,"x":x,"y":y,"points":points}));
        }
        serde_json::to_string(&segments).unwrap_or_default()
    });
    let live = live_frame();
    let fresh = live.is_some() && now() - received_at() < 8_000.0;
    let mut missing = Vec::new();
    if fresh {
        let value = live.as_ref().unwrap();
        if value["display_available"] != true {
            missing.push("дисплей");
        }
        if value["ring"].is_null() {
            missing.push("кольцо");
        }
        if value["logo"].is_null() {
            missing.push("логотип");
        }
    }
    let label = if fresh && missing.is_empty() {
        "3D · С колонки"
    } else if fresh {
        "3D · С колонки + предпросмотр"
    } else {
        "3D · Предпросмотр"
    };
    let live_json = if fresh {
        serde_json::to_string(live.as_ref().unwrap()).unwrap_or_default()
    } else {
        "null".to_owned()
    };
    let hint = if let Some(error) = live_error() {
        format!("Предпросмотр · {error}")
    } else if fresh && !missing.is_empty() {
        format!(
            "Предпросмотр: {}. Остальное читается с колонки каждые 2 секунды.",
            missing.join(", ")
        )
    } else if fresh {
        "Состояние колонки обновляется каждые 2 секунды".to_owned()
    } else if live.is_some() {
        "Данные устарели. Показан предпросмотр; ждём обновления.".to_owned()
    } else {
        "Показан предпросмотр. Читаем состояние колонки…".to_owned()
    };
    let preview_json = serde_json::to_string(&preview).unwrap_or_default();
    let frame_json = serde_json::to_string(&frame).unwrap_or_default();
    rsx! {
        document::Stylesheet { href: asset!("/assets/station-model.css") }
        document::Script { r#type: "module", src: asset!("/assets/station-model.js") }
        div { class: "station-model", "data-engine": asset!("/assets/vendor/three-quasar-0.187.0.min.js").to_string(),
            "data-preview": preview_json, "data-display": frame_json, "data-layout": layout, "data-live": live_json,
            div { class: "station-model-stage",
                canvas { class: "station-model-canvas", tabindex: "0", role: "img", aria_label: "3D-модель Миди. Вращение перетаскиванием или клавишами со стрелками." }
                div { class: "station-model-fallback", super::illustration::Midi {} }
            }
            if show_details {
            div { class: "station-model-toolbar",
                span { class: "station-model-label", "{label}" }
                div { class: "station-model-controls", role: "group", aria_label: "Положение 3D-колонки",
                    button { class: "icon-button", "data-turn": "-1", aria_label: "Повернуть колонку влево", Icon { name: "previous", size: 14 } }
                    button { class: "icon-button", "data-reset-model": "", aria_label: "Вернуть исходный ракурс", Icon { name: "refresh", size: 14 } }
                    button { class: "icon-button", "data-turn": "1", aria_label: "Повернуть колонку вправо", Icon { name: "next", size: 14 } }
                }
            }
            p { class: "station-model-hint", role: "status", "{hint}" }
            p { class: "station-model-unavailable", role: "status", "3D недоступно в этом браузере. Показана иллюстрация." }
            }
        }
    }
}
