use std::collections::BTreeMap;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    api,
    device_access::send,
    state::ControlState,
    station_preview::{
        DISPLAY_AUXILIARY_POLYGONS, DISPLAY_DIGIT_X, DISPLAY_EXTRA_POLYGONS,
        DISPLAY_SEGMENT_POLYGONS,
    },
};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Frame {
    segments: BTreeMap<String, u8>,
    duration_ms: u64,
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            segments: BTreeMap::new(),
            duration_ms: 500,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Preset {
    name: String,
    frames: Vec<Frame>,
    repeat: u8,
}

#[component]
pub fn AnimationTimeline(
    state: ControlState,
    duid: String,
    bridge: Signal<Option<document::Eval>>,
    ready: Signal<bool>,
    mut busy: Signal<bool>,
    mut status: Signal<Option<Value>>,
) -> Element {
    let mut frames = use_signal(|| vec![Frame::default()]);
    let mut selected = use_signal(|| 0usize);
    let mut duration = use_signal(|| "500".to_owned());
    let mut paint = use_signal(|| "255".to_owned());
    let mut repeat = use_signal(|| "1".to_owned());
    let mut name = use_signal(String::new);
    let mut presets = use_signal(Vec::<Preset>::new);
    let mut feedback = use_signal(|| None::<String>);
    let mut previewing = use_signal(|| false);
    let mut preview_frame = use_signal(|| 0usize);
    let mut sending = use_signal(|| false);
    let mut working = use_signal(|| false);
    let endpoint = format!("/devices/{}/animations", api::encoded(&duid));
    let load_endpoint = endpoint.clone();
    use_future(move || {
        let endpoint = load_endpoint.clone();
        async move {
            match api::get::<Vec<Preset>>(state, &endpoint).await {
                Ok(value) => presets.set(value),
                Err(error) => feedback.set(Some(error)),
            }
        }
    });
    let mut preview_task = use_future(move || {
        let enabled = *previewing.peek();
        async move {
            if !enabled {
                return;
            }
            let animation = frames.peek().clone();
            let repetitions = repeat.peek().parse::<u8>().unwrap_or(1).clamp(1, 10);
            for _ in 0..repetitions {
                for (index, frame) in animation.iter().enumerate() {
                    preview_frame.set(index);
                    TimeoutFuture::new(frame.duration_ms.clamp(50, 5000) as u32).await;
                }
            }
            previewing.set(false);
        }
    });
    use_effect(move || {
        if previewing() {
            preview_task.restart();
        } else {
            preview_task.cancel();
        }
    });
    use_effect(move || {
        if !sending() {
            return;
        }
        if !ready() {
            sending.set(false);
            busy.set(false);
            feedback.set(Some(
                "Соединение прервано; проверьте дисплей перед повтором".into(),
            ));
            return;
        }
        if let Some(event) = status() {
            match event["type"].as_str().unwrap_or_default() {
                "display_done" => {
                    sending.set(false);
                    busy.set(false);
                    feedback.set(Some(
                        if event["outcome"] == "interrupted" {
                            "Показ уступил место штатной индикации."
                        } else {
                            "Показ завершён, состояние дисплея восстановлено."
                        }
                        .into(),
                    ));
                }
                "display_error" | "error" => {
                    sending.set(false);
                    busy.set(false);
                    feedback.set(Some(
                        event["message"].as_str().unwrap_or("Ошибка дисплея").into(),
                    ));
                }
                _ => {}
            }
        }
    });
    let paint_value = paint().parse::<u8>().ok();
    let duration_value = duration()
        .parse::<u64>()
        .ok()
        .filter(|value| (50..=5000).contains(value));
    let repeat_value = repeat()
        .parse::<u8>()
        .ok()
        .filter(|value| (1..=10).contains(value));
    let mut current_frames = frames();
    if let (Some(frame), Some(value)) = (current_frames.get_mut(selected()), duration_value) {
        frame.duration_ms = value;
    }
    let total = current_frames
        .iter()
        .map(|frame| frame.duration_ms)
        .sum::<u64>()
        * u64::from(repeat_value.unwrap_or(0));
    let valid =
        duration_value.is_some() && repeat_value.is_some() && (500..=15000).contains(&total);
    let current = current_frames
        .get(if previewing() {
            preview_frame()
        } else {
            selected()
        })
        .cloned()
        .unwrap_or_default();
    let save_frames = current_frames.clone();
    let play_frames = current_frames.clone();
    let save_endpoint = endpoint.clone();
    let mut edit_segment = move |id: String| {
        if busy() || previewing() {
            return;
        }
        if let Some(value) = paint_value {
            frames.with_mut(|frames| {
                if let Some(frame) = frames.get_mut(selected()) {
                    if frame.segments.get(&id).copied().unwrap_or(0) == value {
                        frame.segments.remove(&id);
                    } else {
                        frame.segments.insert(id, value);
                    }
                }
            });
        }
    };
    rsx! {
        details { class: "animation-timeline",
            summary { "Своя анимация по кадрам" }
            p { class: "midi-editor-help", "Нарисуйте кадры, задайте яркость сегментов и время каждого кадра. Пресеты сохраняются на сервере для этой колонки." }
            div { class: "mode-tabs", role: "group", aria_label: "Кадры анимации",
                for (index, frame) in current_frames.iter().enumerate() {
                    button { class: if selected() == index { "mode-tab selected" } else { "mode-tab" }, disabled: busy() || previewing() || duration_value.is_none(), onclick: move |_| {
                        if let Some(value) = duration_value { frames.with_mut(|frames| frames[selected()].duration_ms = value); }
                        selected.set(index); duration.set(frames.peek()[index].duration_ms.to_string());
                    }, "{index + 1} · {frame.duration_ms} мс" }
                }
            }
            div { class: "midi-preview",
                svg { view_box: "0 0 300 92", role: "group", "aria-label": "Сегменты кадра",
                    for (index, x) in DISPLAY_DIGIT_X.into_iter().enumerate() {
                        g { transform: "translate({x} 8)",
                            for (letter, points) in DISPLAY_SEGMENT_POLYGONS {
                                {let id = format!("{letter}{}",index+1); let key_id=id.clone(); let level=current.segments.get(&id).copied().unwrap_or(0); rsx! {
                                    polygon { points, class: if level>0 { "midi-segment lit" } else { "midi-segment" }, style: "--midi-preview-level:{f64::from(level)/255.0}", role: "button", tabindex: "0", "aria-label": "Сегмент {id}, яркость {level}",
                                        onclick: move |_| edit_segment(id.clone()), onkeydown: move |event| { if event.key()==Key::Enter || event.key()==Key::Character(" ".into()) { event.prevent_default(); edit_segment(key_id.clone()); } }
                                    }
                                }}
                            }
                        }
                    }
                    for (id, x, y, points) in DISPLAY_AUXILIARY_POLYGONS {
                        {let level=current.segments.get(id).copied().unwrap_or(0); rsx! {
                            polygon { transform: "translate({x} {y})", points,
                                class: if level>0 { "midi-segment lit" } else { "midi-segment" },
                                style: "--midi-preview-level:{f64::from(level)/255.0}", role: "button", tabindex: "0",
                                "aria-label": "Сегмент {id}, яркость {level}",
                                onclick: move |_| edit_segment(id.into()),
                                onkeydown: move |event| { if event.key()==Key::Enter || event.key()==Key::Character(" ".into()) { event.prevent_default(); edit_segment(id.into()); } }
                            }
                        }}
                    }
                    for (id, points) in DISPLAY_EXTRA_POLYGONS {
                        {let level=current.segments.get(id).copied().unwrap_or(0); rsx! {
                            polygon { points, class: if level>0 { "midi-segment lit" } else { "midi-segment" }, style: "--midi-preview-level:{f64::from(level)/255.0}", role: "button", tabindex: "0", "aria-label": "Сегмент {id}", onclick: move |_| edit_segment(id.into()), onkeydown: move |event| { if event.key()==Key::Enter { edit_segment(id.into()); } } }
                        }}
                    }
                }
            }
            details { class: "midi-extra", summary { "Дополнительные сегменты" }
                div { for id in ["H1","H2","L1","L2","M1","M2","P1","P2","P4","P5","Q1","Q2","Q4","Q5"] {
                    button { class: if current.segments.get(id).copied().unwrap_or(0)>0 { "selected" } else { "" }, disabled: busy() || previewing(), onclick: move |_| edit_segment(id.into()), "{id}" }
                } }
            }
            div { class: "midi-editor-options",
                label { class: "form-field", span { "Яркость кисти, 0–255" } input { inputmode: "numeric", value: "{paint}", disabled: busy() || previewing(), aria_invalid: paint_value.is_none(), oninput: move |e| paint.set(e.value()) } }
                label { class: "form-field", span { "Длительность кадра, мс" } input { inputmode: "numeric", value: "{duration}", disabled: busy() || previewing(), aria_invalid: duration_value.is_none(), oninput: move |e| duration.set(e.value()), onblur: move |_| { if let Some(value)=duration().parse::<u64>().ok().filter(|v|(50..=5000).contains(v)) { frames.with_mut(|v|v[selected()].duration_ms=value); } } } }
                label { class: "form-field", span { "Повторы" } input { inputmode: "numeric", value: "{repeat}", disabled: busy() || previewing(), aria_invalid: repeat_value.is_none(), oninput: move |e| repeat.set(e.value()) } }
            }
            div { class: "midi-frame-actions",
                button { class: "button secondary", disabled: busy() || previewing() || frames().len()>=60 || duration_value.is_none(), onclick: move |_| { frames.with_mut(|v|v[selected()].duration_ms=duration_value.unwrap_or(500)); let mut next=frames(); next.push(Frame::default()); let i=next.len()-1; frames.set(next); selected.set(i); duration.set("500".into()); }, "Добавить кадр" }
                button { class: "button secondary", disabled: busy() || previewing() || frames().len()>=60 || duration_value.is_none(), onclick: move |_| { let mut next=frames(); let mut frame=next[selected()].clone(); frame.duration_ms=duration_value.unwrap_or(500); next.insert(selected()+1,frame); frames.set(next); selected.set(selected()+1); }, "Дублировать" }
                button { class: "button secondary", disabled: busy() || previewing() || selected()==0, onclick: move |_| { let i=selected(); frames.with_mut(|v|v.swap(i,i-1)); selected.set(i-1); }, "← Кадр" }
                button { class: "button secondary", disabled: busy() || previewing() || selected()+1>=frames().len(), onclick: move |_| { let i=selected(); frames.with_mut(|v|v.swap(i,i+1)); selected.set(i+1); }, "Кадр →" }
                button { class: "button secondary", disabled: busy() || previewing(), onclick: move |_| frames.with_mut(|v|v[selected()].segments.clear()), "Очистить" }
                button { class: "button secondary", disabled: busy() || previewing() || frames().len()==1, onclick: move |_| { let mut next=frames(); next.remove(selected()); let i=selected().min(next.len()-1); duration.set(next[i].duration_ms.to_string()); frames.set(next); selected.set(i); }, "Удалить кадр" }
            }
            p { class: "midi-editor-help", "Всего {total} мс. Допустимо 500–15000 мс; кадр 50–5000 мс." }
            div { class: "midi-frame-actions",
                button { class: "button secondary", disabled: (!valid && !previewing()) || busy(), onclick: move |_| { if let Some(value)=duration_value { frames.with_mut(|v|v[selected()].duration_ms=value); } preview_frame.set(0); previewing.set(!previewing()); }, if previewing() { "Остановить предпросмотр" } else { "Предпросмотр" } }
                button { class: "button primary", disabled: !valid || !ready() || busy(), onclick: move |_| { previewing.set(false); status.set(None); sending.set(true); busy.set(true); feedback.set(None); send(bridge,json!({"type":"display_sequence","frames":play_frames,"repeat":repeat_value.unwrap_or(1)})); }, if sending() { "Показываем…" } else { "Показать на Миди" } }
            }
            label { class: "form-field", span { "Название пресета" } input { value: "{name}", maxlength: "80", oninput: move |e|name.set(e.value()) } }
            button { class: "button secondary", disabled: !valid || name().trim().is_empty() || working(), onclick: move |_| {
                let endpoint=save_endpoint.clone(); let body=json!({"name":name().trim(),"frames":save_frames,"repeat":repeat_value.unwrap_or(1)}); working.set(true);
                spawn(async move { match api::mutate(state,"POST",&endpoint,Some(&body)).await { Ok(response)=>match response.json::<Vec<Preset>>().await { Ok(value)=>{presets.set(value);feedback.set(Some("Пресет сохранён".into()));},Err(_)=>feedback.set(Some("Не удалось прочитать сохранённые пресеты".into())) },Err(error)=>feedback.set(Some(error)) };working.set(false); });
            }, "Сохранить пресет" }
            for preset in presets() {
                { let label=preset.name.clone();let remove_name=label.clone();let remove_endpoint=endpoint.clone();rsx! {
                    div { class: "midi-frame-actions",
                        button { class:"button secondary",disabled:busy()||previewing(),onclick:move |_| { frames.set(preset.frames.clone());selected.set(0);duration.set(preset.frames.first().map(|v|v.duration_ms).unwrap_or(500).to_string());repeat.set(preset.repeat.to_string());name.set(preset.name.clone()); }, "{label}" }
                        button { class:"text-button",disabled:working(),onclick:move |_| { let url=remove_endpoint.clone();let body=json!({"name":remove_name});working.set(true);spawn(async move {match api::mutate(state,"DELETE",&url,Some(&body)).await {Ok(r)=>if let Ok(v)=r.json::<Vec<Preset>>().await {presets.set(v)},Err(e)=>feedback.set(Some(e))};working.set(false);});},"Удалить пресет" }
                    }
                }}
            }
            if let Some(message)=feedback() { p { class:"form-feedback",role:"status","{message}" } }
        }
    }
}
