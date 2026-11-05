mod field;
mod mcp;
mod secret;

use std::collections::BTreeMap;

use dioxus::prelude::*;
use serde::Deserialize;
use serde_json::json;

use crate::{api, icons::Icon, state::ControlState};

const ENDPOINT: &str = "/settings/server";
type Changes = BTreeMap<String, Option<String>>;

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Setting {
    key: String,
    label: String,
    description: String,
    group: String,
    kind: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    default_value: String,
    value: Option<String>,
    is_set: bool,
    secret: bool,
    source: String,
    env_present: bool,
    env_name: Option<String>,
    baseline_value: Option<String>,
    baseline_is_set: bool,
    #[serde(default, alias = "minimum")]
    min: Option<f64>,
    #[serde(default, alias = "maximum")]
    max: Option<f64>,
}

impl Setting {
    fn fallback(&self) -> &'static str {
        if self.env_present {
            "env"
        } else {
            "значению по умолчанию"
        }
    }

    fn invalid(&self, changes: &Changes) -> bool {
        let Some(Some(value)) = changes.get(&self.key) else {
            return false;
        };
        if value.is_empty() {
            return matches!(self.kind.as_str(), "integer" | "number")
                && !self.default_value.is_empty();
        }
        if self.kind == "integer" && !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return true;
        }
        if matches!(self.kind.as_str(), "integer" | "number") {
            return value
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .is_none_or(|number| {
                    self.min.is_some_and(|min| number < min)
                        || self.max.is_some_and(|max| number > max)
                });
        }
        false
    }
}

#[derive(Clone, Deserialize, PartialEq)]
struct SettingsResponse {
    revision: u64,
    persistent: bool,
    ready: bool,
    #[serde(default)]
    issues: Vec<String>,
    fields: Vec<Setting>,
    bootstrap: Bootstrap,
    #[serde(default)]
    mcp: Option<mcp::McpSettings>,
}

#[derive(Clone, Deserialize, PartialEq)]
struct Bootstrap {
    port: u16,
    api_port: u16,
    env_only: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
struct Editor {
    response: Signal<Option<SettingsResponse>>,
    changes: Signal<Changes>,
    loading: Signal<bool>,
    saving: Signal<bool>,
    error: Signal<Option<String>>,
    feedback: Signal<Option<String>>,
    conflict: Signal<bool>,
    mcp: Signal<Option<mcp::Draft>>,
}

impl Editor {
    fn change(mut self, field: &Setting, value: String) {
        self.feedback.set(None);
        let unchanged = if field.secret {
            value.is_empty()
        } else {
            field.value.as_deref().unwrap_or_default() == value
        };
        if unchanged {
            self.changes.write().remove(&field.key);
        } else {
            self.changes.write().insert(field.key.clone(), Some(value));
        }
    }

