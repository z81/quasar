# Whisper detector

Local YAMNet classifier for the station's OGG/Opus utterances. It is
display-only: the panel shows the last phrase and whether it looked whispered,
but the answer's voice is chosen by the manual whisper setting.

The service combines the AudioSet `Whispering` score with a voicing check:
whispered speech is unvoiced (weak autocorrelation pitch) and has a flatter
spectrum than voiced vowels. YAMNet alone is conservative for intelligible
whispered speech, so both signals are reported.

## Run

```sh
docker build -t alise-whisper:latest docker/whisper
docker run -d --name alise-whisper --restart unless-stopped \
  --network dokploy-network -p 127.0.0.1:8096:8080 alise-whisper:latest
```

The server reaches it by the container name on the same network:
`http://alise-whisper:8080`.

Enable it in the panel under **Настройки сервера → Озвучивание → Детектор
шёпота** (`WHISPER_DETECT_URL`). An empty value disables detection. The last
result is shown on the main page and in the station's whisper settings.

## API

`POST /classify` with the raw OGG/Opus body; returns:

```json
{"whisper": true, "score": 0.96, "yamnet": 0.0, "pitch": 0.23, "flatness": 0.06,
 "threshold": 0.45, "frames": 74, "seconds": 1.92}
```

`GET /health` reports the model and threshold. Audio is decoded in memory and
never stored; only the last transcript and scores are kept by the server.
