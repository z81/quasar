//! Explicit per-station whisper replies. Whispering is synthesized by an
//! expressive model; neither low microphone level nor low playback volume
//! establishes whispered speech.

use std::sync::Arc;

use anyhow::{ensure, Result};
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::storage::Storage;

pub struct WhisperService {
    storage: Arc<Storage>,
}

#[derive(Serialize)]
pub struct View {
    pub enabled: bool,
    pub available: bool,
    pub persistent: bool,
    pub model: String,
    pub voice: String,
    pub detection: &'static str,
    pub message: &'static str,
}

impl WhisperService {
    pub fn new(storage: Arc<Storage>) -> Self {
        Self { storage }
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
        Ok(View {
            enabled: self.enabled(duid).await?,
            available: available(config),
            persistent: self.storage.is_persistent(),
            model: config.whisper_tts_model.clone(),
            voice: config.whisper_tts_voice.clone(),
            detection: "manual",
            message: if available(config) {
                "Настоящий шёпот через ElevenLabs в OpenRouter. Включается переключателем или командой «говори шёпотом», выключается командой «говори обычным голосом». Автоопределение шёпота пока недоступно."
            } else {
                "Для шёпота добавьте ключ OpenRouter в настройках сервера. Piper не поддерживает шёпот. Автоопределение шёпота пока недоступно."
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