    fn reset(mut self, field: &Setting) {
        self.feedback.set(None);
        if field.source == "override" {
            self.changes.write().insert(field.key.clone(), None);
        } else {
            self.changes.write().remove(&field.key);
        }
    }
}

async fn load(state: ControlState, mut editor: Editor) {
    if !state.allowed() || *editor.loading.peek() {
        return;
    }
    editor.loading.set(true);
    let result = api::get::<SettingsResponse>(state, ENDPOINT).await;
    if editor.response.try_read().is_err() {
        return;
    }
    match result {
        Ok(response) => {
            editor
                .changes
                .write()
                .retain(|key, _| response.fields.iter().any(|field| &field.key == key));
            editor.response.set(Some(response));
            editor.error.set(None);
            if *editor.conflict.peek() {
                editor.feedback.set(Some("Исходные значения обновлены. Ваши изменения сохранены в форме — проверьте их перед сохранением.".into()));
            }
            editor.conflict.set(false);
        }
        Err(error) => editor.error.set(Some(error)),
    }
    editor.loading.set(false);
}

fn save(state: ControlState, mut editor: Editor) {
    if !state.allowed()
        || *editor.saving.peek()
        || *editor.loading.peek()
        || *editor.conflict.peek()
    {
        return;
    }
    let Some(response) = editor.response.peek().clone() else {
        return;
    };
    let changes = editor.changes.peek().clone();
    let mcp = editor.mcp.peek().clone();
    if (changes.is_empty() && mcp.is_none())
        || !response.persistent
        || response.fields.iter().any(|field| field.invalid(&changes))
        || mcp.as_ref().is_some_and(mcp::Draft::invalid)
    {
        return;
    }
    editor.saving.set(true);
    editor.error.set(None);
    editor.feedback.set(None);
    dioxus::core::spawn_forever(async move {
        let mut body = json!({"revision":response.revision,"changes":changes});
        if let Some(mcp) = mcp {
            body["mcp"] = json!(mcp);
        }
        let result = match api::mutate(state, "PATCH", ENDPOINT, Some(&body)).await {
            Ok(response) => response.json::<SettingsResponse>().await.map_err(|_| {
                "Сервер вернул непонятный ответ. Обновите настройки перед повтором.".to_string()
            }),
            Err(error) => Err(error),
        };
        if editor.response.try_read().is_err() {
            return;
        }
        match result {
            Ok(response) => {
                let message = if response.ready {
                    "Настройки сохранены и применены к новым запросам."
                } else {
                    "Настройки сохранены. Для ответов Алисы заполните оставшиеся параметры."
                };
                editor.response.set(Some(response));
                editor.changes.set(Changes::new());
                editor.mcp.set(None);
                editor.feedback.set(Some(message.into()));
                editor.conflict.set(false);
            }
            Err(error) => {
                editor.conflict.set(error.starts_with("HTTP 409:"));
                editor.error.set(Some(error));
            }
        }
        editor.saving.set(false);
    });
}

fn group_label(group: &str) -> &str {
    match group {
        "processor" | "ai" => "Ответы Алисы",
        "stt" => "Распознавание речи",
        "tts" => "Голос",
        "integrations" => "Интеграции",
        "general" => "Общие",
        "advanced" => "Дополнительно",
        other => other,
    }
}

#[component]
pub fn ServerSettings(state: ControlState) -> Element {
    let mut editor = Editor {
        response: use_signal(|| None),
        changes: use_signal(Changes::new),
        loading: use_signal(|| false),
        saving: use_signal(|| false),
        error: use_signal(|| None),
        feedback: use_signal(|| None),
        conflict: use_signal(|| false),
        mcp: use_signal(|| None),
    };
    let mut group = use_signal(String::new);
    use_future(move || load(state, editor));
    let response = (editor.response)();
    let changes = (editor.changes)();
    let busy = (editor.loading)() || (editor.saving)();
    let fields = response
        .as_ref()
        .map(|response| response.fields.clone())
        .unwrap_or_default();
    let bootstrap_keys = response
        .as_ref()
        .map(|response| response.bootstrap.env_only.join(", "))
        .unwrap_or_default();
    let mut groups = Vec::new();
    for field in &fields {
        if !groups.contains(&field.group) {
            groups.push(field.group.clone());
        }
    }
    let selected = if groups.contains(&group()) {
        group()
    } else {
        groups.first().cloned().unwrap_or_default()
    };
    let mcp_draft = (editor.mcp)();
    let change_count = changes.len() + usize::from(mcp_draft.is_some());
    let invalid = fields.iter().any(|field| field.invalid(&changes))
        || mcp_draft.as_ref().is_some_and(mcp::Draft::invalid);
    let editable = response
        .as_ref()
        .is_some_and(|response| response.persistent)
        && !busy;
    rsx! {
        div { class: "server-settings",
            section { class: "server-settings-intro",
                p { "Общие для всех колонок · применяются к новым запросам" }
                button { class: "text-button", disabled: busy, onclick: move |_| { spawn(load(state, editor)); }, Icon { name: "refresh", size: 15 } "Обновить значения" }
            }
            if let Some(error) = (editor.error)() {
                div { class: "notice error server-settings-notice", role: "alert", Icon { name: "alert", size: 18 } div { p { "{error}" }
                    if (editor.conflict)() { p { "Настройки изменились в другой вкладке. Обновите исходные значения и проверьте свои изменения перед сохранением." }
                        button { class: "text-button", disabled: busy, onclick: move |_| { spawn(load(state, editor)); }, "Обновить исходные значения" }
                    }
                } }
            }
            if let Some(message) = (editor.feedback)() {
                p { class: "form-feedback success server-settings-feedback", role: "status", "{message}" }
            }
            if let Some(response) = response.as_ref() {
                if !response.persistent {
                    div { class: "notice error", role: "alert", Icon { name: "alert", size: 18 } span { "Нет подключения к постоянному хранилищу. Сохранение настроек недоступно." } }
                }
                if !response.ready && !response.issues.is_empty() {
                    div { class: "server-settings-readiness", role: "status", Icon { name: "alert", size: 18 }
                        div { strong { "Нужно завершить настройку" } ul { for issue in &response.issues { li { "{issue}" } } } }
                    }
                }
                div { class: "server-settings-layout",
                    nav { class: "server-settings-groups", aria_label: "Группы настроек сервера",
                        for name in groups {
                            {
                                let selected_group = name.clone();
                                let count = fields.iter().filter(|field| field.group == name && changes.contains_key(&field.key)).count()
                                    + usize::from(name == "integrations" && mcp_draft.is_some());
                                rsx! { button { class: if selected == name { "server-settings-group active" } else { "server-settings-group" }, aria_current: if selected == name { "page" } else { "false" }, onclick: move |_| group.set(selected_group.clone()),
                                    span { "{group_label(&name)}" } if count > 0 { span { class: "setting-change-count", "{count}" } }
                                } }
                            }
                        }
                    }
                    div { class: "card server-settings-fields",
                        div { class: "server-settings-group-heading", h2 { "{group_label(&selected)}" } }
                        div { class: "server-settings-grid",
                            if selected == "integrations" {
                                if let Some(mcp) = response.mcp.as_ref() {
                                    mcp::McpEditor { settings: mcp.clone(), fields: fields.clone(), editor, state, revision: response.revision, disabled: !editable }
                                }
                            }
                            for setting in fields.iter().filter(|field| field.group == selected && (response.mcp.is_none() || !mcp::is_key(&field.key))) {
                                field::SettingField { key: "{setting.key}", setting: setting.clone(), editor, state, revision: response.revision, disabled: !editable }
                            }
                        }
                    }
                }
                div { class: "server-settings-savebar",
                    div { strong { if change_count == 0 { "Все изменения сохранены" } else { "Несохранённых изменений: {change_count}" } }
                        if invalid { small { class: "failure", "Проверьте значения в отмеченных полях." } }
                        else { small { "Значения в env сохраняются. Переопределение можно сбросить отдельно у каждой строки." } }
                    }
                    div { class: "server-settings-save-actions",
                        button { class: "text-button", disabled: busy || change_count == 0, onclick: move |_| { editor.changes.set(Changes::new()); editor.mcp.set(None); editor.feedback.set(None); }, "Отменить изменения" }
                        button { class: "button primary", disabled: !editable || change_count == 0 || invalid || (editor.conflict)(), onclick: move |_| save(state, editor),
                            if (editor.saving)() { span { class: "spinner" } "Применяем…" } else { Icon { name: "check", size: 16 } "Сохранить и применить" }
                        }
                    }
                }
                details { class: "server-settings-bootstrap", summary { "Параметры запуска остаются в env" }
                    p { "Порт Quasar: {response.bootstrap.port}. Порт пульта и API: {response.bootstrap.api_port}." }
                    p { "Эти параметры задаются при запуске: {bootstrap_keys}." }
                }
            } else if (editor.loading)() {
                div { class: "card server-settings-loading", role: "status", span { class: "spinner" } "Загружаем настройки…" }
            }
        }
    }
}
