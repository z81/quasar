# Piper speech synthesis for alice-server's TTS_PROVIDER=piper
# (https://github.com/OHF-Voice/piper1-gpl, GPL-3.0): its HTTP server answers
# POST /synthesize {"text": "..."} with WAV on port 5000. Built from the
# repository root: docker build -f docker/piper/Dockerfile -t piper .
# The voice (PIPER_VOICE) is downloaded into the /data volume at the first start.
FROM python:3.12-slim
ARG PIPER_VERSION=1.8.0
RUN pip install --no-cache-dir "piper-tts[http]==${PIPER_VERSION}"
COPY docker/piper/entrypoint.sh /usr/local/bin/piper-entrypoint
ENV PIPER_VOICE=ru_RU-irina-medium PIPER_DATA_DIR=/data
VOLUME /data
EXPOSE 5000
# the first start downloads the voice (~63 MB) before it listens
HEALTHCHECK --start-period=180s CMD python3 -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:5000/info', timeout=5)"
ENTRYPOINT ["sh", "/usr/local/bin/piper-entrypoint"]
