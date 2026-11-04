use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize};

use super::{Current, Daily, Location};

const SEARCH_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
const LOCATION_URL: &str = "https://geocoding-api.open-meteo.com/v1/get";
const FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";

#[derive(Debug, thiserror::Error)]
#[error("Сервис погоды временно недоступен (HTTP {0})")]
struct ProviderHttpError(u16);

pub(super) struct Provider {
    client: Client,
}

#[derive(Clone)]
pub(super) struct Forecast {
    pub timezone: String,
    pub current: Option<Current>,
    pub daily: Vec<Daily>,
}

impl Provider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent("Quasar-Weather/1.0")
                .build()
                .context("Не удалось настроить сервис погоды")?,
        })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<Location>> {
        let result: SearchResponse = self
            .get(
                SEARCH_URL,
                &[
                    ("name", query.to_owned()),
                    ("language", "ru".to_owned()),
                    ("count", "10".to_owned()),
                    ("format", "json".to_owned()),
                ],
            )
            .await?;
        result
            .results
            .into_iter()
            .map(GeoLocation::validated)
            .collect()
    }

    pub async fn location(&self, id: i64) -> Result<Option<Location>> {
        let result: GeoLocation = match self
            .get(
                LOCATION_URL,
                &[("id", id.to_string()), ("language", "ru".to_owned())],
            )
            .await
        {
            Ok(location) => location,
            Err(error)
                if matches!(
                    error.downcast_ref::<ProviderHttpError>(),
                    Some(ProviderHttpError(400))
                ) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        if result.id != id {
            bail!("Сервис погоды вернул другой город");
        }
        result.validated().map(Some)
    }

    pub async fn forecast(&self, location: &Location) -> Result<Forecast> {
        let response: ForecastResponse = self.get(FORECAST_URL, &[
            ("latitude", location.latitude.to_string()),
            ("longitude", location.longitude.to_string()),
            ("current", "temperature_2m,apparent_temperature,precipitation,wind_speed_10m,weather_code,is_day".to_owned()),
            ("daily", "temperature_2m_max,temperature_2m_min,precipitation_probability_max,weather_code".to_owned()),
            ("wind_speed_unit", "ms".to_owned()),
            ("temperature_unit", "celsius".to_owned()),
            ("precipitation_unit", "mm".to_owned()),
            ("forecast_days", "5".to_owned()),
            ("timezone", "auto".to_owned()),
            ("timeformat", "unixtime".to_owned()),
        ]).await?;
        response.convert()
    }

    async fn get<T: DeserializeOwned>(
        &self,
        url: &'static str,
        query: &[(&str, String)],
    ) -> Result<T> {
        // All callers use the three fixed HTTPS endpoints; redirects are disabled.
        let mut response = self
            .client
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    anyhow::anyhow!("Сервис погоды не ответил вовремя")
                } else {
                    anyhow::anyhow!("Не удалось связаться с сервисом погоды")
                }
            })?;
        if !response.status().is_success() {
            return Err(ProviderHttpError(response.status().as_u16()).into());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("Не удалось получить данные погоды"))?
        {
            if body.len() + chunk.len() > 256 * 1024 {
                bail!("Сервис погоды вернул слишком большой ответ");
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).context("Сервис погоды вернул некорректные данные")
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<GeoLocation>,
}

#[derive(Deserialize)]
struct GeoLocation {
    id: i64,
    name: String,
    admin1: Option<String>,
    country: Option<String>,
    latitude: f64,
    longitude: f64,
    timezone: Option<String>,
}

impl GeoLocation {
    fn validated(self) -> Result<Location> {
        let location = Location {
            id: Some(self.id),
            name: self.name,
            admin1: self.admin1,
            country: self.country,
            latitude: self.latitude,
            longitude: self.longitude,
            timezone: self.timezone,
        };
        if !location.valid() {
            bail!("Сервис погоды вернул некорректное местоположение");
        }
        Ok(location)
    }
}

#[derive(Deserialize)]
struct ForecastResponse {
    timezone: String,
    utc_offset_seconds: i64,
    current: Option<CurrentResponse>,
    daily: Option<DailyResponse>,
}

