# alice-server (Rust)

Rust-переписывание [alice-server](../alice-server) — эмулятора backend-сервера Яндекс.Станции с голосовым ассистентом, у которого все AI-вызовы (STT, TTS, диалоговый «мозг») идут через один токен OpenRouter.

## Статус: фазы 1–3 — портирование завершено

**Готово:**
- protobuf-кодген из оригинальных схем quasar (protox + prost, без системного protoc) — `TClientMessage`/`TServerMessage` и вся ветка NAlice
- uni.ws на axum: AAPI-фреймы (`AAPI` + protobuf / 4-байтовый stream id + аудио), VoiceInput → сбор аудио → StreamControl close → конвейер ответа
- OpenRouter-клиент: `/audio/transcriptions` (JSON + base64 WAV), `/audio/speech` (байтовый стрим PCM), `/chat/completions` (SSE-стриминг дельт)
- ffmpeg: OGG→WAV для STT и потоковый PCM→OGG/Opus для TTS (с таймаутами жизни процесса)
- агент: JSON-протокол LLM (`function_calls` с `schedule`, `text`, `continue_dialog`, `can_cache`), агентский цикл с возвратом результатов функций, LRU-кэш первого хода, handlebars-промпты (те же файлы, что в TS-версии)
- ответ станции: AsrResult, AliceResponse, TtsSpeak + потоковое аудио, StreamControl

**Фаза 2 (готово):** эмуляция доменов — `quasar.yandex.net` (register, check_updates, update_device_state, glagol/token + check_token v1/v2 на HS256-JWT, glagol/device_list, get_sync_info, push_subscribe), `oauth.yandex.ru/token`, `clck.yandex.net`, `rpc.alice.yandex.net` (пустой protobuf), `scbh` (generate_204), заглушки startup/appmetrica; реестр станций in-memory с самоподписанными сертификатами (rcgen). Хранилище PostgreSQL — фаза 3.

**Фаза 3 (готово):** PostgreSQL-хранилище станций (с фолбэком in-memory), management-API на `API_PORT` (`GET /devices`, `POST /devices/{duid}/push` — событие в агента, `POST /devices/{duid}/push-raw` — прямая озвучка; `duid=all` для всех), реестр live uni.ws-соединений, updates-конфиг (`ALLOW_UPDATES` + `updates-config.yml`), remote function/state-серверы (HTTP-протокол processor; `PROCESSOR_FUNCTION_SERVER_URLS` / `PROCESSOR_STATE_SERVER_URLS`), Docker-образ.

**Не портировано (осознанно):** MCP-клиент (rmcp) — добавим при необходимости; точная форма AliceResponse/`get_sync_info` требует сверски на живой станции — на текущий момент проверено на протокольном уровне, не на прошивке.

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
