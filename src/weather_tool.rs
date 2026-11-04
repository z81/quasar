//! The assistant uses the same weather service and cache as the control UI.

use serde_json::{json, Value};

use crate::agent::{FunctionDefinition, FunctionKind};
use crate::weather::WeatherService;

pub fn definition() -> FunctionDefinition {
    FunctionDefinition {
        description: "Get current weather and a 5-day forecast from Open-Meteo. Always call this immediately before answering any weather question; never invent weather or reuse weather from conversation history. Set can_cache=false for weather answers. Do not schedule this function. Omit city to use this station's selected weather location; pass city only when the user explicitly asks about another place. If no location is set or a city is ambiguous, ask the user to clarify. Report stale data as an older forecast with its fetched_at time, never as live conditions. Temperatures are Celsius, wind is m/s, precipitation is mm, probabilities are percent; null means unavailable.".to_string(),
        kind: FunctionKind::Remote { has_response: true, directive: None },
        arguments_json_schema: Some(json!({
            "type": "object",
            "properties": {
                "city": {"type": "string", "minLength": 2, "maxLength": 120, "description": "Explicit city name; separate region/country with commas when clarifying an ambiguous city, e.g. Paris, France"}
            },
            "additionalProperties": false
        })),
    }
}

pub async fn call(service: &WeatherService, duid: &str, arguments: Option<&Value>) -> String {
    let city = match arguments {
        None | Some(Value::Null) => None,
        Some(Value::Object(args)) if args.keys().all(|key| key == "city") => {
            match args.get("city") {
                None => None,
                Some(Value::String(city)) if (2..=120).contains(&city.trim().chars().count()) => Some(city.trim()),
                _ => return "Error: city must be a city name of 2–120 characters; omit it to use the station's location.".to_string(),
            }
        }
        _ => return "Error: expected an object with optional city.".to_string(),
    };
    let result = match city {
        Some(city) => service.for_city(city).await,
        None => service.for_device(duid).await,
    };
    match result {
        Ok(view) if view.location.is_none() => "Error: Город для погоды не выбран. Уточни город у пользователя; постоянный город можно выбрать в настройках пульта.".to_string(),
        Ok(view) if view.current.is_none() && view.daily.is_empty() => format!(
            "Error: {}. Сообщи, что погода сейчас недоступна; не придумывай прогноз.",
            view.error.as_deref().unwrap_or("Прогноз пока недоступен")
        ),
        Ok(view) => json!({
            "source": "Open-Meteo",
            "source_url": "https://open-meteo.com/",
            "units": {"temperature": "°C", "wind_speed": "m/s", "precipitation": "mm", "precipitation_probability": "%"},
            "weather": view,
        }).to_string(),
        Err(cause) => format!("Error: {cause}. Не придумывай прогноз; уточни город, если он неоднозначен, или сообщи, что погода сейчас недоступна."),
    }
}
