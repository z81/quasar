use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Editor, Setting};
use crate::icons::Icon;

pub(super) fn is_key(key: &str) -> bool {
    matches!(
        key,
        "PROCESSOR_MCP_SERVER_URLS" | "PROCESSOR_MCP_SERVER_TOKENS"
    )
}

#[derive(Clone, Deserialize, PartialEq)]
pub(super) struct McpSettings {
    servers: Vec<Server>,
    baseline_servers: Vec<Server>,
    baseline_valid: bool,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Server {
    prefix: String,
    url: String,
    token_set: bool,
}

#[derive(Clone, PartialEq, Serialize)]
pub(super) struct ServerEdit {
    #[serde(skip)]
    id: usize,
    original_prefix: Option<String>,
    prefix: String,
    url: String,
    token: Option<String>,
}

#[derive(Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(super) enum Draft {
    Replace { servers: Vec<ServerEdit> },
    Reset,
}

impl Draft {
    pub(super) fn invalid(&self) -> bool {
        match self {
            Self::Reset => false,
            Self::Replace { servers } => servers.iter().any(|row| row.error(servers).is_some()),
        }
    }
}

impl ServerEdit {
    fn error(&self, rows: &[Self]) -> Option<&'static str> {
        let prefix = self.prefix.trim();
        if prefix.is_empty()
            || prefix.len() > 64
            || !prefix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        {
            return Some("Префикс: до 64 латинских букв, цифр, _ или -.");
        }
        if rows
            .iter()
            .filter(|row| row.prefix.trim() == prefix)
            .count()
            > 1
        {
            return Some("У каждого сервера должен быть свой префикс.");
        }
        let url = self.url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://"))
            || url.contains([',', '\n', '\r', '#'])
        {
            return Some("Укажите HTTP(S) адрес без #префикса и запятых.");
        }
        if self
            .token
            .as_ref()
            .is_some_and(|token| token.contains([',', '\n', '\r']))
        {
            return Some("В ключе не должно быть запятых и переносов строк.");
        }
        None
    }
}

fn rows(servers: &[Server]) -> Vec<ServerEdit> {
    servers
        .iter()
        .enumerate()
        .map(|(id, server)| ServerEdit {
            id,
            original_prefix: Some(server.prefix.clone()),
            prefix: server.prefix.clone(),
            url: server.url.clone(),
            token: None,
        })
        .collect()
}

fn update(mut editor: Editor, change: impl FnOnce(&mut Vec<ServerEdit>)) {
    let Some(settings) = editor
        .response
        .peek()
        .as_ref()
        .and_then(|response| response.mcp.clone())
    else {
        return;
    };
    let baseline = rows(&settings.servers);
    let mut servers = match editor.mcp.peek().as_ref() {
        Some(Draft::Replace { servers }) => servers.clone(),
        Some(Draft::Reset) => return,
        None => baseline.clone(),
    };
    change(&mut servers);
    editor
        .mcp
        .set((servers != baseline).then_some(Draft::Replace { servers }));
    editor.feedback.set(None);
}

