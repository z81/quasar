//! Per-station explicit whisper replies and the display-only result of the
//! local whisper detector. Whispering is synthesized by an expressive model;
//! neither low microphone level nor low playback volume establishes it.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{ensure, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::storage::Storage;

pub struct WhisperService {
    storage: Arc<Storage>,
    /// The last result per station; the panel reads it on every device poll.
    last: Mutex<HashMap<String, Detection>>,
}

/// When the station answers in a whisper. `Auto` uses the local YAMNet
/// detector on the current utterance; text inputs stay in a normal voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Always,
    Auto,
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
    pub mode: Mode,
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

    pub async fn mode(&self, duid: &str) -> Result<Mode> {
        match self.storage.get_control_value(&format!("whisper:{duid}")).await? {
            None => Ok(Mode::Off),
            Some(Value::Bool(true)) => Ok(Mode::Always),
            Some(Value::Bool(false)) => Ok(Mode::Off),
            Some(Value::String(value)) if value == "always" => Ok(Mode::Always),
            Some(Value::String(value)) if value == "auto" => Ok(Mode::Auto),
            Some(Value::String(value)) if value == "off" => Ok(Mode::Off),
            Some(_) => Err(anyhow::anyhow!("Invalid stored whisper preference")),
        }
    }

    pub async fn enabled(&self, duid: &str) -> Result<bool> {
        Ok(self.mode(duid).await? != Mode::Off)
    }

    pub async fn set_mode(&self, config: &Config, duid: &str, mode: Mode) -> Result<()> {
        ensure!(
            mode == Mode::Off || available(config),
            "Для шёпота нужен локальный сервер Qwen3-TTS или ключ OpenRouter в настройках сервера"
        );
        let value = match mode {
            Mode::Off => "off",
            Mode::Always => "always",
            Mode::Auto => "auto",
        };
        self.storage.set_control_value(&format!("whisper:{duid}"), &json!(value)).await
    }

    pub async fn set(&self, config: &Config, duid: &str, enabled: bool) -> Result<()> {
        self.set_mode(config, duid, if enabled { Mode::Always } else { Mode::Off }).await
    }

    pub async fn view(&self, config: &Config, duid: &str) -> Result<View> {
        let detection_available = !config.whisper_detect_url.trim().is_empty();
        let local = config.whisper_tts_url.is_some();
        let mode = self.mode(duid).await?;
        let (model, voice) = if local {
            (config.whisper_tts_local_model.clone(), config.whisper_tts_local_voice.clone())
        } else {
            (config.whisper_tts_model.clone(), config.whisper_tts_voice.clone())
        };
        Ok(View {
            enabled: mode != Mode::Off,
            mode,
            available: available(config),
            persistent: self.storage.is_persistent(),
            model,
            voice,
            detection: if detection_available { "yamnet" } else { "off" },
            detection_available,
            last: self.last_detection(duid).await?,
            message: if local {
                if mode == Mode::Auto {
                    if detection_available {
                        "Шёпот локально через Qwen3-TTS. Авто: если фраза сказана шёпотом — ответ шёпотом, иначе обычным голосом."
                    } else {
                        "Авто-режим требует детектор шёпота: укажите адрес YAMNet в настройках сервера."
                    }
                } else if detection_available {
                    "Настоящий шёпот локально через Qwen3-TTS. Определение шёпота показывает последнюю фразу; режим включается переключателем или командой «говори шёпотом»."
                } else {
                    "Настоящий шёпот локально через Qwen3-TTS. Включается переключателем или командой «говори шёпотом»."
                }
            } else if available(config) {
                if mode == Mode::Auto {
                    if detection_available {
                        "Шёпот через ElevenLabs в OpenRouter. Авто: если фраза сказана шёпотом — ответ шёпотом, иначе обычным голосом."
                    } else {
                        "Авто-режим требует детектор шёпота: укажите адрес YAMNet в настройках сервера."
                    }
                } else if detection_available {
                    "Настоящий шёпот через ElevenLabs в OpenRouter. Определение шёпота показывает последнюю фразу; режим включается переключателем или командой «говори шёпотом»."
                } else {
                    "Настоящий шёпот через ElevenLabs в OpenRouter. Включается переключателем или командой «говори шёпотом»."
                }
            } else if mode == Mode::Auto {
                "Для авто-шёпота нужен локальный Qwen3-TTS или ключ OpenRouter в настройках сервера."
            } else if detection_available {
                "Piper не поддерживает шёпот: для ответов шёпотом нужен локальный Qwen3-TTS или ключ OpenRouter. Определение шёпота уже работает и показывает последнюю фразу."
            } else {
                "Для шёпота добавьте локальный Qwen3-TTS или ключ OpenRouter в настройках сервера. Piper не поддерживает шёпот."
            },
        })
    }
}

pub fn available(config: &Config) -> bool {
    config.whisper_tts_url.is_some() || !config.openrouter_api_key.is_empty()
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
