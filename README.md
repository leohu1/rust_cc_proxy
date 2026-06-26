# rust_cc_proxy
English | [中文](README.zh-CN.md)

A modular, extensible Claude Code proxy server in Rust (actix-web). Features multi-vendor routing, protocol translation, token compression, usage monitoring, and cc-switch compatibility.

## Features

- **Multi-vendor routing** — route requests by model name to different backends (DeepSeek, Anthropic)
- **DeepSeek compatibility** — three protocol fixes (thinking normalization, thinking injection, system role extraction)
- **CC Switch** — `GET /v1/models` endpoint for Claude Code's `/model` picker
- **Auto-detect provider** — DeepSeek auto-enabled when `DEEPSEEK_API_KEY` is set; upstream URL defaults automatically
- **Token compression** — content-aware compression for JSON arrays (BM25 relevance), JSON objects, diffs, logs, and prose
- **CCR (Compress-Cache-Retrieve)** — reversible compression via BLAKE3 hashing + in-memory cache
- **Usage monitoring** — `/v1/usage`, `/metrics`, `/status` endpoints with streaming token extraction
- **cc-switch compatible** — `/user/balance`, `/v1/usage`, custom-script usage query support
- **Dev mode** — verbose request logging, pipeline timing, token tracking (`--dev` flag)

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

# Dev mode: verbose logging + /metrics endpoint
cargo run -- --dev

# Token compression enabled
COMPRESSION_ENABLED=true DEEPSEEK_API_KEY=sk-... cargo run

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

| Variable | Default | Description |
| --- | --- | --- |
| `PROXY_HOST` | `127.0.0.1` | Bind address |
| `PROXY_PORT` | `8787` | Bind port |
| `PROXY_LOG_LEVEL` | `info` | Log level |
| `PROXY_DEV_MODE` | `false` | Dev mode: verbose logging + `/metrics` |
| `PROXY_UPSTREAM` | `https://api.anthropic.com` | Default upstream URL |
| `PROXY_API_KEY` | — | API key forwarded to upstream |
| `PROXY_TIMEOUT` | `600` | Request timeout in seconds |
| `PROXY_POOL_MAX` | `20` | Max connections in pool |
| `PROXY_DUMP_DIR` | — | Traffic dump directory for debugging |
| `COMPRESSION_ENABLED` | `false` | Enable token compression |

### DeepSeek Provider

| Variable | Default | Description |
| --- | --- | --- |
| `DEEPSEEK_UPSTREAM` | `https://api.deepseek.com/anthropic` | DeepSeek API URL (auto-defaulted) |
| `DEEPSEEK_API_KEY` | — | DeepSeek API key (set to auto-enable) |
| `DEEPSEEK_DEFAULT_MODEL` | `deepseek-v4-flash` | Default model |
| `DEEPSEEK_MODEL_MAP` | — | Model name mappings: `client=upstream,...` |

### CLI Flags

```
--host          Bind address (overrides PROXY_HOST)
--port          Bind port (overrides PROXY_PORT)
--log-level     Log level (overrides PROXY_LOG_LEVEL)
--upstream      Default upstream URL (overrides PROXY_UPSTREAM)
--dev           Enable dev mode (verbose logging + /metrics)
```

## Architecture

```
Claude Code CLI
  │ POST /v1/messages, GET /v1/models, ...
  ▼
rust_cc_proxy (actix-web)
  ├─ Pipeline: SystemRoleNormalizer → CompressionStage → ProviderTransform
  ├─ ProviderRegistry → route by model name (auto-defaults to DeepSeek if configured)
  └─ ProxyClient (reqwest) → upstream LLM
```

### Endpoints

| Route | Description |
| --- | --- |
| `GET /health` | Health check (cc-switch compatible: `{"status":"healthy"}`) |
| `GET /status` | Proxy status with stats |
| `GET /v1/usage` | Token usage (cc-switch compatible) |
| `GET /user/balance` | DeepSeek-format balance (cc-switch built-in template) |
| `POST /v1/retrieve` | CCR content retrieval `{"hash":"..."}` → `{"content":"..."}` |
| `GET /v1/compression/stats` | CCR cache statistics |
| `GET /v1/models` | Model discovery for CC Switch |
| `POST /v1/messages` | Chat completion (streaming + non-streaming) |
| `POST /v1/messages/count_tokens` | Token counting |
| `GET /metrics` | Dev-mode monitoring (requires `--dev` or `PROXY_DEV_MODE=true`) |