#[component]
pub(super) fn McpEditor(
    settings: McpSettings,
    fields: Vec<Setting>,
    mut editor: Editor,
    disabled: bool,
) -> Element {
    let draft = (editor.mcp)();
    let reset = matches!(draft, Some(Draft::Reset));
    let changed = draft.is_some();
    let servers = match draft {
        Some(Draft::Replace { servers }) => servers,
        Some(Draft::Reset) => rows(&settings.baseline_servers),
        None => rows(&settings.servers),
    };
    let overridden = fields
        .iter()
        .any(|field| is_key(&field.key) && field.source == "override");
    let locked = disabled || reset;
    rsx! {
        section { class: "server-setting-row wide mcp-editor", aria_label: "MCP-серверы",
            div { class: "mcp-heading",
                div { class: "server-setting-heading", h3 { "MCP-серверы" } span { class: "mcp-count", "{servers.len()} / 32" }
                    if changed { span { class: if reset { "setting-source reset" } else { "setting-source override" }, if reset { "Сброс после сохранения" } else { "Не сохранено" } } }
                }
                button { class: "text-button", disabled: locked || servers.len() >= 32, onclick: move |_| update(editor, |rows| {
                    let id = rows.iter().map(|row| row.id).max().map_or(0, |id| id + 1);
                    rows.push(ServerEdit { id, original_prefix: None, prefix: String::new(), url: String::new(), token: None });
                }), Icon { name: "plus", size: 14 } "Добавить сервер" }
            }
            if reset {
                p { class: "mcp-hint", "После сохранения вернутся серверы и ключи из env. Сохраните сброс, чтобы продолжить редактирование." }
            }
            if servers.is_empty() {
                p { class: "mcp-empty", "Серверов пока нет. Добавьте MCP-сервер, чтобы Алиса могла использовать его инструменты." }
            }
            div { class: "mcp-rows",
                for row in servers.iter() {
                    {
                        let id = row.id;
                        let error = (!reset).then(|| row.error(&servers)).flatten();
                        let original = settings.servers.iter().find(|server| Some(&server.prefix) == row.original_prefix.as_ref());
                        let token_set = if reset { settings.baseline_servers.iter().find(|server| server.prefix == row.prefix).is_some_and(|server| server.token_set) }
                            else { original.is_some_and(|server| server.token_set) };
                        let token_value = row.token.clone().unwrap_or_default();
                        let token_changed = row.token.is_some();
                        let token_cleared = row.token.as_ref().is_some_and(|token| token.is_empty());
                        let token_label = if token_cleared { "Ключ будет удалён" } else if token_changed { "Новый ключ" } else if token_set { "Ключ сохранён" } else { "Без ключа" };
                        rsx! {
                            div { key: "{id}", class: "mcp-row",
                                div { class: "mcp-fields",
                                    label { class: "form-field server-setting-input mcp-prefix", r#for: "mcp-prefix-{id}", span { "Префикс" }
                                        input { id: "mcp-prefix-{id}", value: "{row.prefix}", placeholder: "home", disabled: locked, spellcheck: false, autocomplete: "off", maxlength: 64,
                                            oninput: move |event| update(editor, |rows| { if let Some(row) = rows.iter_mut().find(|row| row.id == id) { row.prefix = event.value(); } }) }
                                    }
                                    label { class: "form-field server-setting-input mcp-url", r#for: "mcp-url-{id}", span { "Адрес сервера" }
                                        input { id: "mcp-url-{id}", r#type: "url", value: "{row.url}", placeholder: "https://example.com/mcp", disabled: locked, spellcheck: false, autocomplete: "off",
                                            oninput: move |event| update(editor, |rows| { if let Some(row) = rows.iter_mut().find(|row| row.id == id) { row.url = event.value(); } }) }
                                    }
                                    label { class: "form-field server-setting-input mcp-token", r#for: "mcp-token-{id}",
                                        span { "Ключ доступа" small { class: if token_changed { "mcp-token-changed" } else { "" }, "{token_label}" } }
                                        input { id: "mcp-token-{id}", r#type: "password", value: token_value, placeholder: if token_set && !token_cleared { "Новый ключ для замены" } else { "Необязательно" }, disabled: locked, spellcheck: false, autocomplete: "new-password",
                                            oninput: move |event| update(editor, |rows| { if let Some(row) = rows.iter_mut().find(|row| row.id == id) { let value = event.value(); row.token = (!value.is_empty()).then_some(value); } }) }
                                    }
                                }
                                div { class: "mcp-row-footer",
                                    if let Some(error) = error { small { class: "failure", role: "status", "{error}" } }
                                    div { class: "mcp-row-actions",
                                        if token_changed { button { class: "text-button", disabled: locked, onclick: move |_| update(editor, |rows| { if let Some(row) = rows.iter_mut().find(|row| row.id == id) { row.token = None; } }), "Отменить замену ключа" } }
                                        else if token_set { button { class: "text-button", disabled: locked, onclick: move |_| update(editor, |rows| { if let Some(row) = rows.iter_mut().find(|row| row.id == id) { row.token = Some(String::new()); } }), "Убрать ключ" } }
                                        button { class: "text-button mcp-remove", disabled: locked, aria_label: "Удалить сервер {row.prefix}", onclick: move |_| update(editor, |rows| rows.retain(|row| row.id != id)), Icon { name: "trash", size: 13 } "Удалить" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "mcp-footer",
                div { class: "mcp-sources",
                    for field in fields.iter().filter(|field| is_key(&field.key)) {
                        span { class: if field.source == "override" { "setting-source override" } else { "setting-source" }, title: "{field.key}",
                            if field.secret { "Ключи: " } else { "Адреса: " }
                            if field.source == "override" { if field.env_present { "переопределяют env" } else { "из настроек" } }
                            else if field.source == "env" { "из env" } else { "по умолчанию" }
                        }
                    }
                }
                div { class: "server-setting-actions",
                    if changed { button { class: "text-button", disabled, onclick: move |_| { editor.mcp.set(None); editor.feedback.set(None); }, "Отменить" } }
                    if overridden && !reset { button { class: "text-button", disabled: disabled || !settings.baseline_valid, title: "Сбросить серверы и ключи к env или значениям по умолчанию", onclick: move |_| { editor.mcp.set(Some(Draft::Reset)); editor.feedback.set(None); }, Icon { name: "refresh", size: 13 } "Сбросить к env" } }
                    details { class: "server-setting-help",
                        summary { aria_label: "Как настроить MCP-сервер", "?" }
                        div { class: "server-setting-help-body",
                            p { "Префикс отличает инструменты сервера: например, home. В адрес вставьте полный HTTP(S) URL; добавлять #префикс не нужно." }
                            p { "Сохранённые ключи скрыты. Пустое поле сохраняет ключ, в том числе при смене адреса или префикса. Для удаления ключа нажмите «Убрать ключ»." }
                            p { "Удаление сервера убирает и его ключ. Изменения применяются общей кнопкой сохранения." }
                        }
                    }
                }
            }
            if !settings.baseline_valid { p { class: "mcp-hint failure", "В env некорректные настройки MCP. Сброс станет доступен после их исправления и перезапуска сервера." } }
        }
    }
}
