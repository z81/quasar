//! Anthropic's Messages API for the dialog (PROCESSOR_PROVIDER=anthropic):
//! the conversation the agent keeps for OpenRouter, streamed over SSE.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::{Value, json};

use crate::openrouter::{ChatMessage, ChatStream, ChatTiming, StreamFormat, one_line};

/// The API version the requests are written for
const API_VERSION: &str = "2026-10-21";

/// Room for the model's thinking as well as the answer; the agent's
/// deadline for an answer ends a runaway one long before
const MAX_TOKENS: u32 = 64_000;

/// A refused request goes on with the model Anthropic recommends for the
/// refusal's category (`fallbacks: "default"`)
const FALLBACKS_BETA: &str = "server-side-fallback-2026-07-01";

#[derive(Clone)]
pub struct Anthropic {
    client: Client,
    base_url: String,
    api_key: String,
    /// ANTHROPIC_PROCESSOR_EFFORT, sent as `output_config.effort`
    effort: Option<String>,
}

impl Anthropic {
    pub fn new(base_url: &str, api_key: &str, timeout_ms: u64, effort: Option<&str>) -> Result<Self> {
        let timeout = Duration::from_millis(timeout_ms);
        // as for OpenRouter: a total timeout would also cap reading the stream
        let client = Client::builder()
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            effort: effort.map(str::to_string),
        })
    }

    /// Streams the answer's text deltas over SSE
    pub async fn chat_stream(&self, model: &str, messages: &[ChatMessage]) -> Result<ChatStream> {
        let started = Instant::now();
        let mut request = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&request_body(model, messages, self.effort.as_deref()));
        if takes_fallbacks(model) {
            request = request.header("anthropic-beta", FALLBACKS_BETA);
        }
        let response = request.send().await.context("messages request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("messages failed: {status}: {}", readable_error(&body));
        }

        let timing = ChatTiming { started, response: started.elapsed(), first_event: None, first_content: None };
        Ok(ChatStream::new(response, StreamFormat::Messages, self.effort.clone(), timing))
    }
}

/// The agent's messages as a Messages request: its system messages (the
/// prompt, then the turn's state) become `system`, the prompt cached, as it
/// is the same for every turn of a dialog
fn request_body(model: &str, messages: &[ChatMessage], effort: Option<&str>) -> Value {
    let mut system: Vec<Value> = messages
        .iter()
        // a template that failed to render leaves an empty one, which the API refuses
        .filter(|message| message.role == "system" && !message.content.trim().is_empty())
        .map(|message| json!({ "type": "text", "text": message.content }))
        .collect();
    if let Some(prompt) = system.first_mut() {
        prompt["cache_control"] = json!({ "type": "ephemeral" });
    }
    let conversation: Vec<Value> = messages
        .iter()
        .filter(|message| message.role != "system")
        .map(|message| json!({ "role": message.role, "content": message.content }))
        .collect();

    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "messages": conversation,
    });
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if let Some(effort) = effort {
        body["output_config"] = json!({ "effort": effort });
    }
    if takes_fallbacks(model) {
        body["fallbacks"] = json!("default");
    }
    body
}

/// The models that take `fallbacks: "default"`: Claude Haiku 5.5 has no
/// fallback, and other models are not documented to take this form
fn takes_fallbacks(model: &str) -> bool {
    ["claude-opus-5", "claude-sonnet-5-5", "claude-fable-5"].iter().any(|family| model.starts_with(family))
}

/// An error body as one line: the error's type and message, and the
/// request's id, which Anthropic's support asks for
fn readable_error(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return one_line(body);
    };
    let Some(message) = value["error"]["message"].as_str() else {
        return one_line(&value.to_string());
    };
    let kind = value["error"]["type"].as_str().unwrap_or("error");
    match value["request_id"].as_str() {
        Some(request_id) => one_line(&format!("{kind}: {message} (request {request_id})")),
        None => one_line(&format!("{kind}: {message}")),
    }
}
