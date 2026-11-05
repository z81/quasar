use dioxus::prelude::*;
use serde_json::{json, Value};

use crate::{
    api,
    icons::Icon,
    state::{ControlState, Device},
};

#[derive(Clone, PartialEq)]
struct DisplayForm {
    automatic: bool,
    brightness: String,
    clock: String,
    auto_style: bool,
    style: String,
}

impl DisplayForm {
    fn from_config(config: &Value) -> Self {
        let led = &config["deviceConfig"]["led"];
        let level = led["brightness"]["value"]
            .as_f64()
            .filter(|value| value.is_finite())
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);
        Self {
            automatic: led["brightness"]["auto"].as_bool().unwrap_or(true),
            brightness: ((level * 10_000.0).round() / 100.0).to_string(),
            clock: led["timeVisualization"]["format"]
                .as_str()
                .unwrap_or("24h")
                .to_owned(),
            auto_style: led["musicEqualizerVisualization"]["auto"]
                .as_bool()
                .unwrap_or(false),
            style: led["musicEqualizerVisualization"]["style"]
                .as_str()
                .unwrap_or("lava_beat")
                .to_owned(),
        }
    }

    fn apply(&self, original: &Self, config: &mut Value) {
        let led = &mut config["deviceConfig"]["led"];
        if self.automatic != original.automatic {
            led["brightness"]["auto"] = json!(self.automatic);
        }
        if self.brightness != original.brightness {
            if let Some(units) = percent_units(&self.brightness) {
                led["brightness"]["value"] = json!(f64::from(units) / 10_000.0);
            }
        }
        if self.clock != original.clock {
            led["timeVisualization"]["format"] = json!(self.clock);
        }
        if self.auto_style != original.auto_style {
            led["musicEqualizerVisualization"]["auto"] = json!(self.auto_style);
        }
        if self.style != original.style {
            led["musicEqualizerVisualization"]["style"] = json!(self.style);
        }
    }
}

// Work in hundredths of a percent so typed decimals round-trip without float noise.
fn percent_units(value: &str) -> Option<u16> {
    let value = value.trim().replace(',', ".");
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
        return None;
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((&value, ""));
    if fraction.len() > 2 || fraction.contains('.') || (whole.is_empty() && fraction.is_empty()) {
        return None;
    }
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u16>().ok()?
    };
    let fraction = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u16>().ok()? * 10,
        _ => fraction.parse::<u16>().ok()?,
    };
    whole
        .checked_mul(100)?
        .checked_add(fraction)
        .filter(|value| *value <= 10_000)
}

