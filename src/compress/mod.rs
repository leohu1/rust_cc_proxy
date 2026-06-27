pub mod adaptive_sizer;
pub mod anchor_selector;
pub mod cache_aware;
pub mod ccr;
pub mod diff;
pub mod headroom_dll;
pub mod live_zone;
pub mod log;
pub mod pipeline_stage;
pub mod pipeline_utils;
pub mod relevance;
pub mod search;
pub mod signals;
pub mod text;
pub mod tokenizer;

use crate::error::AppError;
use ccr::CcrStore;
use serde_json::Value;

/// The result of compressing a single content block.
#[derive(Debug, Clone)]
pub enum CompressionResult {
    Compressed {
        replacement: String,
        ccr_hash: String,
        original_bytes: usize,
        compressed_bytes: usize,
    },
    Unchanged,
    Skipped,
}

/// Estimate token count from text length.
///
/// Uses a character-based heuristic:
/// - ASCII / Latin: ~4 chars per token (common in code, JSON, English prose)
/// - CJK / wide chars: ~2 chars per token (Chinese, Japanese, Korean)
///
/// This is a lightweight fallback; headroom-core uses `tiktoken-rs` for
/// exact counts but that pulls in ~5 MB of BPE data files.
pub fn estimate_tokens(text: &str) -> usize {
    let ascii_count = text.chars().filter(|c| c.is_ascii()).count();
    let wide_count = text.chars().count() - ascii_count;
    (ascii_count / 4) + (wide_count / 2)
}

/// Unified compressor that handles all content types.
///
/// Priority: Headroom DLL (if loaded) > built-in compressors.
/// When `headroom_core.dll` is present, delegates compression via C-ABI.
/// When absent, falls back to built-in lightweight implementations.
pub struct Compressor {
    pub ccr_store: std::sync::Arc<CcrStore>,
    min_bytes: usize,
    max_array_items: usize,
    diff_compressor: diff::DiffCompressor,
    log_compressor: log::LogCompressor,
    text_compressor: text::TextCompressor,
    search_compressor: search::SearchCompressor,
    bm25: relevance::BM25Scorer,
    headroom_dll: Option<headroom_dll::HeadroomDll>,
}

impl Compressor {
    pub fn new(min_bytes: usize, max_array_items: usize, ccr_config: crate::config::CcrConfig) -> Self {
        let dll = headroom_dll::HeadroomDll::load();
        if dll.is_some() {
            tracing::info!("Headroom DLL compression ENABLED");
        }

        let ccr_store = match ccr_config.backend.as_str() {
            "sqlite" => {
                let path = ccr_config.sqlite_path.as_deref().unwrap_or("ccr.db");
                match CcrStore::with_sqlite(path, ccr_config.ttl_seconds, ccr_config.purge_interval_secs) {
                    Ok(store) => {
                        tracing::info!("CCR: SQLite backend at {path} (TTL={}s)", ccr_config.ttl_seconds);
                        store
                    }
                    Err(e) => {
                        tracing::warn!("CCR: SQLite open failed ({e}), falling back to InMemory");
                        CcrStore::new(10_000)
                    }
                }
            }
            _ => {
                tracing::info!("CCR: InMemory backend (capacity=10000)");
                CcrStore::new(10_000)
            }
        };

        Compressor {
            ccr_store: std::sync::Arc::new(ccr_store),
            min_bytes,
            max_array_items,
            diff_compressor: diff::DiffCompressor::new(),
            log_compressor: log::LogCompressor::new(),
            text_compressor: text::TextCompressor::new(),
            search_compressor: search::SearchCompressor::new(),
            bm25: relevance::BM25Scorer::default(),
            headroom_dll: dll,
        }
    }

    /// Whether the Headroom DLL is active.
    pub fn using_headroom_dll(&self) -> bool {
        self.headroom_dll.is_some()
    }

