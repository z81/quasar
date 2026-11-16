//! A narrow allowlist: firmware credentials, endpoints and hardware wiring never reach the form.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Toggle,
    Number { min: f64, max: f64, step: f64 },
    Addresses,
    ReadOnly,
}
#[derive(Clone, Copy)]
pub(super) struct Field {
    pub id: &'static str,
    pub path: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    pub kind: Kind,
    scale: f64,
    string: bool,
}
impl Field {
    const fn toggle(id: &'static str, path: &'static str, group: &'static str, label: &'static str, hint: &'static str) -> Self {
        Self { id, path, group, label, hint, kind: Kind::Toggle, scale: 1., string: false }
    }
    const fn number(id: &'static str, path: &'static str, group: &'static str, label: &'static str, hint: &'static str, min: f64, max: f64, step: f64) -> Self {
        Self { id, path, group, label, hint, kind: Kind::Number { min, max, step }, scale: 1., string: false }
    }
    const fn scaled(mut self, scale: f64) -> Self { self.scale = scale; self }
    const fn string(mut self) -> Self { self.string = true; self }
    const fn read(id: &'static str, path: &'static str, group: &'static str, label: &'static str, hint: &'static str) -> Self {
        Self { id, path, group, label, hint, kind: Kind::ReadOnly, scale: 1., string: false }
    }
    pub fn value(&self, config: &Value) -> Option<Value> {
        let raw = config.pointer(self.path)?;
        match self.kind {
            Kind::Toggle => raw.as_bool().map(Value::Bool),
            Kind::Number { .. } => {
                let number = if self.string { raw.as_str()?.parse::<f64>().ok()? } else { raw.as_f64()? };
                // Canonical display precision; never return binary rounding tails to editable fields.
                Some(json!(((number / self.scale) * 1_000_000.).round() / 1_000_000.))
            }
            Kind::Addresses => raw.as_array().filter(|a| a.iter().all(Value::is_string)).map(|_| raw.clone()),
            Kind::ReadOnly => Some(raw.clone()),
        }
    }
    pub fn view(&self, config: &Value) -> Value {
        let (kind, min, max, step) = match self.kind {
            Kind::Toggle => ("toggle", None, None, None),
            Kind::Number { min, max, step } => ("number", Some(min), Some(max), Some(step)),
            Kind::Addresses => ("addresses", None, None, None),
            Kind::ReadOnly => ("readonly", None, None, None),
        };
        json!({"id":self.id,"group":self.group,"label":self.label,"hint":self.hint,
            "kind":kind,"min":min,"max":max,"step":step,"value":self.value(config)})
    }
    fn encode(&self, value: &Value) -> Result<Value, String> {
        let invalid = || format!("Проверьте поле «{}»", self.label);
        match self.kind {
            Kind::Toggle => value.as_bool().map(Value::Bool).ok_or_else(invalid),
            Kind::Number { min, max, step } => {
                let n = value.as_f64().filter(|n| n.is_finite() && *n >= min && *n <= max).ok_or_else(invalid)?;
                if ((n / step).round() - n / step).abs() > 0.000001 { return Err(invalid()); }
                let raw = (n * self.scale * 1_000_000.).round() / 1_000_000.;
                let raw = if raw.fract() == 0. { json!(raw as i64) } else { json!(raw) };
                Ok(if self.string { json!(raw.to_string()) } else { raw })
            }
            Kind::Addresses => {
                let items = value.as_array().filter(|a| !a.is_empty() && a.len() <= 8).ok_or_else(invalid)?;
                let mut addresses = Vec::new();
                for item in items {
                    let address = item.as_str().ok_or_else(invalid)?.parse::<std::net::IpAddr>().map_err(|_| invalid())?;
                    if address.is_unspecified() || address.is_multicast() { return Err(invalid()); }
                    let text = address.to_string();
                    if !addresses.contains(&text) { addresses.push(text); }
                }
                Ok(json!(addresses))
            }
            Kind::ReadOnly => Err("Это информационное поле; изменение не поддерживается".into()),
        }
    }
}

