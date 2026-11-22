//! OpenAI Codex for the dialog (PROCESSOR_PROVIDER=codex): the ChatGPT Codex
//! backend or the OpenAI Responses API, streamed over SSE.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use reqwest::Client;
use serde_json::{Value, json};

use crate::openrouter::{ChatMessage, ChatStream, ChatTiming, StreamFormat, one_line};

/// The Codex backend expects the client that originated the request
const ORIGINATOR: &str = "codex_cli_rs";

#[derive(Clone)]
pub struct Codex {
    client: Client,
    base_url: String,
    token: String,
    account_id: Option<String>,
    /// CODEX_PROCESSOR_EFFORT, sent as `reasoning.effort`
    effort: Option<String>,
}

impl Codex {
    /// `token` is a Codex access token or an OpenAI API key. A Codex access
    /// token carries the account id in its JWT; `CODEX_ACCOUNT_ID` overrides it.
    pub fn new(
        base_url: &str,
        token: &str,
        account_id: Option<&str>,
        timeout_ms: u64,
        effort: Option<&str>,
    ) -> Result<Self> {
        let timeout = Duration::from_millis(timeout_ms);
        let client = Client::builder()
            .connect_timeout(timeout)
            .read_timeout(timeout)
            .build()
            .context("failed to build HTTP client")?;
        let token = token.trim().to_string();
        let account_id = account_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| account_from_jwt(&token));
        if account_id.is_none() && base_url.contains("chatgpt.com") {
            tracing::warn!(
                "CODEX_ACCOUNT_ID is not set and the token has no chatgpt_account_id; \
                 the Codex backend may reject the request"
            );
        }
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            account_id,
            effort: effort.map(str::to_string),
        })
    }

    /// Streams the answer's text deltas over SSE
    pub async fn chat_stream(&self, model: &str, messages: &[ChatMessage]) -> Result<ChatStream> {
        let started = Instant::now();
        let mut request = self
            .client
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(&self.token)
            .header("originator", ORIGINATOR)
            .header("X-Title", "alice-server")
            .json(&request_body(model, messages, self.effort.as_deref()));
        if let Some(account_id) = &self.account_id {
            request = request.header("ChatGPT-Account-ID", account_id);
        }
        let response = request.send().await.context("responses request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("responses failed: {status}: {}", readable_error(&body));
        }

        let timing = ChatTiming {
            started,
            response: started.elapsed(),
            first_event: None,
            first_content: None,
        };
        Ok(ChatStream::new(
            response,
            StreamFormat::Responses,
            self.effort.clone(),
            timing,
        ))
    }
}

/// The agent's messages as a Responses request. Codex takes the system
/// messages as `instructions` and the conversation as typed input items.
fn request_body(model: &str, messages: &[ChatMessage], effort: Option<&str>) -> Value {
    let instructions = messages
        .iter()
        .filter(|message| message.role == "system" && !message.content.trim().is_empty())
        .map(|message| message.content.trim())
        .collect::<Vec<_>>()
        .join("\n\n");
    let input: Vec<Value> = messages
        .iter()
        .filter(|message| message.role != "system")
        .map(|message| {
            let (role, kind) = if message.role == "assistant" {
                ("assistant", "output_text")
            } else {
                ("user", "input_text")
            };
            json!({
                "type": "message",
                "role": role,
                "content": [{ "type": kind, "text": message.content }],
            })
        })
        .collect();

    let mut body = json!({
        "model": model,
        "stream": true,
        "input": input,
        "tools": [],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
        "text": { "format": { "type": "text" } },
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(effort) = effort {
        body["reasoning"] = json!({ "effort": effort });
    }
    body
}

/// The `chatgpt_account_id` claim of a Codex access token, when it has one
fn account_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .ok()?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .or_else(|| value.get("chatgpt_account_id").and_then(Value::as_str))
        .map(str::to_string)
}

/// An error body as one line: the Codex backend's `detail` or OpenAI's error
fn readable_error(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return one_line(body);
    };
    if let Some(detail) = value["detail"].as_str() {
        return one_line(detail);
    }
    if let Some(message) = value["error"]["message"].as_str() {
        return one_line(message);
    }
    one_line(&value.to_string())
}