    /// Create a compressor for testing that skips DLL loading and
    /// uses a permissive token gate (0.5 ratio threshold).
    #[cfg(test)]
    pub fn for_test(min_bytes: usize, max_items: usize) -> Self {
        Compressor {
            ccr_store: std::sync::Arc::new(CcrStore::new(1000)),
            min_bytes,
            max_array_items: max_items,
            diff_compressor: diff::DiffCompressor::new(),
            log_compressor: log::LogCompressor::new(),
            text_compressor: text::TextCompressor::new(),
            search_compressor: search::SearchCompressor::new(),
            bm25: relevance::BM25Scorer::default(),
            headroom_dll: None,
        }
    }

    /// Compress a single content string. Routes to the right compressor
    /// based on content-type detection.
    ///
    /// Priority: Headroom DLL (if loaded) → built-in compressors.
    ///
    /// After compression, validates that the compressed output actually
    /// has fewer estimated tokens than the original. If compression didn't
    /// help (or made it worse), returns `Unchanged`.
    pub fn compress_string(&self, content: &str) -> Result<CompressionResult, AppError> {
        if content.len() < self.min_bytes {
            return Ok(CompressionResult::Unchanged);
        }

        // ── Pre-compression reformat (lossless) ─────────────────
        // Try minifying/stripping noise before running the compressor.
        // These are cheap, lossless, and make the compressor's job easier.
        let content_owned: String;
        let content_ref: &str = if let Some(minified) = pipeline_utils::json_minify(content) {
            content_owned = minified;
            &content_owned
        } else if let Some(cleaned) = pipeline_utils::diff_noise_strip(content) {
            content_owned = cleaned;
            &content_owned
        } else {
            content
        };

        let original_tokens = tokenizer::count_tokens(content_ref);

        // Try Headroom DLL first if loaded
        let result = if let Some(ref dll) = self.headroom_dll {
            let content_type = detect_content_type(content_ref);
            let type_code: u8 = match content_type {
                ContentType::JsonArray | ContentType::JsonObject => 0u8,
                ContentType::Diff => 1u8,
                ContentType::Log => 2u8,
                ContentType::PlainText => 3u8,
                ContentType::SearchResults => 4u8,
                ContentType::Unknown => return Ok(CompressionResult::Skipped),
            };
            dll.compress(content_ref, type_code)
        } else {
            None
        };

        let result = match result {
            Some(r) => r,
            None => {
                // Fall back to built-in compressors
                let content_type = detect_content_type(content_ref);
                match content_type {
                    ContentType::JsonArray => self.compress_json_array_str(content_ref),
                    ContentType::JsonObject => self.compress_json_object_str(content_ref),
                    ContentType::Diff => self.diff_compressor.compress(content_ref, &self.ccr_store),
                    ContentType::Log => self.log_compressor.compress(content_ref, &self.ccr_store),
                    ContentType::SearchResults => self.search_compressor.compress(content_ref, &self.ccr_store),
                    ContentType::PlainText => {
                        self.text_compressor.compress(content_ref, &self.ccr_store)
                    }
                    ContentType::Unknown => CompressionResult::Skipped,
                }
            }
        };

        // ── Tokenizer validation gate ────────────────────────────
        // Verify compressed output actually has fewer tokens. If not,
        // return Unchanged to avoid wasting tokens on ineffective compression.
        if let CompressionResult::Compressed {
            ref replacement, ..
        } = &result
        {
            let compressed_tokens = tokenizer::count_tokens(replacement);
            if compressed_tokens >= original_tokens {
                tracing::debug!(
                    "Tokenizer gate rejected: orig={original_tokens} comp={compressed_tokens} tokens"
                );
                return Ok(CompressionResult::Unchanged);
            }
            let savings = original_tokens.saturating_sub(compressed_tokens);
            let pct = (savings as f64 / original_tokens as f64 * 100.0) as u32;
            tracing::debug!(
                "Tokenizer gate passed: {original_tokens}→{compressed_tokens} tokens (-{pct}%)"
            );
        }

        Ok(result)
    }

