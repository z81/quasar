use super::{update, ControlState, Editor};
use crate::server_settings::secret::{RevealTarget, SecretInput};
use dioxus::prelude::*;

#[component]
pub(super) fn McpToken(
    id: usize,
    original_prefix: Option<String>,
    token: Option<String>,
    token_set: bool,
    state: ControlState,
    editor: Editor,
    revision: u64,
    disabled: bool,
) -> Element {
    let changed = token.is_some();
    let label = if let Some(value) = token.as_ref() {
        if value.is_empty() {
            "Ключ будет удалён"
        } else {
            "Новый ключ"
        }
    } else if token_set {
        "Ключ сохранён"
    } else {
        "Без ключа"
    };
    rsx! {
        div { class: "form-field server-setting-input mcp-token",
            label { r#for: "mcp-token-{id}", "Ключ доступа" small { class: if changed { "mcp-token-changed" } else { "" }, "{label}" } }
            SecretInput { id: "mcp-token-{id}", label: "Ключ MCP", target: original_prefix.map(|prefix| RevealTarget::Mcp { prefix }), draft: token, is_set: token_set, state, editor, revision, disabled,
                onchange: move |value: String| update(editor, |rows| {
                    if let Some(row) = rows.iter_mut().find(|row| row.id == id) { row.token = (!value.is_empty()).then_some(value); }
                })
            }
        }
    }
}
