//! Remote function and state servers (the HTTP protocols of v3rt3p/processor):
//! functions are called by the LLM, state entries are shown to it each turn.

use std::collections::HashMap;

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{FunctionDefinition, FunctionKind};

#[derive(Deserialize)]
struct RemoteFunctionCatalogEntry {
    #[serde(default)]
    arguments: HashMap<String, Value>,
    description: String,
    /// Extension of the protocol (TS ignores the key): the LLM gets the
    /// call's response body and continues the turn with it, as with MCP tools
    #[serde(default, rename = "hasResponse")]
    has_response: bool,
    /// Extension of the protocol too: a station directive function (like
    /// smart_speaker_player_pause) the answer carries whenever the function
    /// is called right away, so the station acts with the answer itself
    #[serde(default)]
    directive: Option<String>,
}

pub struct RemoteServers {
    function_urls: Vec<String>,
    /// which server owns each function name (built by `functions`)
    owners: std::sync::RwLock<HashMap<String, String>>,
    state_urls: Vec<String>,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct StateEntry {
    description: String,
    value: Value,
}

pub(crate) struct PlayerCover {
    pub content_type: String,
    pub body: Vec<u8>,
}

pub(crate) fn is_cover_key(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl RemoteServers {
    pub fn new(function_urls: Vec<String>, state_urls: Vec<String>) -> Self {
        Self {
            function_urls,
            owners: std::sync::RwLock::new(HashMap::new()),
            state_urls,
            // a hung function/state server must not freeze startup or a dialog turn
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("HTTP client for remote servers"),
        }
    }

    /// Calls the server that owns the given function name; `session_id` and
    /// `metadata` (speaker biometry, server type) describe the dialog, as in
    /// TS. Returns the response body (the result of a has-response function)
    pub async fn call_by_name(
        &self,
        name: &str,
        arguments: &Option<Value>,
        session_id: &str,
        metadata: &Value,
    ) -> anyhow::Result<String> {
        let owner = self
            .owners
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no remote server owns function '{name}'"))?;
        // like TS: a call without arguments sends an empty object, not null
        let arguments = arguments.clone().filter(|arguments| !arguments.is_null()).unwrap_or_else(|| json!({}));
        self.call_function(&owner, session_id, metadata, name, &arguments).await
    }

    /// Whether a function server offers `name`
    pub fn owns(&self, name: &str) -> bool {
        self.owners.read().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(name)
    }

    /// Cover keys originate in bridge events. Only configured state servers
    /// receive requests, and each bridge checks its own station and key cache.
    pub(crate) async fn player_cover(&self, duid: &str, key: &str) -> Option<PlayerCover> {
        if !is_cover_key(key) {
            return None;
        }
        let mut requests = self
            .state_urls
            .iter()
            .map(|url| self.fetch_player_cover(url, duid, key))
            .collect::<futures_util::stream::FuturesUnordered<_>>();
        while let Some(result) = requests.next().await {
            if result.is_some() {
                return result;
            }
        }
        None
    }

    async fn fetch_player_cover(&self, state_url: &str, duid: &str, key: &str) -> Option<PlayerCover> {
        const MAX_BYTES: usize = 2 * 1024 * 1024;
        let base = reqwest::Url::parse(state_url).ok()?;
        let mut url = base.join(&format!("cover/{key}")).ok()?;
        url.set_query(base.query());
        url.query_pairs_mut().append_pair("duid", duid);
        let mut response = self.client.get(url).send().await.ok()?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > MAX_BYTES as u64)
        {
            return None;
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)?
            .to_str()
            .ok()?
            .split(';')
            .next()?
            .trim()
            .to_owned();
        if !matches!(content_type.as_str(), "image/jpeg" | "image/png" | "image/webp") {
            return None;
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if body.len() + chunk.len() > MAX_BYTES {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        (!body.is_empty()).then_some(PlayerCover { content_type, body })
    }

    /// Fetches all function catalogs concurrently, skipping failed servers
    pub async fn functions(&self) -> HashMap<String, FunctionDefinition> {
        let mut functions = HashMap::new();
        for url in &self.function_urls {
            match self.client.get(url).send().await {
                Ok(response) if response.status().is_success() => {
                    let Ok(catalog) = response
                        .json::<HashMap<String, RemoteFunctionCatalogEntry>>()
                        .await
                    else {
                        tracing::warn!("invalid function catalog from {url}");
                        continue;
                    };
                    for (name, entry) in catalog {
                        self.owners
                            .write()
                            .unwrap()
                            .insert(name.clone(), url.clone());
                        functions.insert(
                            name,
                            FunctionDefinition {
                                arguments_json_schema: Some(json!({
                                    "type": "object",
                                    "properties": entry.arguments,
                                })),
                                description: entry.description,
                                kind: FunctionKind::Remote {
                                    has_response: entry.has_response,
                                    directive: entry.directive,
                                },
                            },
                        );
                    }
                }
                _ => tracing::warn!("function server {url} unavailable"),
            }
        }
        functions
    }

    /// Calls a remote function; returns the response body
    pub async fn call_function(
        &self,
        url: &str,
        session_id: &str,
        metadata: &Value,
        name: &str,
        arguments: &Value,
    ) -> anyhow::Result<String> {
        let response = self
            .client
            .patch(url)
            .json(&json!({ "arguments": arguments, "metadata": metadata, "name": name, "sessionId": session_id }))
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("function server {url} answered {}", response.status());
        }
        Ok(response.text().await?)
    }

    /// Collects the per-turn state entries of all state servers (queried
    /// concurrently; on duplicate keys the first server wins, as in TS)
    pub async fn state(&self, session_id: &str, metadata: &Value) -> HashMap<String, (String, String)> {
        let body = &json!({ "metadata": metadata, "sessionId": session_id });
        let fetches = self.state_urls.iter().map(|url| async move {
            let fetch = async {
                let response = self.client.post(url).json(body).send().await?;
                let entries = response.json::<HashMap<String, StateEntry>>().await?;
                Ok::<_, anyhow::Error>(entries)
            };
            (url, fetch.await)
        });

        let mut state = HashMap::new();
        for (url, result) in futures_util::future::join_all(fetches).await {
            match result {
                Ok(entries) => {
                    for (key, entry) in entries {
                        let value = match entry.value {
                            // the protocol's values are strings; keep them unquoted
                            Value::String(value) => value,
                            value => value.to_string(),
                        };
                        state.entry(key).or_insert((entry.description, value));
                    }
                }
                Err(error) => tracing::warn!("state server {url} failed: {error}"),
            }
        }
        state
    }
}