### Module Structure

```
src/
├── main.rs              CLI + server startup
├── lib.rs               Library root
├── config.rs            Environment-based configuration
├── error.rs             AppError types with HTTP response mapping
├── monitor/             TokenMonitor: atomic counters, usage snapshots
├── server/              actix-web app, route handlers, SSE streaming
├── pipeline/            PipelineStage trait, system role normalizer
├── providers/           Provider trait, DeepSeek + Anthropic implementations
├── protocol/            Anthropic Messages API, SSE types, model types
├── proxy/               reqwest client pool, SSE streaming adapter
└── compress/            Token compression
    ├── mod.rs           Content detection + dispatch + tokenizer validator
    ├── ccr.rs           BLAKE3 hashing → in-memory cache
    ├── cache_aware.rs   cache_control detection, frozen zone identification
    ├── live_zone.rs     Byte-level surgery + SHA-256 prefix integrity
    ├── relevance.rs     BM25 keyword relevance scorer
    ├── diff.rs          Unified diff compressor
    ├── log.rs           Build/test log compressor (pytest/cargo/npm/jest)
    ├── text.rs          Extractive prose compressor
    ├── headroom_dll.rs  Headroom DLL loader (LoadLibraryW/dlsym)
    └── pipeline_stage.rs 5-stage orchestrator (detect→hash→compress→validate→verify)
```

### Token Compression

Enabled via `COMPRESSION_ENABLED=true`. The `CompressionStage` finds the latest user message and compresses `tool_result` blocks by content type:

| Content Type | Detection | Strategy |
| --- | --- | --- |
| JSON array | `[...]` + parse | BM25 relevance → keep top N + first/last |
| JSON object | `{...}` + parse | Field truncation + CCR |
| Diff | `diff --git` / `@@` headers | File cap (5), hunk cap (3), context trim (2) |
| Log | 3+ log keywords | Line scoring: ERROR(100), WARN(30), STACK(50) |
| Prose | >800 chars | Sentence scoring, keep 50% |
| Other | — | Skip (unchanged) |

Compressed content embeds a `<<ccr:HASH>>` marker. Original data can be retrieved via `POST /v1/retrieve {"hash":"..."}`.

#### Pipeline Orchestration

5-stage compression pipeline in `CompressionStage`:

```
1. Detect live zone  →  find latest user message
2. Cache safety       →  SHA-256 frozen prefix hash + cache_control check
3. Content-detect     →  JSON/diff/log/text → dispatch to compressor
4. Token validate     →  estimate_tokens(compressed) < estimate_tokens(original)
5. Verify integrity   →  re-hash frozen prefix, compare with original
```

#### Live-zone Byte Surgery

`src/compress/live_zone.rs` performs byte-level surgery on the request body:
- SHA-256 hash of frozen messages (the cache-pinned prefix)
- Only the latest user message's tool_result blocks are compressed
- Post-compression: verify frozen prefix hash still matches
- Hash mismatch → WARN and preserve compressed data (cache may re-warm)

#### Tokenizer Validation

All compressed output passes through a token count gate (`estimate_tokens()`):
- ASCII/Latin: ~4 chars/token | CJK: ~2 chars/token
- If `compressed_tokens >= original_tokens` → reject compression, return unchanged
- Prevents ineffective compression from wasting tokens on metadata overhead

### DeepSeek Compatibility Fixes

| Fix | Description |
| --- | --- |
| Thinking normalization | `adaptive`/`auto` → `enabled`, strip `reasoning_effort` |
| Thinking injection | Insert empty `thinking` block before `tool_use` blocks |
| System role extraction | Move `role: "system"` from `messages[]` → top-level `system` |

## Dev Mode

```bash
cargo run -- --dev
```

Dev mode enables:
- **Verbose request logging**: model, stream mode, pipeline timing, token usage
- **File + line number** in log output
- **`/metrics` endpoint**: request counts, token totals, error counts
- **Automatic `debug` log level** (override with `PROXY_LOG_LEVEL`)

