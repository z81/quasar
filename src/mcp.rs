//! MCP client (Model Context Protocol, Streamable HTTP transport) for the
//! servers of PROCESSOR_MCP_SERVER_URLS (`<url>#<prefix>`, as in TS): their
//! tools become functions `<prefix>_<tool>` whose results go back to the LLM
//! (TS hasResponse), with the tools' descriptions and JSON schemas.
//! PROCESSOR_MCP_SERVER_TOKENS (`<prefix>:<token>`) authorizes a server with
//! `Authorization: Bearer` (Home Assistant: a long-lived access token).
//!
//! Each server is connected in the background (`initialize`, the
//! `notifications/initialized` notification, `tools/list` page by page), with
//! retries and backoff while it is unreachable, and its tools are listed
//! again every 5 minutes and after a new session. An unreachable server
//! offers no functions. Requests carry the session the server assigned
//! (`Mcp-Session-Id`) and the protocol version; an expired session (HTTP 404)
//! is replaced by a new one and the request repeated once. Answers come as
//! a JSON body or as an SSE stream that ends with the response.
//!
//! A server with a `GetLiveContext` tool (Home Assistant: the entities
//! exposed to Assist with their areas and states; `homeassistant__GetLiveContext`
//! since HA 2026.9, with one more prefix when HA serves several LLM APIs)
//! also feeds the turn's state as `<prefix>_live_context`
//! (PROCESSOR_MCP_LIVE_CONTEXT, on by default), fetched as early as possible
//! within a short budget.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::agent::{FunctionDefinition, FunctionKind};
use crate::config::McpServerConfig;

/// The protocol revision asked for; the server answers with the one it speaks
const PROTOCOL_VERSION: &str = "2025-06-18";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A request, and a whole tool call (a new session included)
const TIMEOUT: Duration = Duration::from_secs(10);

/// The tool list is fetched again this often
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// Between failed connection attempts: doubling from the first to the last
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_LAST: Duration = Duration::from_secs(60);

/// How long the live context may take from the start of its fetch (for a
/// voice input, the start of the speech): a later one is skipped for the turn
const LIVE_CONTEXT_BUDGET: Duration = Duration::from_millis(1500);

/// Home Assistant's tool that lists the exposed entities with their areas and states
const LIVE_CONTEXT_TOOL: &str = "GetLiveContext";

/// Longer results are cut, with a note
const MAX_RESULT_CHARS: usize = 8000;

/// Pages of one tool list at most
const MAX_PAGES: usize = 50;

/// The MCP servers and the tools they offer now
pub struct Mcp {
    servers: Vec<Arc<Server>>,
    live_context: bool,
}

/// One state entry: key, (description, value)
pub type StateEntry = (String, (String, String));

impl Mcp {
    pub fn new(servers: &[McpServerConfig], live_context: bool) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("HTTP client for MCP servers");
        let servers = servers.iter().map(|config| Arc::new(Server::new(config, http.clone()))).collect();
        Arc::new(Self { servers, live_context })
    }

    /// Connects every server in the background: a hanging one never delays startup
    pub fn start(&self) {
        for server in &self.servers {
            tracing::info!("MCP server {}: connecting in the background", server.name());
            tokio::spawn(server.clone().maintain());
        }
    }

    /// The functions of the tools the servers list now
    pub fn functions(&self) -> Vec<(String, FunctionDefinition)> {
        self.servers.iter().flat_map(|server| server.functions()).collect()
    }

    /// Whether `function` names a tool of a server (`<prefix>_<tool>`)
    pub fn routes(&self, function: &str) -> bool {
        self.route(function).is_some()
    }

    /// The server whose prefix `function` carries (the longest one), and the tool
    fn route<'a>(&self, function: &'a str) -> Option<(&Arc<Server>, &'a str)> {
        self.servers
            .iter()
            .filter_map(|server| Some((server, function.strip_prefix(server.prefix.as_str())?.strip_prefix('_')?)))
            .filter(|(_, tool)| !tool.is_empty())
            .max_by_key(|(server, _)| server.prefix.len())
    }

    /// Calls a tool for the LLM: its result, or `error: …` — never a failure
    pub async fn call(&self, function: &str, arguments: &Option<Value>) -> String {
        match self.route(function) {
            Some((server, tool)) => {
                let arguments = arguments.clone().filter(|arguments| !arguments.is_null()).unwrap_or_else(|| json!({}));
                server.call_for_llm(tool, arguments).await
            }
            None => format!("error: no MCP server offers {function}"),
        }
    }

    /// Starts fetching the live context of the servers that offer one, so it
    /// is there by the time the turn's state is assembled: at the start of a
    /// voice input (while the user speaks) or of a turn
    pub fn live_context(&self) -> Option<LiveContext> {
        if !self.live_context {
            return None;
        }
        let servers: Vec<_> = self
            .servers
            .iter()
            .filter_map(|server| Some((server.clone(), server.live_context_tool()?)))
            .collect();
        if servers.is_empty() {
            return None;
        }
        Some(LiveContext(tokio::spawn(async move {
            let fetches = servers.iter().map(|(server, tool)| server.live_context(tool));
            futures_util::future::join_all(fetches).await.into_iter().flatten().collect()
        })))
    }
}

