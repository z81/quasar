//! OpenRouter client: chat completions (SSE streaming), speech synthesis
//! (raw PCM byte stream) and transcription (JSON with base64 audio).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct OpenRouter {
    client: Client,
    base_url: String,
    api_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub content: String,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "user".to_string()
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { content: content.into(), role: "system".into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { content: content.into(), role: "user".into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { content: content.into(), role: "assistant".into() }
    }
}

/// One streamed content delta of a chat completion
#[derive(Debug)]
pub struct ChatDelta(pub String);

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

impl OpenRouter {
    pub fn new(base_url: &str, api_key: &str, timeout_ms: u64) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            client,
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Streams chat completion content deltas over SSE
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
    ) -> Result<impl futures_util::Stream<Item = Result<ChatDelta>>> {
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-Title", "alice-server")
            .json(&json!({
                "model": model,
                "stream": true,
                "messages": messages,
            }))
            .send()
            .await
            .context("chat completions request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("chat completions failed: {status}: {body}");
        }

        Ok(SseDeltaStream {
            stream: Box::pin(response.bytes_stream()),
            buffer: String::new(),
            done: false,
        })
    }

    /// Streams synthesized speech as raw signed 16-bit little-endian mono PCM
    pub async fn speech_stream(
        &self,
        model: &str,
        voice: &str,
        instructions: &Option<String>,
        speed: f32,
        text: &str,
    ) -> Result<impl futures_util::Stream<Item = Result<bytes::Bytes>>> {
        let mut body = json!({
            "model": model,
            "input": text,
            "voice": voice,
            "response_format": "pcm",
        });
        if (speed - 1.0).abs() > f32::EPSILON {
            body["speed"] = json!(speed);
        }
        if let Some(instructions) = instructions {
            body["instructions"] = json!(instructions);
        }

        let response = self
            .client
            .post(format!("{}/audio/speech", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-Title", "alice-server")
            .json(&body)
            .send()
            .await
            .context("speech request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("speech synthesis failed: {status}: {body}");
        }

        Ok(response.bytes_stream().map(|chunk| chunk.map_err(anyhow::Error::from)))
    }

    /// Transcribes an OGG/Opus utterance (converted to WAV by the caller)
    pub async fn transcribe(&self, model: &str, language: &str, wav: &[u8]) -> Result<String> {
        let audio_base64 = base64::engine::general_purpose::STANDARD.encode(wav);
        let response = self
            .client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-Title", "alice-server")
            .json(&json!({
                "model": model,
                "language": language,
                "input_audio": {
                    "data": audio_base64,
                    "format": "wav",
                },
            }))
            .send()
            .await
            .context("transcription request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("transcription failed: {status}: {body}");
        }

        let parsed: TranscriptionResponse = response.json().await.context("invalid transcription response")?;
        Ok(parsed.text)
    }
}

struct SseDeltaStream {
    stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    buffer: String,
    done: bool,
}

impl futures_util::Stream for SseDeltaStream {
    type Item = Result<ChatDelta>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        if self.done {
            return Poll::Ready(None);
        }

        loop {
            if let Some(newline) = self.buffer.find('\n') {
                let line: String = self.buffer.drain(..=newline).collect();
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        self.done = true;
                        return Poll::Ready(None);
                    }
                    if let Ok(Value::String(content)) =
                        serde_json::from_str::<Value>(data).map(|value| {
                            value["choices"][0]["delta"]["content"].clone()
                        })
                    {
                        if !content.is_empty() {
                            return Poll::Ready(Some(Ok(ChatDelta(content))));
                        }
                    }
                }
                continue;
            }

            match self.stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buffer.push_str(&String::from_utf8_lossy(&bytes));
                }
                Poll::Ready(Some(Err(error))) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(anyhow::Error::from(error))));
                }
                Poll::Ready(None) => {
                    self.done = true;
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
