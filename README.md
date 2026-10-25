# alice-server (Rust)

Rust-переписывание [alice-server](../alice-server) — эмулятора backend-сервера Яндекс.Станции с голосовым ассистентом, у которого все AI-вызовы (STT, TTS, диалоговый «мозг») идут через один токен OpenRouter.

## Статус: фаза 1 из 3 — рабочий скелет голосового конвейера

**Готово:**
- protobuf-кодген из оригинальных схем quasar (protox + prost, без системного protoc) — `TClientMessage`/`TServerMessage` и вся ветка NAlice
- uni.ws на axum: AAPI-фреймы (`AAPI` + protobuf / 4-байтовый stream id + аудио), VoiceInput → сбор аудио → StreamControl close → конвейер ответа
- OpenRouter-клиент: `/audio/transcriptions` (JSON + base64 WAV), `/audio/speech` (байтовый стрим PCM), `/chat/completions` (SSE-стриминг дельт)
- ffmpeg: OGG→WAV для STT и потоковый PCM→OGG/Opus для TTS (с таймаутами жизни процесса)
- агент: JSON-протокол LLM (`function_calls` с `schedule`, `text`, `continue_dialog`, `can_cache`), агентский цикл с возвратом результатов функций, LRU-кэш первого хода, handlebars-промпты (те же файлы, что в TS-версии)
- ответ станции: AsrResult, AliceResponse, TtsSpeak + потоковое аудио, StreamControl

**Фаза 2 (следующее):** HTTP-эмуляция доменов (`quasar.yandex.net`: check_updates/register/device_list/glagol-токены, `get_sync_info`, заглушки), PostgreSQL-хранилище, генерация TLS-сертификатов, API управления (`/devices`), полная форма AliceResponse (настройки/карточки — сверить с прошивкой).

**Фаза 3:** remote function/state-серверы и MCP, обновления прошивки (updates-config.yml), Docker.

> До окончания фазы 2 станцию подключать к этой версии нельзя — используйте TS-версию (`../alice-server`).

## Запуск

```bash
cargo build --release
OPENROUTER_API_KEY=sk-or-... cargo run --release
```

Енвы те же, что в TS-версии (`PORT`, `OPENROUTER_*`, `PROCESSOR_*`, см. README TS-версии); конфиг — `.env.local` / `.env`.

## Тесты

```bash
cargo test   # протокол агента, schedule-парсер, ffmpeg OGG→WAV roundtrip
```

## Известные упрощения фазы 1

- директивы станции отправляются как `TDirective{name, type: client_action}` без protobuf-`payload` (маппинг аргументов — фаза 2);
- функции исполняются после завершения стрима LLM, а не в момент стриминга (как в TS);
- AliceResponse содержит минимальный набор полей — полную форму нужно сверить на живой станции.

## Лицензия

GPL-3.0
