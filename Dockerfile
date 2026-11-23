# syntax=docker/dockerfile:1.6
# ---------- Build stage ----------
FROM rust:1.83-slim-bookworm AS builder

WORKDIR /app

# Install minimal build deps. rustls is used by reqwest, so no OpenSSL needed.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Pre-fetch dependencies for better layer caching.
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main(){}" > src/main.rs \
 && cargo build --release \
 && rm -rf src target/release/deps/telegram_cybersec_bot*

# Copy real sources and build.
COPY src ./src
RUN cargo build --release \
 && strip target/release/telegram-cybersec-bot

# ---------- Runtime stage ----------
FROM debian:bookworm-slim AS runtime

# CA certificates are needed for HTTPS requests (Telegram, RSS, GitHub).
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/*

# Non-root user.
RUN useradd --create-home --shell /bin/false --uid 10001 botuser
USER botuser
WORKDIR /home/botuser

COPY --from=builder /app/target/release/telegram-cybersec-bot /usr/local/bin/telegram-cybersec-bot

# Persistent volume for state.json
VOLUME ["/home/botuser/data"]
ENV STATE_PATH=/home/botuser/data/state.json
ENV RUST_LOG=info

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/telegram-cybersec-bot"]