pub(super) const USER: &[Field] = &[
    Field::toggle("idle_animation", "/deviceConfig/led/idleAnimation", "standby", "Анимация в простое", "Штатная подсветка, когда колонка не занята."),
    Field::toggle("standby_enabled", "/deviceConfig/standby/deepStandbyEnabled", "standby", "Глубокий сон", "Переходить в энергосберегающий режим после простоя."),
    Field::number("standby_minutes", "/deviceConfig/standby/deepStandbyTimeoutMinutes", "standby", "Простой перед сном, мин", "Это время бездействия колонки, а не таймер остановки музыки.", 1., 1440., 1.),
    Field::toggle("crossfade", "/accountConfig/audioPlayer/crossfadeEnabled", "playback", "Плавный переход между треками", "Настройка штатного плеера. Spotify Connect управляет переходами самостоятельно."),
    Field::toggle("smart_crossfade", "/systemConfig/audioPlayerCapability/enableSmartCrossfade", "playback", "Умный переход", "Разрешить штатному плееру подбирать переход между треками."),
    Field::toggle("smart_volume", "/systemConfig/smartVolume/enabled", "volume", "Автоматическая громкость колонки", "Разрешить штатную подстройку громкости по шуму в комнате."),
    Field::toggle("call_agc", "/systemConfig/calld/autoGainControl", "calls", "Автоматическое усиление микрофона", "Для штатных звонков; не меняет громкость ответов Алисы."),
    Field::toggle("call_preamp", "/systemConfig/calld/audioProcessingConfig/preAmplifier/enabled", "calls", "Предварительное усиление", "Обработка микрофона перед отправкой звука звонка."),
    Field::number("call_gain", "/systemConfig/calld/audioProcessingConfig/preAmplifier/fixedGainFactor", "calls", "Коэффициент предусилителя", "Значение прошивки fixedGainFactor. Большое усиление может вызывать искажения.", 1., 100., 0.1),
    Field::number("call_compression", "/systemConfig/calld/txAgcDigitalCompressionGain", "calls", "Усиление AGC", "Значение прошивки txAgcDigitalCompressionGain; не процент громкости.", 0., 100., 1.),
    Field::toggle("online_spotter", "/systemConfig/onlineSpotterEnabled", "recognition", "Онлайн-проверка активации", "Флаг штатного распознавания; доступность зависит от подключения к сервисам."),
    Field::toggle("pack_sound", "/systemConfig/voiceDialogSettings/recognizer/packSoundBuffer", "recognition", "Упаковка аудиобуфера", "Параметр передачи звука штатным распознавателем."),
    Field::toggle("history", "/accountConfig/saveHistoryUsage", "privacy", "История в штатных сервисах", "Не относится к истории диалогов нашего сервера."),
    Field::toggle("no_user_logs", "/accountConfig/doNotUseUserLogs", "privacy", "Не использовать пользовательские логи", "Запрос штатным сервисам. Сам по себе не отключает все службы телеметрии."),
    Field::toggle("dns_enabled", "/systemConfig/unbound/enable", "network", "Штатный DNS-резолвер", "Настройка unbound. Не заменяет DNS, полученный от роутера, и перенаправление доменов на наш сервер."),
    Field { id: "dns_forwarders", path: "/systemConfig/unbound/forwarders", group: "network", label: "DNS-серверы для unbound", hint: "IPv4 или IPv6, каждый с новой строки. Сохранение может повлиять на доступность сервисов.", kind: Kind::Addresses, scale: 1., string: false },
    Field::toggle("adb_network", "/systemConfig/adbd/network", "access", "ADB по сети", "После отключения терминал, файлы и чтение настроек по ADB могут стать недоступны. Включение зависит от запуска adbd в прошивке."),
    Field::toggle("adb_remote", "/systemConfig/adbd/remoteActivation", "access", "Удалённая активация ADB", "Разрешение в пользовательском конфиге; не запускает остановленную службу adbd."),
];

