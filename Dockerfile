# ── Build stage ───────────────────────────────────────────────────
FROM rust:1.88-slim-bookworm AS builder

RUN echo "">/etc/apt/sources.list && \
echo "deb https://mirrors.tuna.tsinghua.edu.cn/debian/ bookworm main contrib non-free non-free-firmware">>/etc/apt/sources.list && \
echo "deb https://mirrors.tuna.tsinghua.edu.cn/debian/ bookworm-updates main contrib non-free non-free-firmware">>/etc/apt/sources.list && \
echo "deb https://mirrors.tuna.tsinghua.edu.cn/debian/ bookworm-backports main contrib non-free non-free-firmware">>/etc/apt/sources.list && \
echo "deb https://mirrors.tuna.tsinghua.edu.cn/debian-security bookworm-security main contrib non-free non-free-firmware">>/etc/apt/sources.list

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

# Copy Cargo mirror config (use Tsinghua mirror for crates.io)
COPY .cargo/config.toml .cargo/config.toml

# Download and compile dependencies (cached layer)
RUN cargo build --release

# Copy real source files
COPY src/ src/
COPY headroom-ffi/ headroom-ffi/

# Touch all sources that differ from dummies to ensure cargo detects changes
# (Docker COPY may preserve old timestamps — cargo fingerprint relies on mtime as a tiebreaker)
RUN touch src/main.rs src/lib.rs headroom-ffi/src/lib.rs headroom-ffi/Cargo.toml && \
    cargo build --release && \
    cargo build -p headroom-ffi --release && \
    ls -lh target/release/rust_cc_proxy target/release/libheadroom_ffi.so

# ── Runtime stage ──────────────────────────────────────────────────
FROM debian:bookworm-slim

# Use HTTP mirror to avoid HTTPS cert chicken-and-egg (ca-certificates not yet installed)
RUN echo "">/etc/apt/sources.list && \
echo "deb http://mirrors.tuna.tsinghua.edu.cn/debian/ bookworm main contrib non-free non-free-firmware">>/etc/apt/sources.list && \
echo "deb http://mirrors.tuna.tsinghua.edu.cn/debian/ bookworm-updates main contrib non-free non-free-firmware">>/etc/apt/sources.list && \
echo "deb http://mirrors.tuna.tsinghua.edu.cn/debian/ bookworm-backports main contrib non-free non-free-firmware">>/etc/apt/sources.list && \
echo "deb http://mirrors.tuna.tsinghua.edu.cn/debian-security bookworm-security main contrib non-free non-free-firmware">>/etc/apt/sources.list && \
apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/rust_cc_proxy /usr/local/bin/rust_cc_proxy
RUN --mount=type=bind,from=builder,source=/app/target/release,target=/tmp/build \
    cp /tmp/build/libheadroom_ffi.so /usr/local/bin/headroom_core.so 2>/dev/null || true

EXPOSE 8787

ENV PROXY_HOST=0.0.0.0
ENV PROXY_PORT=8787
ENV PROXY_LOG_LEVEL=info

# Diagnostic entrypoint — verifies binary health before exec
RUN echo '#!/bin/sh' > /usr/local/bin/entrypoint.sh && \
    echo 'echo "=== rust_cc_proxy starting ===" >&2' >> /usr/local/bin/entrypoint.sh && \
    echo 'ls -la /usr/local/bin/rust_cc_proxy >&2' >> /usr/local/bin/entrypoint.sh && \
    echo 'ldd /usr/local/bin/rust_cc_proxy 2>&1 || true' >> /usr/local/bin/entrypoint.sh && \
    echo 'echo "=== executing binary ===" >&2' >> /usr/local/bin/entrypoint.sh && \
    echo 'exec /usr/local/bin/rust_cc_proxy "$@"' >> /usr/local/bin/entrypoint.sh && \
    chmod +x /usr/local/bin/entrypoint.sh

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
