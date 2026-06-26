# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`rust_cc_proxy` — a modular, extensible Claude Code proxy server in Rust (actix-web) providing multi-vendor LLM routing, token compression, and model discovery (CC Switch compatibility).

Reference projects:
- `D:\projects\dsv4-cc-proxy` (Python/Starlette) — working DeepSeek proxy with thinking fixes and protocol translation
- `D:\projects\headroom` (Rust workspace) — token compression algorithms (SmartCrusher, DiffCompressor, LogCompressor, TextCrusher), live-zone byte-range surgery, CCR storage

## Build & Run

```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo run                # Build and run
cargo test               # Run all tests
cargo test -p <name>     # Run a single test
cargo check              # Fast compile check (no codegen)
cargo fmt                # Format code
cargo clippy             # Lint
```

## High-Level Architecture

```
Claude Code CLI
  │ POST /v1/messages, GET /v1/models, etc.
  ▼
rust_cc_proxy (actix-web)
  │
  ├─ HTTP Layer: /health, /v1/messages, /v1/messages/count_tokens, /v1/models
  │
  ├─ Pipeline (sequential stages):
  │   SystemRoleNormalizer → Compression (optional) → ProviderTransform
  │
  ├─ Provider Registry: routes to correct backend by model name
  │   ├─ DeepSeek Provider (first implementation)
  │   ├─ Anthropic Provider (passthrough)
  │   └─ (OpenAI, etc. — future)
  │
  └─ Proxy Client (reqwest): upstream forwarding, SSE streaming
```

## Project Structure

```
src/
├── main.rs                    # CLI args (clap), config loading, server startup
├── config.rs                  # Config struct, env var loading
├── error.rs                   # AppError enum
│
├── server/
│   ├── mod.rs                 # actix-web App factory, router setup
│   ├── handlers.rs            # Route handlers: health, models, messages, count_tokens
│   └── sse.rs                 # SSE event serialization
│
├── pipeline/
│   ├── mod.rs                 # Pipeline struct, PipelineStage trait
│   └── system_normalizer.rs   # Extract role:system from messages[] → top-level system field
│
├── providers/
│   ├── mod.rs                 # Provider trait, ProviderRegistry, ProviderKind enum
│   ├── deepseek.rs            # DeepSeek provider (thinking fixes, request transform)
│   └── anthropic.rs           # Passthrough provider for chain/fallback
│
├── protocol/
│   ├── mod.rs                 # Re-exports
│   ├── messages.rs            # Anthropic Messages API types (request/response structs)
│   ├── sse_types.rs           # SSE event types (message_start, content_block_start, etc.)
│   └── models.rs              # Model list types for GET /v1/models
│
├── proxy/
│   ├── mod.rs                 # reqwest client pool, upstream forwarding
│   └── streaming.rs           # SSE stream parsing and pass-through
│
└── compress/                  # Future (Phase 4)
    ├── mod.rs                 # Compressor trait
    ├── noop.rs                # No-op passthrough
    └── headroom.rs            # Headroom integration (feature-gated)
```

## Core Module Design

### Provider Trait (`providers/mod.rs`)

The central abstraction for multi-vendor support:

```rust
pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn upstream_url(&self) -> &str;
    fn prepare_headers(&self, incoming: &HeaderMap) -> HeaderMap;
    fn transform_request(&self, body: &Value) -> Result<Value, AppError>;
    fn transform_response(&self, body: &Value) -> Result<Value, AppError>;
    fn model_list(&self) -> Vec<ModelInfo>;
    fn resolve_model(&self, client_model: &str) -> String;
    fn requires_sse_translation(&self) -> bool;
    fn translate_sse_event(&self, line: &str, state: &mut SseState) -> Option<String>;
}
```

`ProviderRegistry` maps `ProviderKind → Arc<dyn Provider>` and resolves `requested_model → (ProviderKind, upstream_model)`.

### DeepSeek Provider — Three Compatibility Fixes

From `dsv4-cc-proxy`:

1. **Thinking normalization** (`_normalize_thinking`): Convert `adaptive`/`auto` → `enabled` in `thinking.type`, strip `reasoning_effort` and `output_config`. When disabled, strip historical `thinking`/`redacted_thinking` blocks.

2. **Thinking injection** (`_inject_thinking_blocks`): When thinking is enabled and model starts with `deepseek-v4`, insert `{"type":"thinking","thinking":""}` before the first `tool_use` block in each assistant message (DeepSeek returns 400 without these).

