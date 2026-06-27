# rust_cc_proxy

English | [中文](README.zh-CN.md)

A modular, production-ready Claude Code proxy server in Rust (actix-web). Multi-vendor LLM routing, protocol translation, token compression with adaptive sizing, Prometheus metrics, and API key authentication.

## Features

- **Multi-vendor routing** — route requests by model name to DeepSeek or Anthropic backends
- **API key authentication** — middleware validates `x-api-key` header, gated by `PROXY_AUTH_TOKENS`
- **DeepSeek compatibility** — three protocol fixes (thinking normalization, thinking injection, system role extraction)
- **CC Switch** — `/v1/models` endpoint for Claude Code's `/model` picker + cc-switch usage query support
- **Token compression** — content-aware compression for JSON arrays, diffs, logs, search results, and prose, with adaptive sizing (Kneedle algorithm), anchor selection, and pre-compression reformat
- **Unified signal scoring** — `LineImportanceDetector` trait + `KeywordDetector` with context-aware keyword activation across all compressors
- **CCR (Compress-Cache-Retrieve)** — reversible compression via BLAKE3 + InMemory (LRU) or SQLite (WAL, TTL, background purge)
- **Prometheus metrics** — `/metrics` endpoint with counters, histograms, gauges; always on
- **Usage monitoring** — `/v1/usage`, `/status` endpoints with streaming token extraction via SSE interception
- **Headroom DLL** — optional dynamic-load fallback to `headroom_core.dll` for production-grade compression
- **Production hardening** — rate limiting, 20 MB body cap, 30 s graceful shutdown, cache-aware live-zone surgery

## Quick Start

### Prerequisites

- Rust 1.80+ (edition 2021)
- A DeepSeek API key (or Anthropic key for passthrough)

### One-Line Install

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/leohu1/rust_cc_proxy/master/install.sh | bash

# Windows (PowerShell as Administrator)
iwr -Uri https://raw.githubusercontent.com/leohu1/rust_cc_proxy/master/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File install.ps1
```

### Build from Source

```bash
git clone https://github.com/leohu1/rust_cc_proxy.git
cd rust_cc_proxy
cargo build --release                       # proxy only
cargo build -p headroom-ffi --release        # optional compression DLL
```

### Run

```bash
# DeepSeek backend — auto-detected when DEEPSEEK_API_KEY is set
DEEPSEEK_API_KEY=sk-your-deepseek-key cargo run

# With authentication
PROXY_AUTH_TOKENS=sk-proxy-key-1,sk-proxy-key-2 DEEPSEEK_API_KEY=sk-... cargo run

# Token compression enabled
COMPRESSION_ENABLED=true DEEPSEEK_API_KEY=sk-... cargo run

# SQLite CCR persistence
COMPRESSION_ENABLED=true CCR_BACKEND=sqlite CCR_SQLITE_PATH=ccr.db cargo run

# Custom port
PROXY_PORT=8787 DEEPSEEK_API_KEY=sk-... cargo run
```

### Use with Claude Code

```bash
ANTHROPIC_BASE_URL=http://localhost:8787 \
ANTHROPIC_API_KEY="" \
ANTHROPIC_AUTH_TOKEN=any-value \
CLAUDE_CODE_ATTRIBUTION_HEADER=0 \
CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1 \
claude
```

Type `/model` in Claude Code to switch between backends.

## Configuration

Priority: CLI flags > environment variables > defaults.

### Core

| Variable | Default | Description |
| --- | --- | --- |
| `PROXY_HOST` | `127.0.0.1` | Bind address |
| `PROXY_PORT` | `8787` | Bind port |
| `PROXY_LOG_LEVEL` | `info` | Log level |
| `PROXY_DEV_MODE` | `false` | Dev mode: verbose logging + `/v1/metrics` |
| `PROXY_UPSTREAM` | `https://api.anthropic.com` | Default upstream URL |
| `PROXY_API_KEY` | — | API key forwarded to upstream |
| `PROXY_TIMEOUT` | `600` | Request timeout in seconds |
| `PROXY_POOL_MAX` | `20` | Max connections in pool |
| `PROXY_DUMP_DIR` | — | Traffic dump directory |

### Authentication

| Variable | Default | Description |
| --- | --- | --- |
| `PROXY_AUTH_TOKENS` | — | Comma-separated API keys. Empty = no auth. Clients must set `x-api-key` header. `/health` is always public. |

### DeepSeek Provider

