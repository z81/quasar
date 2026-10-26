//! Server configuration from environment variables (same names as the
//! TypeScript version), loaded from `.env.local` then `.env`.

use std::env;

/// Who turns the station's speech into text
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SttProvider {
    /// one /audio/transcriptions request after the end of speech
    OpenRouter,
    /// a gigastt server recognizing while the station records (OpenRouter as fallback)
    GigaAm,
}

/// What OpenRouter's speech synthesis returns
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TtsFormat {
    /// raw PCM16 at OPENROUTER_TTS_PCM_SAMPLE_RATE
    Pcm,
    /// MP3, decoded by ffmpeg chunk by chunk: its frames carry their own rate
    Mp3,
}

/// The reasoning efforts of OPENROUTER_PROCESSOR_REASONING
const REASONING_EFFORTS: [&str; 5] = ["none", "minimal", "low", "medium", "high"];

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
    pub stt_provider: SttProvider,
    /// the gigastt server's HTTP address (STT_PROVIDER=gigaam), e.g. http://gigastt:9876
    pub gigastt_url: Option<String>,
    pub end_of_speech: EndOfSpeech,
    /// the trailing silence after which gigastt's VAD ends an utterance
    /// (its per-session min_silence_ms; a gigastt without --vad ignores it)
    pub gigastt_min_silence_ms: u32,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_instructions: Option<String>,
    pub tts_pcm_sample_rate: u32,
    pub tts_format: TtsFormat,
    pub tts_speed: f32,
    /// speak an answer while the LLM still writes it (the TS fork's
    /// OPENROUTER_TTS_STREAMING); false: the whole text once it is complete
    pub tts_streaming: bool,
    pub processor_model: String,
    /// the dialog LLM's reasoning effort, sent as `reasoning: {effort}`; None:
    /// no `reasoning` field, the model's default
    pub processor_reasoning: Option<String>,
    pub processor_prompt_path: String,
    pub processor_state_prompt_path: String,
    pub processor_cache_size: u64,
    /// the music service of the basic backend (TS BasicMusicBackend); none: no music
    pub music_service_basic_url: Option<String>,
}

fn var_or(name: &str, default: &str) -> String {
    env::var(name).ok().filter(|value| !value.is_empty()).unwrap_or_else(|| default.to_string())
}

fn opt_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

/// STT_END_OF_SPEECH: `server` by default with gigastt, which alone can tell
/// the end of speech while the station records
fn end_of_speech(stt_provider: SttProvider) -> EndOfSpeech {
    match (opt_var("STT_END_OF_SPEECH").map(|value| value.trim().to_ascii_lowercase()).as_deref(), stt_provider) {
        (None | Some("server"), SttProvider::GigaAm) => EndOfSpeech::Server,
        (None | Some("station"), _) => EndOfSpeech::Station,
        (Some("server"), SttProvider::OpenRouter) => {
            tracing::warn!("STT_END_OF_SPEECH=server needs STT_PROVIDER=gigaam: the station ends utterances");
            EndOfSpeech::Station
        }
        (Some(other), _) => panic!("STT_END_OF_SPEECH must be server or station, not '{other}'"),
    }
}

impl Config {
    pub fn from_env() -> Self {
        let stt_provider = match var_or("STT_PROVIDER", "openrouter").trim().to_ascii_lowercase().as_str() {
            "openrouter" => SttProvider::OpenRouter,
            "gigaam" => SttProvider::GigaAm,
            other => panic!("STT_PROVIDER must be openrouter or gigaam, not '{other}'"),
        };
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
            stt_provider,
            gigastt_url: opt_var("GIGASTT_URL"),
            end_of_speech: end_of_speech(stt_provider),
            gigastt_min_silence_ms: match var_or("GIGASTT_MIN_SILENCE_MS", "800").trim().parse() {
                Ok(0) | Err(_) => panic!("GIGASTT_MIN_SILENCE_MS must be a positive number of milliseconds"),
                Ok(milliseconds) => milliseconds,
            },
            tts_model: var_or("OPENROUTER_TTS_MODEL", "google/gemini-3.8-flash-lite-tts"),
            tts_voice: var_or("OPENROUTER_TTS_VOICE", "Kore"),
            tts_instructions: opt_var("OPENROUTER_TTS_INSTRUCTIONS"),
            tts_pcm_sample_rate: var_or("OPENROUTER_TTS_PCM_SAMPLE_RATE", "24000").parse().expect("OPENROUTER_TTS_PCM_SAMPLE_RATE must be a number"),
            tts_format: match var_or("OPENROUTER_TTS_FORMAT", "pcm").trim().to_ascii_lowercase().as_str() {
                "pcm" => TtsFormat::Pcm,
                "mp3" => TtsFormat::Mp3,
                other => panic!("OPENROUTER_TTS_FORMAT must be pcm or mp3, not '{other}'"),
            },
            tts_speed: var_or("OPENROUTER_TTS_SPEED", "1").parse().expect("OPENROUTER_TTS_SPEED must be a number"),
            tts_streaming: var_or("OPENROUTER_TTS_STREAMING", "true").trim().eq_ignore_ascii_case("true"),
            processor_model: var_or("OPENROUTER_PROCESSOR_MODEL", "google/gemini-3.8-flash"),
            processor_reasoning: opt_var("OPENROUTER_PROCESSOR_REASONING")
                .map(|effort| effort.trim().to_ascii_lowercase())
                .filter(|effort| !effort.is_empty())
                .inspect(|effort| {
                    if !REASONING_EFFORTS.contains(&effort.as_str()) {
                        panic!("OPENROUTER_PROCESSOR_REASONING must be one of {}, not '{effort}'", REASONING_EFFORTS.join("|"));
                    }
                }),
            processor_prompt_path: var_or("PROCESSOR_PROMPT_TEMPLATE_PATH", "prompt.handlebars"),
            // TS name first; the earlier Rust-only name keeps working
            processor_state_prompt_path: opt_var("PROCESSOR_PROMPT_STATE_TEMPLATE_PATH")
                .or_else(|| opt_var("PROCESSOR_STATE_TEMPLATE_PATH"))
                .unwrap_or_else(|| "prompt-state.handlebars".to_string()),
            processor_cache_size: match var_or("PROCESSOR_CACHE_SIZE", "1000").parse() {
                Ok(0) | Err(_) => panic!("PROCESSOR_CACHE_SIZE must be a positive number"),
                Ok(size) => size,
            },
            music_service_basic_url: opt_var("MUSIC_SERVICE_BASIC_URL"),
        }
    }
}
