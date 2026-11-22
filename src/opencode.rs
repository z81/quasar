//! OpenCode Console (PROCESSOR_PROVIDER=opencode): the OpenAI-compatible
//! Chat Completions gateway of an OpenCode Go subscription.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::{Value, json};

use crate::openrouter::{ChatMessage, ChatStream, ChatTiming, StreamFormat, one_line};

/// OpenCode Go rejects clients without their own user agent
const USER_AGENT: &str = "alice-server/1.0";

#[derive(Clone)]
pub struct OpenCode {
    client: Client,
    base_url: String,
    api_key: String,
    /// OPENCODE_PROCESSOR_REASONING, sent as `reasoning_effort`
    reasoning: Option<String>,
}

impl OpenCode {
    pub fn new(
        base_url: &str,
        api_key: &str,
        timeout_ms: u64,
        reasoning: Option<&str>,
    ) -> Result<Self> {
        let timeout = Duration::from_millis(timeout_ms);
        let client = Client::builder()
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .user_agent(USER_AGENT)
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            reasoning: reasoning.map(str::to_string),
        })
    }

    /// Streams chat completion content deltas over SSE. `session_id` is the
    /// dialog's stable ID; OpenCode uses it for routing and prompt caching.
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        session_id: &str,
    ) -> Result<ChatStream> {
        let started = Instant::now();
        let mut request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("X-Title", "alice-server")
            .header("x-opencode-session", opencode_session(session_id))
            .json(&request_body(model, messages, self.reasoning.as_deref()));
        if !self.api_key.is_empty() {
            request = request.bearer_auth(&self.api_key);
        }
        let response = request
            .send()
            .await
            .context("chat completions request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!(
                "chat completions failed: {status}: {}",
                readable_error(&body)
            );
        }

        let timing = ChatTiming {
            started,
            response: started.elapsed(),
            first_event: None,
            first_content: None,
        };
        Ok(ChatStream::new(
            response,
            StreamFormat::ChatCompletions,
            self.reasoning.clone(),
            timing,
        ))
    }
}

fn request_body(model: &str, messages: &[ChatMessage], reasoning: Option<&str>) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": messages,
        // The dialog protocol requires JSON even when the user asks for plain text.
        "response_format": { "type": "json_object" },
    });
    if let Some(reasoning) = reasoning {
        body["reasoning_effort"] = json!(reasoning);
    }
    body
}

/// OpenCode Go expects a session ID of the form `ses_...`
fn opencode_session(session_id: &str) -> String {
    if session_id.starts_with("ses_") {
        session_id.to_string()
    } else {
        format!("ses_{}", session_id.replace('-', ""))
    }
}

/// An error body as one line: OpenCode's `error.message`
fn readable_error(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return one_line(body);
    };
    if let Some(message) = value["error"]["message"].as_str() {
        return one_line(message);
    }
    one_line(&value.to_string())
}