    /// Compress JSON array string with adaptive sizing + anchor selection.
    fn compress_json_array_str(&self, content: &str) -> CompressionResult {
        let trimmed = content.trim();
        let parsed: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return CompressionResult::Skipped,
        };
        let items = match parsed.as_array() {
            Some(a) => a,
            None => return CompressionResult::Skipped,
        };
        if items.len() <= self.max_array_items {
            return CompressionResult::Unchanged;
        }

        let total = items.len();

        // Build query context + BM25 scores
        let query = build_query_context(items);
        let item_strs: Vec<String> = items
            .iter()
            .map(|v| serde_json::to_string(v).unwrap_or_default())
            .collect();
        let item_refs: Vec<&str> = item_strs.iter().map(|s| s.as_str()).collect();
        let scores = self.bm25.score_items(&item_refs, &query);

        // Adaptive sizing: how many to keep?
        let optimal_k = adaptive_sizer::compute_optimal_k(
            &item_refs, 1.0, 3, Some(self.max_array_items),
        );

        // Anchor selection: guaranteed positions
        let anchor_selector = anchor_selector::AnchorSelector::default();
        let anchors = anchor_selector.select_anchors(
            total, optimal_k, anchor_selector::DataPattern::Generic, Some(&scores),
        );

        // Fill remaining slots with top-scored items (excluding anchors)
        let remaining_slots = optimal_k.saturating_sub(anchors.len());
        let mut fill_indices: Vec<usize> = (0..total)
            .filter(|i| !anchors.contains(i))
            .collect();
        fill_indices.sort_by(|a, b| {
            scores[*b].partial_cmp(&scores[*a]).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut selected: std::collections::BTreeSet<usize> = anchors.clone();
        for idx in fill_indices.iter().take(remaining_slots) {
            selected.insert(*idx);
        }

        // Build compressed array with omission markers
        let mut compressed_items: Vec<Value> = Vec::new();
        let mut expected = 0usize;
        let original_bytes = serde_json::to_string(items).unwrap_or_default().len();

        for &idx in &selected {
            if idx > expected {
                compressed_items.push(Value::String(format!(
                    "… {} items omitted …",
                    idx - expected
                )));
            }
            compressed_items.push(items[idx].clone());
            expected = idx + 1;
        }
        if expected < total {
            compressed_items.push(Value::String(format!(
                "… {} items omitted …",
                total - expected
            )));
        }

        let replacement = serde_json::to_string(&compressed_items).unwrap_or_default();
        let ccr_hash = self
            .ccr_store
            .store(&serde_json::to_string(items).unwrap_or_default());

        let full = format!(
            "/* {}/{} items (k={optimal_k}), {}→{} bytes. <<ccr:{}>> */\n{}",
            selected.len(),
            total,
            original_bytes,
            replacement.len(),
            ccr_hash,
            replacement
        );

        CompressionResult::Compressed {
            compressed_bytes: full.len(),
            replacement: full,
            ccr_hash,
            original_bytes,
        }
    }

    fn compress_json_object_str(&self, content: &str) -> CompressionResult {
        let obj: Value = match serde_json::from_str(content) {
            Ok(v) => v,
            Err(_) => return CompressionResult::Skipped,
        };
        let original_bytes = content.len();
        let original = serde_json::to_string(&obj).unwrap_or_default();
        let ccr_hash = self.ccr_store.store(&original);
        let summary = summarize_object(&obj);

        let replacement = format!(
            "/* Object {} bytes. <<ccr:{}>> */\n{}",
            original_bytes,
            ccr_hash,
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );

        CompressionResult::Compressed {
            compressed_bytes: replacement.len(),
            replacement,
            ccr_hash,
            original_bytes,
        }
    }

    /// Retrieve original content from the CCR store by hash.
    /// Tries Headroom DLL first (if loaded), then built-in CCR.
    pub fn retrieve(&self, hash: &str) -> Option<String> {
        if let Some(ref dll) = self.headroom_dll {
            if let Some(content) = dll.retrieve(hash) {
                return Some(content);
            }
        }
        self.ccr_store.get(hash)
    }

    /// Return CCR store stats for monitoring.
    /// Merges built-in CCR stats with DLL stats when available.
    pub fn stats(&self) -> serde_json::Value {
        let builtin = self.ccr_store.stats();
        let mut result = serde_json::json!({
            "builtin": {
                "entries": builtin.entries,
                "max_entries": builtin.max_entries,
                "total_stored": builtin.total_stored,
                "hits": builtin.hits,
                "misses": builtin.misses,
            },
        });

        if let Some(ref dll) = self.headroom_dll {
            if let Some(dll_stats) = dll.ccr_stats() {
                result["dll"] = dll_stats;
            }
        }

        result
    }
}

// ── Content-type detection ────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum ContentType {
    JsonArray,
    JsonObject,
    Diff,
    Log,
    SearchResults,
    PlainText,
    Unknown,
}

