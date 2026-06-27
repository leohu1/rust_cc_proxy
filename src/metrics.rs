//! Prometheus metrics — production observability.
//!
//! Exposes a `/metrics` endpoint in Prometheus text format (always on).
//! All metrics use the `cc_proxy_` prefix.
//!
//! ## Metrics
//!
//! | Metric | Type | Labels |
//! |--------|------|--------|
//! | `cc_proxy_requests_total` | Counter | `route`, `status` |
//! | `cc_proxy_requests_active` | Gauge | — |
//! | `cc_proxy_request_duration_seconds` | Histogram | `route` |
//! | `cc_proxy_tokens_total` | Counter | `type` (input/output) |
//! | `cc_proxy_errors_total` | Counter | — |
//! | `cc_proxy_compression_bytes` | Gauge | `kind` (original/compressed) |
//! | `cc_proxy_ccr_entries` | Gauge | — |
//! | `cc_proxy_uptime_seconds` | Counter | — |

use std::sync::OnceLock;

use prometheus::{Counter, Encoder, Gauge, Histogram, HistogramOpts, Opts, Registry, TextEncoder};

// ── Registry ───────────────────────────────────────────────────────

fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| {
        let r = Registry::new();
        // Process metrics are only available on Linux with the "process" feature
        r
    })
}

// ── Metric helpers ─────────────────────────────────────────────────

macro_rules! lazy_counter {
    ($name:expr, $help:expr $(, $label_key:expr => $label_val:expr)* $(,)?) => {{
        static METRIC: OnceLock<Counter> = OnceLock::new();
        METRIC.get_or_init(|| {
            let opts = Opts::new($name, $help)
                $(.const_label($label_key, $label_val))*;
            let c = Counter::with_opts(opts).unwrap();
            registry().register(Box::new(c.clone())).ok();
            c
        })
    }};
}

macro_rules! lazy_gauge {
    ($name:expr, $help:expr) => {{
        static METRIC: OnceLock<Gauge> = OnceLock::new();
        METRIC.get_or_init(|| {
            let g = Gauge::with_opts(Opts::new($name, $help)).unwrap();
            registry().register(Box::new(g.clone())).ok();
            g
        })
    }};
}

macro_rules! lazy_histogram {
    ($name:expr, $help:expr, $buckets:expr) => {{
        static METRIC: OnceLock<Histogram> = OnceLock::new();
        METRIC.get_or_init(|| {
            let opts = HistogramOpts::new($name, $help).buckets($buckets);
            let h = Histogram::with_opts(opts).unwrap();
            registry().register(Box::new(h.clone())).ok();
            h
        })
    }};
}

// ── Metric accessors ───────────────────────────────────────────────

#[inline]
fn requests_total() -> &'static Counter {
    lazy_counter!("cc_proxy_requests_total", "Total proxy requests")
}

#[inline]
fn requests_active() -> &'static Gauge {
    lazy_gauge!("cc_proxy_requests_active", "Currently active requests")
}

#[inline]
fn request_duration() -> &'static Histogram {
    // 5 ms → 10 s, 12 buckets (exponential base 2)
    lazy_histogram!(
        "cc_proxy_request_duration_seconds",
        "Request latency distribution",
        vec![0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    )
}

#[inline]
fn tokens_input() -> &'static Counter {
    lazy_counter!(
        "cc_proxy_tokens_total",
        "Total tokens processed by type",
        "type" => "input"
    )
}

#[inline]
fn tokens_output() -> &'static Counter {
    lazy_counter!(
        "cc_proxy_tokens_total",
        "Total tokens processed by type",
        "type" => "output"
    )
}

#[inline]
fn errors_total() -> &'static Counter {
    lazy_counter!("cc_proxy_errors_total", "Total request errors")
}

#[inline]
fn compression_bytes_gauge() -> &'static Gauge {
    lazy_gauge!(
        "cc_proxy_compression_bytes",
        "Compression savings (original - compressed)"
    )
}

#[inline]
fn ccr_entries_gauge() -> &'static Gauge {
    lazy_gauge!("cc_proxy_ccr_entries", "Current CCR cache entries")
}

#[inline]
fn uptime_counter() -> &'static Counter {
    lazy_counter!("cc_proxy_uptime_seconds", "Server uptime in seconds")
}

// ── Recording functions ───────────────────────────────────────────

/// Record a completed request with duration.
pub fn record_request(duration_secs: f64) {
    requests_total().inc();
    request_duration().observe(duration_secs);
}

/// Mark start of active request.
pub fn inc_active_requests() {
    requests_active().inc();
}

/// Mark end of active request.
pub fn dec_active_requests() {
    requests_active().dec();
}

/// Record an error.
pub fn record_error() {
    errors_total().inc();
}

/// Record input/output tokens.
pub fn record_tokens(input: u64, output: u64) {
    if input > 0 {
        tokens_input().inc_by(input as f64);
    }
    if output > 0 {
        tokens_output().inc_by(output as f64);
    }
}

/// Record compression bytes (original vs compressed).
pub fn record_compression(original_bytes: usize, compressed_bytes: usize) {
    let saved = original_bytes.saturating_sub(compressed_bytes) as f64;
    compression_bytes_gauge().set(saved);
}

/// Update CCR entry count.
pub fn set_ccr_entries(count: usize) {
    ccr_entries_gauge().set(count as f64);
}

/// Update uptime.
pub fn set_uptime(secs: u64) {
    uptime_counter().inc_by(secs as f64);
}

/// Gather all metrics in Prometheus text format.
///
/// This is the main output for `GET /metrics`.
pub fn gather() -> String {
    let encoder = TextEncoder::new();
    let metric_families = registry().gather();
    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        tracing::warn!("Prometheus encode error: {e}");
        return String::new();
    }
    String::from_utf8(buffer).unwrap_or_default()
}
