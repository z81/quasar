//! Server configuration from environment variables (same names as the
//! TypeScript version), loaded from `.env.local` then `.env`.

use std::env;

#[derive(Clone)]
pub struct Config {
    pub allow_updates: bool,
    pub api_port: u16,
    pub port: u16,
    pub postgres_url: String,
    pub updates_config_path: Option<String>,
    pub glagol_jwt_key: Vec<u8>,
    pub openrouter_api_key: String,
    pub openrouter_base_url: String,
    pub openrouter_timeout_ms: u64,
    pub stt_model: String,
    pub stt_language: Option<String>,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_instructions: Option<String>,
    pub tts_pcm_sample_rate: u32,
    pub tts_speed: f32,
    pub processor_model: String,
    pub processor_prompt_path: String,
    pub processor_state_prompt_path: String,
    pub processor_cache_size: u64,
}

fn var_or(name: &str, default: &str) -> String {
    env::var(name).ok().filter(|value| !value.is_empty()).unwrap_or_else(|| default.to_string())
}

fn opt_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            allow_updates: var_or("ALLOW_UPDATES", "false").trim().eq_ignore_ascii_case("true"),
            port: var_or("PORT", "31115").parse().expect("PORT must be a number"),
            postgres_url: var_or("POSTGRES_URL", "postgres://quasar:quasar@localhost/quasar"),
            updates_config_path: opt_var("UPDATES_CONFIG_PATH"),
            api_port: var_or("API_PORT", "31116").parse().expect("API_PORT must be a number"),
            // like TS: a random key per start unless one is configured
            glagol_jwt_key: opt_var("GLAGOL_JWT_KEY")
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string())
                .into_bytes(),
            openrouter_api_key: opt_var("OPENROUTER_API_KEY")
                .expect("OPENROUTER_API_KEY is required: STT, TTS and the dialog processor all go through OpenRouter"),
            openrouter_base_url: var_or("OPENROUTER_BASE_URL", "https://openrouter.ai/api/v1"),
            openrouter_timeout_ms: var_or("OPENROUTER_TIMEOUT_MS", "10000").parse().expect("OPENROUTER_TIMEOUT_MS must be a number"),
            stt_model: var_or("OPENROUTER_STT_MODEL", "openai/whisper-large-v3"),
            // an explicitly empty value lets the model detect the language, as in TS
            stt_language: match env::var("OPENROUTER_STT_LANGUAGE") {
                Ok(value) if value.is_empty() => None,
                Ok(value) => Some(value),
                Err(_) => Some("ru".to_string()),
            },
            tts_model: var_or("OPENROUTER_TTS_MODEL", "google/gemini-3.8-flash-lite-tts"),
            tts_voice: var_or("OPENROUTER_TTS_VOICE", "Kore"),
            tts_instructions: opt_var("OPENROUTER_TTS_INSTRUCTIONS"),
            tts_pcm_sample_rate: var_or("OPENROUTER_TTS_PCM_SAMPLE_RATE", "24000").parse().expect("OPENROUTER_TTS_PCM_SAMPLE_RATE must be a number"),
            tts_speed: var_or("OPENROUTER_TTS_SPEED", "1").parse().expect("OPENROUTER_TTS_SPEED must be a number"),
            processor_model: var_or("OPENROUTER_PROCESSOR_MODEL", "google/gemini-3.8-flash"),
            processor_prompt_path: var_or("PROCESSOR_PROMPT_TEMPLATE_PATH", "prompt.handlebars"),
            // TS name first; the earlier Rust-only name keeps working
            processor_state_prompt_path: opt_var("PROCESSOR_PROMPT_STATE_TEMPLATE_PATH")
                .or_else(|| opt_var("PROCESSOR_STATE_TEMPLATE_PATH"))
                .unwrap_or_else(|| "prompt-state.handlebars".to_string()),
            processor_cache_size: match var_or("PROCESSOR_CACHE_SIZE", "1000").parse() {
                Ok(0) | Err(_) => panic!("PROCESSOR_CACHE_SIZE must be a positive number"),
                Ok(size) => size,
            },
        }
    }
}