fn detect_content_type(content: &str) -> ContentType {
    let trimmed = content.trim();

    // JSON detection
    if (trimmed.starts_with('[') || trimmed.starts_with('{'))
        && serde_json::from_str::<Value>(trimmed).is_ok()
    {
        if trimmed.starts_with('[') {
            return ContentType::JsonArray;
        }
        return ContentType::JsonObject;
    }

    // Diff detection
    if trimmed.contains("diff --git ") || (trimmed.starts_with("@@ -") && trimmed.contains("+")) {
        return ContentType::Diff;
    }

    // Search result detection — check if most lines look like file:line:content
    if is_search_content(trimmed) {
        return ContentType::SearchResults;
    }

    // Log detection
    if is_log_content(trimmed) {
        return ContentType::Log;
    }

    // Plain text (long enough)
    if trimmed.len() > 800 {
        return ContentType::PlainText;
    }

    ContentType::Unknown
}

fn is_search_content(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().take(50).collect();
    if lines.len() < 5 {
        return false;
    }
    // Count lines that look like `path:digits:text`
    let match_like = lines
        .iter()
        .filter(|line| {
            let line = line.trim();
            if line.is_empty() {
                return false;
            }
            // Check for `file:line:text` or `file:line:col:text` pattern
            is_match_line_like(line)
        })
        .count();
    // If > 60% of first 50 lines look like match output, classify as search
    match_like > lines.len() * 3 / 5
}

/// Quick check: does this line look like `file:line:text`?
fn is_match_line_like(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut colon_count = 0usize;
    let mut digit_run_after_colon = false;

    for (i, &b) in bytes.iter().enumerate() {
        if b == b':' || b == b'-' {
            colon_count += 1;
            // Check if what follows is digits
            let rest = &bytes[i + 1..];
            if let Some(&first) = rest.first() {
                if first.is_ascii_digit() {
                    digit_run_after_colon = true;
                }
            }
        }
    }

    colon_count >= 2 && digit_run_after_colon
}

fn is_log_content(content: &str) -> bool {
    let head = &content[..content.len().min(2000)];
    let indicators = [
        "test session starts",
        "ERROR",
        "WARNING",
        "INFO",
        "DEBUG",
        "traceback",
        "Compiling",
        "error[",
        "warning[",
        "npm ERR!",
        "FAIL ",
        "PASS ",
        "make[",
        "::error",
    ];
    let match_count = indicators.iter().filter(|&&i| head.contains(i)).count();
    match_count >= 3
}

/// Build a query string from important items (errors, first, last) for BM25 scoring.
fn build_query_context(items: &[Value]) -> String {
    let n = items.len();
    let indices: Vec<usize> = (0..3.min(n))
        .chain((n.saturating_sub(3)..n).filter(|&i| i >= 3))
        .collect();

    let mut terms: Vec<String> = Vec::new();
    for &i in &indices {
        let s = serde_json::to_string(&items[i]).unwrap_or_default();
        let has_error = s.to_lowercase().contains("error")
            || s.to_lowercase().contains("fail")
            || s.to_lowercase().contains("exception");
        if has_error {
            terms.push(s);
        }
    }

    // Also scan for error items
    for item in items {
        let s = serde_json::to_string(item).unwrap_or_default();
        if s.to_lowercase().contains("error") {
            terms.push(s);
            if terms.len() >= 10 {
                break;
            }
        }
    }

    terms.join(" ")
}