3. **System role extraction**: Remove `role: "system"` entries from `messages[]` array and merge into top-level `system` field. Claude Code v2.1.154+ injects system prompts as messages, which standard Anthropic API (and DeepSeek's `/anthropic` endpoint) rejects.

### Model Resolution (deepseek.rs)

1. Already starts with `deepseek-` → passthrough
2. Exact match in `model_map` → mapped value
3. Longest prefix match in `model_map`
4. Fallback to `default_model`

### Pipeline (`pipeline/mod.rs`)

```rust
pub trait PipelineStage: Send + Sync {
    fn process(&self, request: &mut ProxyRequest) -> Result<(), AppError>;
}

pub struct Pipeline { stages: Vec<Box<dyn PipelineStage>> }
```

Default stages: `SystemRoleNormalizer` → `ProviderTransform`. Compression is an optional stage added when enabled.

### Configuration (`config.rs`)

Three-source priority: CLI args (`clap`) → environment variables → defaults.

Key env vars: `PROXY_HOST`, `PROXY_PORT`, `PROXY_LOG_LEVEL`, `PROXY_DUMP_DIR`, `DEEPSEEK_UPSTREAM`, `DEEPSEEK_API_KEY`, `DEEPSEEK_DEFAULT_MODEL`, `DEEPSEEK_MODEL_MAP`, `COMPRESSION_ENABLED`.

### HTTP Handlers

| Route | Purpose |
| --- | --- |
| `GET /health` | Health check, returns `{"status":"ok","version":"0.1.0"}` |
| `GET /v1/models` | Model list for CC Switch (model IDs prefixed `claude-` for Claude Code recognition) |
| `POST /v1/messages` | Main endpoint: pipeline → proxy → upstream → SSE response |
| `POST /v1/messages/count_tokens` | Token counting (forward or estimate) |

### Compressor Trait (Phase 4)

```rust
pub trait Compressor: Send + Sync {
    fn compress(&self, body: &mut Value) -> Result<Vec<CcrMarker>, AppError>;
}
```

`NoOpCompressor` (default) vs `HeadroomCompressor` (feature-gated, delegates to headroom-core live-zone dispatcher).

## SSE Streaming Protocol

Streaming responses use `Content-Type: text/event-stream`. Anthropic SSE event sequence (strict order):

```
message_start → content_block_start → content_block_delta* → content_block_stop → message_delta → message_stop
```

Delta sub-types: `text_delta`, `input_json_delta` (tool args), `thinking_delta`, `signature_delta`.

## Implementation Phases

### Phase 1 — Foundation Proxy
Pass-through proxy forwarding `/v1/messages` with system role normalization. SSE pass-through streaming. Verifies the basic proxy chain works against real Anthropic API.

### Phase 2 — DeepSeek Provider
Provider trait + ProviderRegistry + DeepSeek implementation with all three fixes. Claude Code works through proxy to DeepSeek.

### Phase 3 — Model Discovery (CC Switch)
`GET /v1/models` endpoint aggregating all providers' model lists. Model IDs mapped to start with `claude-` (e.g., `claude-deepseek-v4-pro`). Enables Claude Code's `/model` command.

### Phase 4 — Token Compression
Optional `compress` feature with headroom-core integration. Live-zone byte-range surgery compresses tool outputs, logs, search results. CCR (Compress-Cache-Retrieve) for reversible compression.

### Phase 5 — Production Hardening
`/v1/messages/count_tokens`, traffic dump, structured logging (`tracing`), error resilience (502 on upstream failure), request/response size limits.

## Key Design Decisions

1. **Single crate** — modules share a build, not independently versioned
2. **Feature-gated compression** — `compress` feature pulls in heavy ML deps only when needed
3. **DeepSeek `/anthropic` endpoint first** — SSE-compatible passthrough; `/v1/chat/completions` translation deferred
4. **Provider env vars** — each provider reads its own prefixed env vars (`DEEPSEEK_UPSTREAM`, etc.)
5. **Model ID prefix** — `claude-` or `anthropic-` prefix required for Claude Code model picker recognition

## Testing

```bash
# Start the proxy (with DeepSeek)
PROXY_PORT=8787 DEEPSEEK_UPSTREAM=https://api.deepseek.com/anthropic \
  DEEPSEEK_API_KEY=sk-... DEFAULT_PROVIDER=deepseek cargo run

# Test with Claude Code
ANTHROPIC_BASE_URL=http://localhost:8787 ANTHROPIC_AUTH_TOKEN=any-value \
  CLAUDE_CODE_ATTRIBUTION_HEADER=0 \
  CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1 \
  claude -p "Hello"
```