use std::collections::BTreeMap;

use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Device {
    pub duid: String,
    pub name: String,
    #[serde(default)]
    pub connected: Option<bool>,
    #[serde(default, rename = "ipAddress")]
    pub ip_address: Option<String>,
    #[serde(default, rename = "quasarConfig")]
    pub quasar_config: Value,
    #[serde(default)]
    pub timezone: Option<String>,
}

impl Device {
    pub fn can_receive(&self) -> bool {
        self.connected != Some(false)
    }

    pub fn connection_label(&self) -> &'static str {
        match self.connected {
            Some(true) => "Подключена",
            Some(false) => "Нет соединения",
            None => "Статус неизвестен",
        }
    }

    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.duid
        } else {
            &self.name
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Mode {
    #[default]
    Command,
    Speak,
    Event,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Command, Self::Speak, Self::Event];

    pub fn label(self) -> &'static str {
        match self {
            Self::Command => "Команда",
            Self::Speak => "Озвучить",
            Self::Event => "Событие",
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Self::Command => "push-text",
            Self::Speak => "push-raw",
            Self::Event => "push",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Command => "Алиса ответит вслух и выполнит вашу просьбу.",
            Self::Speak => "Колонка произнесёт текст точно так, как вы его напишете.",
            Self::Event => "Алиса получит событие и решит, как на него отреагировать.",
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Command => "Алиса, какая сегодня погода?",
            Self::Speak => "Напишите, что должна сказать колонка…",
            Self::Event => "Например: доставка приехала к подъезду",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Panel {
    #[default]
    Control,
    History,
    Advanced,
    Favorites,
    Schedules,
    Timers,
    Settings,
    DeviceAccess,
    MidiDisplay,
    SmartHome,
    Access,
}

impl Panel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Control => "Управление",
            Self::History => "История команд",
            Self::Advanced => "Директивы",
            Self::Favorites => "Избранное",
            Self::Schedules => "Будильники",
            Self::Timers => "Таймеры",
            Self::Settings => "Настройки",
            Self::DeviceAccess => "Терминал и файлы",
            Self::MidiDisplay => "Своя Миди",
            Self::SmartHome => "Умный дом",
            Self::Access => "Доступ",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub fn load() -> Self {
        let window = web_sys::window();
        let saved = window
            .as_ref()
            .and_then(|window| window.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item("quasar.theme").ok().flatten());
        match saved.as_deref() {
            Some("dark") => Self::Dark,
            Some("light") => Self::Light,
            _ if window
                .and_then(|window| {
                    window
                        .match_media("(prefers-color-scheme: dark)")
                        .ok()
                        .flatten()
                })
                .is_some_and(|query| query.matches()) =>
            {
                Self::Dark
            }
            _ => Self::Light,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn save(self) {
        if let Some(storage) =
            web_sys::window().and_then(|window| window.local_storage().ok().flatten())
        {
            let _ = storage.set_item("quasar.theme", self.name());
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Action {
    pub name: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Entry {
    pub id: String,
    pub created_at: String,
    pub station: String,
    pub duid: String,
    pub label: String,
    pub text: String,
    pub path: String,
    pub body: Value,
    pub status: String,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub delivered: usize,
}

impl Entry {
    pub fn time(&self) -> String {
        let date = js_sys::Date::new(&self.created_at.clone().into());
        if date.get_time().is_nan() {
            return self.created_at.clone();
        }
        format!(
            "{:02}.{:02} {:02}:{:02}",
            date.get_date(),
            date.get_month() + 1,
            date.get_hours(),
            date.get_minutes()
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Favorite {
    pub id: String,
    pub label: String,
    pub duid: String,
    pub path: String,
    pub body: Value,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub struct AuthStatus {
    #[serde(default = "auth_available")]
    pub available: bool,
    pub configured: bool,
    pub authenticated: bool,
}

fn auth_available() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct PlayerCapabilities {
    pub pause: bool,
    pub resume: bool,
    pub next: bool,
    pub previous: bool,
    pub set_volume: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Player {
    pub connected: bool,
    pub state: String,
    pub source: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    pub updated_at: Option<String>,
    pub capabilities: PlayerCapabilities,
}

#[derive(Clone, Copy, PartialEq)]
pub struct ControlState {
    pub devices: Signal<Vec<Device>>,
    pub selected: Signal<String>,
    pub loading: Signal<bool>,
    pub server_error: Signal<Option<String>>,
    pub sending: Signal<bool>,
    pub history: Signal<Vec<Entry>>,
    pub history_error: Signal<Option<String>>,
    pub favorites: Signal<Vec<Favorite>>,
    pub persistent: Signal<Option<bool>>,
    pub player: Signal<Option<Player>>,
    pub player_error: Signal<Option<String>>,
    pub weather: Signal<Option<crate::weather::WeatherView>>,
    pub weather_error: Signal<Option<String>>,
    pub weather_loading: Signal<bool>,
    pub weather_request: Signal<u64>,
    pub station_previews: Signal<BTreeMap<String, crate::station_preview::StationPreview>>,
    pub auth: Signal<AuthStatus>,
    pub auth_loaded: Signal<bool>,
    pub auth_error: Signal<Option<String>>,
    pub mode: Signal<Mode>,
    pub text: Signal<String>,
    pub panel: Signal<Panel>,
    pub theme: Signal<Theme>,
    pub notice: Signal<Option<String>>,
}

impl ControlState {
    pub fn device(self) -> Option<Device> {
        self.devices
            .read()
            .iter()
            .find(|device| device.duid == *self.selected.read())
            .cloned()
    }

    pub fn can_send(self) -> bool {
        !(self.sending)()
            && self.server_error.read().is_none()
            && self.device().is_some_and(|device| device.can_receive())
    }

    pub fn allowed(self) -> bool {
        (self.auth_loaded)()
            && (self.auth)().available
            && (!(self.auth)().configured || (self.auth)().authenticated)
    }
}