| Variable | Default | Description |
| --- | --- | --- |
| `DEEPSEEK_UPSTREAM` | `https://api.deepseek.com/anthropic` | DeepSeek API URL |
| `DEEPSEEK_API_KEY` | — | Set to auto-enable DeepSeek |
| `DEEPSEEK_DEFAULT_MODEL` | `deepseek-v4-flash` | Default model |
| `DEEPSEEK_MODEL_MAP` | — | Model name mappings: `client=upstream,...` |

### Compression

| Variable | Default | Description |
| --- | --- | --- |
| `COMPRESSION_ENABLED` | `false` | Enable token compression |
| `CCR_BACKEND` | `memory` | CCR storage: `memory` (LRU) or `sqlite` (persistent) |
| `CCR_SQLITE_PATH` | — | SQLite database path (when backend = `sqlite`) |
| `CCR_TTL_SECONDS` | `1800` | TTL for cached entries (0 = never expire) |
| `CCR_PURGE_INTERVAL_SECONDS` | `300` | Background TTL sweep interval (SQLite only; 0 = disabled) |

### CLI Flags

```
--host          Bind address (overrides PROXY_HOST)
--port          Bind port (overrides PROXY_PORT)
--log-level     Log level (overrides PROXY_LOG_LEVEL)
--upstream      Default upstream URL (overrides PROXY_UPSTREAM)
--dev           Enable dev mode (verbose logging + /v1/metrics)
```

## Architecture

```
Claude Code CLI
  │ POST /v1/messages, GET /v1/models, ...
  ▼
rust_cc_proxy (actix-web)
  ├─ Auth middleware          → x-api-key validation (optional)
  ├─ Pipeline                 → SystemRoleNormalizer → CompressionStage → ProviderTransform
  ├─ ProviderRegistry         → route by model name (auto-defaults to DeepSeek if configured)
  ├─ ProxyClient (reqwest)    → upstream LLM
  └─ Prometheus /metrics      → counters, histograms, gauges (always on)
```

### Endpoints

| Route | Auth | Description |
| --- | --- | --- |
| `GET /health` | No | Health check: `{"status":"healthy"}` |
| `GET /metrics` | Yes | **Prometheus text format** (always on) |
| `GET /status` | Yes | Proxy status with usage stats |
| `GET /v1/usage` | Yes | Token usage (cc-switch compatible) |
| `GET /user/balance` | Yes | DeepSeek-format balance |
| `POST /v1/retrieve` | Yes | CCR content retrieval `{"hash":"..."}` → `{"content":"..."}` |
| `GET /v1/compression/stats` | Yes | CCR cache statistics |
| `GET /v1/models` | Yes | Model discovery for CC Switch |
| `POST /v1/messages` | Yes | Chat completion (streaming + non-streaming) |
| `POST /v1/messages/count_tokens` | Yes | Token counting |
| `GET /v1/metrics` | Yes | Dev-mode JSON metrics (requires `PROXY_DEV_MODE=true`) |

### Module Structure

```
src/
├── main.rs                   CLI entry point + server startup
├── lib.rs                    Module declarations
├── auth.rs                   API key middleware (x-api-key)
├── config.rs                 Environment-based configuration
├── error.rs                  AppError types with HTTP response mapping
├── metrics.rs                Prometheus metrics (counters, histograms, gauges)
├── monitor/                  TokenMonitor: atomic counters, usage/SSE parsing
├── server/
│   ├── mod.rs                actix-web app factory, route wiring
│   ├── handlers.rs           Route handlers (10 endpoints)
│   ├── rate_limiter.rs       Token-bucket rate limiter (per-provider)
│   └── shutdown.rs           SIGTERM / Ctrl+C graceful shutdown
├── pipeline/
│   ├── mod.rs                PipelineStage trait + Pipeline runner
│   └── system_normalizer.rs  System role extraction/merge
├── providers/
│   ├── mod.rs                Provider trait + ProviderRegistry
│   ├── deepseek.rs           DeepSeek protocol fixes (thinking, system role)
│   └── anthropic.rs          Anthropic passthrough
├── protocol/
│   ├── mod.rs                Protocol types re-export
│   ├── messages.rs           Anthropic Messages API types
│   ├── models.rs             Model list types
│   └── sse_types.rs          SSE event types + streaming parsing
├── proxy/
│   ├── mod.rs                reqwest client pool, forward helpers
│   └── streaming.rs          SSE stream passthrough + token extraction
└── compress/
    ├── mod.rs                Content-type detection, Compressor dispatcher, token-gate
    ├── pipeline_stage.rs     CompressionStage: 5-phase orchestrator
    ├── cache_aware.rs        cache_control detection, frozen-zone identification
    ├── live_zone.rs          Byte-level surgery + SHA-256 prefix integrity
    ├── headroom_dll.rs       Headroom DLL loader (LoadLibraryW/dlsym)
    ├── tokenizer.rs          tiktoken-rs (o200k_base) + fallback char estimate
    ├── signals.rs            LineImportanceDetector trait + KeywordDetector (unified scoring)
    ├── pipeline_utils.rs     JsonMinify + DiffNoise pre-compression reformat + bloat gating
    ├── adaptive_sizer.rs     Kneedle algorithm for optimal item count
    ├── anchor_selector.rs    Weighted 3-region anchor allocation
    ├── relevance.rs          BM25 keyword relevance scorer
    ├── diff.rs               Unified diff compressor
    ├── log.rs                Build/test log compressor (pytest/cargo/npm/jest)
    ├── search.rs             grep/ripgrep search result compressor
    ├── text.rs               Extractive prose compressor
    └── ccr/
        ├── mod.rs            CcrBackend trait + CcrStore wrapper + BLAKE3 hashing + stats
        ├── memory.rs         InMemory backend (LRU eviction)
        └── sqlite.rs         SQLite backend (WAL, TTL expiry, background purge)
```

