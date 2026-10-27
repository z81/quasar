# Vosk speech synthesis for alice-server's TTS_PROVIDER=piper
# (https://github.com/alphacep/vosk-tts, Apache-2.0): docker/vosk/server.py answers
# POST /synthesize {"text": "..."} with WAV on port 5000, as Piper's server does.
# Built from the repository root: docker build -f docker/vosk/Dockerfile -t vosk-tts .
# The model (VOSK_MODEL, ~750 MB) is downloaded into the /data volume at the first start.
FROM python:3.12-slim
ARG VOSK_TTS_VERSION=0.3.61
RUN pip install --no-cache-dir --retries 10 --timeout 60 "vosk-tts==${VOSK_TTS_VERSION}"
COPY docker/vosk/server.py /usr/local/bin/vosk-tts-server
ENV VOSK_MODEL=vosk-model-tts-ru-0.9-multi VOSK_SPEAKER=female_2 VOSK_THREADS=4 VOSK_DATA_DIR=/data \
    PYTHONUNBUFFERED=1
VOLUME /data
EXPOSE 5000
# the first start downloads the model before it listens
HEALTHCHECK --start-period=1800s CMD python3 -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:5000/info', timeout=5)"
ENTRYPOINT ["python3", "/usr/local/bin/vosk-tts-server"]
