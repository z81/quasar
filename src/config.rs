//! Server configuration from environment variables (same names as the
//! TypeScript version), loaded from `.env.local` then `.env`.

use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub allow_updates: bool,
    pub api_port: u16,
    pub port: u16,
    pub postgres_url: String,
    pub updates_config_path: Option<String>,
    pub openrouter_api_key: String,
    pub openrouter_base_url: String,
    pub openrouter_timeout_ms: u64,
    pub stt_model: String,
    pub stt_language: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_instructions: Option<String>,
    pub tts_pcm_sample_rate: u32,
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
            allow_updates: var_or("ALLOW_UPDATES", "false").eq_ignore_ascii_case("true"),
            port: var_or("PORT", "31115").parse().expect("PORT must be a number"),
            postgres_url: var_or("POSTGRES_URL", "postgres://quasar:quasar@localhost/quasar"),
            updates_config_path: opt_var("UPDATES_CONFIG_PATH"),
            api_port: var_or("API_PORT", "31116").parse().expect("API_PORT must be a number"),
            openrouter_api_key: env::var("OPENROUTER_API_KEY")
                .expect("OPENROUTER_API_KEY is required: STT, TTS and the dialog processor all go through OpenRouter"),
            openrouter_base_url: var_or("OPENROUTER_BASE_URL", "https://openrouter.ai/api/v1"),
            openrouter_timeout_ms: var_or("OPENROUTER_TIMEOUT_MS", "10000").parse().expect("OPENROUTER_TIMEOUT_MS must be a number"),
            stt_model: var_or("OPENROUTER_STT_MODEL", "openai/whisper-large-v3"),
            stt_language: var_or("OPENROUTER_STT_LANGUAGE", "ru"),
            tts_model: var_or("OPENROUTER_TTS_MODEL", "google/gemini-3.8-flash-lite-tts"),
            tts_voice: var_or("OPENROUTER_TTS_VOICE", "Kore"),
            tts_instructions: opt_var("OPENROUTER_TTS_INSTRUCTIONS"),
            tts_pcm_sample_rate: var_or("OPENROUTER_TTS_PCM_SAMPLE_RATE", "24000").parse().expect("OPENROUTER_TTS_PCM_SAMPLE_RATE must be a number"),
            processor_model: var_or("OPENROUTER_PROCESSOR_MODEL", "google/gemini-3.8-flash"),
            processor_prompt_path: var_or("PROCESSOR_PROMPT_TEMPLATE_PATH", "prompt.handlebars"),
            processor_state_prompt_path: var_or("PROCESSOR_STATE_TEMPLATE_PATH", "prompt-state.handlebars"),
            processor_cache_size: var_or("PROCESSOR_CACHE_SIZE", "1000").parse().expect("PROCESSOR_CACHE_SIZE must be a number"),
        }
    }
}