/// The live context being fetched for a turn; dropping it stops the fetch
pub struct LiveContext(JoinHandle<Vec<StateEntry>>);

impl LiveContext {
    /// The entries that came within the budget
    pub async fn entries(mut self) -> Vec<StateEntry> {
        (&mut self.0).await.unwrap_or_default()
    }
}

impl Drop for LiveContext {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A listed tool
#[derive(Clone, PartialEq)]
struct Tool {
    name: String,
    description: String,
    input_schema: Value,
}

/// The session a server assigned at `initialize`
#[derive(Clone)]
struct Session {
    /// `Mcp-Session-Id`, if the server keeps sessions
    id: Option<String>,
    protocol_version: String,
}

/// The session is over: start a new one (HTTP 404 to a request with a session id)
#[derive(Debug, thiserror::Error)]
#[error("the session expired")]
struct Expired;

struct Server {
    url: String,
    prefix: String,
    token: Option<String>,
    http: reqwest::Client,
    session: Mutex<Option<Session>>,
    tools: RwLock<Vec<Tool>>,
    /// wakes the background task to list the tools again
    refresh: Notify,
    next_id: AtomicU64,
}

impl Server {
    fn new(config: &McpServerConfig, http: reqwest::Client) -> Self {
        Self {
            url: config.url.clone(),
            prefix: config.prefix.clone(),
            token: config.token.clone(),
            http,
            session: Mutex::new(None),
            tools: RwLock::new(Vec::new()),
            refresh: Notify::new(),
            next_id: AtomicU64::new(1),
        }
    }

    /// For the logs: the prefix and the URL without its query
    fn name(&self) -> String {
        format!("'{}' ({})", self.prefix, self.url.split('?').next().unwrap_or_default())
    }

    /// Keeps the tool list current: lists it, retrying with backoff while
    /// the server is unreachable (it offers no tools then), and again every
    /// REFRESH_INTERVAL or after a new session
    async fn maintain(self: Arc<Self>) {
        let mut retry = RETRY_FIRST;
        let mut failing = false;
        loop {
            match self.list_tools().await {
                Ok(tools) => {
                    self.set_tools(tools, failing);
                    retry = RETRY_FIRST;
                    failing = false;
                    tokio::select! {
                        () = tokio::time::sleep(REFRESH_INTERVAL) => {}
                        () = self.refresh.notified() => {}
                    }
                }
                Err(error) => {
                    if failing {
                        tracing::debug!("MCP server {}: still unavailable ({error:#})", self.name());
                    } else {
                        tracing::warn!(
                            "MCP server {}: unavailable ({error:#}); it offers no functions until it answers, retrying",
                            self.name()
                        );
                    }
                    failing = true;
                    self.tools.write().unwrap_or_else(PoisonError::into_inner).clear();
                    *self.session.lock().await = None;
                    tokio::time::sleep(retry).await;
                    retry = (retry * 2).min(RETRY_LAST);
                }
            }
        }
    }

    fn set_tools(&self, tools: Vec<Tool>, reconnected: bool) {
        let mut current = self.tools.write().unwrap_or_else(PoisonError::into_inner);
        if *current != tools || reconnected {
            let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
            tracing::info!("MCP server {}: {} tools: {}", self.name(), tools.len(), names.join(", "));
        }
        *current = tools;
    }

    /// The tool that tells the live context, plain or namespaced
    fn live_context_tool(&self) -> Option<String> {
        let namespaced = format!("__{LIVE_CONTEXT_TOOL}");
        let tools = self.tools.read().unwrap_or_else(PoisonError::into_inner);
        tools.iter().map(|tool| &tool.name).find(|name| *name == LIVE_CONTEXT_TOOL || name.ends_with(&namespaced)).cloned()
    }

    fn functions(&self) -> Vec<(String, FunctionDefinition)> {
        self.tools
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|tool| {
                let definition = FunctionDefinition {
                    description: tool.description.clone(),
                    kind: FunctionKind::Remote { has_response: true, directive: None },
                    arguments_json_schema: Some(tool.input_schema.clone()),
                };
                (format!("{}_{}", self.prefix, tool.name), definition)
            })
            .collect()
    }