### Token Compression Pipeline

Enabled via `COMPRESSION_ENABLED=true`. The `CompressionStage` locates the latest user message and compresses `tool_result` blocks:

```
1. Pre-compression reformat  →  JsonMinify (lossless) + DiffNoise (lockfile/whitespace strip)
2. Content-type detection    →  JSON array/object, diff, log, search results, plain text
3. Signal scoring            →  LineImportanceDetector::score() — keyword + structural boosts
4. Adaptive sizing           →  Kneedle on bigram coverage curve → optimal k
5. Anchor selection          →  Front/middle/back region allocation with data-pattern weights
6. Compressor dispatch       →  Content-type-specific compressor with fill
7. Token-gate validation     →  tiktoken-rs (o200k_base) → reject if not smaller
8. CCR storage               →  BLAKE3 hash → store original → embed <<ccr:HASH>> marker
```

#### Content-Type Strategies

| Type | Detection | Strategy |
| --- | --- | --- |
| JSON array | `[...]` + parse | AdaptiveSizer → AnchorSelector → BM25 fill → CCR |
| JSON object | `{...}` + parse | Field truncation + CCR |
| Diff | `diff --git` / `@@` | Noise strip → file cap (5) → hunk cap (3) → CCR |
| Log | 3+ log keywords | Unified KeywordDetector scoring (Error=0.9, Warning=0.6) + context windows |
| Search | `file:digit:` pattern | First-separator parser → adaptive file/match caps → CCR |
| Prose | >800 chars | Sentence scoring (Error/Warning signals + recency + density) → keep top half |
| Other | — | Skip (unchanged) |

Compressed content embeds a `<<ccr:HASH>>` marker. Original data is retrieved via `POST /v1/retrieve {"hash":"..."}`.

#### Adaptive Sizing (Kneedle Algorithm)

Replaces hardcoded "keep N items" caps with data-driven decisions:
- Computes cumulative unique bigram coverage over items in order
- Finds the knee (elbow) of the curve — maximum perpendicular distance from y=x diagonal
- Applies bias multiplier, clamped to `[min_k, max_k]`
- Works for JSON arrays (item count) and search results (match count)

#### Signals Framework

The `LineImportanceDetector` trait unifies all line-level scoring:
- **KeywordDetector**: Error (0.9), Security (0.85), Warning (0.6), Importance (0.45)
- **Context-aware**: Warning disabled in Diff, Security only in Diff, Error in all
- **Structural boosts**: ALLCAPS words (+0.03 each), digit density (+0.1)
- **Word-boundary matching**: `"error"` matches `"error:"` but not `"terrorism"`

### CCR Storage Backends

| Backend | Eviction | Persistence | Use Case |
| --- | --- | --- | --- |
| InMemory | LRU | No | Development, testing, low-scale |
| SQLite | TTL + background purge | Yes | Production single-instance |

SQLite backend features: WAL mode, `synchronous=NORMAL`, `busy_timeout=5000`, startup expiry cleanup, optional background `tokio` purge task.

### DeepSeek Compatibility Fixes

| Fix | Description |
| --- | --- |
| Thinking normalization | `adaptive`/`auto` → `enabled`, strip `reasoning_effort` |
| Thinking injection | Insert empty `thinking` block before `tool_use` blocks |
| System role extraction | Move `role: "system"` from `messages[]` → top-level `system` |

