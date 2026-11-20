//! Optional local YAMNet classifier for the station's voice utterance.
//! Detection is display-only: it never changes the synthesized voice.

use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Clone)]
pub struct Detector {
    url: String,
    client: reqwest::Client,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
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

impl Detector {
    pub fn from_config(config: &Config) -> Option<Self> {
        let url = config.whisper_detect_url.trim().trim_end_matches('/');
        if url.is_empty() {
            return None;
        }
        let Ok(parsed) = reqwest::Url::parse(url) else {
            tracing::warn!("WHISPER_DETECT_URL: некорректный адрес; определение шёпота отключено");
            return None;
        };
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            tracing::warn!("WHISPER_DETECT_URL: нужен HTTP(S)-адрес; определение шёпота отключено");
            return None;
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .ok()?;
        Some(Self {
            url: url.to_owned(),
            client,
        })
    }

    /// The station's OGG/Opus utterance. The detector decodes it itself, so
    /// this runs in parallel with transcription and never delays the answer.
    pub async fn classify(&self, ogg: &[u8]) -> Result<Reply> {
        ensure!(!ogg.is_empty(), "пустая запись");
        let response = self
            .client
            .post(format!("{}/classify", self.url))
            .header(reqwest::header::CONTENT_TYPE, "audio/ogg")
            .body(ogg.to_vec())
            .send()
            .await
            .context("детектор шёпота недоступен")?
            .error_for_status()
            .context("детектор шёпота вернул ошибку")?;
        let reply = response
            .json::<Reply>()
            .await
            .context("детектор шёпота вернул непонятный ответ")?;
        ensure!(
            reply.score.is_finite() && (0.0..=1.0).contains(&reply.score),
            "детектор шёпота вернул неверную оценку"
        );
        Ok(reply)
    }
}
