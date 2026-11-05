//! Typed operational settings, independent of their environment/database source.

mod catalog;
mod parse;

use std::collections::BTreeMap;
use std::env;

use anyhow::{ensure, Context, Result};

pub use catalog::{defaults, definitions, SettingDefinition};

pub type Values = BTreeMap<String, String>;

/// The immutable process settings needed before the database can be opened.
#[derive(Clone)]
pub struct BootstrapConfig {
    pub port: u16,
    pub api_port: u16,
    pub postgres_url: String,
    pub glagol_jwt_key: Vec<u8>,
}

impl BootstrapConfig {
    pub fn from_env() -> Result<Self> {
        let port = bootstrap_port("PORT", "31115")?;
        let api_port = bootstrap_port("API_PORT", "31116")?;
        ensure!(port != api_port, "PORT и API_PORT должны различаться");
        let postgres_url = env::var("POSTGRES_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "postgres://quasar:quasar@localhost/quasar".to_string());
        // Use the database driver's parser, including its supported keyword and
        // Unix-socket formats. Its error is replaced so credentials stay private.
        postgres_url
            .parse::<tokio_postgres::Config>()
            .map_err(|_| anyhow::anyhow!("POSTGRES_URL: некорректные параметры PostgreSQL"))?;
        let glagol_jwt_key = env::var("GLAGOL_JWT_KEY")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string())
            .into_bytes();
        Ok(Self {
            port,
            api_port,
            postgres_url,
            glagol_jwt_key,
        })
    }
}

fn bootstrap_port(key: &str, default: &str) -> Result<u16> {
    let value = env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string());
    let port = value
        .parse::<u16>()
        .with_context(|| format!("{key}: нужен порт от 1 до 65535"))?;
    ensure!(port > 0, "{key}: нужен порт от 1 до 65535");
    Ok(port)
}

/// Capture only supported operational env names once. Secrets are never Debug.
#[derive(Clone, Default)]
pub struct Environment {
    pub values: Values,
    /// Canonical setting key -> the actual environment variable supplying it.
    pub sources: BTreeMap<String, String>,
}

impl Environment {
    pub fn capture() -> Self {
        let mut captured = Self::default();
        for definition in definitions() {
            if let Ok(value) = env::var(&definition.key) {
                captured
                    .sources
                    .insert(definition.key.clone(), definition.key.clone());
                captured.values.insert(definition.key, value);
            }
        }
        const CANONICAL: &str = "PROCESSOR_PROMPT_STATE_TEMPLATE_PATH";
        const LEGACY: &str = "PROCESSOR_STATE_TEMPLATE_PATH";
        if !captured
            .values
            .get(CANONICAL)
            .is_some_and(|value| !value.is_empty())
        {
            if let Ok(value) = env::var(LEGACY) {
                if !value.is_empty() {
                    captured.values.insert(CANONICAL.to_string(), value);
                    captured
                        .sources
                        .insert(CANONICAL.to_string(), LEGACY.to_string());
                }
            }
        }
        captured
    }
}

/// Who turns the station's speech into text
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SttProvider {
    /// one /audio/transcriptions request after the end of speech
    OpenRouter,
    /// a gigastt server recognizing while the station records (OpenRouter as fallback)
    GigaAm,
}

/// Who synthesizes speech
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TtsProvider {
    /// OpenRouter's /audio/speech (OPENROUTER_TTS_*)
    OpenRouter,
    /// a Piper HTTP server (PIPER_*), OpenRouter as fallback
    Piper,
}

/// TTS_PROVIDER=piper: the Piper HTTP server and how it speaks
#[derive(Clone)]
pub struct PiperConfig {
    /// the server's address, e.g. http://piper:5000
    pub url: String,
    /// a voice the server has (its -m voice when unset)
    pub voice: Option<String>,
    /// the speaker of a multi-speaker voice, by name or by id
    pub speaker: Option<String>,
    pub speaker_id: Option<u32>,
    /// the speaking pace: below 1 faster, above 1 slower
    pub length_scale: Option<f32>,
    /// a chunk Piper fails on is spoken by OpenRouter (PIPER_FALLBACK=openrouter)
    pub fallback: bool,
}

/// What OpenRouter's speech synthesis returns
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TtsFormat {
    /// raw PCM16 at OPENROUTER_TTS_PCM_SAMPLE_RATE
    Pcm,
    /// MP3, decoded by ffmpeg chunk by chunk: its frames carry their own rate
    Mp3,
}

