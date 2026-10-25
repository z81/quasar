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
            client: reqwest::Client::new(),
        }
    }

    /// Calls the server that owns the given function name
    pub async fn call_by_name(&self, name: &str, arguments: &Option<Value>) -> anyhow::Result<()> {
        let owner = self
            .owners
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no remote server owns function '{name}'"))?;
        let arguments = arguments.clone().unwrap_or(Value::Null);
        self.call_function(&owner, "uniproxy", name, &arguments).await
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
                                kind: FunctionKind::Remote { has_response: false },
                            },
                        );
                    }
                }
                _ => tracing::warn!("function server {url} unavailable"),
            }
        }
        functions
    }

    /// Calls a remote function (fire-and-forget in the protocol)
    pub async fn call_function(&self, url: &str, session_id: &str, name: &str, arguments: &Value) -> anyhow::Result<()> {
        let response = self
            .client
            .patch(url)
            .json(&json!({ "arguments": arguments, "name": name, "sessionId": session_id }))
            .send()
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("function server {url} answered {}", response.status());
        }
        Ok(())
    }

    /// Collects state entries from all state servers
    pub async fn state(&self, session_id: &str) -> HashMap<String, (String, String)> {
        let mut state = HashMap::new();
        for url in &self.state_urls {
            let fetch = async {
                let response = self
                    .client
                    .post(url)
                    .json(&json!({ "sessionId": session_id }))
                    .send()
                    .await?;
                let entries = response.json::<HashMap<String, StateEntry>>().await?;
                Ok::<_, anyhow::Error>(entries)
            };
            match fetch.await {
                Ok(entries) => {
                    for (key, entry) in entries {
                        state.insert(key, (entry.description, entry.value.to_string()));
                    }
                }
                Err(error) => tracing::warn!("state server {url} failed: {error}"),
            }
        }
        state
    }
}
