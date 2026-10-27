//! Handlebars prompt generation, reusing the templates of the TypeScript
//! version (prompt.handlebars / prompt-state.handlebars).

use std::sync::Arc;

use std::collections::BTreeMap;
use anyhow::{Context, Result};

use crate::agent::{FunctionDefinition, FunctionKind, Functions};
use crate::time_zone::Zone;

/// The state shown to the LLM: name -> (what it is, its value), shown as
/// `name (description): value` lines like TS
pub type State = BTreeMap<String, (String, String)>;

pub struct PromptGenerator {
    prompt_path: String,
    state_prompt_path: String,
}

impl PromptGenerator {
    pub fn new(prompt_path: &str, state_prompt_path: &str) -> Self {
        Self {
            prompt_path: prompt_path.to_string(),
            state_prompt_path: state_prompt_path.to_string(),
        }
    }

    pub fn generate(&self, functions: &Functions) -> Result<String> {
        let mut functions_text = String::new();
        for (name, definition) in functions.iter() {
            let kind = match definition.kind {
                FunctionKind::Directive => "station directive",
                FunctionKind::Remote { has_response: true } => "has response",
                FunctionKind::Remote { has_response: false } => "no response",
            };
            let schema = definition
                .arguments_json_schema
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?
                .unwrap_or_else(|| "null".to_string());
            functions_text.push_str(&format!(
                "{} ({}; {kind}; JSON schema: {schema})\n",
                name, definition.description
            ));
        }

        let template = std::fs::read_to_string(&self.prompt_path)
            .with_context(|| format!("reading {}", self.prompt_path))?;
        let mut data = BTreeMap::new();
        data.insert("functionsText".to_string(), functions_text);
        Ok(unescaped_handlebars().render_template(&template, &data)?)
    }

    pub fn generate_state(&self, state: &State) -> Result<String> {
        let template = std::fs::read_to_string(&self.state_prompt_path)
            .with_context(|| format!("reading {}", self.state_prompt_path))?;
        let state_text = state
            .iter()
            .map(|(key, (description, value))| match description.as_str() {
                "" => format!("{key}: {value}"),
                description => format!("{key} ({description}): {value}"),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut data = BTreeMap::new();
        data.insert("stateText".to_string(), state_text);
        Ok(unescaped_handlebars().render_template(&template, &data)?)
    }

    /// Fails early when a template is missing, like TS does at startup
    pub fn check_templates(&self) -> Result<()> {
        for path in [&self.prompt_path, &self.state_prompt_path] {
            std::fs::metadata(path).with_context(|| format!("prompt template {path} is not readable"))?;
        }
        Ok(())
    }
}

/// Prompts are plain text for the LLM, so HTML escaping (the Handlebars
/// default) would turn JSON schemas into `&quot;` soup; TS uses noEscape too
fn unescaped_handlebars() -> handlebars::Handlebars<'static> {
    let mut handlebars = handlebars::Handlebars::new();
    handlebars.register_escape_fn(handlebars::no_escape);
    handlebars
}

/// The state shown to the LLM every turn (mirrors the TS system state
/// server); the time is the station's local time in `zone` — the LLM answers
/// "который час" from it
pub fn system_state(zone: &Zone) -> State {
    let mut state = State::new();
    state.insert("assistant_name".to_string(), ("name of assistant (YOU)".to_string(), "Алиса".to_string()));
    state.insert(
        "date_time".to_string(),
        (
            "current date and time where the user is, in DD-MM-YYYY HH:MM:SS format, then the weekday, the time zone and its UTC offset".to_string(),
            zone.now(),
        ),
    );
    state
}


/// Station control functions exposed to the LLM as directives; music comes
/// from a music service's own functions (docker/spotify)
pub fn builtin_directive_functions() -> Functions {
    let mut functions: std::collections::HashMap<String, FunctionDefinition> = std::collections::HashMap::new();
    let mut add = |name: &str, description: &str, schema: Option<serde_json::Value>| {
        functions.insert(
            name.to_string(),
            FunctionDefinition {
                description: description.to_string(),
                kind: FunctionKind::Directive,
                arguments_json_schema: schema,
            },
        );
    };
    add("smart_speaker_player_pause", "Pause whatever the smart speaker plays", None);
    add("smart_speaker_bluetooth", "Enable or disable Bluetooth on the smart speaker",
        Some(serde_json::json!({"type":"object","properties":{"enable":{"type":"boolean"}},"required":["enable"]})));
    add("smart_speaker_set_sound_volume", "Set volume on the smart speaker",
        Some(serde_json::json!({"type":"object","properties":{"volume":{"type":"integer","minimum":1,"maximum":10}},"required":["volume"]})));
    add("smart_speaker_set_sound_volume_louder", "Atomically make the smart speaker volume louder", None);
    add("smart_speaker_set_sound_volume_quieter", "Atomically make the smart speaker volume quieter", None);
    add("smart_speaker_play_iot_sound", "Play IoT power on/power off sound on the smart speaker",
        Some(serde_json::json!({"type":"object","properties":{
            "isPowerOn":{"type":"boolean","description":"true if \"on\" sound, false if \"off\" sound"}},
            "required":["isPowerOn"]})));
    Arc::new(functions.into_iter().collect())
}