#[component]
pub fn DisplaySettings(mut state: ControlState, device: Device) -> Element {
    let mut original = use_signal(|| DisplayForm::from_config(&device.quasar_config));
    let mut draft = use_signal(|| DisplayForm::from_config(&device.quasar_config));
    let mut busy = use_signal(|| false);
    let mut feedback = use_signal(|| None::<(bool, String)>);
    use_effect(use_reactive((&device.quasar_config,), move |(config,)| {
        if *draft.peek() == *original.peek() && !*busy.peek() {
            let next = DisplayForm::from_config(&config);
            if next != *original.peek() {
                original.set(next.clone());
                draft.set(next);
            }
        }
    }));
    let form = draft();
    let changed = form != original();
    let level = percent_units(&form.brightness);
    let slider = f64::from(level.unwrap_or(0)) / 100.0;
    let duid = device.duid.clone();
    let cancel_duid = device.duid.clone();
    rsx! {
        document::Stylesheet { href: asset!("/assets/display-settings.css") }
        section { class: "card settings-card station-settings display-settings", aria_label: "Экран и подсветка",
            div { class: "card-heading", span { class: "card-kicker", Icon { name: "sun", size: 16 } "ЭКРАН И ПОДСВЕТКА" } }
            h2 { "Свет по вашему настроению" }
            p { class: "settings-intro", "Яркость дисплея, часы и подсветка во время музыки." }
            form { onsubmit: move |event| {
                event.prevent_default();
                if busy() || !state.allowed() || !changed || level.is_none() { return; }
                let Some(latest) = state.devices.peek().iter().find(|current| current.duid == duid).cloned() else { return; };
                let mut config = latest.quasar_config;
                draft.peek().apply(&original.peek(), &mut config);
                let url = format!("/devices/{}", api::encoded(&duid));
                let body = json!({"quasarConfig":config});
                busy.set(true); feedback.set(None);
                dioxus::core::spawn_forever(async move {
                    let result = match api::mutate(state, "PATCH", &url, Some(&body)).await {
                        Ok(response) => response.json::<Device>().await.map_err(|_| "Настройки приняты, но не удалось прочитать ответ. Обновите страницу.".to_owned()),
                        Err(error) => Err(error),
                    };
                    if draft.try_read().is_err() { return; }
                    match result {
                        Ok(saved) => {
                            let next = DisplayForm::from_config(&saved.quasar_config);
                            original.set(next.clone()); draft.set(next);
                            feedback.set(Some((true, if saved.connected == Some(true) { "Сохранено. Колонке отправлено обновление настроек." } else { "Сохранено. Настройки доступны колонке при следующем подключении." }.into())));
                            api::refresh(state).await;
                        }
                        Err(error) => feedback.set(Some((false, error))),
                    }
                    if let Ok(mut value) = busy.try_write() { *value = false; }
                });
            },
                label { class: "toggle-field display-auto", span { strong { "Автоматическая яркость" } small { "Подстраивать экран под освещение комнаты" } }
                    input { id: "display-auto-brightness", r#type: "checkbox", checked: form.automatic, disabled: busy(), onchange: move |event| { draft.write().automatic = event.checked(); feedback.set(None); } }
                }
                div { class: "display-brightness",
                    div { class: "display-brightness-heading", label { r#for: "display-brightness", "Яркость экрана" }
                        button { class: "text-button", r#type: "button", disabled: busy(), onclick: move |_| { let mut form = draft.write(); form.automatic = false; form.brightness = "0".into(); feedback.set(None); }, "Выключить экран" }
                    }
                    div { class: "display-brightness-controls",
                        input { r#type: "range", min: "0", max: "100", step: "0.01", value: slider, disabled: busy() || form.automatic, aria_label: "Яркость экрана", oninput: move |event| { draft.write().brightness = event.value(); feedback.set(None); } }
                        label { class: "form-field display-brightness-number", input { id: "display-brightness", r#type: "text", inputmode: "decimal", value: "{form.brightness}", maxlength: "6", disabled: busy() || form.automatic, aria_invalid: level.is_none(), oninput: move |event| { draft.write().brightness = event.value(); feedback.set(None); } } span { "%" } }
                    }
                    p { class: if level.is_none() { "form-help failure" } else { "form-help" },
                        if level.is_none() { "Введите число от 0 до 100, не больше двух знаков после запятой." }
                        else if form.automatic { "Ручная яркость сохранена и будет использоваться после выключения автоматики." }
                        else if level == Some(0) { "После сохранения экран погаснет. Для включения увеличьте яркость или включите автоматику." }
                        else { "0% выключает экран. Изменения применяются после сохранения." }
                    }
                }
                div { class: "form-columns display-options",
                    label { class: "form-field", span { "Формат часов" }
                        select { id: "display-clock-format", value: "{form.clock}", disabled: busy(), onchange: move |event| { draft.write().clock = event.value(); feedback.set(None); },
                            option { value: "24h", selected: form.clock == "24h", "24 часа" } option { value: "12h", selected: form.clock == "12h", "12 часов" }
                        }
                    }
                    label { class: "form-field", span { "Визуализация музыки" }
                        select { id: "display-music-style", value: "{form.style}", disabled: busy() || form.auto_style, onchange: move |event| { draft.write().style = event.value(); feedback.set(None); },
                            for (value, label) in [("lava_beat", "Лава"), ("blink", "Мерцание"), ("polar_shining", "Полярное сияние"), ("none", "Без подсветки")] {
                                option { value, selected: form.style == value, "{label}" }
                            }
                            if !["lava_beat", "blink", "polar_shining", "none"].contains(&form.style.as_str()) { option { value: "{form.style}", selected: true, "Текущий стиль: {form.style}" } }
                        }
                    }
                }
                label { class: "toggle-field", span { strong { "Автоматическая смена визуализаций" } small { "Колонка сама выбирает стиль во время музыки" } }
                    input { id: "display-auto-style", r#type: "checkbox", checked: form.auto_style, disabled: busy(), onchange: move |event| { draft.write().auto_style = event.checked(); feedback.set(None); } }
                }
                div { class: "settings-actions",
                    button { class: "button primary", r#type: "submit", disabled: busy() || !changed || level.is_none(), if busy() { span { class: "spinner" } "Сохраняем…" } else { Icon { name: "check", size: 16 } "Сохранить экран" } }
                    button { class: "text-button", r#type: "button", disabled: busy() || !changed, onclick: move |_| {
                        if let Some(latest) = state.devices.peek().iter().find(|device| device.duid == cancel_duid) {
                            let next = DisplayForm::from_config(&latest.quasar_config); original.set(next.clone()); draft.set(next);
                        }
                        feedback.set(None);
                    }, "Отменить изменения" }
                }
                if let Some((success, message)) = feedback() { p { class: if success { "form-feedback success display-feedback" } else { "form-feedback failure display-feedback" }, role: "status", "{message}" } }
            }
        }
    }
}
