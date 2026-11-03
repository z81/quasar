//! OpenRouter client: chat completions (SSE streaming), speech synthesis
//! (a byte stream of raw PCM or MP3) and transcription (JSON with base64 audio).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::TtsFormat;

/// At most this many characters of an error body go into a log line
const ERROR_BODY_CHARS: usize = 600;

#[derive(Clone)]
pub struct OpenRouter {
    client: Client,
    base_url: String,
    api_key: String,
    timeout: Duration,
    /// OPENROUTER_PROCESSOR_REASONING, for the chat completions
    reasoning: Option<Arc<Reasoning>>,
}

/// The reasoning effort asked of the dialog LLM, until OpenRouter refuses it
struct Reasoning {
    effort: String,
    /// refused once: every later request goes without it
    refused: AtomicBool,
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
    /// `reasoning`: the effort of OPENROUTER_PROCESSOR_REASONING, if set
    pub fn new(base_url: &str, api_key: &str, timeout_ms: u64, reasoning: Option<&str>) -> Result<Self> {
        let timeout = Duration::from_millis(timeout_ms);
        // no total timeout on the client: it would also cap reading streamed
        // bodies (LLM SSE, TTS PCM); stalls are caught by the read timeout and
        // the agent's per-iteration budget, like openai-node in TS
        let client = Client::builder()
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            client,
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            timeout,
            reasoning: reasoning
                .map(|effort| Arc::new(Reasoning { effort: effort.to_string(), refused: AtomicBool::new(false) })),
        })
    }

    /// Streams chat completion content deltas over SSE. A reasoning effort
    /// OpenRouter refuses (a 4xx that names it, or one a request without it
    /// does not get) costs one retry without it and is not sent again
    pub async fn chat_stream(&self, model: &str, messages: &[ChatMessage]) -> Result<ChatStream> {
        let started = Instant::now();
        let mut reasoning = self.reasoning.as_deref().filter(|reasoning| !reasoning.refused.load(Ordering::Relaxed));
        let mut response = self.post_chat(model, messages, reasoning).await?;
        if let Some(asked) = reasoning.filter(|_| refuses_request(response.status())) {
            let status = response.status();
            let refusal = readable_error(&response.text().await.unwrap_or_default());
            response = self.post_chat(model, messages, None).await?;
            reasoning = None;
            let blamed = names_reasoning(&refusal) || response.status().is_success();
            if blamed && !asked.refused.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "OpenRouter refused OPENROUTER_PROCESSOR_REASONING={} for {model} ({status}: {refusal}); \
                     the dialog LLM is asked without it from now on",
                    asked.effort
                );
            }
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("chat completions failed: {status}: {}", readable_error(&body));
        }

        let timing = ChatTiming { started, response: started.elapsed(), first_event: None, first_content: None };
        Ok(ChatStream::new(
            response,
            StreamFormat::ChatCompletions,
            reasoning.map(|reasoning| reasoning.effort.clone()),
            timing,
        ))
    }

    async fn post_chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        reasoning: Option<&Reasoning>,
    ) -> Result<reqwest::Response> {
        let mut body = json!({
            "model": model,
            "stream": true,
            "messages": messages,
        });
        if let Some(reasoning) = reasoning {
            body["reasoning"] = json!({ "effort": reasoning.effort });
        }
        self.client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-Title", "alice-server")
            .json(&body)
            .send()
            .await
            .context("chat completions request failed")
    }

    /// Streams synthesized speech: raw signed 16-bit little-endian mono PCM, or MP3
    pub async fn speech_stream(
        &self,
        model: &str,
        voice: &str,
        instructions: &Option<String>,
        speed: f32,
        format: TtsFormat,
        text: &str,
    ) -> Result<impl futures_util::Stream<Item = Result<bytes::Bytes>>> {
        let mut body = json!({
            "model": model,
            "input": text,
            "voice": voice,
            "response_format": match format {
                TtsFormat::Pcm => "pcm",
                TtsFormat::Mp3 => "mp3",
            },
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
            bail!("speech synthesis failed: {status}: {}", readable_error(&body));
        }

        Ok(response.bytes_stream().map(|chunk| chunk.map_err(anyhow::Error::from)))
    }

    /// Transcribes an OGG/Opus utterance (converted to WAV by the caller)
    pub async fn transcribe(&self, model: &str, language: Option<&str>, wav: &[u8]) -> Result<String> {
        let audio_base64 = base64::engine::general_purpose::STANDARD.encode(wav);
        let mut body = json!({
            "model": model,
            "input_audio": {
                "data": audio_base64,
                "format": "wav",
            },
        });
        if let Some(language) = language {
            body["language"] = json!(language);
        }
        let response = self
            .client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-Title", "alice-server")
            // a plain JSON answer, so a total deadline is fine here
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .context("transcription request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("transcription failed: {status}: {}", readable_error(&body));
        }

        let parsed: TranscriptionResponse = response.json().await.context("invalid transcription response")?;
        Ok(parsed.text)
    }
}

/// A 4xx about the request itself, not about the account (401, 402, 403) or the load (408, 429)
fn refuses_request(status: StatusCode) -> bool {
    status.is_client_error() && !matches!(status.as_u16(), 401 | 402 | 403 | 408 | 429)
}

