use serde::Serialize;

use super::Values;

/// Public form metadata. Actual secret values are never part of this catalog.
#[derive(Clone, Serialize)]
pub struct SettingDefinition {
    pub key: String,
    pub label: String,
    pub description: String,
    pub group: String,
    pub kind: String,
    pub options: Vec<String>,
    pub default_value: String,
    pub secret: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
}

impl SettingDefinition {
    fn new(
        key: &str,
        label: &str,
        description: &str,
        group: &str,
        kind: &str,
        default: &str,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            description: description.into(),
            group: group.into(),
            kind: kind.into(),
            options: Vec::new(),
            default_value: default.into(),
            secret: kind == "password",
            minimum: None,
            maximum: None,
        }
    }

    fn options(mut self, options: &[&str]) -> Self {
        self.options = options.iter().map(|value| (*value).into()).collect();
        self
    }

    fn range(mut self, minimum: f64, maximum: f64) -> Self {
        self.minimum = Some(minimum);
        self.maximum = Some(maximum);
        self
    }
}

/// Stable order is also the order in the settings form.
pub fn definitions() -> Vec<SettingDefinition> {
    let field = SettingDefinition::new;
    vec![
        field("PROCESSOR_PROVIDER", "Провайдер модели", "Сервис, который отвечает на запросы Алисы.", "processor", "select", "openrouter").options(&["openrouter", "anthropic", "codex", "opencode"]),
        field("OPENROUTER_API_KEY", "Ключ OpenRouter", "Для модели и распознавания/синтеза речи, в том числе резервных.", "processor", "password", ""),
        field("OPENROUTER_BASE_URL", "Адрес OpenRouter API", "Базовый HTTP(S) адрес совместимого API.", "processor", "text", "https://openrouter.ai/api/v1"),
        field("OPENROUTER_TIMEOUT_MS", "Таймаут OpenRouter, мс", "Ожидание подключения и данных от API.", "processor", "integer", "10000").range(1., 300_000.),
        field("OPENROUTER_PROCESSOR_MODEL", "Модель OpenRouter", "Используется, когда провайдер модели — OpenRouter.", "processor", "text", "google/gemini-3.8-flash"),
        field("OPENROUTER_PROCESSOR_REASONING", "Размышление модели OpenRouter", "Пустое значение оставляет выбор самой модели.", "processor", "select", "").options(&["", "none", "minimal", "low", "medium", "high"]),
        field("ANTHROPIC_API_KEY", "Ключ Anthropic", "Для выбранной модели Anthropic.", "processor", "password", ""),
        field("ANTHROPIC_BASE_URL", "Адрес Anthropic API", "Базовый HTTP(S) адрес Messages API.", "processor", "text", "https://api.anthropic.com"),
        field("ANTHROPIC_TIMEOUT_MS", "Таймаут Anthropic, мс", "Ожидание подключения и данных от API.", "processor", "integer", "10000").range(1., 300_000.),
        field("ANTHROPIC_PROCESSOR_MODEL", "Модель Anthropic", "Используется, когда провайдер модели — Anthropic.", "processor", "text", "claude-opus-5-5"),
        field("ANTHROPIC_PROCESSOR_EFFORT", "Размышление модели Anthropic", "Пустое значение оставляет выбор самой модели.", "processor", "select", "low").options(&["", "low", "medium", "high", "xhigh", "max"]),
        field("CODEX_TOKEN", "Токен Codex", "Access token из ~/.codex/auth.json или ключ OpenAI API. Для токена Codex аккаунт берётся из JWT.", "processor", "password", ""),
        field("CODEX_BASE_URL", "Адрес Codex API", "https://chatgpt.com/backend-api/codex для токена Codex; https://api.openai.com/v1 для ключа OpenAI.", "processor", "text", "https://chatgpt.com/backend-api/codex"),
        field("CODEX_ACCOUNT_ID", "ID аккаунта Codex", "Необязательно: нужен, если в токене нет chatgpt_account_id.", "processor", "text", ""),
        field("CODEX_TIMEOUT_MS", "Таймаут Codex, мс", "Ожидание подключения и данных от API.", "processor", "integer", "30000").range(1., 300_000.),
        field("CODEX_PROCESSOR_MODEL", "Модель Codex", "Используется, когда провайдер модели — Codex, например gpt-5.5.", "processor", "text", "gpt-5.5"),
        field("CODEX_PROCESSOR_EFFORT", "Размышление модели Codex", "Пустое значение оставляет выбор самой модели.", "processor", "select", "low").options(&["", "low", "medium", "high", "xhigh"]),
        field("OPENCODE_API_KEY", "Ключ OpenCode Go", "API key из OpenCode Console (oc_sk_...).", "processor", "password", ""),
        field("OPENCODE_BASE_URL", "Адрес OpenCode API", "https://opencode.ai/inference/go/openai/v1 для Go; https://opencode.ai/inference/openai/v1 для pay-as-you-go.", "processor", "text", "https://opencode.ai/inference/go/openai/v1"),
        field("OPENCODE_PROCESSOR_MODEL", "Модель OpenCode", "Например deepseek-v4.1-flash.", "processor", "text", "deepseek-v4.1-flash"),
        field("OPENCODE_PROCESSOR_REASONING", "Размышление модели OpenCode", "reasoning_effort; пустое значение оставляет выбор модели.", "processor", "select", "").options(&["", "low", "high", "max"]),
        field("OPENCODE_TIMEOUT_MS", "Таймаут OpenCode, мс", "Ожидание подключения и данных от API.", "processor", "integer", "30000").range(1., 300_000.),
        field("PROCESSOR_CACHE_SIZE", "Кэш ответов", "Максимальное число ответов в кэше. Ноль отключает кэш.", "processor", "integer", "1000").range(0., 100_000.),
        field("STT_PROVIDER", "Распознавание речи", "GigaAM распознаёт речь локально; OpenRouter используется при сбое, если задан ключ.", "stt", "select", "openrouter").options(&["openrouter", "gigaam"]),
        field("GIGASTT_URL", "Адрес GigaAM", "Адрес gigastt: http:// или ws://, например http://gigastt:9876.", "stt", "text", ""),
        field("SPEAKER_EMBEDDING_URL", "Распознавание домашних голосов", "Адрес локального сервиса speaker-embedding, например http://speaker-embedding:8091. Пустое значение отключает распознавание. Профили настраиваются отдельно.", "stt", "text", ""),
        field("GIGASTT_MIN_SILENCE_MS", "Пауза после речи, мс", "Сколько тишины ждать, прежде чем считать фразу законченной.", "stt", "integer", "800").range(1., 60_000.),
        field("STT_END_OF_SPEECH", "Окончание фразы", "Автоматически: для GigaAM — сервер, иначе — колонка.", "stt", "select", "").options(&["", "server", "station"]),
        field("OPENROUTER_STT_MODEL", "Модель распознавания OpenRouter", "Основное или резервное распознавание речи.", "stt", "text", "openai/whisper-large-v3"),
        field("OPENROUTER_STT_LANGUAGE", "Язык распознавания", "Например ru. Пустое значение — определять язык автоматически.", "stt", "text", "ru"),
        field("WHISPER_STT_URL", "Whisper STT (GPU)", "Адрес локального whisper.cpp server, например http://192.168.1.133:8098. Если задан, финальный текст уточняется Whisper large-v3; стриминг остаётся за GigaAM.", "stt", "text", ""),
        field("TTS_PROVIDER", "Синтез речи", "Провайдер, который озвучивает ответы Алисы: OpenRouter, Piper или локальный Qwen3-TTS.", "tts", "select", "openrouter").options(&["openrouter", "piper", "local"]),
        field("OPENROUTER_TTS_MODEL", "Модель голоса OpenRouter", "Основной голос или резервный при сбое Piper.", "tts", "text", "google/gemini-3.8-flash-lite-tts"),
        field("OPENROUTER_TTS_VOICE", "Голос OpenRouter", "Имя голоса, поддерживаемого выбранной моделью.", "tts", "text", "Kore"),
        field("OPENROUTER_TTS_INSTRUCTIONS", "Инструкции для голоса", "Дополнительные указания модели синтеза речи, если она их поддерживает.", "tts", "textarea", ""),
        field("OPENROUTER_TTS_FORMAT", "Формат звука OpenRouter", "Формат ответа API синтеза речи.", "tts", "select", "pcm").options(&["pcm", "mp3"]),
        field("OPENROUTER_TTS_PCM_SAMPLE_RATE", "Частота PCM, Гц", "Используется только для формата PCM.", "tts", "integer", "24000").range(8_000., 192_000.),
        field("OPENROUTER_TTS_SPEED", "Скорость речи", "1 — обычная скорость. Также задаёт скорость Piper, если его длительность фонем не указана.", "tts", "number", "1").range(0.1, 10.),
        field("OPENROUTER_TTS_STREAMING", "Говорить во время подготовки ответа", "Начинает озвучивать ответ до окончания генерации. Работает и с Piper.", "tts", "boolean", "true"),
        field("LOCAL_TTS_URL", "Адрес локального синтеза", "OpenAI-совместимый адрес Qwen3-TTS, например http://192.168.1.133:8090/v1. Обязателен при TTS_PROVIDER=local.", "tts", "text", ""),
        field("LOCAL_TTS_MODEL", "Модель локального синтеза", "Имя модели на локальном сервере Qwen3-TTS.", "tts", "text", "qwen3-tts-ru"),
        field("LOCAL_TTS_VOICE", "Голос локального синтеза", "Имя голоса на локальном сервере Qwen3-TTS.", "tts", "text", "alice"),
        field("DESIGN_TTS_URL", "Адрес VoiceDesign", "OpenAI-совместимый адрес VoiceDesign, например http://192.168.1.133:8091/v1. Пусто — свободные стили от LLM отключены.", "tts", "text", ""),
        field("DESIGN_TTS_MODEL", "Модель VoiceDesign", "Имя модели на сервере VoiceDesign.", "tts", "text", "qwen3-tts-design"),
        field("DESIGN_TTS_VOICE", "Голос VoiceDesign", "Обычно пусто: VoiceDesign выбирает голос по инструкции.", "tts", "text", ""),
        field("DESIGN_TTS_PREFIX", "Префикс стиля", "Необязательный префикс перед style от LLM. Пусто — style используется как полное описание голоса.", "tts", "textarea", ""),
        field("WHISPER_TTS_MODEL", "Модель шёпота", "Отдельный выразительный голос через тот же ключ OpenRouter: Piper и Eleven Flash не поддерживают шёпот. Используется только при включённых ответах шёпотом.", "tts", "select", "elevenlabs/eleven-v3").options(&["elevenlabs/eleven-v3", "elevenlabs/eleven-v3-conversational", "elevenlabs/eleven-v4", "elevenlabs/eleven-v4-turbo"]),
        field("WHISPER_TTS_VOICE", "Голос шёпота", "Голос ElevenLabs для шёпота, например alice. Для шёпота требуется ключ OpenRouter даже при основном голосе Piper.", "tts", "text", "alice"),
        field("WHISPER_TTS_URL", "Адрес локального шёпота", "OpenAI-совместимый адрес Qwen3-TTS, например http://192.168.1.133:8090/v1. Пустое значение — ElevenLabs через OpenRouter.", "tts", "text", ""),
        field("WHISPER_TTS_LOCAL_MODEL", "Модель локального шёпота", "Имя модели на локальном сервере Qwen3-TTS.", "tts", "text", "qwen3-tts-ru"),
        field("WHISPER_TTS_LOCAL_VOICE", "Голос локального шёпота", "Имя голоса на локальном сервере Qwen3-TTS.", "tts", "text", "whisper"),
        field("WHISPER_DETECT_URL", "Детектор шёпота", "Адрес локального сервиса YAMNet, например http://alise-whisper:8080. Пустое значение отключает определение шёпота; голос ответа не меняется.", "tts", "text", ""),
        field("PIPER_URL", "Адрес Piper", "Базовый HTTP(S) адрес сервера синтеза, например http://piper:5000.", "tts", "text", ""),
        field("PIPER_VOICE", "Голос Piper", "Пустое значение — голос, выбранный на сервере Piper.", "tts", "text", ""),
        field("PIPER_SPEAKER", "Имя диктора Piper", "Для моделей с несколькими дикторами; необязательно.", "tts", "text", ""),
        field("PIPER_SPEAKER_ID", "Номер диктора Piper", "Для моделей с несколькими дикторами; необязательно.", "tts", "integer", "").range(0., u32::MAX as f64),
        field("PIPER_LENGTH_SCALE", "Длительность фонем Piper", "Меньше 1 — быстрее. Пустое значение использует общую скорость речи.", "tts", "number", "").range(0.01, 100.),
        field("PIPER_FALLBACK", "Резервный синтез для Piper", "При сбое Piper использовать OpenRouter или пропустить озвучивание.", "tts", "select", "openrouter").options(&["openrouter", "none"]),
        field("PROCESSOR_FUNCTION_SERVER_URLS", "Серверы функций", "HTTP(S) адреса каталогов функций, по одному в строке или через запятую.", "integrations", "textarea", ""),
        field("PROCESSOR_STATE_SERVER_URLS", "Серверы состояния", "HTTP(S) адреса состояния для контекста Алисы, по одному в строке или через запятую.", "integrations", "textarea", ""),
        field("PROCESSOR_MCP_SERVER_URLS", "MCP-серверы", "Каждая строка или запись через запятую: http(s)://адрес#префикс.", "integrations", "textarea", ""),
        field("PROCESSOR_MCP_SERVER_TOKENS", "Ключи MCP-серверов", "Записи префикс:токен, по одной в строке или через запятую. Значения скрыты.", "integrations", "password", ""),
        field("PROCESSOR_MCP_LIVE_CONTEXT", "Контекст от MCP-серверов", "Запрашивать доступный GetLiveContext для каждого обращения к Алисе.", "integrations", "boolean", "true"),
        field("TZ", "Часовой пояс сервера", "IANA, например Europe/Moscow. Используется, если колонка не задала свой часовой пояс.", "general", "text", ""),
        field("RUST_LOG", "Уровень журналирования", "Фильтр событий сервера, например info, warn или alice_server=debug.", "general", "text", "info"),
        field("ALLOW_UPDATES", "Обновления прошивки колонки", "Разрешить отдавать колонке обновления из файла конфигурации.", "advanced", "boolean", "false"),
        field("UPDATES_CONFIG_PATH", "Файл обновлений", "Путь к YAML-файлу на сервере. Используется, когда обновления разрешены.", "advanced", "text", ""),
        field("PROCESSOR_PROMPT_TEMPLATE_PATH", "Шаблон системного промпта", "Путь к Handlebars-файлу на сервере.", "advanced", "text", "prompt.handlebars"),
        field("PROCESSOR_PROMPT_STATE_TEMPLATE_PATH", "Шаблон состояния", "Путь к Handlebars-файлу состояния на сервере.", "advanced", "text", "prompt-state.handlebars"),
    ]
}

pub fn defaults() -> Values {
    definitions()
        .into_iter()
        .map(|field| (field.key, field.default_value))
        .collect()
}
