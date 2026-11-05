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
        field("PROCESSOR_PROVIDER", "Провайдер модели", "Сервис, который отвечает на запросы Алисы.", "processor", "select", "openrouter").options(&["openrouter", "anthropic"]),
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
        field("PROCESSOR_CACHE_SIZE", "Кэш ответов", "Максимальное число ответов в кэше. Ноль отключает кэш.", "processor", "integer", "1000").range(0., 100_000.),
        field("STT_PROVIDER", "Распознавание речи", "GigaAM распознаёт речь локально; OpenRouter используется при сбое, если задан ключ.", "stt", "select", "openrouter").options(&["openrouter", "gigaam"]),
        field("GIGASTT_URL", "Адрес GigaAM", "Адрес gigastt: http:// или ws://, например http://gigastt:9876.", "stt", "text", ""),
        field("GIGASTT_MIN_SILENCE_MS", "Пауза после речи, мс", "Сколько тишины ждать, прежде чем считать фразу законченной.", "stt", "integer", "800").range(1., 60_000.),
        field("STT_END_OF_SPEECH", "Окончание фразы", "Автоматически: для GigaAM — сервер, иначе — колонка.", "stt", "select", "").options(&["", "server", "station"]),
        field("OPENROUTER_STT_MODEL", "Модель распознавания OpenRouter", "Основное или резервное распознавание речи.", "stt", "text", "openai/whisper-large-v3"),
        field("OPENROUTER_STT_LANGUAGE", "Язык распознавания", "Например ru. Пустое значение — определять язык автоматически.", "stt", "text", "ru"),
        field("TTS_PROVIDER", "Синтез речи", "Провайдер, который озвучивает ответы Алисы.", "tts", "select", "openrouter").options(&["openrouter", "piper"]),
        field("OPENROUTER_TTS_MODEL", "Модель голоса OpenRouter", "Основной голос или резервный при сбое Piper.", "tts", "text", "google/gemini-3.8-flash-lite-tts"),
        field("OPENROUTER_TTS_VOICE", "Голос OpenRouter", "Имя голоса, поддерживаемого выбранной моделью.", "tts", "text", "Kore"),
        field("OPENROUTER_TTS_INSTRUCTIONS", "Инструкции для голоса", "Дополнительные указания модели синтеза речи, если она их поддерживает.", "tts", "textarea", ""),
        field("OPENROUTER_TTS_FORMAT", "Формат звука OpenRouter", "Формат ответа API синтеза речи.", "tts", "select", "pcm").options(&["pcm", "mp3"]),
        field("OPENROUTER_TTS_PCM_SAMPLE_RATE", "Частота PCM, Гц", "Используется только для формата PCM.", "tts", "integer", "24000").range(8_000., 192_000.),
        field("OPENROUTER_TTS_SPEED", "Скорость речи", "1 — обычная скорость. Также задаёт скорость Piper, если его длительность фонем не указана.", "tts", "number", "1").range(0.1, 10.),
        field("OPENROUTER_TTS_STREAMING", "Говорить во время подготовки ответа", "Начинает озвучивать ответ до окончания генерации. Работает и с Piper.", "tts", "boolean", "true"),
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
