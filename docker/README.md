# Запуск сервисов

alice-server работает и один: речь и ответы идут через OpenRouter. Остальные контейнеры добавляют локальное распознавание и синтез речи, Spotify и базу для станций. Здесь описано, как их запустить и подключить к alice-server. Настройки самого alice-server — в [основном README](../README.md#настройка).

| Сервис | Зачем | Образ | Порт | Данные |
|---|---|---|---|---|
| [PostgreSQL](#postgresql) | Хранит станции между перезапусками alice-server | `postgres:15` | 5432 | `/var/lib/postgresql/data` |
| [gigastt](#gigastt) | Распознаёт речь моделью GigaAM v3 и замечает конец фразы | `ghcr.io/ekhodzitsky/gigastt:2.18.0` | 9876 | `/home/gigastt/.gigastt/models` |
| [Piper](#piper) | Синтез речи, самый быстрый | `docker/piper` | 5000 | `/data` |
| [Vosk](#vosk) | Синтез речи, звучит естественнее Piper | `docker/vosk` | 5000 | `/data` |
| [Spotify](#spotify) | Делает станцию колонкой Spotify Connect | `docker/spotify` | 8090 | `/data` |

Любой из них можно не запускать, а из синтезаторов достаточно одного. Образы из `docker/` собираются из корня репозитория.

## Как сервисы связаны

- **Общая сеть.** Контейнеры обращаются друг к другу по имени, например `http://gigastt:9876`. В стандартной сети Docker имена не работают, поэтому для `docker run` нужна своя сеть: `docker network create alice`. Docker Compose создаёт её сам.
- **Наружу публикуются только три порта:**
  - `31115` alice-server — к нему подключается станция;
  - `8090` Spotify — отсюда станция сама забирает музыку, здесь же страница входа в Spotify;
  - `31116` alice-server — API управления и страница `/debug`, по желанию.

  gigastt, Piper, Vosk и PostgreSQL нужны только alice-server, их порты публиковать не надо.
- **alice-server запускается последним.** К базе он подключается и каталоги функций читает только при запуске. Если база не отвечала, станции хранятся в памяти до перезапуска. Если не отвечал Spotify, ассистент не узнает о его функциях, пока alice-server не перезапустят. gigastt и синтезатор могут подняться и позже: пока их нет, речь распознаёт и озвучивает OpenRouter.
- **Только домашняя сеть.** У API управления, страницы Spotify и порта станции нет авторизации. Не открывайте их в интернет.

### Какая переменная в какой контейнер

Каждый контейнер читает только свои переменные. Например, `GIGASTT_VAD` в окружении alice-server ничего не делает: её читает gigastt.

| Контейнер | Переменные |
|---|---|
| alice-server | `OPENROUTER_API_KEY`, `POSTGRES_URL`, `STT_PROVIDER`, `GIGASTT_URL`, `GIGASTT_MIN_SILENCE_MS`, `TTS_PROVIDER`, `PIPER_URL`, `PROCESSOR_FUNCTION_SERVER_URLS`, `PROCESSOR_STATE_SERVER_URLS` и остальные из [основного README](../README.md#настройка) |
| PostgreSQL | `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DB` |
| gigastt | `GIGASTT_VAD` |
| Piper | `PIPER_VOICE` |
| Vosk | `VOSK_SPEAKER`, `VOSK_THREADS`, `VOSK_MODEL` |
| Spotify | `STREAM_URL`, `ALICE_API_URL`, `STATION_ID`, `SPOTIFY_CLIENT_ID`, `SPOTIFY_CLIENT_SECRET` и другие `SPOTIFY_*`, `MP3_BITRATE` |

`PIPER_VOICE` есть и у alice-server, но голос удобнее выбирать в контейнере Piper: он скачивает только этот голос.

## Всё вместе через Docker Compose

Положите в корень репозитория `compose.yaml`:

```yaml
services:
  postgres:
    image: postgres:15
    restart: unless-stopped
    environment:
      POSTGRES_USER: quasar
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?задайте POSTGRES_PASSWORD в .env}
      POSTGRES_DB: quasar
    volumes:
      - postgres-data:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U quasar -d quasar"]
      interval: 5s
      retries: 20

  gigastt:
    image: ghcr.io/ekhodzitsky/gigastt:2.18.0
    restart: unless-stopped
    environment:
      GIGASTT_VAD: "true"
    volumes:
      - gigastt-models:/home/gigastt/.gigastt/models

  tts:
    build:
      context: .
      dockerfile: docker/piper/Dockerfile  # для Vosk: docker/vosk/Dockerfile
    restart: unless-stopped
    volumes:
      - tts-data:/data

  spotify:
    build:
      context: .
      dockerfile: docker/spotify/Dockerfile
    restart: unless-stopped
    ports:
      - "8090:8090"
    environment:
      STREAM_URL: http://${HOST_IP:?задайте HOST_IP в .env}:8090/stream.mp3
      ALICE_API_URL: http://alice-server:31116
      SPOTIFY_CLIENT_ID: ${SPOTIFY_CLIENT_ID:-}
      SPOTIFY_CLIENT_SECRET: ${SPOTIFY_CLIENT_SECRET:-}
    volumes:
      - spotify-data:/data

  alice-server:
    build: .
    restart: unless-stopped
    ports:
      - "31115:31115"
      - "31116:31116"
    environment:
      OPENROUTER_API_KEY: ${OPENROUTER_API_KEY:?задайте OPENROUTER_API_KEY в .env}
      GLAGOL_JWT_KEY: ${GLAGOL_JWT_KEY:-}
      TZ: Europe/Moscow
      POSTGRES_URL: postgres://quasar:${POSTGRES_PASSWORD}@postgres/quasar
      STT_PROVIDER: gigaam
      GIGASTT_URL: http://gigastt:9876
      TTS_PROVIDER: piper
      PIPER_URL: http://tts:5000
      PROCESSOR_FUNCTION_SERVER_URLS: http://spotify:8090/functions
      PROCESSOR_STATE_SERVER_URLS: http://spotify:8090/state
    depends_on:
      postgres:
        condition: service_healthy
      spotify:
        condition: service_healthy

volumes:
  postgres-data:
  gigastt-models:
  tts-data:
  spotify-data:
```

Рядом — `.env` со своими значениями:

```
# адрес этой машины в домашней сети: по нему станция забирает музыку
HOST_IP=192.168.1.10
OPENROUTER_API_KEY=sk-or-...
# пароль базы, например из openssl rand -hex 16
POSTGRES_PASSWORD=
# ключ glagol-токенов, например из openssl rand -hex 32; без него ключ новый при каждом запуске
GLAGOL_JWT_KEY=
# приложение с developer.spotify.com для голосового управления Spotify, по желанию
SPOTIFY_CLIENT_ID=
SPOTIFY_CLIENT_SECRET=
```

Запуск:

```bash
docker compose up -d --build
docker compose logs -f alice-server
```

- Первая сборка идёт долго: librespot для Spotify компилируется несколько минут. При первом запуске gigastt и Piper скачивают модели в тома.
- alice-server стартует, когда база и Spotify пройдут проверку здоровья. Spotify проверяется раз в 30 секунд.
- Чтобы взять Vosk вместо Piper, поменяйте `dockerfile` у сервиса `tts`. `PIPER_URL` останется прежним.
- Ненужный сервис уберите вместе с его переменными у alice-server и упоминанием в `depends_on`. Для базы это `POSTGRES_URL`, для gigastt — `STT_PROVIDER` и `GIGASTT_URL`, для синтезатора — `TTS_PROVIDER` и `PIPER_URL`, для Spotify — `PROCESSOR_*`.
- В `.env` лежат ключи. Он записан в `.gitignore` и в git не попадёт.

Затем [войдите в Spotify](#вход-в-spotify) и [подключите станцию](../README.md#подключение-станции).

## По одному через docker run

Сначала общая сеть:

```bash
docker network create alice
```

Затем сервисы в порядке разделов ниже, alice-server — последним.

## PostgreSQL

```bash
docker run -d --name postgres --network alice --restart unless-stopped \
  -e POSTGRES_USER=quasar -e POSTGRES_PASSWORD=<пароль> -e POSTGRES_DB=quasar \
  -v postgres-data:/var/lib/postgresql/data postgres:15
```

В alice-server:

```
POSTGRES_URL=postgres://quasar:<пароль>@postgres/quasar
```

Пароль входит в URL, поэтому лучше без спецсимволов, например из `openssl rand -hex 16`. Таблицу станций alice-server создаёт сам. Если база подключилась, при запуске он пишет в лог `postgres storage connected`.

## gigastt

[gigastt](https://github.com/ekhodzitsky/gigastt) распознаёт речь, пока станция записывает, и замечает, когда пользователь замолчал.

```bash
docker run -d --name gigastt --network alice --restart unless-stopped \
  -e GIGASTT_VAD=true -v gigastt-models:/home/gigastt/.gigastt/models \
  ghcr.io/ekhodzitsky/gigastt:2.18.0
```

В alice-server:

```
STT_PROVIDER=gigaam
GIGASTT_URL=http://gigastt:9876
```

- При первом запуске gigastt скачивает модель в том. Пока она загружается, речь распознаёт OpenRouter.
- `GIGASTT_VAD=true` включает детектор речи Silero VAD, и конец фразы определяется точнее. Без него gigastt заканчивает фразу по паузам, которые замечает сам распознаватель, и это грубее.
- Сколько тишины ждать после последнего слова, задаёт `GIGASTT_MIN_SILENCE_MS` в alice-server.

Проверка: скажите что-нибудь станции. В логе alice-server появится `end of utterance by gigastt (vad, …)`. Если вместо `vad` написано `blank`, gigastt работает без VAD.

## Piper

[Piper](https://github.com/OHF-Voice/piper1-gpl) синтезирует фразу примерно за 0,1 с.

```bash
docker build -f docker/piper/Dockerfile -t piper .
docker run -d --name piper --network alice --restart unless-stopped \
  -e PIPER_VOICE=ru_RU-irina-medium -v piper-data:/data piper
```

В alice-server:

```
TTS_PROVIDER=piper
PIPER_URL=http://piper:5000
```

| Переменная | По умолчанию | Описание |
|---|---|---|
| `PIPER_VOICE` | `ru_RU-irina-medium` | Голос из [каталога Piper](https://huggingface.co/rhasspy/piper-voices). Скачивается в том `/data` при первом запуске, около 63 МБ |

Русские голоса:
- `ru_RU-irina-medium` — женский;
- `ru_RU-dmitri-medium`, `ru_RU-denis-medium`, `ru_RU-ruslan-medium` — мужские.

Ударения Piper ставит по словарю espeak-ng и иногда ошибается.

## Vosk

[Vosk TTS](https://github.com/alphacep/vosk-tts) берёт ударения из большого словаря, и голоса звучат естественнее, чем у Piper. Зато он медленнее: 0,3–0,8 с на фразу в четыре потока на Ryzen 7 7735HS. Занимает около 2,3 ГБ памяти и синтезирует по одной фразе за раз.

```bash
docker build -f docker/vosk/Dockerfile -t vosk-tts .
docker run -d --name vosk --network alice --restart unless-stopped \
  -e VOSK_SPEAKER=female_2 -v vosk-data:/data vosk-tts
```

Vosk отвечает по протоколу Piper, поэтому в alice-server:

```
TTS_PROVIDER=piper
PIPER_URL=http://vosk:5000
```

| Переменная | По умолчанию | Описание |
|---|---|---|
| `VOSK_SPEAKER` | `female_2` | Голос: `female_0`, `female_1`, `female_2`, `male_0`, `male_1` |
| `VOSK_THREADS` | `4` | Потоки процессора на один синтез. Больше потоков не значит быстрее: на виртуальной машине с 14 ядрами, где работают и другие сервисы, 14 потоков синтезировали вдвое медленнее четырёх |
| `VOSK_MODEL` | `vosk-model-tts-ru-0.9-multi` | Модель с [сайта Vosk](https://alphacephei.com/vosk/models) |

Модель весит около 750 МБ и скачивается в том `/data` при первом запуске. Пока она не загрузится, контейнер не станет `healthy`. Голос можно выбрать и со стороны alice-server: `PIPER_SPEAKER=female_1`.

## Spotify

Делает станцию колонкой Spotify Connect: внутри работает [librespot](https://github.com/librespot-org/librespot), и в приложении Spotify появляется устройство «Алиса». Как это устроено, описано в [основном README](../README.md#spotify). Нужен Spotify Premium.

```bash
docker build -f docker/spotify/Dockerfile -t spotify-bridge .
docker run -d --name spotify --network alice --restart unless-stopped \
  -p 8090:8090 -v spotify-data:/data \
  -e STREAM_URL=http://192.168.1.10:8090/stream.mp3 \
  -e ALICE_API_URL=http://alice-server:31116 \
  spotify-bridge
```

- `192.168.1.10` замените адресом этой машины в домашней сети. Поток станция открывает сама, поэтому `localhost` и имена контейнеров в `STREAM_URL` не подходят.
- Если порт 8090 на машине занят, опубликуйте другой, например `-p 8095:8090`, и укажите его в `STREAM_URL`: `http://192.168.1.10:8095/stream.mp3`.
- Сборка librespot занимает несколько минут.
- В томе `/data` лежат вход в Spotify и токены его API. Никому не передавайте его содержимое.

| Переменная | По умолчанию | Описание |
|---|---|---|
| `STREAM_URL` | — | **Обязательна.** Адрес `/stream.mp3` этого сервиса, по которому до него дотянется станция |
| `ALICE_API_URL` | — | API управления alice-server, например `http://alice-server:31116`. Без него станция не узнает, что Spotify начал играть |
| `STATION_ID` | `all` | Станция, которой отдаются команды (`duid` из `GET /devices`) |
| `SPOTIFY_NAME` | `Алиса` | Имя колонки в Spotify |
| `SPOTIFY_PROXY` | — | HTTP-прокси для librespot и Web API, если Spotify недоступен напрямую |
| `SPOTIFY_CLIENT_ID`, `SPOTIFY_CLIENT_SECRET` | — | Приложение с developer.spotify.com для [голосового управления](#голосовое-управление) |
| `SPOTIFY_REDIRECT_URI` | `http://127.0.0.1:8888/callback` | Redirect URI этого приложения |
| `MP3_BITRATE` | `256k` | Битрейт потока |

### Вход в Spotify

Нужен один раз:
1. Откройте `http://192.168.1.10:8090/` и перейдите по ссылке входа.
2. Войдите в Spotify. Браузер перейдёт на адрес `http://127.0.0.1/login?code=…` и покажет ошибку, так и должно быть.
3. Скопируйте этот адрес и вставьте его на странице сервиса.

После входа в приложении Spotify появится устройство «Алиса».

### Голосовое управление

Чтобы ассистент управлял музыкой голосом, добавьте сервис в alice-server и перезапустите его: каталог функций читается при запуске.

```
PROCESSOR_FUNCTION_SERVER_URLS=http://spotify:8090/functions
PROCESSOR_STATE_SERVER_URLS=http://spotify:8090/state
```

Так работают пауза, продолжение и «что играет». Для поиска музыки и переключения треков нужно ещё приложение Spotify:
1. Создайте приложение на [developer.spotify.com](https://developer.spotify.com/dashboard) с Redirect URI `http://127.0.0.1:8888/callback`.
2. Задайте его `SPOTIFY_CLIENT_ID` и `SPOTIFY_CLIENT_SECRET` контейнеру Spotify, а не alice-server.
3. Откройте страницу сервиса и подключите управление тем же способом, что и вход. Страница напишет «Голосовое управление подключено».

## alice-server

Последним запускается сам alice-server в той же сети. Ключи удобнее передать файлом, чем в командной строке:

```bash
docker build -t alice-server .
docker run -d --name alice-server --network alice --restart unless-stopped \
  -p 31115:31115 -p 31116:31116 --env-file .env alice-server
```

`.env` для всех сервисов выше:

```
OPENROUTER_API_KEY=sk-or-...
TZ=Europe/Moscow
POSTGRES_URL=postgres://quasar:<пароль>@postgres/quasar
STT_PROVIDER=gigaam
GIGASTT_URL=http://gigastt:9876
TTS_PROVIDER=piper
PIPER_URL=http://piper:5000
PROCESSOR_FUNCTION_SERVER_URLS=http://spotify:8090/functions
PROCESSOR_STATE_SERVER_URLS=http://spotify:8090/state
```

Строки сервисов, которые вы не запускали, уберите. `.env` записан в `.gitignore` и в git не попадёт.

## Dokploy

Каждый сервис — отдельное приложение (Application) в одном проекте. Dokploy подключает приложения к сети `dokploy-network`, где каждое доступно по своему App Name, например `myapp-gigastt-1a2b3c`. Его и подставляйте в адреса вместо `gigastt`, `spotify` и других имён из примеров.

- **gigastt** — из образа `ghcr.io/ekhodzitsky/gigastt:2.18.0`.
- **PostgreSQL** удобно создать как базу Dokploy и взять для `POSTGRES_URL` её внутренний адрес подключения.
- **alice-server, Piper, Vosk и Spotify** собираются из этого репозитория: тип сборки Dockerfile, путь — `Dockerfile` или `docker/<сервис>/Dockerfile`, контекст сборки — корень репозитория.
- **Переменные** задаются во вкладке Environment того приложения, которому они нужны: см. [таблицу](#какая-переменная-в-какой-контейнер).
- **Порты** публикуйте только [нужные снаружи](#как-сервисы-связаны). **Тома** подключите к `/data` и к папке моделей gigastt, иначе после каждого деплоя модели будут скачиваться заново, а вход в Spotify потеряется.
- **alice-server деплойте последним.** После изменений в функциях Spotify передеплойте и его.

Особенности Docker Swarm, на котором работает Dokploy:
- После смены опубликованного порта он может не открыться, а в журнале Docker появится `Failed add IP alias … file exists`. Помогает `docker service update --force <App Name>` на сервере.
- При деплое новый контейнер запускается раньше, чем останавливается старый. Около 30 секунд в Spotify видны две колонки «Алиса», и команда может попасть в старую.

## Если что-то не работает

| Что видно | Что сделать |
|---|---|
| Ассистент не знает функций Spotify, а в логе alice-server при запуске `function server … unavailable` | Spotify не отвечал, когда запускался alice-server, или неверен `PROCESSOR_FUNCTION_SERVER_URLS`. Перезапустите alice-server |
| В логе alice-server `postgres unavailable` | База не была готова при запуске alice-server, и станции хранятся в памяти. Проверьте `POSTGRES_URL` и перезапустите alice-server |
| После перезагрузки машины пропали функции Spotify или база | Docker поднимает контейнеры без учёта порядка. Перезапустите alice-server |
| Spotify играет, станция молчит, а на странице Spotify «Станция не слушает поток» | Станция не может открыть `STREAM_URL`: в нём должны быть адрес машины в домашней сети и опубликованный порт. Проверьте с другого компьютера: `curl http://192.168.1.10:8090/health` должен ответить `ok` |
| Станция не реагирует на Spotify, а в логе Spotify `ALICE_API_URL is not set` | Задайте `ALICE_API_URL` контейнеру Spotify |
| На странице Spotify «Голосовое управление выключено» | `SPOTIFY_CLIENT_ID` и `SPOTIFY_CLIENT_SECRET` нужны контейнеру Spotify, а не alice-server |
| В логе alice-server `end of utterance by gigastt (blank, …)` | gigastt работает без VAD: задайте `GIGASTT_VAD=true` контейнеру gigastt |
| В логе alice-server `MCP server 'ha' (…): unavailable (HTTP 401 Unauthorized …` | Home Assistant не принял токен. Проверьте токен и префикс в `PROCESSOR_MCP_SERVER_TOKENS`: он должен совпадать с префиксом после `#` в `PROCESSOR_MCP_SERVER_URLS` |
| В логе alice-server `MCP server 'ha' (…): unavailable (HTTP 404 Not Found …` | В Home Assistant нет интеграции «Model Context Protocol Server», или он старше 2026.8 и не знает адреса `/api/mcp/assist`: укажите `/api/mcp` |
| В логе alice-server `MCP server 'ha' (…): unavailable (…error sending request…` | Home Assistant недоступен из контейнера. Укажите его IP вместо `homeassistant.local` |
