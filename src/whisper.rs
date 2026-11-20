//! Per-station explicit whisper replies and the display-only result of the
//! local whisper detector. Whispering is synthesized by an expressive model;
//! neither low microphone level nor low playback volume establishes it.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{ensure, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::storage::Storage;

pub struct WhisperService {
    storage: Arc<Storage>,
    /// The last result per station; the panel reads it on every device poll.
    last: Mutex<HashMap<String, Detection>>,
}

/// Display-only result of the local YAMNet classifier.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Detection {
    pub at: String,
    pub text: String,
    pub whisper: bool,
    pub score: f32,
    #[serde(default)]
    pub threshold: Option<f32>,
    #[serde(default)]
    pub frames: Option<u32>,
    #[serde(default)]
    pub seconds: Option<f32>,
    #[serde(default)]
    pub yamnet: Option<f32>,
    #[serde(default)]
    pub pitch: Option<f32>,
    #[serde(default)]
    pub flatness: Option<f32>,
}

#[derive(Serialize)]
pub struct View {
    pub enabled: bool,
    pub available: bool,
    pub persistent: bool,
    pub model: String,
    pub voice: String,
    pub detection: &'static str,
    pub detection_available: bool,
    pub last: Option<Detection>,
    pub message: &'static str,
}

impl WhisperService {
    pub fn new(storage: Arc<Storage>) -> Self {
        Self {
            storage,
            last: Mutex::new(HashMap::new()),
        }
    }

    /// Stores the last detected utterance. The transcript is kept only in the
    /// single control record for this station, not in a conversation history.
    pub async fn record_detection(
        &self,
        duid: &str,
        text: &str,
        reply: &crate::voice_detection::Reply,
    ) -> Result<()> {
        let text: String = text.trim().chars().take(2000).collect();
        if text.is_empty() {
            return Ok(());
        }
        let detection = Detection {
            at: Utc::now().to_rfc3339(),
            text,
            whisper: reply.whisper,
            score: reply.score,
            threshold: reply.threshold,
            frames: reply.frames,
            seconds: reply.seconds,
            yamnet: reply.yamnet,
            pitch: reply.pitch,
            flatness: reply.flatness,
        };
        self.storage
            .set_control_value(
                &format!("voice:last:{duid}"),
                &serde_json::to_value(&detection)?,
            )
            .await?;
        self.last.lock().await.insert(duid.to_owned(), detection);
        Ok(())
    }

    pub async fn last_detection(&self, duid: &str) -> Result<Option<Detection>> {
        if let Some(detection) = self.last.lock().await.get(duid).cloned() {
            return Ok(Some(detection));
        }
        let Some(value) = self
            .storage
            .get_control_value(&format!("voice:last:{duid}"))
            .await?
        else {
            return Ok(None);
        };
        let Ok(detection) = serde_json::from_value::<Detection>(value) else {
            return Ok(None);
        };
        self.last
            .lock()
            .await
            .insert(duid.to_owned(), detection.clone());
        Ok(Some(detection))
    }

    pub async fn enabled(&self, duid: &str) -> Result<bool> {
        match self.storage.get_control_value(&format!("whisper:{duid}")).await? {
            None => Ok(false),
            Some(value) => value.as_bool().ok_or_else(|| anyhow::anyhow!("Invalid stored whisper preference")),
        }
    }

    pub async fn set(&self, config: &Config, duid: &str, enabled: bool) -> Result<()> {
        ensure!(!enabled || available(config), "Для шёпота нужен ключ OpenRouter в настройках сервера");
        self.storage.set_control_value(&format!("whisper:{duid}"), &json!(enabled)).await
    }

    pub async fn view(&self, config: &Config, duid: &str) -> Result<View> {
        let detection_available = !config.whisper_detect_url.trim().is_empty();
        Ok(View {
            enabled: self.enabled(duid).await?,
            available: available(config),
            persistent: self.storage.is_persistent(),
            model: config.whisper_tts_model.clone(),
            voice: config.whisper_tts_voice.clone(),
            detection: if detection_available { "yamnet" } else { "off" },
            detection_available,
            last: self.last_detection(duid).await?,
            message: if available(config) {
                if detection_available {
                    "Настоящий шёпот через ElevenLabs в OpenRouter. Определение шёпота показывает последнюю фразу, но голос ответа не меняет."
                } else {
                    "Настоящий шёпот через ElevenLabs в OpenRouter. Включается переключателем или командой «говори шёпотом»."
                }
            } else if detection_available {
                "Piper не поддерживает шёпот: для ответов шёпотом нужен ключ OpenRouter. Определение шёпота уже работает и показывает последнюю фразу."
            } else {
                "Для шёпота добавьте ключ OpenRouter в настройках сервера. Piper не поддерживает шёпот."
            },
        })
    }
}

pub fn available(config: &Config) -> bool {
    !config.openrouter_api_key.is_empty()
}

/// Deliberately exact commands: an explanation, quote, negation or external
/// event mentioning whispering must not persistently change the station.
pub fn command(text: &str) -> Option<bool> {
    let normalized = text.to_lowercase().replace('ё', "е");
    let words: Vec<&str> = normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let mut words = words.as_slice();
    if words.first().is_some_and(|word| matches!(*word, "алиса" | "яндекс" | "ясмина")) {
        words = &words[1..];
    }
    if words.first() == Some(&"пожалуйста") {
        words = &words[1..];
    }
    if words.last() == Some(&"пожалуйста") {
        words = &words[..words.len() - 1];
    }
    match words {
        ["говори" | "отвечай", "шепотом" | "шопотом"]
        | ["включи", "шепот" | "шопот"]
        | ["включи", "режим", "шепота"] => Some(true),
        ["говори" | "отвечай", "обычным" | "нормальным", "голосом"]
        | ["выключи" | "отключи", "шепот" | "шопот"]
        | ["выключи" | "отключи", "режим", "шепота"]
        | ["не", "шепчи"]
        | ["не", "говори" | "отвечай", "шепотом" | "шопотом"] => Some(false),
        _ => None,
    }
}
