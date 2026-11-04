//! Open-Meteo weather, with weather-only station preferences.

mod cache;
mod provider;

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::storage::Storage;
use cache::Cache;
use provider::{Forecast, Provider};

#[derive(Debug, thiserror::Error)]
#[error("Город не найден. Выберите город из результатов поиска")]
pub struct LocationNotFound;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Location {
    pub id: Option<i64>,
    pub name: String,
    pub admin1: Option<String>,
    pub country: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    pub timezone: Option<String>,
}

impl Location {
    fn valid(&self) -> bool {
        !self.name.trim().is_empty()
            && valid_coordinates(self.latitude, self.longitude)
            && self.id.is_none_or(|id| id > 0)
    }

    fn label(&self) -> String {
        let mut parts = vec![self.name.as_str()];
        for part in [self.admin1.as_deref(), self.country.as_deref()]
            .into_iter()
            .flatten()
        {
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
        parts.join(", ")
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Current {
    /// RFC3339 UTC; the location's timezone is returned separately.
    pub time: String,
    pub temperature: Option<f64>,
    pub apparent_temperature: Option<f64>,
    pub precipitation: Option<f64>,
    /// Metres per second.
    pub wind_speed: Option<f64>,
    pub weather_code: Option<u16>,
    pub is_day: Option<bool>,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Daily {
    /// Calendar date in the forecast location's timezone.
    pub date: String,
    pub temperature_min: Option<f64>,
    pub temperature_max: Option<f64>,
    pub precipitation_probability: Option<f64>,
    pub weather_code: Option<u16>,
    pub description: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WeatherView {
    pub location: Option<Location>,
    pub current: Option<Current>,
    pub daily: Vec<Daily>,
    pub fetched_at: Option<String>,
    pub stale: bool,
    pub error: Option<String>,
}

pub struct WeatherService {
    storage: Arc<Storage>,
    provider: Provider,
    forecasts: Cache<Forecast>,
    searches: Cache<Vec<Location>>,
    locations: Cache<Option<Location>>,
}

impl WeatherService {
    pub fn new(storage: Arc<Storage>) -> Result<Self> {
        Ok(Self {
            storage,
            provider: Provider::new()?,
            forecasts: Cache::new(),
            searches: Cache::new(),
            locations: Cache::new(),
        })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<Location>> {
        let query = query.trim();
        if !(2..=120).contains(&query.chars().count()) || query.chars().any(char::is_control) {
            bail!("Введите название города от 2 до 120 символов");
        }
        self.searches
            .get_or_fetch(query.to_lowercase(), || self.provider.search(query))
            .await
            .fresh()
    }

    pub async fn set_location(&self, duid: &str, id: Option<i64>) -> Result<Option<Location>> {
        let key = location_key(duid);
        let Some(id) = id else {
            self.storage
                .delete_control_value(&key)
                .await
                .context("Не удалось сбросить город погоды")?;
            return self.location(duid).await;
        };
        if !(1..=i64::from(i32::MAX)).contains(&id) {
            return Err(LocationNotFound.into());
        }
        // Resolve the ID server-side; never persist client-supplied coordinates.
        let location = self
            .locations
            .get_or_fetch(id.to_string(), || self.provider.location(id))
            .await
            .fresh()?
            .ok_or(LocationNotFound)?;
        self.storage
            .set_control_value(&key, &serde_json::to_value(&location)?)
            .await
            .context("Не удалось сохранить город погоды")?;
        Ok(Some(location))
    }

    /// Storage only: safe to add to assistant context without a network lookup.
    pub async fn location(&self, duid: &str) -> Result<Option<Location>> {
        if let Some(value) = self
            .storage
            .get_control_value(&location_key(duid))
            .await
            .context("Не удалось прочитать город погоды")?
        {
            let location: Location = serde_json::from_value(value)
                .context("Не удалось прочитать сохранённый город погоды")?;
            if !location.valid() || location.id.is_none() {
                bail!("Сохранённый город погоды некорректен. Выберите город заново");
            }
            return Ok(Some(location));
        }
        let stations = self
            .storage
            .list()
            .await
            .context("Не удалось прочитать местоположение колонки")?;
        let station = stations
            .iter()
            .find(|station| station.duid == duid)
            .ok_or_else(|| anyhow::anyhow!("Колонка не найдена"))?;
        let coordinates = &station.quasar_config["deviceConfig"]["location"];
        let Some((latitude, longitude)) = coordinates["latitude"]
            .as_f64()
            .zip(coordinates["longitude"].as_f64())
            .filter(|(lat, lon)| valid_coordinates(*lat, *lon))
        else {
            return Ok(None);
        };
        Ok(Some(Location {
            id: None,
            name: "Местоположение колонки".to_owned(),
            admin1: None,
            country: None,
            latitude,
            longitude,
            timezone: None,
        }))
    }

    pub async fn for_device(&self, duid: &str) -> Result<WeatherView> {
        let Some(location) = self.location(duid).await? else {
            return Ok(WeatherView {
                location: None,
                current: None,
                daily: Vec::new(),
                fetched_at: None,
                stale: false,
                error: None,
            });
        };
        Ok(self.forecast(location).await)
    }

    pub async fn for_city(&self, query: &str) -> Result<WeatherView> {
        // The provider accepts a single country/admin1 qualifier; further
        // commas are silently ignored there, so do not silently ignore them here.
        if query.matches(',').count() > 1 || query.trim().ends_with(',') {
            bail!("Укажите город и один регион или страну через запятую, например: Москва, Москва");
        }
        let locations = self.search(query).await?;
        let requested_name = query
            .split(',')
            .next()
            .unwrap_or(query)
            .trim()
            .to_lowercase();
        let exact: Vec<_> = locations
            .iter()
            .filter(|location| location.name.to_lowercase() == requested_name)
            .collect();
        let location = if locations.len() == 1 {
            &locations[0]
        } else if exact.len() == 1 {
            exact[0]
        } else if locations.is_empty() {
            bail!("Город не найден. Уточните название и страну");
        } else {
            let candidates = locations
                .iter()
                .take(5)
                .map(Location::label)
                .collect::<Vec<_>>()
                .join("; ");
            bail!("Найдено несколько городов. Уточните регион или страну: {candidates}");
        };
        Ok(self.forecast(location.clone()).await)
    }

    async fn forecast(&self, mut location: Location) -> WeatherView {
        let key = format!("{},{}", location.latitude, location.longitude);
        let cached = self
            .forecasts
            .get_or_fetch(key, || self.provider.forecast(&location))
            .await;
        let (current, daily) = match cached.value {
            Some(forecast) => {
                location.timezone = Some(forecast.timezone);
                (forecast.current, forecast.daily)
            }
            None => (None, Vec::new()),
        };
        WeatherView {
            location: Some(location),
            current,
            daily,
            fetched_at: cached.fetched_at,
            stale: cached.stale,
            error: cached.error,
        }
    }
}

fn location_key(duid: &str) -> String {
    format!("weather-location:{duid}")
}

fn valid_coordinates(latitude: f64, longitude: f64) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}
