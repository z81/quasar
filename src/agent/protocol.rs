//! The JSON protocol the LLM answers in: function_calls (with args and
//! schedule), text, continue_dialog, can_cache.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A function invocation requested by the LLM; `schedule` delays the call
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_ms: Option<u64>,
}

/// A station directive to pass through to the speaker as-is
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectiveResult {
    pub name: String,
    pub arguments: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct StructuredResponse {
    pub can_cache: bool,
    pub function_calls: Vec<FunctionCall>,
    pub require_more_input: bool,
    pub text: String,
}

#[derive(Deserialize)]
struct RawFunctionCall {
    name: String,
    #[serde(default)]
    args: Option<Value>,
    #[serde(default)]
    schedule: Option<String>,
}

#[derive(Deserialize)]
struct RawResponse {
    #[serde(default)]
    can_cache: Option<bool>,
    #[serde(default)]
    continue_dialog: Option<bool>,
    #[serde(default)]
    function_calls: Option<Vec<RawFunctionCall>>,
    #[serde(default)]
    text: Option<String>,
}

pub fn parse_llm_response(raw: &str) -> anyhow::Result<StructuredResponse> {
    let raw: RawResponse = serde_json::from_str(strip_code_fence(raw))?;
    Ok(StructuredResponse {
        can_cache: raw.can_cache.unwrap_or(false),
        function_calls: raw
            .function_calls
            .unwrap_or_default()
            .into_iter()
            .map(|call| FunctionCall {
                name: call.name,
                arguments: call.args,
                schedule_ms: call.schedule.as_deref().map(parse_schedule),
            })
            .collect(),
        require_more_input: raw.continue_dialog.unwrap_or(false),
        text: raw.text.unwrap_or_default(),
    })
}

/// Some models wrap JSON into markdown fences despite the prompt
fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("```")
        .map(|rest| rest.trim_start_matches(|character: char| character != '{' && character != '['))
        .and_then(|rest| rest.split("```").next())
        .map(str::trim)
        .unwrap_or(trimmed)
}

/// Parses human schedules like "1h 20m", "5m", "30s"
pub fn parse_schedule(schedule: &str) -> u64 {
    let mut total_ms = 0u64;
    for part in schedule.split_whitespace() {
        let (digits, suffix) = part.split_at(part.find(|character: char| !character.is_ascii_digit()).unwrap_or(part.len()));
        let value: f64 = digits.parse().unwrap_or(0.0);
        let milliseconds = match suffix {
            "ms" => value,
            "s" => value * 1000.0,
            "m" => value * 60.0 * 1000.0,
            "h" => value * 60.0 * 60.0 * 1000.0,
            "d" => value * 24.0 * 60.0 * 60.0 * 1000.0,
            _ => {
                tracing::warn!("failed to parse schedule part from LLM: '{part}'");
                0.0
            }
        };
        total_ms += milliseconds as u64;
    }
    total_ms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_response() {
        let response = parse_llm_response(
            r#"{"function_calls": [{"name": "lights", "args": {"state": 1}, "schedule": "1h 20m"}], "text": "Готово", "continue_dialog": false, "can_cache": true}"#,
        )
        .unwrap();
        assert_eq!(response.text, "Готово");
        assert!(response.can_cache);
        assert!(!response.require_more_input);
        assert_eq!(response.function_calls.len(), 1);
        assert_eq!(response.function_calls[0].name, "lights");
        assert_eq!(response.function_calls[0].schedule_ms, Some((1.0 * 3600.0 + 20.0 * 60.0) as u64 * 1000));
    }

    #[test]
    fn parses_minimal_and_fenced_responses() {
        let minimal = parse_llm_response(r#"{"text": "Привет"}"#).unwrap();
        assert_eq!(minimal.text, "Привет");
        assert!(minimal.function_calls.is_empty());

        let fenced = parse_llm_response("```\n{\"text\": \"Ок\"}\n```").unwrap();
        assert_eq!(fenced.text, "Ок");
    }

    #[test]
    fn parses_schedules() {
        assert_eq!(parse_schedule("5m"), 300_000);
        assert_eq!(parse_schedule("30s"), 30_000);
        assert_eq!(parse_schedule("2h 5m 10s"), 7_510_000);
        assert_eq!(parse_schedule("1500ms"), 1_500);
    }
}
