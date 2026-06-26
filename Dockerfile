# ── Build stage ───────────────────────────────────────────────────
FROM rust:1.80-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy workspace manifest and lockfile first (dependency caching)
COPY Cargo.toml Cargo.lock ./

# Create dummy sources for dep resolution
RUN mkdir -p src headroom-ffi/src && \
    echo "fn main() {}" > src/main.rs && \
    echo "" > src/lib.rs && \
    echo 'fn main() {}' > headroom-ffi/src/lib.rs && \
    echo '[package]' > headroom-ffi/Cargo.toml && \
    echo 'name = "headroom-ffi"' >> headroom-ffi/Cargo.toml && \
    echo 'version = "0.1.0"' >> headroom-ffi/Cargo.toml && \
    echo 'edition = "2021"' >> headroom-ffi/Cargo.toml && \
    echo '' >> headroom-ffi/Cargo.toml && \
    echo '[lib]' >> headroom-ffi/Cargo.toml && \
    echo 'crate-type = ["cdylib"]' >> headroom-ffi/Cargo.toml

# Download and compile dependencies (cached layer)
RUN cargo build --release 2>/dev/null || true

# Copy real source files
COPY src/ src/
COPY headroom-ffi/ headroom-ffi/

# Build proxy + DLL
RUN cargo build --release && \
    cargo build -p headroom-ffi --release

# ── Runtime stage ──────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/rust_cc_proxy /usr/local/bin/rust_cc_proxy
COPY --from=builder /app/target/release/libheadroom_ffi.so /usr/local/bin/headroom_core.so 2>/dev/null || true

EXPOSE 8787

ENV PROXY_HOST=0.0.0.0
ENV PROXY_PORT=8787
ENV PROXY_LOG_LEVEL=info

ENTRYPOINT ["rust_cc_proxy"]
