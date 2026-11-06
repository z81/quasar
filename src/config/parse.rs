use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};

use super::{
    definitions, AnthropicConfig, Config, EndOfSpeech, McpServerConfig, PiperConfig, SttProvider,
    TtsFormat, TtsProvider, Values,
};

pub(super) fn config(supplied: &Values) -> Result<Config> {
    let fields = definitions();
    ensure!(
        supplied
            .keys()
            .all(|key| fields.iter().any(|field| &field.key == key)),
        "Неизвестное имя серверной настройки"
    );
    let mut values: Values = fields
        .iter()
        .map(|field| (field.key.clone(), field.default_value.clone()))
        .collect();
    values.extend(supplied.clone());
    for field in &fields {
        let value = values[&field.key].trim();
        ensure!(
            value.len() <= 16_384,
            "{}: значение слишком длинное",
            field.key
        );
        let optional_number = matches!(
            field.key.as_str(),
            "PIPER_SPEAKER_ID" | "PIPER_LENGTH_SCALE"
        );
        match field.kind.as_str() {
            "boolean" => {
                ensure!(
                    matches!(value.to_ascii_lowercase().as_str(), "true" | "false"),
                    "{}: нужно true или false",
                    field.key
                );
            }
            "select" => {
                ensure!(
                    field.options.contains(&value.to_ascii_lowercase()),
                    "{}: выберите допустимый вариант",
                    field.key
                );
            }
            "integer" | "number" if !value.is_empty() || !optional_number => {
                let number = if field.kind == "integer" {
                    ensure!(
                        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
                        "{}: нужно целое неотрицательное число",
                        field.key
                    );
                    value
                        .parse::<u64>()
                        .map(|number| number as f64)
                        .with_context(|| format!("{}: слишком большое число", field.key))?
                } else {
                    value
                        .parse::<f64>()
                        .with_context(|| format!("{}: нужно число", field.key))?
                };
                ensure!(number.is_finite(), "{}: нужно конечное число", field.key);
                if let (Some(minimum), Some(maximum)) = (field.minimum, field.maximum) {
                    ensure!(
                        (minimum..=maximum).contains(&number),
                        "{}: допустимо от {minimum} до {maximum}",
                        field.key
                    );
                }
            }
            _ => {}
        }
        if !matches!(field.kind.as_str(), "textarea") && field.key != "PROCESSOR_MCP_SERVER_TOKENS"
        {
            ensure!(
                !value.chars().any(char::is_control),
                "{}: управляющие символы недопустимы",
                field.key
            );
        }
    }
    let values = Parsed(values);
    for key in ["OPENROUTER_BASE_URL", "ANTHROPIC_BASE_URL"] {
        url(key, &values.required(key)?, &["http", "https"])?;
    }
    for key in ["OPENROUTER_API_KEY", "ANTHROPIC_API_KEY"] {
        if let Some(secret) = values.optional(key) {
            credential(key, &secret)?;
        }
    }
    let piper_url = values.optional("PIPER_URL");
    if let Some(address) = piper_url.as_deref() {
        url("PIPER_URL", address, &["http", "https"])?;
    }
    let gigastt_url = values.optional("GIGASTT_URL");
    let speaker_embedding_url = values.optional("SPEAKER_EMBEDDING_URL");
    if let Some(address) = speaker_embedding_url.as_deref() {
        let parsed = url("SPEAKER_EMBEDDING_URL", address, &["http", "https"])?;
        ensure!(parsed.query().is_none() && parsed.fragment().is_none(), "SPEAKER_EMBEDDING_URL: нужен базовый адрес без query и fragment");
    }
    if let Some(address) = gigastt_url.as_deref() {
        let parsed = url("GIGASTT_URL", address, &["http", "ws"])?;
        ensure!(
            parsed.query().is_none() && parsed.fragment().is_none(),
            "GIGASTT_URL: укажите базовый адрес без query и fragment"
        );
    }
    let timezone = values
        .optional("TZ")
        .map(|value| value.strip_prefix(':').unwrap_or(&value).to_owned());
    if let Some(zone) = timezone.as_deref() {
        ensure!(
            zone.parse::<chrono_tz::Tz>().is_ok(),
            "TZ: нужен существующий часовой пояс IANA"
        );
    }
    let processor_provider = values.lower("PROCESSOR_PROVIDER");
    let log_filter = values.text("RUST_LOG");
    ensure!(
        tracing_subscriber::EnvFilter::try_new(&log_filter).is_ok(),
        "RUST_LOG: некорректный фильтр журналирования"
    );
    let anthropic = if processor_provider == "anthropic" {
        Some(AnthropicConfig {
            api_key: values.text("ANTHROPIC_API_KEY"),
            base_url: values.text("ANTHROPIC_BASE_URL"),
            timeout_ms: values.integer("ANTHROPIC_TIMEOUT_MS")?,
            effort: values
                .optional("ANTHROPIC_PROCESSOR_EFFORT")
                .map(|value| value.to_ascii_lowercase()),
        })
    } else {
        None
    };
    let stt_provider = match values.lower("STT_PROVIDER").as_str() {
        "gigaam" => SttProvider::GigaAm,
        _ => SttProvider::OpenRouter,
    };
    let tts_provider = match values.lower("TTS_PROVIDER").as_str() {
        "piper" => TtsProvider::Piper,
        _ => TtsProvider::OpenRouter,
    };
    let tts_speed = values.number("OPENROUTER_TTS_SPEED")? as f32;
    let speaker_id = values
        .optional("PIPER_SPEAKER_ID")
        .map(|_| values.integer("PIPER_SPEAKER_ID").map(|id| id as u32))
        .transpose()?;
    let length_scale = match values.optional("PIPER_LENGTH_SCALE") {
        Some(_) => Some(values.number("PIPER_LENGTH_SCALE")? as f32),
        None if (tts_speed - 1.).abs() > f32::EPSILON => Some(1. / tts_speed),
        None => None,
    };
    let piper = if tts_provider == TtsProvider::Piper {
        piper_url.map(|url| PiperConfig {
            url: url.trim_end_matches('/').into(),
            voice: values.optional("PIPER_VOICE"),
            speaker: values.optional("PIPER_SPEAKER"),
            speaker_id,
            length_scale,
            fallback: values.lower("PIPER_FALLBACK") == "openrouter",
        })
    } else {
        None
    };
    let end_of_speech = match (values.lower("STT_END_OF_SPEECH").as_str(), stt_provider) {
        ("" | "server", SttProvider::GigaAm) => EndOfSpeech::Server,
        _ => EndOfSpeech::Station,
    };
    Ok(Config {
        allow_updates: values.boolean("ALLOW_UPDATES"),
        updates_config_path: values.optional("UPDATES_CONFIG_PATH"),
        openrouter_api_key: values.text("OPENROUTER_API_KEY"),
        openrouter_base_url: values.text("OPENROUTER_BASE_URL"),
        openrouter_timeout_ms: values.integer("OPENROUTER_TIMEOUT_MS")?,
        stt_model: values.required("OPENROUTER_STT_MODEL")?,
        stt_language: values.optional("OPENROUTER_STT_LANGUAGE"),
        stt_provider,
        gigastt_url,
        speaker_embedding_url,
        end_of_speech,
        gigastt_min_silence_ms: values.integer("GIGASTT_MIN_SILENCE_MS")? as u32,
        tts_model: values.required("OPENROUTER_TTS_MODEL")?,
        tts_voice: values.required("OPENROUTER_TTS_VOICE")?,
        tts_instructions: values.optional("OPENROUTER_TTS_INSTRUCTIONS"),
        whisper_tts_model: values.required("WHISPER_TTS_MODEL")?,
        whisper_tts_voice: values.required("WHISPER_TTS_VOICE")?,
        tts_pcm_sample_rate: values.integer("OPENROUTER_TTS_PCM_SAMPLE_RATE")? as u32,
        tts_format: if values.lower("OPENROUTER_TTS_FORMAT") == "mp3" {
            TtsFormat::Mp3
        } else {
            TtsFormat::Pcm
        },
        tts_speed,
        tts_provider,
        piper,
        tts_streaming: values.boolean("OPENROUTER_TTS_STREAMING"),
        processor_model: values.required(if anthropic.is_some() {
            "ANTHROPIC_PROCESSOR_MODEL"
        } else {
            "OPENROUTER_PROCESSOR_MODEL"
        })?,
        anthropic,
        processor_reasoning: values
            .optional("OPENROUTER_PROCESSOR_REASONING")
            .map(|value| value.to_ascii_lowercase()),
        processor_prompt_path: values.required("PROCESSOR_PROMPT_TEMPLATE_PATH")?,
        processor_state_prompt_path: values.required("PROCESSOR_PROMPT_STATE_TEMPLATE_PATH")?,
        processor_cache_size: values.integer("PROCESSOR_CACHE_SIZE")?,
        mcp_servers: mcp_servers(&values.0)?,
        mcp_live_context: values.boolean("PROCESSOR_MCP_LIVE_CONTEXT"),
        function_server_urls: urls(
            "PROCESSOR_FUNCTION_SERVER_URLS",
            &values.text("PROCESSOR_FUNCTION_SERVER_URLS"),
        )?,
        state_server_urls: urls(
            "PROCESSOR_STATE_SERVER_URLS",
            &values.text("PROCESSOR_STATE_SERVER_URLS"),
        )?,
        timezone,
        log_filter,
    })
}

