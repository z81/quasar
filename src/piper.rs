//! Speech synthesis by Piper (https://github.com/OHF-Voice/piper1-gpl, its
//! HTTP server `python3 -m piper.http_server`; docker/piper builds one):
//! `POST /synthesize` with the text, and optionally the voice, the speaker
//! and the pace, answers the WAV of the whole text once it is synthesized.
//! The WAV's own header tells its rate (22050 Hz for most voices).

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::json;

use crate::config::{Config, PiperConfig};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// The whole request: Piper answers once the text is synthesized
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// At most this much of an error body goes into a log line
const ERROR_BODY_CHARS: usize = 300;

pub struct Piper {
    client: reqwest::Client,
    synthesize_url: String,
    config: PiperConfig,
}

impl Piper {
    /// The synthesizer of TTS_PROVIDER=piper
    pub fn from_config(config: &Config) -> Option<Self> {
        let piper = config.piper.clone()?;
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("HTTP client for Piper");
        tracing::info!(
            "speech synthesis: Piper at {} (voice {}{}), {}",
            piper.url,
            piper.voice.as_deref().unwrap_or("of the server"),
            piper.length_scale.map(|scale| format!(", length scale {scale:.3}")).unwrap_or_default(),
            if piper.fallback { "OpenRouter for the chunks it fails on" } else { "no fallback" }
        );
        Some(Self { client, synthesize_url: format!("{}/synthesize", piper.url), config: piper })
    }

    /// Whether OpenRouter speaks a chunk Piper fails on (PIPER_FALLBACK)
    pub fn falls_back(&self) -> bool {
        self.config.fallback
    }

    /// The WAV of `text`, as it arrives
    pub async fn synthesize(&self, text: &str) -> Result<impl Stream<Item = Result<Bytes>>> {
        let mut body = json!({ "text": text });
        if let Some(voice) = &self.config.voice {
            body["voice"] = json!(voice);
        }
        if let Some(speaker) = &self.config.speaker {
            body["speaker"] = json!(speaker);
        }
        if let Some(speaker_id) = self.config.speaker_id {
            body["speaker_id"] = json!(speaker_id);
        }
        if let Some(scale) = self.config.length_scale {
            // 0.8, not 0.800000011920929
            body["length_scale"] = json!((f64::from(scale) * 1000.0).round() / 1000.0);
        }
        let response = self.client.post(&self.synthesize_url).json(&body).send().await.map_err(describe)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
            let cut = body.char_indices().nth(ERROR_BODY_CHARS).map_or(body.as_str(), |(end, _)| &body[..end]);
            bail!("Piper answered {status}: {cut}");
        }
        Ok(response.bytes_stream().map(|piece| piece.map_err(describe)))
    }
}

/// A failed request in a few words: no answer in time, unreachable, or what else
fn describe(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow!("Piper did not answer within {REQUEST_TIMEOUT:?}")
    } else if error.is_connect() {
        anyhow!("Piper is unreachable: {:#}", anyhow::Error::from(error))
    } else {
        anyhow!("Piper request failed: {:#}", anyhow::Error::from(error))
    }
}
