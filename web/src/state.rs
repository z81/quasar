use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Device {
    pub duid: String,
    pub name: String,
    #[serde(default)]
    pub connected: Option<bool>,
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
}

impl Panel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Control => "Управление",
            Self::History => "История команд",
            Self::Advanced => "Директивы",
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

#[derive(Clone, PartialEq)]
pub enum Delivery {
    Sending,
    Sent,
    Accepted,
    Failed,
}

#[derive(Clone, PartialEq)]
pub struct Entry {
    pub id: u64,
    pub time: String,
    pub station: String,
    pub duid: String,
    pub label: String,
    pub text: String,
    pub path: String,
    pub body: Value,
    pub delivery: Delivery,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq)]
pub struct ControlState {
    pub devices: Signal<Vec<Device>>,
    pub selected: Signal<String>,
    pub loading: Signal<bool>,
    pub server_error: Signal<Option<String>>,
    pub sending: Signal<bool>,
    pub history: Signal<Vec<Entry>>,
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
}
