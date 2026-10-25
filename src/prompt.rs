//! Handlebars prompt generation, reusing the templates of the TypeScript
//! version (prompt.handlebars / prompt-state.handlebars).

use std::sync::Arc;

use std::collections::BTreeMap;
use anyhow::{Context, Result};

use crate::agent::{FunctionDefinition, FunctionKind, Functions};

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
        let handlebars = handlebars::Handlebars::new();
        Ok(handlebars.render_template(&template, &data)?)
    }

    pub fn generate_state(&self, state: &BTreeMap<String, String>) -> Result<String> {
        let template = std::fs::read_to_string(&self.state_prompt_path)
            .with_context(|| format!("reading {}", self.state_prompt_path))?;
        let state_text = state
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut data = BTreeMap::new();
        data.insert("stateText".to_string(), state_text);
        let handlebars = handlebars::Handlebars::new();
        Ok(handlebars.render_template(&template, &data)?)
    }
}

/// The state shown to the LLM every turn (mirrors the system state server)
pub fn system_state() -> BTreeMap<String, String> {
    let mut state = BTreeMap::new();
    state.insert("assistant_name".to_string(), "Алиса".to_string());
    let now = chrono_like_now();
    state.insert("date_time".to_string(), now);
    state
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    // days since epoch -> civil date (Howard Hinnant's algorithm)
    let days = (seconds / 86_400) as i64;
    let time_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{day:02}-{month:02}-{year} {:02}:{:02}:{:02}",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}


/// Station control functions exposed to the LLM as directives
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
    add("smart_speaker_player_play", "Starts music, or continues if was paused", None);
    add("smart_speaker_player_pause", "Pause music if it is playing", None);
    add("smart_speaker_player_next_track", "Switch to next track", None);
    add("smart_speaker_player_previous_track", "Switch to previous track", None);
    add("smart_speaker_bluetooth", "Enable or disable Bluetooth on the smart speaker",
        Some(serde_json::json!({"type":"object","properties":{"enable":{"type":"boolean"}}})));
    add("smart_speaker_set_sound_volume", "Set volume on the smart speaker (1..10)",
        Some(serde_json::json!({"type":"object","properties":{"volume":{"type":"number"}}})));
    add("smart_speaker_set_sound_volume_louder", "Make the smart speaker volume louder", None);
    add("smart_speaker_set_sound_volume_quieter", "Make the smart speaker volume quieter", None);
    Arc::new(functions.into_iter().collect())
}
