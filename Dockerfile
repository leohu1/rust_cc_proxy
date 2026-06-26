# ── Build stage ───────────────────────────────────────────────────
FROM rust:1.80-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release 2>/dev/null || true

COPY src/ src/
RUN cargo build --release

# ── Runtime stage ──────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/rust_cc_proxy /usr/local/bin/rust_cc_proxy

EXPOSE 8787

ENV PROXY_HOST=0.0.0.0
ENV PROXY_PORT=8787
ENV PROXY_LOG_LEVEL=info

ENTRYPOINT ["rust_cc_proxy"]