## Dev Mode

```bash
cargo run -- --dev
# or: PROXY_DEV_MODE=true cargo run
```

Dev mode enables:
- **Verbose request logging**: model, stream mode, pipeline timing, token usage
- **File + line number** in log output
- **`/v1/metrics` endpoint**: JSON request counts, token totals, error counts
- **Automatic `debug` log level** (override with `PROXY_LOG_LEVEL`)

Example log output:
```
→ REQ  model=deepseek-v4-pro  stream=false  est_input_tokens=3200
  pipeline: 2 stages in 0ms
  provider=DeepSeek  upstream_model=deepseek-v4-pro
← OK   model=deepseek-v4-pro  latency=2100ms  upstream=2050ms  tokens  in=15000  out=800
```

## cc-switch Integration

### Proxy chaining

```
Claude Code → cc-switch (15721) → rust_cc_proxy (8787) → DeepSeek / Anthropic
                 │                        │
                 │ Model mgmt             │ Protocol fixes
                 │ Failover               │ Token compression
                 │ Usage stats            │ Prometheus metrics
                 │                        │
                 └── GET /v1/usage ──────→┘
```

### Usage query custom script

In cc-switch, select **"Custom"** template in Usage Query config:

```javascript
({
  request: {
    url: "{{baseUrl}}/user/balance",
    method: "GET",
    headers: { "Authorization": "Bearer {{apiKey}}" }
  },
  extractor: function(response) {
    if (response.balance_infos && response.balance_infos.length > 0) {
      var info = response.balance_infos[0];
      var total  = parseFloat(info.total_balance)  || 0;
      var topped = parseFloat(info.topped_up_balance) || 0;
      var granted = parseFloat(info.granted_balance) || 0;
      return {
        planName: "DeepSeek",
        remaining: total,
        used: Math.max(0, topped + granted - total),
        total: topped + granted,
        unit: info.currency || "CNY",
        isValid: response.is_available
      };
    }
    return { isValid: false, invalidMessage: "No data" };
  }
})
```

## Production Hardening

### Rate Limiting
Token-bucket rate limiter (10 req/s, burst 20), per-provider buckets.

### Request Size Limit
20 MB body limit on all routes.

### Graceful Shutdown
30-second drain on SIGTERM / Ctrl+C for in-flight requests.

### Cache-Aware Compression
Detects `cache_control` markers — skips compression on frozen-zone messages. Live-zone byte surgery with SHA-256 integrity verification.

### Headroom DLL Integration

For production-grade compression, the proxy optionally loads `headroom_core.dll` at startup. All compression delegates to Headroom's C-ABI functions. Falls back to built-in compressors when absent.

```bash
# Build
cd D:\projects\headroom
cargo build -p headroom-ffi --release

# Use
cp target/release/headroom_ffi.dll ./headroom_core.dll
HEADROOM_DLL_PATH=./headroom_core.dll COMPRESSION_ENABLED=true DEEPSEEK_API_KEY=sk-... cargo run
```

## Docker

```bash
docker build -t rust_cc_proxy .
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... rust_cc_proxy
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... -e PROXY_AUTH_TOKENS=my-key rust_cc_proxy
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... rust_cc_proxy -- --dev
```

## Testing

```bash
cargo test                    # Full suite (137+ tests)
cargo test --lib              # Unit tests
cargo test --test integration # Integration tests
COMPRESSION_ENABLED=1 CCR_BACKEND=memory cargo test --lib  # Quick check
```

## Implementation Status

| Phase | Description | Status |
| --- | --- | --- |
| Foundation | Passthrough proxy, system role normalization, DeepSeek protocol fixes | ✅ |
| CC Switch | `/v1/models`, model discovery, cc-switch usage/balance endpoints | ✅ |
| Compression | 6 content-type compressors, BM25, CCR, tiktoken-rs tokenizer | ✅ |
| Batch 1 | SQLite CCR, SearchCompressor, tiktoken-rs, CcrBackend trait, headroom_ccr_stats FFI | ✅ |
| Batch 2 | LineImportanceDetector, AdaptiveSizer, AnchorSelector, Pipeline reformat/offload | ✅ |
| Batch 3 | Auth middleware, LRU cache stabilization, Prometheus metrics, server wiring | ✅ |
| Production | Rate limiting, graceful shutdown, Docker, live-zone surgery, cache-awareness | ✅ |

## License

MIT