/// Numeric and enum getters run only after the complete metadata validation above.
struct Parsed(Values);

impl Parsed {
    fn text(&self, key: &str) -> String {
        self.0[key].trim().to_owned()
    }
    fn lower(&self, key: &str) -> String {
        self.text(key).to_ascii_lowercase()
    }
    fn optional(&self, key: &str) -> Option<String> {
        let value = self.text(key);
        (!value.is_empty()).then_some(value)
    }
    fn required(&self, key: &str) -> Result<String> {
        self.optional(key)
            .with_context(|| format!("{key}: значение не может быть пустым"))
    }
    fn integer(&self, key: &str) -> Result<u64> {
        self.0[key]
            .trim()
            .parse()
            .with_context(|| format!("{key}: нужно целое число"))
    }
    fn number(&self, key: &str) -> Result<f64> {
        self.0[key]
            .trim()
            .parse()
            .with_context(|| format!("{key}: нужно число"))
    }
    fn boolean(&self, key: &str) -> bool {
        self.lower(key) == "true"
    }
}

fn url(key: &str, value: &str, schemes: &[&str]) -> Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| anyhow::anyhow!("{key}: некорректный адрес сервиса"))?;
    ensure!(
        schemes.contains(&parsed.scheme()) && parsed.host_str().is_some(),
        "{key}: недопустимый протокол или не задан адрес сервера"
    );
    ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "{key}: задайте учётные данные в отдельном поле ключа"
    );
    ensure!(
        parsed.fragment().is_none(),
        "{key}: fragment в адресе сервиса недопустим"
    );
    Ok(parsed)
}

