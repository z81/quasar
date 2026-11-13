use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::api;
use crate::icons::Icon;
use crate::state::{ControlState, Panel};

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Location {
    pub id: Option<i64>,
    pub name: String,
    pub admin1: Option<String>,
    pub country: Option<String>,
}

impl Location {
    fn detail(&self) -> String {
        self.admin1
            .iter()
            .chain(self.country.iter())
            .filter(|part| !part.is_empty() && **part != self.name)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Current {
    pub temperature: Option<f64>,
    pub apparent_temperature: Option<f64>,
    pub precipitation: Option<f64>,
    pub wind_speed: Option<f64>,
    pub weather_code: Option<u16>,
    pub is_day: Option<bool>,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Daily {
    pub date: String,
    pub temperature_min: Option<f64>,
    pub temperature_max: Option<f64>,
    pub precipitation_probability: Option<f64>,
    pub weather_code: Option<u16>,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct WeatherView {
    pub location: Option<Location>,
    pub current: Option<Current>,
    pub daily: Vec<Daily>,
    pub fetched_at: Option<String>,
    pub stale: bool,
    pub error: Option<String>,
}

pub async fn refresh(mut state: ControlState) {
    if !state.allowed() {
        return;
    }
    let duid = (state.selected)();
    let request = *state.weather_request.peek() + 1;
    state.weather_request.set(request);
    if duid.is_empty() {
        state.weather.set(None);
        state.weather_error.set(None);
        state.weather_loading.set(false);
        return;
    }
    state.weather_loading.set(true);
    let result =
        api::get::<WeatherView>(state, &format!("/devices/{}/weather", api::encoded(&duid))).await;
    // A city save or a station switch can start a newer request before this one finishes.
    if *state.selected.peek() != duid || *state.weather_request.peek() != request {
        return;
    }
    match result {
        Ok(view) => {
            state.weather.set(Some(view));
            state.weather_error.set(None);
        }
        Err(error) => state.weather_error.set(Some(error)),
    }
    state.weather_loading.set(false);
}

fn temperature(value: Option<f64>) -> String {
    match value.filter(|value| value.is_finite()) {
        Some(value) => {
            let rounded = value.round() as i64;
            format!("{}{rounded}°", if rounded > 0 { "+" } else { "" })
        }
        None => "—".to_string(),
    }
}

fn measurement(value: Option<f64>, unit: &str) -> String {
    match value.filter(|value| value.is_finite()) {
        Some(value) => {
            let rounded = (value * 10.0).round() / 10.0;
            format!("{rounded} {unit}")
        }
        None => "—".to_string(),
    }
}

fn weather_icon(code: Option<u16>, is_day: Option<bool>) -> &'static str {
    match code {
        Some(0 | 1) if is_day == Some(false) => "moon",
        Some(0 | 1) => "sun",
        Some(2 | 3) => "cloud",
        Some(45 | 48) => "fog",
        Some(51..=67 | 80..=82) => "rain",
        Some(71..=77 | 85 | 86) => "snow",
        Some(95..=99) => "storm",
        _ => "cloud",
    }
}

fn day_label(value: &str) -> String {
    let date = js_sys::Date::new(&format!("{value}T12:00:00Z").into());
    if date.get_time().is_nan() {
        return value.to_string();
    }
    let day = ["Вс", "Пн", "Вт", "Ср", "Чт", "Пт", "Сб"][date.get_utc_day() as usize];
    format!(
        "{day}, {}.{:02}",
        date.get_utc_date(),
        date.get_utc_month() + 1
    )
}

fn updated_at(value: &str) -> String {
    let date = js_sys::Date::new(&value.into());
    if date.get_time().is_nan() {
        return "Время обновления неизвестно".to_string();
    }
    format!(
        "Обновлено {:02}.{:02} в {:02}:{:02}",
        date.get_date(),
        date.get_month() + 1,
        date.get_hours(),
        date.get_minutes()
    )
}

#[component]
pub fn WeatherCard(mut state: ControlState) -> Element {
    if state.selected.read().is_empty() {
        return rsx! {};
    }
    let weather = (state.weather)();
    let location = weather.as_ref().and_then(|view| view.location.as_ref());
    let current = weather.as_ref().and_then(|view| view.current.as_ref());
    let error =
        (state.weather_error)().or_else(|| weather.as_ref().and_then(|view| view.error.clone()));
    let stale = weather.as_ref().is_some_and(|view| {
        view.stale || (error.is_some() && (view.current.is_some() || !view.daily.is_empty()))
    });
    rsx! {
        document::Stylesheet { href: asset!("/assets/ocean-overview.css") }
        section { class: "weather-card ocean-weather", aria_label: "Погода",
            svg { class: "ocean-weather-shape", view_box: "0 0 628 340", preserve_aspect_ratio: "none", "aria-hidden": "true",
                path { d: "M0 45Q0 0 46 0C193 -8 305 43 426 34C510 29 551 -3 599 19Q628 25 628 56V307Q628 340 595 340H35Q0 340 0 306Z" }
            }
            div { class: "ocean-weather-heading",
                h2 { "Погода" }
                button { r#type: "button", class: "ocean-weather-city", title: location.map(|location| location.detail()), aria_label: "Сменить город",
                    onclick: move |_| state.panel.set(Panel::Settings),
                    if let Some(location) = location { "{location.name}" } else { "Выбрать город" }
                }
            }
            if let Some(current) = current {
                div { class: "ocean-weather-now",
                    strong { class: "ocean-weather-temperature", "{temperature(current.temperature)}" }
                    div { class: "ocean-weather-description", strong { "{current.description}" }
                        span { "Ощущается как {temperature(current.apparent_temperature)}" }
                        span { class: "ocean-weather-details", {format!("Ветер {} · Осадки {}", measurement(current.wind_speed, "м/с"), measurement(current.precipitation, "мм"))} }
                    }
                    span { class: "ocean-weather-art", aria_hidden: "true",
                        if matches!(current.weather_code, Some(2 | 3)) && current.is_day == Some(false) {
                            img { src: asset!("/assets/ocean/weather-cloud-moon.webp"), alt: "", width: "140", height: "140", loading: "lazy", decoding: "async" }
                        } else { Icon { name: weather_icon(current.weather_code, current.is_day), size: 100 } }
                    }
                }
            } else if (state.weather_loading)() {
                div { class: "ocean-weather-empty", role: "status", span { class: "spinner" } p { "Узнаём погоду…" } }
            } else if location.is_some() || error.is_some() {
                div { class: "ocean-weather-empty", Icon { name: "cloud", size: 40 } p { "Погода пока недоступна" } }
            } else {
                div { class: "ocean-weather-empty", Icon { name: "sun", size: 40 }
                    p { "Выберите город — здесь появится прогноз." }
                    button { class: "button secondary", onclick: move |_| state.panel.set(Panel::Settings), "Выбрать город" }
                }
            }
            if let Some(view) = weather.as_ref() {
                if !view.daily.is_empty() {
                    div { class: "ocean-weather-forecast", aria_label: "Прогноз на 5 дней",
                        for day in view.daily.iter().take(5) {
                            div { class: "ocean-weather-day", key: "{day.date}",
                                span { class: "ocean-weather-date", "{day_label(&day.date)}" }
                                span { class: "ocean-weather-day-icon", title: "{day.description}", aria_label: "{day.description}", Icon { name: weather_icon(day.weather_code, Some(true)), size: 24 } }
                                span { class: "ocean-weather-temperatures", strong { "{temperature(day.temperature_max)}" } small { "{temperature(day.temperature_min)}" } }
                                span { class: "ocean-weather-rain", title: "Вероятность осадков", {measurement(day.precipitation_probability, "%")} }
                            }
                        }
                    }
                }
            }
            if let Some(error) = error {
                p { class: "ocean-overview-error", role: "status", if stale { "Показываем последние данные. " } "{error}" }
            } else if stale {
                p { class: "ocean-overview-error", role: "status", "Данные устарели. Обновляем автоматически." }
            }
            div { class: "ocean-weather-footer",
                span { if let Some(time) = weather.as_ref().and_then(|view| view.fetched_at.as_ref()) { "{updated_at(time)}" } }
                a { href: "https://open-meteo.com/", target: "_blank", rel: "noopener noreferrer", "Open-Meteo ↗" }
            }
        }
    }
}

#[component]
pub fn WeatherSettings(state: ControlState, duid: String) -> Element {
    let mut query = use_signal(String::new);
    let mut results = use_signal(Vec::<Location>::new);
    let mut searched = use_signal(|| false);
    let mut searching = use_signal(|| false);
    let mut search_request = use_signal(|| 0_u64);
    let saving = use_signal(|| false);
    let mut feedback = use_signal(|| None::<(bool, String)>);
    let location = (state.weather)().and_then(|view| view.location);
    let clear_duid = duid.clone();
    rsx! {
        section { class: "card settings-card weather-settings",
            span { class: "card-kicker", Icon { name: "sun", size: 16 } "ГОРОД ДЛЯ ПОГОДЫ" }
            h2 { "Прогноз для вашего города" }
            p { class: "settings-intro", "Погода в пульте и в ответах Алисы. Город сохраняется отдельно для каждой колонки." }
            if let Some(location) = location {
                div { class: "weather-saved-location",
                    Icon { name: "pin", size: 19 }
                    div { strong { "{location.name}" } span { "{location.detail()}" } }
                    if location.id.is_some() {
                        button { class: "text-button", disabled: saving(), onclick: move |_| save_location(state, clear_duid.clone(), None, saving, feedback), "Сбросить" }
                    }
                }
                if location.id.is_some() { p { class: "form-help weather-reset-help", "После сброса используем координаты колонки, если они настроены." } }
            }
            form { class: "weather-search", onsubmit: move |event| {
                event.prevent_default();
                let search = query.read().trim().to_string();
                if search.chars().count() < 3 || searching() || saving() || !state.allowed() { return; }
                let request = *search_request.peek() + 1;
                search_request.set(request);
                searching.set(true);
                searched.set(false);
                results.set(Vec::new());
                feedback.set(None);
                spawn(async move {
                    let result = api::get::<Vec<Location>>(state, &format!("/weather/locations?query={}",api::encoded(&search))).await;
                    if *search_request.peek() != request { return; }
                    searching.set(false);
                    match result {
                        Ok(locations) => { results.set(locations); searched.set(true); }
                        Err(error) => feedback.set(Some((false,error))),
                    }
                });
            },
                label { class: "form-field", r#for: "weather-city-query", span { "Найти город" }
                    input { id: "weather-city-query", value: "{query}", maxlength: "120", autocomplete: "off", placeholder: "Название города", disabled: saving(), oninput: move |event| {
                        query.set(event.value());
                        results.set(Vec::new());
                        searched.set(false);
                        searching.set(false);
                        let request = *search_request.peek() + 1;
                        search_request.set(request);
                        feedback.set(None);
                    } }
                }
                button { class: "button secondary", r#type: "submit", disabled: searching() || saving() || query.read().trim().chars().count() < 3,
                    if searching() { span { class: "spinner" } "Ищем…" } else { Icon { name: "search", size: 16 } "Найти" }
                }
            }
            if !results.read().is_empty() {
                div { class: "weather-search-results", aria_label: "Найденные города",
                    for result in results() {
                        if let Some(id) = result.id {
                            button { class: "weather-search-result", disabled: saving(), onclick: {
                                let duid = duid.clone();
                                move |_| save_location(state, duid.clone(), Some(id), saving, feedback)
                            },
                                Icon { name: "pin", size: 17 }
                                span { strong { "{result.name}" } small { "{result.detail()}" } }
                                Icon { name: "chevron", size: 16 }
                            }
                        }
                    }
                }
            } else if searched() {
                p { class: "form-help weather-search-empty", role: "status", "Города не найдены. Попробуйте другое название." }
            }
            if saving() { p { class: "form-help", role: "status", "Сохраняем город…" } }
            if let Some((success, message)) = feedback() {
                p { class: if success { "form-feedback success" } else { "form-feedback failure" }, role: "status", "{message}" }
            }
            p { class: "form-help weather-provider", "Прогноз обновляется каждые 15 минут. " a { href: "https://open-meteo.com/", target: "_blank", rel: "noopener noreferrer", "Данные Open-Meteo ↗" } }
        }
    }
}

fn save_location(
    state: ControlState,
    duid: String,
    id: Option<i64>,
    mut saving: Signal<bool>,
    mut feedback: Signal<Option<(bool, String)>>,
) {
    if *saving.peek() || !state.allowed() {
        return;
    }
    saving.set(true);
    feedback.set(None);
    dioxus::core::spawn_forever(async move {
        let result = api::mutate(
            state,
            "PUT",
            &format!("/devices/{}/weather/location", api::encoded(&duid)),
            Some(&json!({"id":id})),
        )
        .await;
        let (success, message) = match result {
            Ok(_) => {
                if *state.selected.peek() == duid {
                    refresh(state).await;
                }
                (
                    true,
                    if id.is_some() {
                        "Город сохранён"
                    } else {
                        "Выбор города сброшен"
                    }
                    .to_string(),
                )
            }
            Err(error) => (false, error),
        };
        if let Ok(mut value) = feedback.try_write() {
            *value = Some((success, message));
        }
        if let Ok(mut value) = saving.try_write() {
            *value = false;
        }
    });
}