/// Whether an error is about reasoning; Gemini calls it thinking
fn names_reasoning(error: &str) -> bool {
    let error = error.to_lowercase();
    ["reasoning", "effort", "thinking"].iter().any(|word| error.contains(word))
}

/// An error body as one readable line: OpenRouter's message and the
/// provider's own error it wraps (`metadata.raw`, often JSON itself)
fn readable_error(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return one_line(body);
    };
    let Some(message) = value["error"]["message"].as_str() else {
        return one_line(&value.to_string());
    };
    let metadata = &value["error"]["metadata"];
    let raw = match &metadata["raw"] {
        Value::Null => return one_line(message),
        Value::String(raw) => serde_json::from_str::<Value>(raw).map_or_else(|_| raw.clone(), |raw| raw.to_string()),
        raw => raw.to_string(),
    };
    one_line(&format!("{message} ({}: {raw})", metadata["provider_name"].as_str().unwrap_or("provider")))
}

pub(crate) fn one_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(ERROR_BODY_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line,
    }
}

/// A streamed chat completion: its content deltas, and when they came
pub struct ChatStream {
    stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    /// raw bytes: a multi-byte character (Cyrillic) may be split between
    /// network chunks, so lines are decoded only once complete
    buffer: Vec<u8>,
    done: bool,
    format: StreamFormat,
    /// the reasoning effort the request asked for
    reasoning: Option<String>,
    timing: ChatTiming,
}

/// How the SSE events of a chat stream carry the answer
#[derive(Clone, Copy)]
pub enum StreamFormat {
    /// OpenAI's chat completions (OpenRouter): `choices[0].delta.content`, then `[DONE]`
    ChatCompletions,
    /// Anthropic's Messages API: the text deltas of content blocks, then `message_stop`
    Messages,
}

/// What one SSE event of a chat stream means
enum StreamEvent {
    Content(String),
    Failed(anyhow::Error),
    End,
    Other,
}

impl StreamFormat {
    fn event(self, value: &Value) -> StreamEvent {
        // both report a failure mid-stream as an event with an `error` member
        if let Some(error) = value.get("error") {
            return StreamEvent::Failed(anyhow::anyhow!("chat stream error: {error}"));
        }
        let content = match self {
            Self::ChatCompletions => value["choices"][0]["delta"]["content"].as_str(),
            Self::Messages => match value["type"].as_str() {
                // thinking deltas are not the answer; after a refusal the `fallback`
                // block's model goes on with the text
                Some("content_block_delta") if value["delta"]["type"].as_str() == Some("text_delta") => {
                    value["delta"]["text"].as_str()
                }
                // a refused or cut-off answer is partial: it must not pass for a whole one
                Some("message_delta") => {
                    return match value["delta"]["stop_reason"].as_str() {
                        Some("refusal" | "max_tokens" | "model_context_window_exceeded") => {
                            StreamEvent::Failed(anyhow::anyhow!("the answer stopped early: {}", value["delta"]))
                        }
                        _ => StreamEvent::Other,
                    };
                }
                Some("message_stop") => return StreamEvent::End,
                _ => None,
            },
        };
        match content {
            Some(content) if !content.is_empty() => StreamEvent::Content(content.to_string()),
            _ => StreamEvent::Other,
        }
    }
}

/// When a chat completion's parts came, since its request
#[derive(Clone, Copy)]
pub struct ChatTiming {
    pub started: Instant,
    /// the response headers (after a refused reasoning effort: of the retry)
    pub response: Duration,
    /// the first SSE event of any kind (reasoning, role, content)
    pub first_event: Option<Duration>,
    /// the first content token
    pub first_content: Option<Duration>,
}

impl ChatStream {
    /// The answer of a request that `timing` started
    pub fn new(response: reqwest::Response, format: StreamFormat, reasoning: Option<String>, timing: ChatTiming) -> Self {
        Self { stream: Box::pin(response.bytes_stream()), buffer: Vec::new(), done: false, format, reasoning, timing }
    }

    pub fn timing(&self) -> ChatTiming {
        self.timing
    }

    pub fn reasoning(&self) -> Option<&str> {
        self.reasoning.as_deref()
    }
}

impl futures_util::Stream for ChatStream {
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
            if let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let raw_line: Vec<u8> = self.buffer.drain(..=newline).collect();
                let line = String::from_utf8_lossy(&raw_line);
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data:").map(str::trim_start) {
                    if data == "[DONE]" {
                        self.done = true;
                        return Poll::Ready(None);
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(data) {
                        let elapsed = self.timing.started.elapsed();
                        self.timing.first_event.get_or_insert(elapsed);
                        match self.format.event(&value) {
                            StreamEvent::Content(content) => {
                                self.timing.first_content.get_or_insert(elapsed);
                                return Poll::Ready(Some(Ok(ChatDelta(content))));
                            }
                            StreamEvent::Failed(error) => {
                                self.done = true;
                                return Poll::Ready(Some(Err(error)));
                            }
                            StreamEvent::End => {
                                self.done = true;
                                return Poll::Ready(None);
                            }
                            StreamEvent::Other => {}
                        }
                    }
                }
                continue;
            }

            match self.stream.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
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