    /// All tools, page by page
    async fn list_tools(&self) -> Result<Vec<Tool>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.request("tools/list", params).await?;
            for tool in page["tools"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let Some(name) = tool["name"].as_str() else {
                    continue;
                };
                tools.push(Tool {
                    name: name.to_string(),
                    description: tool["description"].as_str().or_else(|| tool["title"].as_str()).unwrap_or(name).to_string(),
                    input_schema: tool.get("inputSchema").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                });
            }
            cursor = page["nextCursor"].as_str().filter(|cursor| !cursor.is_empty()).map(str::to_string);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        bail!("the tool list has more than {MAX_PAGES} pages")
    }

    /// A tool call for the LLM: its result, `error: …` otherwise; logged
    async fn call_for_llm(&self, tool: &str, arguments: Value) -> String {
        let started = Instant::now();
        // the call may wait for a new session first: TIMEOUT bounds the whole of it
        let call = self.request("tools/call", json!({ "name": tool, "arguments": arguments }));
        let outcome = tokio::time::timeout(TIMEOUT, call)
            .await
            .unwrap_or_else(|_| Err(anyhow!("no answer to tools/call within {TIMEOUT:?}")));
        let elapsed = started.elapsed().as_millis();
        match outcome {
            Ok(result) => {
                let (text, is_error) = result_text(&result);
                let text = limited(text);
                if is_error {
                    tracing::warn!("MCP {}_{tool}: error result in {elapsed} ms: {}", self.prefix, cut(&text, 300));
                    format!("error: {text}")
                } else {
                    tracing::info!("MCP {}_{tool}: ok in {elapsed} ms ({} characters)", self.prefix, text.chars().count());
                    text
                }
            }
            Err(error) => {
                tracing::warn!("MCP {}_{tool}: failed in {elapsed} ms: {error:#}", self.prefix);
                format!("error: {error:#}")
            }
        }
    }

    /// The live context as a state entry, if it comes within the budget
    async fn live_context(&self, tool: &str) -> Option<StateEntry> {
        let started = Instant::now();
        let call = self.request("tools/call", json!({ "name": tool, "arguments": {} }));
        let skipped = match tokio::time::timeout(LIVE_CONTEXT_BUDGET, call).await {
            Ok(Ok(result)) => match result_text(&result) {
                (text, false) => {
                    // Home Assistant wraps the listing in {"result": "…"}, unwrapped before the cut
                    let text = serde_json::from_str::<Value>(&text)
                        .ok()
                        .and_then(|value| value["result"].as_str().map(str::to_string))
                        .unwrap_or(text);
                    let text = limited(text);
                    let elapsed = started.elapsed().as_millis();
                    tracing::info!("MCP {}: live context in {elapsed} ms ({} characters)", self.prefix, text.chars().count());
                    let description = format!(
                        "smart home of MCP server '{0}': the devices you can control with the {0}_ functions, with their \
                         names, areas and current states. Answer questions about them from this list without calling \
                         functions; only a device missing from a cut list needs {0}_{tool}. Give the {0}_ functions \
                         device and area names exactly as written here",
                        self.prefix
                    );
                    return Some((format!("{}_live_context", self.prefix), (description, text)));
                }
                (text, true) => format!("an error result: {}", cut(&text, 200)),
            },
            Ok(Err(error)) => format!("{error:#}"),
            Err(_) => format!("no answer within {LIVE_CONTEXT_BUDGET:?}"),
        };
        tracing::info!("MCP {}: live context skipped for this turn ({skipped})", self.prefix);
        None
    }

    /// A request in the current session; an expired session is replaced and
    /// the request repeated once
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let session = self.session(None).await?;
        match self.post(Some(&session), method, params.clone()).await {
            Err(error) if error.is::<Expired>() => {
                tracing::info!("MCP server {}: the session expired, starting a new one", self.name());
                let session = self.session(Some(&session)).await?;
                // a server that lost the session may have new tools too
                self.refresh.notify_one();
                Ok(self.post(Some(&session), method, params).await?.0)
            }
            outcome => Ok(outcome?.0),
        }
    }

    /// The current session; a new one when there is none yet or it is the
    /// `expired` one (another request may have replaced that meanwhile)
    async fn session(&self, expired: Option<&Session>) -> Result<Session> {
        let mut session = self.session.lock().await;
        if let Some(current) = session.as_ref().filter(|current| expired.is_none_or(|expired| expired.id != current.id)) {
            return Ok(current.clone());
        }
        *session = None;
        let fresh = self.initialize().await?;
        *session = Some(fresh.clone());
        Ok(fresh)
    }

    async fn initialize(&self) -> Result<Session> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "alice-server", "version": env!("CARGO_PKG_VERSION") },
        });
        let (result, id) = self.post(None, "initialize", params).await?;
        let protocol_version = result["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION).to_string();
        let session = Session { id, protocol_version };
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        tokio::time::timeout(TIMEOUT, self.send(Some(&session), &initialized))
            .await
            .map_err(|_| anyhow!("no answer to notifications/initialized within {TIMEOUT:?}"))??;
        Ok(session)
    }

    /// A JSON-RPC request: its result, and the session id the server assigned
    async fn post(&self, session: Option<&Session>, method: &str, params: Value) -> Result<(Value, Option<String>)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let exchange = async {
            let response = self.send(session, &body).await?;
            let session_id = response.headers().get("mcp-session-id").and_then(|value| value.to_str().ok()).map(str::to_string);
            let message = read_response(response, id).await.with_context(|| method.to_string())?;
            anyhow::Ok((message, session_id))
        };
        let (message, session_id) = tokio::time::timeout(TIMEOUT, exchange)
            .await
            .map_err(|_| anyhow!("no answer to {method} within {TIMEOUT:?}"))??;
        if let Some(error) = message.get("error") {
            bail!("{method} failed: {} {}", error["code"], error["message"].as_str().unwrap_or_default());
        }
        Ok((message["result"].clone(), session_id))
    }

    async fn send(&self, session: Option<&Session>, body: &Value) -> Result<reqwest::Response> {
        let mut request = self.http.post(&self.url).header("Accept", "application/json, text/event-stream").json(body);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(session) = session {
            request = request.header("MCP-Protocol-Version", &session.protocol_version);
            if let Some(id) = &session.id {
                request = request.header("Mcp-Session-Id", id);
            }
        }
        let response = request.send().await?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND && session.is_some_and(|session| session.id.is_some()) {
            return Err(Expired.into());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("HTTP {status}: {}", cut(&body, 300));
        }
        Ok(response)
    }
}