pub(super) const FIRMWARE: &[Field] = &[
    Field::toggle("earcon_start", "/aliced/playStartVoiceInputEarcon", "earcons", "Начало распознавания", "Сигнал при начале голосового ввода."),
    Field::toggle("earcon_cancel", "/aliced/playCancelEarcon", "earcons", "Отмена запроса", "Сигнал при отмене обращения к Алисе."),
    Field::toggle("earcon_stop", "/aliced/playStopRecognitionEarcon", "earcons", "Завершение распознавания", "Сигнал после окончания голосового ввода."),
    Field::toggle("earcon_error", "/aliced/playRecognitionErrorEarcon", "earcons", "Ошибка распознавания", "Сигнал, когда распознать запрос не удалось."),
    Field::number("alarm_start", "/alarmd/startAlarmUserVolume", "alarms", "Начальная громкость", "Шкала колонки от 1 до 10.", 1., 10., 1.),
    Field::number("alarm_finish", "/alarmd/finishAlarmUserVolume", "alarms", "Конечная громкость", "Не ниже начальной. Одинаковые значения отключают нарастание.", 1., 10., 1.),
    Field::number("alarm_step", "/alarmd/alarmVolumeStepMs", "alarms", "Интервал нарастания, с", "Время между шагами увеличения громкости будильника.", 1., 300., 1.).scaled(1000.),
    Field::number("reminder_min", "/alarmd/minimumReminderUserVolume", "alarms", "Минимальная громкость напоминаний", "Шкала колонки от 1 до 10.", 1., 10., 1.),
    Field::toggle("media_cache", "/audioclient/controller/mediaCacheEnabled", "buffer", "Кэшировать музыку", "Штатный audioclient сохраняет данные в настроенном каталоге кэша."),
    Field::number("buffer_low", "/audioclient/controller/playbackParams/TRACK/gstProperties/DECODEBIN/low-percent", "buffer", "Нижний порог буфера, %", "Должен быть меньше верхнего порога.", 0., 99., 1.).string(),
    Field::number("buffer_high", "/audioclient/controller/playbackParams/TRACK/gstProperties/DECODEBIN/high-percent", "buffer", "Верхний порог буфера, %", "Порог заполнения для продолжения воспроизведения; меньший буфер чувствительнее к сети.", 1., 100., 1.).string(),
    Field::number("buffer_seconds", "/audioclient/controller/playbackParams/TRACK/gstProperties/DECODEBIN/max-size-time", "buffer", "Максимальный буфер, с", "Параметр потока TRACK в audioclient, включая переданный ему поток AirPlay.", 0.1, 60., 0.1).scaled(1_000_000_000.),
    Field::toggle("vad", "/aliced/vadEnabled", "microphones", "Детектор голосовой активности", "Флаг VAD в aliced. Не является автоматическим распознаванием шёпота."),
    Field::toggle("broken_mic", "/audiod/brokenMicDetector/enabled", "microphones", "Проверка неисправных микрофонов", "Детектор сравнивает уровни микрофонных каналов."),
    Field::number("mic_ratio", "/audiod/brokenMicDetector/rmsRatioThreshold", "microphones", "Порог отношения RMS", "Порог детектора неисправных микрофонов.", 0.01, 1., 0.01),
    Field::number("mic_count", "/audiod/brokenMicDetector/rmsRatioExceededCount", "microphones", "Число превышений порога", "Сколько превышений учитывать при определении неисправности.", 1., 1000., 1.),
    Field::read("vqe_type", "/audiod/audioDevice/vqe/type", "microphones", "Обработка звука", "Тип движка из прошивки."),
    Field::read("vqe_preset", "/audiod/audioDevice/vqe/preset", "microphones", "Профиль микрофонов", "Аппаратный профиль модели колонки. Показываем без изменения калибровки."),
    Field::read("vqe_models", "/audiod/audioDevice/vqe/defaultModelStoragePath", "recognition", "Модели обработки звука", "Каталог прошивки; модель сервера для STT настраивается отдельно."),
    Field::read("asr_models", "/aliced/audioInput/model_storage/defaultModelsPath", "recognition", "Модели распознавания", "Каталог штатных моделей. Произвольная замена модели через форму не поддерживается."),
    Field::number("volume_timeout", "/smart_volume/volumeChangeDisableTimeout", "volume", "Пауза после ручной громкости", "Параметр volumeChangeDisableTimeout в единицах прошивки.", 0., 3600., 1.),
    Field::number("volume_quantile", "/smart_volume/splQuantile", "volume", "Квантиль шума", "Параметр оценки окружающего шума: от 0 до 1.", 0.01, 1., 0.01),
    Field::read("volume_strategy", "/smart_volume/volumeSelectionStrategy", "volume", "Правило выбора громкости", "Текущая стратегия прошивки."),
    Field::read("volume_restore", "/smart_volume/volumeRestoreRule", "volume", "Восстановление громкости", "Текущее правило прошивки; save_if_manual сохраняет ручное изменение."),
    Field::read("volume_thresholds", "/smart_volume/percentThresholds", "volume", "Пороги изменения громкости", "Таблица переходов по уровню шума, включая верхние пороги и целевую громкость."),
    Field::read("volume_calibration", "/smart_volume/volumeLevelSplsPercent", "volume", "Калибровка шума и громкости", "Штатная таблица SPL и громкости. Аппаратную калибровку оставляем только для просмотра."),
    Field::toggle("mic_audio_reports", "/audiod/brokenMicDetector/sendAudioData", "privacy", "Аудио для диагностики микрофонов", "Разрешить детектору отправлять диагностическое аудио. Не включает отключённые службы метрик."),
    Field::toggle("button_support", "/builtin_buttons_capability/supports_blocking_by_user", "buttons", "Разрешить штатную блокировку кнопок", "Флаг поддержки блокировки пользователем; сам по себе не блокирует нажатия."),
    Field::read("button_flag", "/builtin_buttons_capability/flag_path_override", "buttons", "Файл состояния блокировки", "Путь, который использует прошивка."),
];

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Change {
    pub id: String,
    pub previous: Value,
    pub value: Value,
}

