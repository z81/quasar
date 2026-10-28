//! Remote function and state servers (the HTTP protocols of v3rt3p/processor):
//! functions are called by the LLM, state entries are shown to it each turn.

use std::collections::HashMap;

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
    /// `metadata` (device state, speaker biometry) describe the dialog, as in
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