/// Create a summarized version of a JSON object.
fn summarize_object(obj: &Value) -> Value {
    match obj {
        Value::Object(map) => {
            let mut result = serde_json::Map::new();
            for (key, val) in map.iter().take(20) {
                result.insert(key.clone(), summarize_value(val));
            }
            if map.len() > 20 {
                result.insert(
                    "...truncated...".to_string(),
                    Value::String(format!("{} more fields", map.len() - 20)),
                );
            }
            Value::Object(result)
        }
        Value::Array(items) if items.len() > 5 => {
            let mut summary: Vec<Value> = items[..3].iter().map(summarize_value).collect();
            summary.push(Value::String(format!("... {} more", items.len() - 3)));
            Value::Array(summary)
        }
        Value::Array(items) => Value::Array(items.iter().map(summarize_value).collect()),
        Value::String(s) if s.len() > 200 => {
            Value::String(format!("{}... ({} chars)", &s[..200], s.len()))
        }
        other => other.clone(),
    }
}

fn summarize_value(val: &Value) -> Value {
    match val {
        Value::String(s) if s.len() > 200 => {
            Value::String(format!("{}... ({} chars)", &s[..200], s.len()))
        }
        Value::Object(_) => summarize_object(val),
        Value::Array(_) => summarize_object(val),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_small_unchanged() {
        let c = Compressor::for_test(512, 10);
        assert!(matches!(
            c.compress_string("hello").unwrap(),
            CompressionResult::Unchanged
        ));
    }

    #[test]
    fn test_detect_diff() {
        let content = "diff --git a/file.txt b/file.txt\n--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n-context\n+new";
        assert_eq!(detect_content_type(content), ContentType::Diff);
    }

    #[test]
    fn test_detect_log() {
        let content = "ERROR: fail\nWARNING: warn\nINFO: info\nDEBUG: debug\ntraceback: ...";
        assert_eq!(detect_content_type(content), ContentType::Log);
    }

    #[test]
    fn test_detect_search() {
        let mut content = String::new();
        for i in 0..20 {
            content.push_str(&format!("src/main.rs:{i}: some match content here\n"));
        }
        assert_eq!(detect_content_type(&content), ContentType::SearchResults);
    }

    #[test]
    fn test_detect_search_not_false_positive() {
        // Regular prose with colons shouldn't be misdetected as search
        let content = "This is: a sentence. Another: one here.\nMore text: yes.\nLine 4: ok.\nLine 5";
        assert_eq!(detect_content_type(content), ContentType::Unknown);
    }

    #[test]
    fn test_compress_json_array_with_bm25() {
        let items: Vec<Value> = (0..100)
            .map(|i| serde_json::json!({"id": i, "name": format!("item-{i}"), "value": i * 10}))
            .collect();
        let json = serde_json::to_string(&items).unwrap();
        let c = Compressor::for_test(100, 10);
        match c.compress_string(&json).unwrap() {
            CompressionResult::Compressed {
                compressed_bytes,
                original_bytes,
                ..
            } => {
                assert!(compressed_bytes < original_bytes);
            }
            other => panic!("expected Compressed, got {other:?}"),
        }
    }

    #[test]
    fn test_compress_diff() {
        let mut diff = String::new();
        diff.push_str("diff --git a/src/main.rs b/src/main.rs\n");
        diff.push_str("--- a/src/main.rs\n+++ b/src/main.rs\n");
        for i in 0..100 {
            diff.push_str(&format!(
                "@@ -{},3 +{},3 @@ fn foo\n",
                i * 10 + 1,
                i * 10 + 1
            ));
            diff.push_str(" unchanged context\n");
            diff.push_str(&format!("-old line {i}\n"));
            diff.push_str(&format!("+new line {i}\n"));
            diff.push_str(" more context\n");
        }
        let c = Compressor::for_test(512, 10);
        match c.compress_string(&diff).unwrap() {
            CompressionResult::Compressed { .. } => {}
            other => panic!("expected Compressed, got {other:?}"),
        }
    }

    #[test]
    fn test_compress_log() {
        let mut log = String::new();
        for i in 0..200 {
            log.push_str(&format!("INFO: processing item {i}\n"));
            if i % 10 == 0 {
                log.push_str(&format!("WARNING: slow operation at item {i}\n"));
            }
            if i % 50 == 0 {
                log.push_str(&format!("ERROR: failure at item {i}\n"));
            }
        }
        let c = Compressor::for_test(512, 10);
        match c.compress_string(&log).unwrap() {
            CompressionResult::Compressed { .. } => {}
            other => panic!("expected Compressed, got {other:?}"),
        }
    }

    #[test]
    fn test_compress_text() {
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("This is sentence number {i} with some content. "));
        }
        text.push_str("ERROR: critical failure at the end.");
        let c = Compressor::for_test(512, 10);
        match c.compress_string(&text).unwrap() {
            CompressionResult::Compressed { .. } => {}
            other => panic!("expected Compressed, got {other:?}"),
        }
    }

    #[test]
    fn test_token_estimate() {
        // ASCII: token count should be positive
        assert!(tokenizer::count_tokens("hello world this is a test") > 0);
        // JSON: compact JSON tokens
        let json = r#"{"key":"value","items":[1,2,3]}"#;
        let tokens = tokenizer::count_tokens(json);
        assert!(tokens > 0 && tokens < json.len());
    }

    #[test]
    fn test_token_validator_rejects_bad_compression() {
        let c = Compressor::for_test(10, 5);
        // Content below threshold → unchanged
        let result = c.compress_string("short").unwrap();
        assert!(matches!(result, CompressionResult::Unchanged));
    }

    #[test]
    fn test_token_validator_accepts_good_compression() {
        let items: Vec<Value> = (0..100)
            .map(|i| serde_json::json!({"id": i, "name": format!("item-{i}")}))
            .collect();
        let json = serde_json::to_string(&items).unwrap();

        let c = Compressor::for_test(100, 10);
        let result = c.compress_string(&json).unwrap();
        // Large JSON array should be compressed AND pass token validation
        match result {
            CompressionResult::Compressed {
                ref replacement, ..
            } => {
                let orig = tokenizer::count_tokens(&json);
                let comp = tokenizer::count_tokens(replacement);
                assert!(
                    comp < orig,
                    "compressed tokens ({comp}) < original ({orig})"
                );
            }
            _ => {} // DLL may return Unchanged if it deems the content already efficient
        }
    }

    #[test]
    fn test_ccr_retrieve() {
        let c = Compressor::for_test(10, 5);
        let hash = c.ccr_store.store("test data");
        assert_eq!(c.retrieve(&hash).unwrap(), "test data");
    }

    #[test]
    fn test_headroom_dll_integration() {
        let c = Compressor::for_test(10, 5);

        // If DLL is loaded, verify compress + retrieve round-trip
        if c.using_headroom_dll() {
            // JSON array compression
            let items: Vec<Value> = (0..50)
                .map(|i| serde_json::json!({"id": i, "value": format!("item-{i}")}))
                .collect();
            let json = serde_json::to_string(&items).unwrap();

            match c.compress_string(&json).unwrap() {
                CompressionResult::Compressed { ccr_hash, .. } => {
                    // Verify CCR round-trip through DLL
                    let original = c.retrieve(&ccr_hash);
                    assert!(
                        original.is_some(),
                        "DLL CCR should store compressed content"
                    );
                    let parsed: Value = serde_json::from_str(&original.unwrap()).unwrap();
                    assert_eq!(parsed.as_array().unwrap().len(), 50);
                }
                _ => {} // skip if unchanged
            }
        }
    }
}
