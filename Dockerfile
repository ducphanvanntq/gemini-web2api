# ─── Build stage ──────────────────────────────────────────────────────────────
FROM rust:1-bookworm AS builder

WORKDIR /app

# Pre-build dependencies for better layer caching: compile a dummy bin first so
# the dependency graph is cached unless Cargo.toml/Cargo.lock change.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Build the real binary.
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ─── Runtime stage ──────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

# reqwest uses native-tls (OpenSSL) on Linux; ca-certificates for HTTPS trust.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/gemini-web2api /usr/local/bin/gemini-web2api
COPY config.example.json /app/config.json

EXPOSE 8081

CMD ["gemini-web2api", "--config", "/app/config.json"]
