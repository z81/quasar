FROM rust:1 AS builder
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY . .
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/alice-server /usr/local/bin/alice-server
COPY prompt.handlebars prompt-state.handlebars /app/
WORKDIR /app
ENV OPENROUTER_API_KEY=
CMD ["alice-server"]
