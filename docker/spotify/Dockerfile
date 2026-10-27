# Spotify on the station: librespot (https://github.com/librespot-org/librespot)
# as a Spotify Connect speaker whose sound docker/spotify/bridge.py streams to
# the station as MP3 on port 8090 (see bridge.py). Built from the repository
# root: docker build -f docker/spotify/Dockerfile -t spotify-bridge .
# librespot's sign-in stays in the /data volume.
FROM rust:1-bookworm AS librespot
ARG LIBRESPOT_VERSION=0.8.0
RUN cargo install librespot --version "${LIBRESPOT_VERSION}" --locked --no-default-features --features native-tls

FROM python:3.12-slim
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg && rm -rf /var/lib/apt/lists/*
COPY --from=librespot /usr/local/cargo/bin/librespot /usr/local/bin/librespot
COPY docker/spotify/bridge.py /usr/local/bin/spotify-bridge
RUN chmod +x /usr/local/bin/spotify-bridge
ENV SPOTIFY_NAME=Алиса DATA_DIR=/data PORT=8090 PYTHONUNBUFFERED=1
VOLUME /data
EXPOSE 8090
HEALTHCHECK CMD python3 -c "import os, urllib.request; urllib.request.urlopen('http://127.0.0.1:%s/health' % os.environ['PORT'], timeout=5)"
ENTRYPOINT ["python3", "/usr/local/bin/spotify-bridge"]
