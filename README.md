# alice-server

Свой сервер для Яндекс.Станции (Quasar) с голосовым ассистентом. Он эмулирует облако Яндекса, с которым общается прошивка станции, а ответы пользователю готовит LLM через [OpenRouter](https://openrouter.ai). Речь распознаётся и синтезируется через OpenRouter или локально: GigaAM, Piper, Vosk.

## Возможности

- **Станция работает без облака Яндекса.** Сервер отвечает за `uniproxy.alice.yandex.net` (голосовой канал uni.ws), `quasar.yandex.net` (регистрация, конфиг станции, glagol-токены, обновления), `oauth.yandex.ru`, `mobileproxy.passport.yandex.net`, `rpc.alice.yandex.net`, `clck.yandex.net`, `scbh.yandex.net` и заглушки метрик.
- **Голосовой диалог.** Речь со станции распознаётся, LLM пишет ответ, и станция начинает говорить, пока модель ещё дописывает текст.
- **Распознавание речи:**
  - OpenRouter (Whisper);
  - GigaAM v3 через [gigastt](https://github.com/ekhodzitsky/gigastt). Он распознаёт речь, пока станция записывает, и сам замечает конец фразы, так что станция перестаёт слушать сразу после паузы, а не через 5 секунд.
- **Синтез речи:** OpenRouter, [Piper](https://github.com/OHF-Voice/piper1-gpl) или [Vosk TTS](https://github.com/alphacep/vosk-tts). Образы локальных синтезаторов лежат в `docker/`. Если локальный синтезатор не ответил, фразу озвучивает OpenRouter.
- **Функции:**
  - управление станцией: громкость, пауза, Bluetooth, звуки умного дома;
  - музыка из [Spotify](#spotify): станция становится колонкой Spotify Connect;
  - свои HTTP-серверы функций и состояния;
  - отложенные вызовы («выключи свет через пять минут»);
  - продолжение диалога без слова «Алиса».
- **Время по часовому поясу станции.** Станция присылает свою зону с каждым запросом.
- **API управления с документацией OpenAPI:**
  - список станций и их конфиги;
  - события для ассистента;
  - произнесение фраз;
  - сырые директивы.
- **Хранилище станций.** Станции хранятся в PostgreSQL. Если база недоступна, они хранятся в памяти до перезапуска.

## Требования

- **Станция с root-доступом,** в конфиге которой адреса Яндекса заменены на адрес этого сервера (см. [Подключение станции](#подключение-станции)). Проверено на Яндекс.Станции Миди с прошивкой 0.369.
- **Токен OpenRouter** ([openrouter.ai/keys](https://openrouter.ai/settings/keys)).
- **Docker,** где ffmpeg уже есть в образе. Или Rust и ffmpeg, чтобы собрать сервер из исходников.
- **По желанию:**
  - PostgreSQL;
  - gigastt для распознавания;
  - Piper или Vosk для синтеза.

## Запуск

В Docker:

```bash
docker build -t alice-server .
docker run -d --name alice-server -p 31115:31115 -p 31116:31116 \
  -e OPENROUTER_API_KEY=sk-or-... -e TZ=Europe/Moscow \
  alice-server
```

Из исходников (нужен `ffmpeg` в `PATH`):

```bash
OPENROUTER_API_KEY=sk-or-... cargo run --release
```

Переменные окружения можно положить в `.env.local` или `.env` в рабочей папке сервера.

## Подключение станции

Домены Яндекса сервер отдаёт как префиксы пути на порту `PORT` (по умолчанию `31115`). В конфиге станции (`quasar.cfg`) адрес каждого домена заменяется на адрес сервера с именем домена в начале пути, например:

| Было | Стало |
|---|---|
| `wss://uniproxy.alice.yandex.net/uni.ws` | `ws://192.168.1.10:31115/uniproxy.alice.yandex.net/uni.ws` |
| `https://quasar.yandex.net` | `http://192.168.1.10:31115/quasar.yandex.net` |

Станция регистрируется сама при первых запросах к `quasar.yandex.net` и появляется в `GET /devices` API управления.

## Настройка

### Основное

| Переменная | По умолчанию | Описание |
|---|---|---|
| `OPENROUTER_API_KEY` | — | **Обязательна.** Токен OpenRouter |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai/api/v1` | Любой OpenRouter-совместимый API |
| `OPENROUTER_TIMEOUT_MS` | `10000` | Таймаут запросов к OpenRouter |
| `PORT` | `31115` | Порт для станции: домены Яндекса и uni.ws |
| `API_PORT` | `31116` | Порт API управления |
| `POSTGRES_URL` | `postgres://quasar:quasar@localhost/quasar` | PostgreSQL. Если база недоступна, станции хранятся в памяти |
| `TZ` | — | Часовой пояс, если станция не прислала свой |
| `RUST_LOG` | `info` | Подробность логов |

### Диалог

| Переменная | По умолчанию | Описание |
|---|---|---|
| `OPENROUTER_PROCESSOR_MODEL` | `google/gemini-3.8-flash` | Модель, которая отвечает пользователю |
| `OPENROUTER_PROCESSOR_REASONING` | — | Глубина рассуждений: `none`, `minimal`, `low`, `medium`, `high`. Пустое значение оставляет настройку модели по умолчанию. Если модель не принимает параметр, сервер перестаёт его отправлять |
| `PROCESSOR_PROMPT_TEMPLATE_PATH` | `prompt.handlebars` | Шаблон системного промпта |
| `PROCESSOR_PROMPT_STATE_TEMPLATE_PATH` | `prompt-state.handlebars` | Шаблон сообщения о состоянии |
| `PROCESSOR_CACHE_SIZE` | `1000` | Сколько ответов помнить в кэше. `0` выключает кэш |
| `PROCESSOR_FUNCTION_SERVER_URLS` | — | [Серверы функций](#свои-функции-и-состояние) через запятую |
| `PROCESSOR_STATE_SERVER_URLS` | — | [Серверы состояния](#свои-функции-и-состояние) через запятую |

### Распознавание речи

| Переменная | По умолчанию | Описание |
|---|---|---|
| `STT_PROVIDER` | `openrouter` | `openrouter`: Whisper после конца фразы. `gigaam`: потоково через gigastt |
| `OPENROUTER_STT_MODEL` | `openai/whisper-large-v3` | Модель распознавания OpenRouter |
| `OPENROUTER_STT_LANGUAGE` | `ru` | Язык речи. Пустое значение — модель определяет язык сама |
| `GIGASTT_URL` | — | Адрес gigastt, например `http://gigastt:9876`. Обязателен при `STT_PROVIDER=gigaam` |
| `STT_END_OF_SPEECH` | `server` с gigastt, иначе `station` | Кто решает, что фраза закончилась: сервер по паузе или станция по своему таймауту |
| `GIGASTT_MIN_SILENCE_MS` | `800` | Сколько тишины считать концом фразы. Работает, если у gigastt включён VAD |

### Синтез речи

| Переменная | По умолчанию | Описание |
|---|---|---|
| `TTS_PROVIDER` | `openrouter` | `openrouter`, либо `piper` для локального сервера Piper или Vosk |
| `OPENROUTER_TTS_MODEL` | `google/gemini-3.8-flash-lite-tts` | Модель синтеза OpenRouter |
| `OPENROUTER_TTS_VOICE` | `Kore` | Голос. Он должен быть у выбранной модели |
| `OPENROUTER_TTS_FORMAT` | `pcm` | `pcm` или `mp3`. `mp3` нужен для моделей, которые не отдают PCM |
| `OPENROUTER_TTS_PCM_SAMPLE_RATE` | `24000` | Частота PCM, которую отдаёт модель |
| `OPENROUTER_TTS_SPEED` | `1` | Скорость речи |
| `OPENROUTER_TTS_INSTRUCTIONS` | — | Инструкции для голоса, если модель их понимает |
| `OPENROUTER_TTS_STREAMING` | `true` | Говорить, пока LLM пишет ответ. `false`: ждать весь текст |
| `PIPER_URL` | — | Адрес сервера Piper или Vosk, например `http://piper:5000`. Обязателен при `TTS_PROVIDER=piper` |
| `PIPER_VOICE` | — | Голос Piper. По умолчанию — голос, с которым запущен сервер |
| `PIPER_SPEAKER`, `PIPER_SPEAKER_ID` | — | Голос многоголосой модели по имени или номеру |
| `PIPER_LENGTH_SCALE` | из `OPENROUTER_TTS_SPEED` | Темп: меньше 1 — быстрее, больше 1 — медленнее |
| `PIPER_FALLBACK` | `openrouter` | Кто озвучит фразу, если локальный синтезатор не ответил: `openrouter` или `none` |

### Прочее

| Переменная | По умолчанию | Описание |
|---|---|---|
| `GLAGOL_JWT_KEY` | новый при каждом запуске | Ключ подписи glagol-токенов. Задайте, чтобы токены переживали перезапуск |
| `ALLOW_UPDATES` | `false` | Разрешить станции обновлять прошивку |
| `UPDATES_CONFIG_PATH` | — | Список разрешённых версий прошивки (пример: `updates-config.yml`). Обновления приходят, только если заданы обе переменные |

## Распознавание через GigaAM

[gigastt](https://github.com/ekhodzitsky/gigastt) — сервер распознавания на модели GigaAM v3 от Сбера. Он слушает речь, пока станция записывает, поэтому текст готов почти сразу после конца фразы. С VAD он сам определяет, что пользователь замолчал.

```bash
docker run -d --name gigastt -p 9876:9876 -e GIGASTT_VAD=true \
  -v gigastt-models:/home/gigastt/.gigastt/models \
  ghcr.io/ekhodzitsky/gigastt:2.18.0
```

В alice-server:

```
STT_PROVIDER=gigaam
GIGASTT_URL=http://gigastt:9876
```

Без `GIGASTT_VAD=true` gigastt тоже замечает конец фразы, но грубее. Если gigastt недоступен или вернул ошибку, фразу распознаёт OpenRouter.

## Локальный синтез речи

Piper и Vosk работают на процессоре и отвечают по одному и тому же HTTP-протоколу (`POST /synthesize` возвращает WAV). Поэтому для обоих в alice-server задаётся одно и то же:

```
TTS_PROVIDER=piper
PIPER_URL=http://piper:5000
```

Оба образа собираются из корня репозитория и при первом запуске скачивают модель в том `/data`.

### Piper

Самый быстрый вариант: фраза синтезируется примерно за 0,1 с.

```bash
docker build -f docker/piper/Dockerfile -t piper .
docker run -d --name piper -p 5000:5000 -e PIPER_VOICE=ru_RU-irina-medium -v piper-data:/data piper
```

Русские голоса:
- `ru_RU-irina-medium` — женский;
- `ru_RU-dmitri-medium`, `ru_RU-denis-medium`, `ru_RU-ruslan-medium` — мужские.

Ударения Piper ставит по словарю espeak-ng и иногда ошибается.

### Vosk

Ударения Vosk берёт из большого словаря, и голоса звучат естественнее, чем у Piper. Зато он медленнее: 0,3–0,8 с на фразу в четыре потока на Ryzen 7 7735HS. Занимает около 2,3 ГБ памяти. Модель `vosk-model-tts-ru-0.9-multi` весит около 750 МБ.

```bash
docker build -f docker/vosk/Dockerfile -t vosk-tts .
docker run -d --name vosk-tts -p 5000:5000 -e VOSK_SPEAKER=female_2 -v vosk-data:/data vosk-tts
```

| Переменная | По умолчанию | Описание |
|---|---|---|
| `VOSK_SPEAKER` | `female_2` | Голос: `female_0`, `female_1`, `female_2`, `male_0`, `male_1` |
| `VOSK_THREADS` | `4` | Потоки процессора на один синтез. Больше потоков не значит быстрее: на виртуальной машине с 14 ядрами, где работают и другие сервисы, 14 потоков синтезировали вдвое медленнее четырёх |
| `VOSK_MODEL` | `vosk-model-tts-ru-0.9-multi` | Модель с [сайта Vosk](https://alphacephei.com/vosk/models) |

Голос можно выбрать и со стороны alice-server через `PIPER_SPEAKER=female_1`.

## Функции

LLM отвечает JSON-объектом: вызовы функций, флаг продолжения диалога, разрешение кэшировать ответ и текст. Формат описан в `prompt.handlebars`.

Встроенные функции управляют станцией:
- пауза;
- громкость от 1 до 10, громче и тише;
- Bluetooth;
- звуки включения и выключения умного дома.

Функции музыки даёт сервис [Spotify](#spotify).

Любой вызов можно отложить: модель добавляет к нему `"schedule": "5m"`. Так работает «выключи свет через пять минут» и «включи музыку на полчаса».

### Свои функции и состояние

**Сервер функций** (`PROCESSOR_FUNCTION_SERVER_URLS`) работает так:
- на `GET` отдаёт каталог функций:
  ```json
  { "turn_on_light": { "description": "Включает свет", "arguments": { "room": { "type": "string" } }, "hasResponse": false } }
  ```
- вызов приходит как `PATCH` с телом `{"name", "arguments", "metadata", "sessionId"}`;
- если у функции `"hasResponse": true`, тело ответа передаётся модели, и она отвечает пользователю по нему. Так работают вопросы вроде «какая погода». Такие функции нельзя отложить.

Каталоги читаются при запуске alice-server, поэтому серверы функций должны быть уже доступны.

**Сервер состояния** (`PROCESSOR_STATE_SERVER_URLS`) на `POST {"metadata", "sessionId"}` отдаёт словарь `{"ключ": {"description": "...", "value": "..."}}`. Модель видит его в каждом ходе вместе с датой и временем станции.

## Spotify

Сервис `docker/spotify` делает станцию колонкой Spotify Connect. Внутри работает [librespot](https://github.com/librespot-org/librespot), и в приложении Spotify появляется устройство «Алиса». Когда на нём включают музыку, сервис кодирует звук в MP3 и через API управления alice-server отправляет станции `audio_play` со ссылкой на поток. Станция играет его своим плеером и сама приглушает музыку, когда говорит Алиса. Пауза или выбор другого устройства в приложении ставит станцию на паузу. Нужен Spotify Premium.

```bash
docker build -f docker/spotify/Dockerfile -t spotify-bridge .
docker run -d --name spotify -p 8090:8090 -v spotify-data:/data \
  -e STREAM_URL=http://192.168.1.10:8090/stream.mp3 \
  -e ALICE_API_URL=http://alice-server:31116 \
  spotify-bridge
```

Сборка librespot занимает несколько минут. Порт 8090 должен быть доступен станции: она забирает поток сама.

**Вход в Spotify** нужен один раз:
1. Откройте `http://<сервер>:8090/` и перейдите по ссылке входа.
2. Войдите в Spotify. Браузер перейдёт на адрес `http://127.0.0.1/login?code=…` и покажет ошибку, так и должно быть.
3. Скопируйте этот адрес и вставьте его на странице сервиса.

Вход сохраняется в томе `/data`.

| Переменная | По умолчанию | Описание |
|---|---|---|
| `STREAM_URL` | — | **Обязательна.** Адрес `/stream.mp3` этого сервиса, по которому до него дотянется станция |
| `ALICE_API_URL` | — | API управления alice-server, например `http://alice-server:31116` |
| `STATION_ID` | `all` | Станция, которой отдаются команды (`duid` из `GET /devices`) |
| `SPOTIFY_NAME` | `Алиса` | Имя колонки в Spotify |
| `SPOTIFY_PROXY` | — | HTTP-прокси для librespot и Web API, если Spotify недоступен напрямую |
| `SPOTIFY_CLIENT_ID`, `SPOTIFY_CLIENT_SECRET` | — | Приложение с developer.spotify.com для голосового управления |
| `SPOTIFY_REDIRECT_URI` | `http://127.0.0.1:8888/callback` | Redirect URI этого приложения |
| `MP3_BITRATE` | `256k` | Битрейт потока |

**Голосовое управление:**
1. Создайте приложение на [developer.spotify.com](https://developer.spotify.com/dashboard) с Redirect URI `http://127.0.0.1:8888/callback`.
2. Задайте `SPOTIFY_CLIENT_ID` и `SPOTIFY_CLIENT_SECRET`.
3. Подключите управление на странице сервиса тем же способом, что и вход.
4. Добавьте сервис в alice-server и перезапустите его: каталог функций читается при запуске.

```
PROCESSOR_FUNCTION_SERVER_URLS=http://spotify:8090/functions
PROCESSOR_STATE_SERVER_URLS=http://spotify:8090/state
```

Ассистент получит функции:
- «включи <исполнитель, песня, альбом или плейлист>»;
- пауза;
- следующий и предыдущий трек;
- «что играет».

Без приложения работают только пауза, продолжение и «что играет».

**Пауза кнопкой станции.** Станция ещё около 15 секунд дочитывает поток, поэтому Spotify встаёт на паузу с задержкой. Кнопка воспроизведения на станции просит ассистента продолжить музыку.

## Промпты

`prompt.handlebars` — системный промпт: кто такая Алиса, как ей говорить и как вызывать функции. `prompt-state.handlebars` — сообщение с текущим состоянием. Файлы перечитываются при каждом ответе, поэтому правки действуют сразу, без перезапуска. В Docker они лежат в `/app`, а свои версии можно примонтировать поверх.

Чтобы станция начинала говорить до конца ответа LLM, поле `"text"` в ответе модели должно идти последним, как в примерах промпта.

## API управления

Работает на порту `API_PORT` (по умолчанию `31116`). Документация: `/openapi`, схема: `/openapi/json`. Страница для отладки — `/debug`: на ней можно выбрать станцию и отправить ей текст как реплику пользователя, как фразу или как событие.

| Запрос | Что делает |
|---|---|
| `GET /devices` | Список станций |
| `PATCH /devices/{duid}` | Меняет имя и конфиг станции (`{"name", "quasarConfig"}`); станция перечитывает конфиг |
| `POST /devices/{duid}/push` | Событие для ассистента: `{"eventText": "..."}`. Модель сама решает, что сказать или сделать |
| `POST /devices/{duid}/push-raw` | Произнести текст как есть: `{"eventText": "..."}` |
| `POST /devices/{duid}/push-text` | Текст как будто его сказали голосом: `{"eventText": "..."}`. Ассистент отвечает вслух и выполняет команды |
| `POST /devices/{duid}/push-directive` | Сырая директива станции (`TDirective`) |

Вместо `{duid}` можно указать `all`, тогда запрос уйдёт всем подключённым станциям:

```bash
curl -X POST http://localhost:31116/devices/all/push-raw \
  -H 'content-type: application/json' -d '{"eventText": "Ужин готов!"}'
```

## Разработка

```bash
cargo build
cargo test
```

Схемы protobuf из `protos/` компилируются при сборке (protox и prost), системный `protoc` не нужен.

## Лицензия

GPL-3.0