#[derive(Deserialize)]
struct CurrentResponse {
    time: i64,
    temperature_2m: Option<f64>,
    apparent_temperature: Option<f64>,
    precipitation: Option<f64>,
    wind_speed_10m: Option<f64>,
    weather_code: Option<u16>,
    is_day: Option<u8>,
}

#[derive(Default, Deserialize)]
struct DailyResponse {
    #[serde(default)]
    time: Vec<i64>,
    #[serde(default)]
    temperature_2m_min: Vec<Option<f64>>,
    #[serde(default)]
    temperature_2m_max: Vec<Option<f64>>,
    #[serde(default)]
    precipitation_probability_max: Vec<Option<f64>>,
    #[serde(default)]
    weather_code: Vec<Option<u16>>,
}

impl ForecastResponse {
    fn convert(self) -> Result<Forecast> {
        if self.timezone.is_empty()
            || !(-24 * 60 * 60..=24 * 60 * 60).contains(&self.utc_offset_seconds)
        {
            bail!("Сервис погоды вернул некорректный часовой пояс");
        }
        let current = self
            .current
            .map(|current| -> Result<Current> {
                Ok(Current {
                    time: timestamp(current.time)?.to_rfc3339_opts(SecondsFormat::Secs, true),
                    temperature: current.temperature_2m,
                    apparent_temperature: current.apparent_temperature,
                    precipitation: current.precipitation,
                    wind_speed: current.wind_speed_10m,
                    weather_code: current.weather_code,
                    is_day: current.is_day.and_then(|value| match value {
                        0 => Some(false),
                        1 => Some(true),
                        _ => None,
                    }),
                    description: description(current.weather_code).to_owned(),
                })
            })
            .transpose()?;
        let data = self.daily.unwrap_or_default();
        let daily = data
            .time
            .iter()
            .take(5)
            .enumerate()
            .map(|(index, time)| -> Result<Daily> {
                // Open-Meteo daily unix timestamps need the UTC offset reapplied to
                // recover the local calendar date (including zones west of UTC).
                let local = time
                    .checked_add(self.utc_offset_seconds)
                    .ok_or_else(|| anyhow::anyhow!("Сервис погоды вернул некорректную дату"))?;
                let code = data.weather_code.get(index).copied().flatten();
                Ok(Daily {
                    date: timestamp(local)?.format("%Y-%m-%d").to_string(),
                    temperature_min: data.temperature_2m_min.get(index).copied().flatten(),
                    temperature_max: data.temperature_2m_max.get(index).copied().flatten(),
                    precipitation_probability: data
                        .precipitation_probability_max
                        .get(index)
                        .copied()
                        .flatten(),
                    weather_code: code,
                    description: description(code).to_owned(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if current.is_none() && daily.is_empty() {
            bail!("Сервис погоды пока не вернул прогноз для этого места");
        }
        Ok(Forecast {
            timezone: self.timezone,
            current,
            daily,
        })
    }
}

fn timestamp(seconds: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp(seconds, 0).context("Сервис погоды вернул некорректную дату")
}

fn description(code: Option<u16>) -> &'static str {
    match code {
        Some(0) => "Ясно",
        Some(1) => "Преимущественно ясно",
        Some(2) => "Переменная облачность",
        Some(3) => "Пасмурно",
        Some(45 | 48) => "Туман",
        Some(51) => "Слабая морось",
        Some(53) => "Морось",
        Some(55) => "Сильная морось",
        Some(56 | 57) => "Ледяная морось",
        Some(61) => "Небольшой дождь",
        Some(63) => "Дождь",
        Some(65) => "Сильный дождь",
        Some(66 | 67) => "Ледяной дождь",
        Some(71) => "Небольшой снег",
        Some(73) => "Снег",
        Some(75) => "Сильный снег",
        Some(77) => "Снежные зёрна",
        Some(80) => "Небольшой ливень",
        Some(81) => "Ливень",
        Some(82) => "Сильный ливень",
        Some(85 | 86) => "Снегопад",
        Some(95) => "Гроза",
        Some(96 | 99) => "Гроза с градом",
        _ => "Нет данных",
    }
}
