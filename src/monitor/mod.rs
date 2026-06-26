//! Token usage monitoring and request metrics.
//!
//! `TokenMonitor` tracks cumulative token usage across all requests
//! using atomic counters (lock-free). Exposed via `/v1/usage` (cc-switch
//! compatible) and `/metrics` (dev mode) endpoints.

use std::sync::atomic::{AtomicU64, Ordering};

/// Thread-safe cumulative token usage tracker.
///
/// All counters use `AtomicU64` with `Relaxed` ordering — acceptable
/// for monitoring where eventual consistency is sufficient.
#[derive(Debug, Default)]
pub struct TokenMonitor {
    /// Total number of requests handled.
    requests_total: AtomicU64,
    /// Number of streaming requests.
    requests_streaming: AtomicU64,
    /// Number of non-streaming requests.
    requests_non_streaming: AtomicU64,
    /// Number of requests that resulted in errors.
    errors_total: AtomicU64,
    /// Cumulative input (prompt) tokens.
    input_tokens_total: AtomicU64,
    /// Cumulative output (completion) tokens.
    output_tokens_total: AtomicU64,
    /// Cumulative cache-read tokens (Anthropic prompt caching).
    cache_read_tokens: AtomicU64,
    /// Cumulative cache-creation tokens.
    cache_creation_tokens: AtomicU64,
    /// Total latency (sum of all request latencies in ms).
    latency_total_ms: AtomicU64,
    /// Server start time (seconds since Unix epoch).
    start_time_secs: AtomicU64,
}

impl TokenMonitor {
    pub fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        TokenMonitor {
            start_time_secs: AtomicU64::new(now),
            ..Default::default()
        }
    }

    /// Record a successful non-streaming request with token counts.
    pub fn record_non_streaming(&self, input_tokens: u64, output_tokens: u64) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.requests_non_streaming.fetch_add(1, Ordering::Relaxed);
        self.input_tokens_total
            .fetch_add(input_tokens, Ordering::Relaxed);
        self.output_tokens_total
            .fetch_add(output_tokens, Ordering::Relaxed);
    }

    /// Record a successful streaming request.
    /// Input tokens from the SSE `message_start` event; output added later.
    pub fn record_streaming_start(&self, input_tokens: u64) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.requests_streaming.fetch_add(1, Ordering::Relaxed);
        self.input_tokens_total
            .fetch_add(input_tokens, Ordering::Relaxed);
    }

    /// Add output tokens for a streaming request (extracted from `message_delta`).
    pub fn record_streaming_output(&self, output_tokens: u64) {
        self.output_tokens_total
            .fetch_add(output_tokens, Ordering::Relaxed);
    }

    /// Add cache tokens.
    pub fn record_cache_tokens(&self, cache_read: u64, cache_creation: u64) {
        self.cache_read_tokens
            .fetch_add(cache_read, Ordering::Relaxed);
        self.cache_creation_tokens
            .fetch_add(cache_creation, Ordering::Relaxed);
    }

    /// Record request latency in ms.
    pub fn record_latency(&self, latency_ms: u64) {
        self.latency_total_ms
            .fetch_add(latency_ms, Ordering::Relaxed);
    }

    /// Record a failed request.
    pub fn record_error(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.errors_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Estimate input tokens from a serialized request body.
    /// Rough heuristic: ~4 characters per token for English text.
    pub fn estimate_input_tokens(body: &serde_json::Value) -> u64 {
        serde_json::to_string(body)
            .map(|s| (s.len() as u64).saturating_div(4))
            .unwrap_or(0)
    }

    /// Parse token usage from an Anthropic-format response JSON (non-streaming).
    pub fn parse_usage(body: &serde_json::Value) -> Option<(u64, u64)> {
        let usage = body.get("usage")?;
        let input = usage.get("input_tokens")?.as_u64()?;
        let output = usage.get("output_tokens")?.as_u64()?;
        Some((input, output))
    }

    /// Parse cache tokens from an Anthropic-format response.
    pub fn parse_cache_tokens(body: &serde_json::Value) -> (u64, u64) {
        let usage = match body.get("usage") {
            Some(u) => u,
            None => return (0, 0),
        };
        let read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        (read, creation)
    }

    /// Try to extract input_tokens from an SSE `message_start` data line.
    pub fn parse_sse_message_start(line: &str) -> Option<u64> {
        let json: serde_json::Value = serde_json::from_str(line).ok()?;
        json.get("message")?
            .get("usage")?
            .get("input_tokens")?
            .as_u64()
    }

    /// Try to extract output_tokens from an SSE `message_delta` data line.
    pub fn parse_sse_message_delta(line: &str) -> Option<u64> {
        let json: serde_json::Value = serde_json::from_str(line).ok()?;
        json.get("usage")?.get("output_tokens")?.as_u64()
    }

    /// Build a usage snapshot for `/v1/usage`.
    pub fn usage_response(&self) -> UsageResponse {
        UsageResponse {
            input_tokens_total: self.input_tokens_total.load(Ordering::Relaxed),
            output_tokens_total: self.output_tokens_total.load(Ordering::Relaxed),
            cache_read_tokens: self.cache_read_tokens.load(Ordering::Relaxed),
            cache_creation_tokens: self.cache_creation_tokens.load(Ordering::Relaxed),
            requests_total: self.requests_total.load(Ordering::Relaxed),
            requests_streaming: self.requests_streaming.load(Ordering::Relaxed),
            requests_non_streaming: self.requests_non_streaming.load(Ordering::Relaxed),
            errors_total: self.errors_total.load(Ordering::Relaxed),
            uptime_secs: self.uptime_secs(),
        }
    }

    /// Build a metrics snapshot for `/metrics`.
    pub fn metrics_response(&self) -> MetricsResponse {
        MetricsResponse {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            requests_streaming: self.requests_streaming.load(Ordering::Relaxed),
            requests_non_streaming: self.requests_non_streaming.load(Ordering::Relaxed),
            errors_total: self.errors_total.load(Ordering::Relaxed),
            input_tokens_total: self.input_tokens_total.load(Ordering::Relaxed),
            output_tokens_total: self.output_tokens_total.load(Ordering::Relaxed),
        }
    }

    fn uptime_secs(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.start_time_secs.load(Ordering::Relaxed))
    }
}

/// Response for `GET /v1/usage` — cc-switch compatible token usage endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageResponse {
    pub input_tokens_total: u64,
    pub output_tokens_total: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub requests_total: u64,
    pub requests_streaming: u64,
    pub requests_non_streaming: u64,
    pub errors_total: u64,
    pub uptime_secs: u64,
}

/// Response for `GET /metrics` — dev-mode monitoring endpoint.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsResponse {
    pub requests_total: u64,
    pub requests_streaming: u64,
    pub requests_non_streaming: u64,
    pub errors_total: u64,
    pub input_tokens_total: u64,
    pub output_tokens_total: u64,
}
