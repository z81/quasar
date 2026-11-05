# The prebuilt Dioxus CLI requires glibc >= 2.39; runtime remains Bookworm.
FROM rust:1-trixie AS ui-builder
ARG TARGETARCH
ARG DIOXUS_VERSION=0.7.10
RUN set -eu; \
    case "${TARGETARCH:-$(dpkg --print-architecture)}" in \
        amd64) dx_target=x86_64-unknown-linux-gnu ;; \
        arm64) dx_target=aarch64-unknown-linux-gnu ;; \
        *) echo 'Unsupported architecture for Dioxus CLI' >&2; exit 1 ;; \
    esac; \
    dx_asset="dx-${dx_target}.tar.gz"; \
    dx_release="https://github.com/DioxusLabs/dioxus/releases/download/v${DIOXUS_VERSION}"; \
    cd /tmp; \
    curl -fsSL "${dx_release}/${dx_asset}" -o "$dx_asset"; \
    curl -fsSL "${dx_release}/dx-${dx_target}.sha256" -o dx.sha256; \
    sha256sum --check --ignore-missing dx.sha256; \
    tar -xzf "$dx_asset" -C /usr/local/bin; \
    rustup target add wasm32-unknown-unknown
WORKDIR /app
COPY web ./web
COPY scripts/build-ui.sh ./scripts/build-ui.sh
RUN sh scripts/build-ui.sh

FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release --bin alice-server && rm -rf src
COPY . .
RUN touch src/main.rs && cargo build --release --bin alice-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends adb ffmpeg ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/alice-server /usr/local/bin/alice-server
COPY --from=ui-builder /app/web/dist/public /app/web/dist/public
COPY prompt.handlebars prompt-state.handlebars updates-config.yml /app/
WORKDIR /app
CMD ["alice-server"]
