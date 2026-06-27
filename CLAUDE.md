# CLAUDE.md — rust_cc_proxy

Claude Code proxy in Rust (actix-web): multi-vendor LLM routing, token compression, Prometheus metrics.

Ref: `D:\projects\dsv4-cc-proxy` (Python), `D:\projects\headroom` (compression algorithms).

## Build & Run

```bash
cargo build | cargo run | cargo test | cargo check | cargo fmt | cargo clippy
# CARGO_HOME must point to D:\env\cargo
```

## Architecture

```
Claude Code → Auth(middleware) → Pipeline → ProviderRegistry → ProxyClient → upstream SSE
                                    └─ CompressionStage? ─┘
Metrics(prometheus) ← TokenMonitor ← handlers
```

## Key Modules

| Module | Purpose |
|--------|---------|
| `server/` | actix-web App, handlers, auth middleware |
| `auth.rs` | API key middleware (`x-api-key`), gated by `PROXY_AUTH_TOKENS` |
| `metrics.rs` | Prometheus counters/histograms, `GET /metrics` text format (always on) |
| `compress/` | Token compression: content-type detection, 6 compressors, CCR storage, unified signals scoring |
| `providers/` | `ProviderRegistry` — DeepSeek, Anthropic passthrough |

## Compression

**Gate**: `COMPRESSION_ENABLED=1`. **CCR backends**: InMemory (LRU, default) or SQLite (WAL, TTL, background purge). **Pre-compression**: JsonMinify + DiffNoise (lockfile/whitespace strip). **Headroom DLL**: optional fallback.

## Key Env Vars

| Var | Default | Notes |
|-----|---------|-------|
| `PROXY_HOST` / `PROXY_PORT` | 127.0.0.1 / 8787 | |
| `DEEPSEEK_UPSTREAM` / `DEEPSEEK_API_KEY` | — | Auto-enables DeepSeek provider |
| `PROXY_AUTH_TOKENS` | — | Comma-separated API keys. Empty = no auth |
| `COMPRESSION_ENABLED` | false | `1` to enable |
| `CCR_BACKEND` / `CCR_TTL_SECONDS` | memory / 1800 | `sqlite` for persistence |
| `CCR_PURGE_INTERVAL_SECONDS` | 300 | Background TTL sweep (SQLite only) |

## Quick Test

`COMPRESSION_ENABLED=1 CCR_BACKEND=memory cargo test --lib | grep failures:`
