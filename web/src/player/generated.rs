use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::{api, icons::Icon, state::ControlState};

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Status {
    configured: bool,
    ready: bool,
    state: String,
    mode: String,
    prompt: String,
    generating: bool,
    generated: usize,
    queued: usize,
    repeating: bool,
    error: Option<String>,
}

#[derive(Clone, Copy)]
struct Editor {
    status: Signal<Option<Status>>,
    busy: Signal<bool>,
    error: Signal<Option<String>>,
    load_error: Signal<Option<String>>,
    revision: Signal<u64>,
}

fn endpoint(duid: &str) -> String {
    format!("/devices/{}/generated-music", api::encoded(duid))
}

fn perform(state: ControlState, mut editor: Editor, body: serde_json::Value) {
    if *editor.busy.peek() {
        return;
    }
    editor.busy.set(true);
    *editor.revision.write() += 1;
    editor.error.set(None);
    let duid = state.selected.peek().clone();
    spawn(async move {
        let result = match api::mutate(state, "POST", &endpoint(&duid), Some(&body)).await {
            Ok(response) => response
                .json::<Status>()
                .await
                .map_err(|_| "Непонятный ответ генератора музыки".to_owned()),
            Err(error) => Err(error),
        };
        match result {
            Ok(status) => editor.status.set(Some(status)),
            Err(error) => editor.error.set(Some(error)),
        }
        editor.busy.set(false);
    });
}

#[component]
pub(super) fn GeneratedMusic(state: ControlState) -> Element {
    let mut prompt = use_signal(String::new);
    let mut duration = use_signal(|| "60".to_owned());
    let mut editor = Editor {
        status: use_signal(|| None::<Status>),
        busy: use_signal(|| false),
        error: use_signal(|| None::<String>),
        load_error: use_signal(|| None::<String>),
        revision: use_signal(|| 0),
    };
    use_future(move || async move {
        let duid = state.selected.peek().clone();
        if duid.is_empty() {
            return;
        }
        loop {
            // A poll started before a button press cannot overwrite its receipt.
            if !*editor.busy.peek() {
                let revision = *editor.revision.peek();
                let result = api::get::<Status>(state, &endpoint(&duid)).await;
                if !*editor.busy.peek() && revision == *editor.revision.peek() {
                    match result {
                        Ok(status) => {
                            editor.status.set(Some(status));
                            editor.load_error.set(None);
                        }
                        Err(error) => editor.load_error.set(Some(error)),
                    }
                }
            }
            gloo_timers::future::TimeoutFuture::new(3000).await;
        }
    });
    let status = (editor.status)();
    let seconds = duration()
        .parse::<u32>()
        .ok()
        .filter(|n| (10..=120).contains(n));
    let enabled = state.can_send() && status.as_ref().is_some_and(|s| s.ready) && !(editor.busy)();
    let valid = !prompt().trim().is_empty() && seconds.is_some();
    let active = status.as_ref().is_some_and(|s| {
        matches!(
            s.state.as_str(),
            "preparing" | "buffering" | "playing" | "paused" | "error"
        )
    });
    let error = (editor.error)().or_else(|| (editor.load_error)()).or_else(|| status.as_ref().and_then(|s| s.error.clone()));
    rsx! {
        document::Stylesheet { href: asset!("/assets/generated-music.css") }
        section { class: "generated-music", aria_label: "Генерация музыки",
            div { class: "generated-music-heading",
                span { class: "generated-music-mark", Icon { name: "spark", size: 22 } }
                div { h2 { "Своя музыка" } p { "Трек по описанию или бесконечное AI-радио" } }
            }
            label { r#for: "generated-music-prompt", "Какую музыку создать?" }
            textarea { id: "generated-music-prompt", rows: "2", maxlength: "1000", value: "{prompt}",
                placeholder: "Lo-fi hip-hop, warm piano, soft drums, slow tempo, no vocals",
                oninput: move |event| prompt.set(event.value()),
            }
            div { class: "generated-music-options",
                label { r#for: "generated-music-duration", "Длина трека",
                    span { input { id: "generated-music-duration", r#type: "text", inputmode: "numeric",
                        value: "{duration}", aria_invalid: seconds.is_none(), oninput: move |event| duration.set(event.value()),
                    } "сек" }
                }
                p { "10–120 секунд. Для радио следующие треки создаются заранее. Описание лучше писать по-английски." }
            }
            div { class: "generated-music-actions",
                button { class: "primary", disabled: !enabled || !valid,
                    onclick: move |_| perform(state, editor, json!({"action":"generate", "prompt":prompt(), "duration":seconds})),
                    Icon { name: "spark", size: 17 } "Создать трек"
                }
                button { class: "secondary", disabled: !enabled || !valid,
                    onclick: move |_| perform(state, editor, json!({"action":"radio", "prompt":prompt(), "duration":seconds})),
                    Icon { name: "music", size: 17 } "Запустить радио"
                }
                if active {
                    button { class: "secondary", disabled: (editor.busy)(),
                        onclick: move |_| perform(state, editor, json!({"action":"stop"})),
                        Icon { name: "stop", size: 17 } "Остановить"
                    }
                }
            }
            div { class: "generated-music-status", role: "status", aria_live: "polite",
                if let Some(status) = status {
                    if !status.configured { p { "Подключите Stable Audio в настройках сервера." } }
                    else if !status.ready && error.is_none() { p { "Stable Audio загружается…" } }
                    else if !status.prompt.is_empty() {
                        p { strong {
                            match status.state.as_str() {
                                "preparing" => "Готовим первый трек…",
                                "buffering" => "Подключаем колонку…",
                                "playing" if status.mode == "radio" => "AI-радио",
                                "playing" => "Созданный трек",
                                "paused" => "На паузе",
                                "finished" => "Трек завершён",
                                "stopped" => "Остановлено",
                                "error" => "Не удалось запустить музыку",
                                _ => "Готово",
                            }
                        } " · {status.prompt}" }
                        if active && status.mode == "radio" {
                            p { "Создано: {status.generated} · В очереди: {status.queued}"
                                if status.generating { " · Готовим следующий трек" }
                            }
                            if status.repeating { p { "Повторяем текущий трек, пока готовится следующий." } }
                        }
                    }
                } else if error.is_none() { p { "Проверяем генератор музыки…" } }
            }
            if let Some(error) = error { p { class: "form-feedback failure", role: "alert", "{error}" } }
        }
    }
}