/// The JSON-RPC response to request `id`: the body itself, or the event of
/// an SSE stream that carries it (the server's own notifications and
/// requests before it are skipped)
async fn read_response(response: reqwest::Response, id: u64) -> Result<Value> {
    let sse = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    if !sse {
        let message: Value = response.json().await.context("invalid JSON response")?;
        ensure!(answers(&message, id), "the response does not answer the request");
        return Ok(message);
    }
    let mut stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut data = String::new();
    loop {
        let chunk = stream.next().await;
        let ended = chunk.is_none();
        match chunk {
            Some(chunk) => buffer.extend_from_slice(&chunk.context("reading the SSE stream")?),
            // a last event without its blank line still counts
            None => buffer.extend_from_slice(b"\n\n"),
        }
        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if let Some(message) = serde_json::from_str(&data).ok().filter(|message| answers(message, id)) {
                    return Ok(message);
                }
                data.clear();
            } else if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
        if ended {
            bail!("the SSE stream ended without the response");
        }
    }
}

/// Whether `message` answers request `id` (one message per request: the
/// protocol has no batches since 2025-06-18)
fn answers(message: &Value, id: u64) -> bool {
    message["id"].as_u64() == Some(id) && (message.get("result").is_some() || message.get("error").is_some())
}

/// A tool's result for the LLM: the text contents joined, the other ones
/// named; and whether it is an error
fn result_text(result: &Value) -> (String, bool) {
    let mut parts = Vec::new();
    for item in result["content"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let kind = item["type"].as_str().unwrap_or("unknown");
        parts.push(match kind {
            "text" => item["text"].as_str().unwrap_or_default().to_string(),
            "image" | "audio" => format!("[{kind} {}]", item["mimeType"].as_str().unwrap_or("of unknown type")),
            "resource" => match item["resource"]["text"].as_str() {
                Some(text) => text.to_string(),
                None => format!("[resource {}]", item["resource"]["uri"].as_str().unwrap_or_default()),
            },
            "resource_link" => format!("[resource link {}]", item["uri"].as_str().unwrap_or_default()),
            other => format!("[{other} content]"),
        });
    }
    if parts.is_empty() {
        if let Some(structured) = result.get("structuredContent") {
            parts.push(structured.to_string());
        }
    }
    (parts.join("\n"), result["isError"].as_bool().unwrap_or(false))
}

/// A huge result cut to MAX_RESULT_CHARS, saying so
fn limited(text: String) -> String {
    match text.char_indices().nth(MAX_RESULT_CHARS) {
        Some((end, _)) => format!("{}… (cut here: {} more characters)", &text[..end], text[end..].chars().count()),
        None => text,
    }
}

/// At most `limit` characters on one line, for a log
fn cut(text: &str, limit: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line,
    }
}