fn entries(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
}

fn urls(key: &str, value: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for address in entries(value) {
        ensure!(
            result.len() < 32,
            "{key}: допускается не больше 32 серверов"
        );
        url(key, address, &["http", "https"])?;
        if !result.iter().any(|existing| existing == address) {
            result.push(address.to_string());
        }
    }
    Ok(result)
}

fn credential(key: &str, value: &str) -> Result<()> {
    ensure!(
        reqwest::header::HeaderValue::from_str(&format!("Bearer {value}")).is_ok(),
        "{key}: некорректный ключ авторизации"
    );
    Ok(())
}

fn prefix(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) fn mcp_tokens(value: &str) -> Result<BTreeMap<String, String>> {
    let mut tokens = BTreeMap::new();
    for entry in entries(value) {
        let Some((name, token)) = entry.split_once(':') else {
            bail!("PROCESSOR_MCP_SERVER_TOKENS: нужны записи префикс:токен");
        };
        let name = name.trim();
        let token = token.trim();
        ensure!(
            prefix(name) && !token.is_empty(),
            "PROCESSOR_MCP_SERVER_TOKENS: некорректный префикс или пустой токен"
        );
        credential("PROCESSOR_MCP_SERVER_TOKENS", token)?;
        ensure!(
            !tokens.contains_key(name),
            "PROCESSOR_MCP_SERVER_TOKENS: префиксы не должны повторяться"
        );
        tokens.insert(name.to_string(), token.to_string());
    }
    Ok(tokens)
}

pub(crate) fn mcp_servers(values: &Values) -> Result<Vec<McpServerConfig>> {
    let mut tokens = mcp_tokens(
        values
            .get("PROCESSOR_MCP_SERVER_TOKENS")
            .map(String::as_str)
            .unwrap_or_default(),
    )?;
    let mut result: Vec<McpServerConfig> = Vec::new();
    for entry in entries(
        values
            .get("PROCESSOR_MCP_SERVER_URLS")
            .map(String::as_str)
            .unwrap_or_default(),
    ) {
        ensure!(
            result.len() < 32,
            "PROCESSOR_MCP_SERVER_URLS: допускается не больше 32 серверов"
        );
        let Some((address, name)) = entry.rsplit_once('#') else {
            bail!("PROCESSOR_MCP_SERVER_URLS: нужны записи адрес#префикс");
        };
        let name = name.trim();
        let address = address.trim();
        ensure!(
            prefix(name),
            "PROCESSOR_MCP_SERVER_URLS: префикс должен содержать буквы, цифры, _ или -"
        );
        ensure!(
            !result.iter().any(|server| server.prefix == name),
            "PROCESSOR_MCP_SERVER_URLS: префиксы не должны повторяться"
        );
        url("PROCESSOR_MCP_SERVER_URLS", address, &["http", "https"])?;
        result.push(McpServerConfig {
            url: address.into(),
            prefix: name.into(),
            token: tokens.remove(name),
        });
    }
    Ok(result)
}
