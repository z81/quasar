use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Alarm,
    Reminder,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub kind: Kind,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub text: String,
    pub time: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub weekdays: Vec<u8>,
    pub timezone: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Schedule {
    pub id: String,
    pub duid: String,
    pub kind: Kind,
    pub title: String,
    pub text: String,
    pub time: String,
    pub date: Option<String>,
    pub weekdays: Vec<u8>,
    pub timezone: String,
    pub enabled: bool,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub status: String,
    pub last_error: Option<String>,
    pub active_run_id: Option<String>,
    pub ringing_until: Option<String>,
    pub snoozed_until: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Schedule {
    pub(super) fn input(&self) -> Input {
        Input {
            kind: self.kind,
            title: self.title.clone(),
            text: self.text.clone(),
            time: self.time.clone(),
            date: self.date.clone(),
            weekdays: self.weekdays.clone(),
            timezone: self.timezone.clone(),
            enabled: self.enabled,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("{0}")]
    Invalid(String),
    #[error("Запись расписания не найдена")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("Хранилище расписания временно недоступно")]
    Unavailable,
}

impl ScheduleError {
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::NotFound => 404,
            Self::Conflict(_) => 409,
            Self::Unavailable => 503,
        }
    }
}

pub type Result<T> = std::result::Result<T, ScheduleError>;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Claim {
    pub token: String,
    pub started_at: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Entry {
    #[serde(flatten)]
    pub schedule: Schedule,
    pub regular_next_at: Option<String>,
    pub claim: Option<Claim>,
}

impl Entry {
    pub fn refresh_next(&mut self) {
        self.schedule.next_run_at = if self.schedule.enabled {
            self.regular_next_at
                .iter()
                .chain(self.schedule.snoozed_until.iter())
                .min()
                .cloned()
        } else {
            None
        };
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Cleanup {
    pub duid: String,
    pub run_id: String,
    pub stop_channel: bool,
    pub claim: Option<Claim>,
    pub retry_at: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Document {
    pub version: u8,
    pub revision: u64,
    pub entries: Vec<Entry>,
    pub cleanup: Vec<Cleanup>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            entries: Vec::new(),
            cleanup: Vec::new(),
        }
    }
}
