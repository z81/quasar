//! Structured MCP editing while retaining the existing environment/storage format.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{config, Document, Result, SettingsError, Values};

pub(super) const URLS: &str = "PROCESSOR_MCP_SERVER_URLS";
pub(super) const TOKENS: &str = "PROCESSOR_MCP_SERVER_TOKENS";

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Patch {
    Replace { servers: Vec<ServerPatch> },
    Reset,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerPatch {
    /// Identifies the existing server whose hidden credential should be retained.
    /// This is deliberately separate from the editable prefix, allowing renames.
    pub original_prefix: Option<String>,
    pub prefix: String,
    pub url: String,
    /// None preserves the credential; an empty string explicitly removes it.
    pub token: Option<String>,
}

#[derive(Serialize)]
pub struct ServerView {
    prefix: String,
    url: String,
    token_set: bool,
}

#[derive(Serialize)]
pub struct View {
    servers: Vec<ServerView>,
    baseline_servers: Vec<ServerView>,
    baseline_valid: bool,
}

pub(super) fn view(values: &Values, baseline: &Values) -> Result<View> {
    let servers = config::mcp_servers(values).map_err(|_| SettingsError::Unavailable)?;
    let baseline = config::mcp_servers(baseline);
    let baseline_valid = baseline.is_ok();
    Ok(View {
        servers: masked(servers),
        baseline_servers: masked(baseline.unwrap_or_default()),
        baseline_valid,
    })
}

fn masked(servers: Vec<config::McpServerConfig>) -> Vec<ServerView> {
    servers
        .into_iter()
        .map(|server| ServerView {
            prefix: server.prefix,
            url: server.url,
            token_set: server.token.is_some(),
        })
        .collect()
}

pub(super) fn apply(patch: Patch, values: &Values, document: &mut Document) -> Result<()> {
    let Patch::Replace { servers } = patch else {
        document.overrides.remove(URLS);
        document.overrides.remove(TOKENS);
        return Ok(());
    };
    if servers.len() > 32 {
        return invalid("MCP: допускается не больше 32 серверов");
    }
    let current = config::mcp_servers(values).map_err(|_| SettingsError::Unavailable)?;
    let current_tokens =
        config::mcp_tokens(values.get(TOKENS).map(String::as_str).unwrap_or_default())
            .map_err(|_| SettingsError::Unavailable)?;
    // Unattached legacy credentials are retained; editing visible servers must
    // not silently erase configuration that the editor did not display.
    let mut tokens = current_tokens.clone();
    for server in &current {
        tokens.remove(&server.prefix);
    }
    let mut identities = BTreeSet::new();
    let mut urls = Vec::with_capacity(servers.len());
    for server in servers {
        let old = match server.original_prefix {
            Some(ref prefix) => {
                if !identities.insert(prefix.clone()) {
                    return invalid("MCP: исходный сервер не должен повторяться");
                }
                Some(
                    current
                        .iter()
                        .find(|old| &old.prefix == prefix)
                        .ok_or_else(|| {
                            SettingsError::Invalid(
                                "MCP: исходный сервер не найден; обновите настройки".into(),
                            )
                        })?,
                )
            }
            None => None,
        };
        let prefix = server.prefix.trim();
        let url = server.url.trim();
        let token = match server.token {
            Some(token) => {
                let token = token.trim();
                (!token.is_empty()).then(|| token.to_owned())
            }
            None => old.and_then(|server| server.token.clone()),
        };
        // Delimiters belong only to the legacy transport representation. Reject
        // them before serialization so one input cannot create extra entries.
        if prefix.contains([',', '\n', '\r', ':', '#'])
            || url.contains([',', '\n', '\r', '#'])
            || token
                .as_deref()
                .is_some_and(|token| token.contains([',', '\n', '\r']))
        {
            return invalid("MCP: недопустимый разделитель в префиксе, адресе или ключе");
        }
        tokens.remove(prefix);
        if let Some(token) = token {
            tokens.insert(prefix.to_owned(), token);
        }
        urls.push(format!("{url}#{prefix}"));
    }
    let url_value = urls.join("\n");
    let token_value = tokens
        .iter()
        .map(|(prefix, token)| format!("{prefix}:{token}"))
        .collect::<Vec<_>>()
        .join("\n");
    if url_value.len() > 16_384 || token_value.len() > 16_384 {
        return invalid("MCP: список серверов или ключей слишком длинный");
    }
    let proposed = Values::from([
        (URLS.to_owned(), url_value.clone()),
        (TOKENS.to_owned(), token_value.clone()),
    ]);
    // The same parser validates flat env values and structured editor requests.
    config::mcp_servers(&proposed).map_err(|error| SettingsError::Invalid(error.to_string()))?;
    let current_urls = current
        .iter()
        .map(|server| format!("{}#{}", server.url, server.prefix))
        .collect::<Vec<_>>();
    if urls != current_urls {
        document.overrides.insert(URLS.to_owned(), url_value);
    }
    if tokens != current_tokens {
        document.overrides.insert(TOKENS.to_owned(), token_value);
    }
    Ok(())
}

fn invalid<T>(message: &str) -> Result<T> {
    Err(SettingsError::Invalid(message.to_owned()))
}
