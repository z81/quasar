//! Final transcription by a local whisper.cpp server (`POST /inference`).
//! Whisper is an offline model: GigaAM still streams partials and endpointing,
//! and this only replaces the final text of the utterance.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;

#[derive(Clone)]
pub struct WhisperStt {
    client: Client,
    base_url: String,
}

impl WhisperStt {
    pub fn new(base_url: &str, connect_timeout_ms: u64) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(connect_timeout_ms))
            .read_timeout(Duration::from_secs(30))
            .build()
            .context("failed to build the Whisper STT client")?;
        Ok(Self { client, base_url: base_url.trim_end_matches('/').to_string() })
    }

    pub async fn transcribe(&self, wav: &[u8], language: Option<&str>) -> Result<String> {
        let part = reqwest::multipart::Part::bytes(wav.to_vec())
            .file_name("utterance.wav")
            .mime_str("audio/wav")
            .context("invalid WAV media type")?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "json")
            .text("temperature", "0");
        if let Some(language) = language {
            form = form.text("language", language.to_string());
        }
        let response = self
            .client
            .post(format!("{}/inference", self.base_url))
            .multipart(form)
            .send()
            .await
            .context("Whisper STT request failed")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let body: String = body.chars().take(300).collect();
            bail!("Whisper STT failed: {status}: {body}");
        }
        let response: Value = response.json().await.context("invalid Whisper STT response")?;
        Ok(response["text"].as_str().unwrap_or_default().trim().to_string())
    }
}