Example log output:
```
→ REQ  model=deepseek-v4-pro  stream=false  est_input_tokens=3200
  pipeline: 2 stages in 0ms
  provider=DeepSeek  upstream_model=deepseek-v4-pro
← OK   model=deepseek-v4-pro  latency=2100ms  upstream=2050ms  tokens  in=15000  out=800
```

## cc-switch Integration

### Usage query with custom script

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
      // DeepSeek API: total_balance IS the remaining balance (granted + topped_up).
      // All values are strings per official API; parse to numbers.
      // Ref: https://api-docs.deepseek.com/api/get-user-balance
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

### Proxy chaining architecture

```
Claude Code → cc-switch (15721) → rust_cc_proxy (8787) → DeepSeek
                 │                        │
                 │ Model mgmt             │ Protocol fixes
                 │ Failover               │ Token compression
                 │ Usage stats            │ Usage monitoring
                 │                        │
                 └── GET /v1/usage ──────→┘
```

## Production Hardening

### Rate Limiting
Token-bucket rate limiter (10 req/s, burst 20), per-provider buckets.

### Request Size Limit
20 MB body limit on all routes.

### Graceful Shutdown
30-second drain on SIGTERM / Ctrl+C for in-flight requests.

### Cache-Aware Compression
Detects `cache_control` markers — skips compression on frozen-zone messages.

### Headroom DLL Integration

For production-grade compression, the proxy can dynamically load `headroom_core.dll` at startup. When present, all compression is delegated to Headroom's C-ABI functions. When absent, the built-in lightweight compressors are used.

### Building the DLL

```bash
cd D:\projects\headroom
cargo build -p headroom-ffi --release
# Output: target/release/headroom_ffi.dll
```

### Usage

```bash
# Copy DLL next to the proxy binary
cp headroom/target/release/headroom_ffi.dll ./headroom_core.dll

# Or set path explicitly
HEADROOM_DLL_PATH=/path/to/headroom_core.dll \
COMPRESSION_ENABLED=true \
DEEPSEEK_API_KEY=sk-... \
cargo run
```

Startup log:
```
Loading headroom DLL: ./headroom_core.dll
Headroom DLL loaded successfully
Headroom DLL compression ENABLED
```

### Architecture

```
rust_cc_proxy.exe (5 MB)
  │
  ├─ compress/mod.rs
  │     │
  │     ├─ HeadroomDll::load()  ← 按需加载 headroom_core.dll
  │     │     ├─ compress(content, type) → CompressionResult
  │     │     ├─ retrieve(hash) → original bytes
  │     │     └─ (CCR 共享内存)
  │     │
  │     └─ 回退: 内置轻量压缩器 (Compressor)
  │
  └─ headroom_core.dll (2 MB, 可选)
        ├─ headroom_compress()
        ├─ headroom_retrieve()
        ├─ headroom_ccr_stats()
        └─ headroom_free()
```

## Docker

```bash
docker build -t rust_cc_proxy .
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... rust_cc_proxy
docker run -p 8787:8787 -e DEEPSEEK_API_KEY=sk-... rust_cc_proxy -- --dev
```

## Testing

```bash
cargo test                    # All tests (67 tests)
cargo test -p rust_cc_proxy   # Unit tests
cargo test --test integration # Integration tests
```

## Implementation Status

| Phase | Status | Description |
| --- | --- | --- |
| 1. Foundation proxy | ✅ | Passthrough proxy, system role normalization |
| 2. DeepSeek provider | ✅ | Provider trait, 3 compatibility fixes |
| 3. CC Switch | ✅ | `/v1/models` endpoint, model discovery |
| 4. Token compression | ✅ | JSON/diff/log/text + BM25 + CCR |
| 5. Dev mode + monitoring | ✅ | Verbose logging, `/metrics`, `/v1/usage` |
| 6. cc-switch compatible | ✅ | `/user/balance`, custom-script usage query |
| 7. Production hardening | ✅ | Rate limiting, graceful shutdown, Docker, live-zone surgery, pipeline orchestrator, token validation |

## License

MIT