/// PROCESSOR_PROVIDER=anthropic: the dialog goes to Anthropic's Messages API
#[derive(Clone)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub base_url: String,
    pub timeout_ms: u64,
    /// `output_config.effort`, how much the model thinks before it answers;
    /// None: not sent, the model's default
    pub effort: Option<String>,
}

/// Who decides that the user stopped speaking
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndOfSpeech {
    /// gigastt's endpointing ends the utterance (STT_PROVIDER=gigaam); the
    /// station's own Close stays the backstop
    Server,
    /// the station's StreamControl Close (a 5 s timeout on the Station Midi)
    Station,
}

#[derive(Clone)]
pub struct Config {
    pub allow_updates: bool,
    pub updates_config_path: Option<String>,
    pub openrouter_api_key: String,
    pub openrouter_base_url: String,
    pub openrouter_timeout_ms: u64,
    pub stt_model: String,
    pub stt_language: Option<String>,
    pub stt_provider: SttProvider,
    /// the gigastt server's HTTP address (STT_PROVIDER=gigaam), e.g. http://gigastt:9876
    pub gigastt_url: Option<String>,
    pub end_of_speech: EndOfSpeech,
    /// the trailing silence after which an utterance ends: gigastt's VAD's
    /// per-session min_silence_ms; without --vad the silence after the latest
    /// word waited for once gigastt's decoder ends it (no less than its 0.6 s)
    pub gigastt_min_silence_ms: u32,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_instructions: Option<String>,
    pub tts_pcm_sample_rate: u32,
    pub tts_format: TtsFormat,
    pub tts_speed: f32,
    pub tts_provider: TtsProvider,
    /// set with TTS_PROVIDER=piper
    pub piper: Option<PiperConfig>,
    /// speak an answer while the LLM still writes it (the TS fork's
    /// OPENROUTER_TTS_STREAMING); false: the whole text once it is complete
    pub tts_streaming: bool,
    /// set with PROCESSOR_PROVIDER=anthropic; None: OpenRouter answers the dialog
    pub anthropic: Option<AnthropicConfig>,
    /// the dialog's model at its provider
    pub processor_model: String,
    /// the dialog LLM's reasoning effort, sent as `reasoning: {effort}`; None:
    /// no `reasoning` field, the model's default
    pub processor_reasoning: Option<String>,
    pub processor_prompt_path: String,
    pub processor_state_prompt_path: String,
    pub processor_cache_size: u64,
    /// MCP servers whose tools the LLM may call (PROCESSOR_MCP_SERVER_URLS)
    pub mcp_servers: Vec<McpServerConfig>,
    /// fetch a `GetLiveContext` tool's result for every turn's state
    pub mcp_live_context: bool,
    pub function_server_urls: Vec<String>,
    pub state_server_urls: Vec<String>,
    pub timezone: Option<String>,
    pub log_filter: String,
}

/// One MCP server: `<url>#<prefix>` of PROCESSOR_MCP_SERVER_URLS, as in TS, and
/// its token from PROCESSOR_MCP_SERVER_TOKENS (`<prefix>:<token>`)
#[derive(Clone)]
pub struct McpServerConfig {
    pub url: String,
    /// its tools become the functions `<prefix>_<tool>`
    pub prefix: String,
    /// sent as `Authorization: Bearer`; never logged
    pub token: Option<String>,
}

impl Config {
    /// Parse a complete snapshot without reading or changing process env.
    pub fn from_values(values: &Values) -> Result<Self> {
        parse::config(values)
    }
}

/// Missing primary-provider credentials are reported to the UI, not a startup panic.
/// An absent optional fallback credential does not disable a healthy local provider.
pub fn readiness_issues(config: &Config) -> Vec<String> {
    let mut issues = Vec::new();
    if config.openrouter_api_key.is_empty()
        && (config.anthropic.is_none()
            || config.stt_provider == SttProvider::OpenRouter
            || config.tts_provider == TtsProvider::OpenRouter)
    {
        issues.push(
            "Задайте OPENROUTER_API_KEY для выбранного провайдера модели или речи".to_string(),
        );
    }
    if config
        .anthropic
        .as_ref()
        .is_some_and(|provider| provider.api_key.is_empty())
    {
        issues.push("Задайте ANTHROPIC_API_KEY для выбранной модели".to_string());
    }
    if config.stt_provider == SttProvider::GigaAm && config.gigastt_url.is_none() {
        issues.push("Задайте GIGASTT_URL для распознавания речи".to_string());
    }
    if config.tts_provider == TtsProvider::Piper && config.piper.is_none() {
        issues.push("Задайте PIPER_URL для синтеза речи".to_string());
    }
    issues
}