pub(super) fn fields(scope: &str) -> Result<&'static [Field], String> {
    match scope { "user" => Ok(USER), "firmware" => Ok(FIRMWARE), _ => Err("Неизвестный источник настроек".into()) }
}

pub(super) fn apply(config: &Value, scope: &str, group: &str, changes: &[Change]) -> Result<Value, String> {
    if changes.is_empty() || changes.len() > 30 { return Err("Выберите от 1 до 30 изменений".into()); }
    let fields = fields(scope)?;
    let mut seen = std::collections::HashSet::new();
    let mut next = config.clone();
    for change in changes {
        let field = fields.iter().find(|f| f.id == change.id && f.group == group).ok_or("Неизвестное поле этой группы")?;
        if !seen.insert(&change.id) { return Err("Поле указано повторно".into()); }
        let current = field.value(config).ok_or_else(|| format!("Поле «{}» отсутствует в этой прошивке", field.label))?;
        // JSON 1 and 1.0 represent the same editable number.
        let same = if let (Some(a), Some(b)) = (current.as_f64(), change.previous.as_f64()) { a == b } else { current == change.previous };
        if !same { return Err(format!("Поле «{}» уже изменилось. Перечитайте значения и повторите изменение.", field.label)); }
        let value = field.encode(&change.value)?;
        *next.pointer_mut(field.path).ok_or("Структура настроек изменилась")? = value;
    }
    if group == "alarms" && next.pointer("/alarmd/startAlarmUserVolume").and_then(Value::as_f64) > next.pointer("/alarmd/finishAlarmUserVolume").and_then(Value::as_f64) {
        return Err("Конечная громкость будильника должна быть не ниже начальной".into());
    }
    if group == "buffer" {
        let low = fields.iter().find(|f| f.id == "buffer_low").and_then(|f| f.value(&next)).and_then(|v| v.as_f64());
        let high = fields.iter().find(|f| f.id == "buffer_high").and_then(|f| f.value(&next)).and_then(|v| v.as_f64());
        if matches!((low, high), (Some(a), Some(b)) if a >= b) { return Err("Нижний порог буфера должен быть меньше верхнего".into()); }
    }
    Ok(next)
}
