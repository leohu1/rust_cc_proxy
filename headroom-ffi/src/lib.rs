//! C-ABI exports for headroom-core compression.
//!
//! This cdylib exposes the headroom compression pipeline through a
//! stable C interface so that downstream consumers (like rust_cc_proxy)
//! can dynamically load it via `libloading` without depending on the
//! heavy headroom-core dependency tree at compile time.
//!
//! ## Memory ownership
//! Every function that returns a heap-allocated string also provides a
//! corresponding `headroom_free_*` function. The caller MUST call the
//! free function to avoid leaks.

use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};
use std::sync::Mutex;

// ── Simple CCR store (in-memory, no heavy deps) ──────────────────

struct CcrStore {
    entries: Mutex<HashMap<String, String>>,
    total_stored: Mutex<usize>,
    hits: Mutex<u64>,
    misses: Mutex<u64>,
}

impl CcrStore {
    fn new() -> Self {
        CcrStore {
            entries: Mutex::new(HashMap::new()),
            total_stored: Mutex::new(0),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
        }
    }
    fn store(&self, content: &str) -> String {
        let hash = blake3::hash(content.as_bytes()).to_hex()[..24].to_string();
        self.entries
            .lock()
            .unwrap()
            .insert(hash.clone(), content.to_string());
        *self.total_stored.lock().unwrap() += 1;
        hash
    }
    fn get(&self, hash: &str) -> Option<String> {
        let entries = self.entries.lock().unwrap();
        match entries.get(hash) {
            Some(v) => {
                *self.hits.lock().unwrap() += 1;
                Some(v.clone())
            }
            None => {
                *self.misses.lock().unwrap() += 1;
                None
            }
        }
    }
}

static CCR: std::sync::LazyLock<Mutex<CcrStore>> =
    std::sync::LazyLock::new(|| Mutex::new(CcrStore::new()));

// ── Helper ────────────────────────────────────────────────────────

unsafe fn cstr_to_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    CStr::from_ptr(ptr).to_string_lossy().to_string()
}

fn str_to_cstring(s: &str) -> *mut c_char {
    CString::new(s).unwrap_or_default().into_raw()
}

// ── Compression ───────────────────────────────────────────────────

/// Compress content. `content_type`: 0=JSON, 1=diff, 2=log, 3=text, 4=search.
/// Returns a JSON string: {"status":"compressed","replacement":"...","original_bytes":N,...}
/// or {"status":"unchanged"} or {"status":"error","message":"..."}.
/// Caller must free with `headroom_free`.
#[no_mangle]
pub extern "C" fn headroom_compress(content: *const c_char, content_type: u8) -> *mut c_char {
    let input = unsafe { cstr_to_str(content) };
    let result = match content_type {
        0 => compress_json(&input),
        1 => compress_diff(&input),
        2 => compress_log(&input),
        3 => compress_text(&input),
        4 => compress_search(&input),
        _ => serde_json::json!({"status": "error", "message": "unknown content_type"}).to_string(),
    };
    str_to_cstring(&result)
}

fn compress_json(input: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(input.trim()) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({"status":"error","message":e.to_string()}).to_string(),
    };

    let items = match parsed.as_array() {
        Some(a) => a,
        None => return serde_json::json!({"status":"unchanged"}).to_string(),
    };

    if items.len() <= 10 {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }

    let original = serde_json::to_string(items).unwrap_or_default();
    let original_bytes = original.len();

    // Keep first 3, last 3, and scan for errors
    let mut selected: Vec<serde_json::Value> = Vec::new();
    let total = items.len();

    for item in items.iter().take(3) {
        selected.push(item.clone());
    }
    let mut omitted = total - 6;
    for item in items.iter().skip(3).take(total.saturating_sub(6)) {
        let s = serde_json::to_string(item).unwrap_or_default();
        if s.to_lowercase().contains("error") || s.to_lowercase().contains("fail") {
            selected.push(item.clone());
            omitted -= 1;
        }
    }
    if omitted > 0 {
        selected.push(serde_json::Value::String(format!(
            "… {omitted} items omitted …"
        )));
    }
    for item in items.iter().skip(total.saturating_sub(3)) {
        selected.push(item.clone());
    }

    let compressed = serde_json::to_string(&selected).unwrap_or_default();
    let ccr_hash = CCR.lock().unwrap().store(&original);

    serde_json::json!({
        "status": "compressed",
        "replacement": format!("/* {}/{} items. <<ccr:{}>> */\n{}", selected.len(), total, ccr_hash, compressed),
        "original_bytes": original_bytes,
        "compressed_bytes": compressed.len(),
        "ccr_hash": ccr_hash,
    }).to_string()
}

fn compress_diff(input: &str) -> String {
    if input.lines().count() < 30 {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }
    let original_bytes = input.len();

    // Simple diff compression: keep header + first/last hunks per file
    let mut compressed = String::new();
    let mut in_hunk = false;
    let mut hunk_lines: Vec<&str> = Vec::new();
    let mut total_hunks = 0;

    for line in input.lines() {
        if line.starts_with("diff --git ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            if in_hunk && !hunk_lines.is_empty() {
                total_hunks += 1;
                if total_hunks <= 2 || hunk_lines.len() <= 3 {
                    for hl in &hunk_lines {
                        compressed.push_str(hl);
                        compressed.push('\n');
                    }
                }
            }
            compressed.push_str(line);
            compressed.push('\n');
            in_hunk = false;
            hunk_lines.clear();
        } else if line.starts_with("@@") {
            if in_hunk && !hunk_lines.is_empty() {
                total_hunks += 1;
                if total_hunks <= 2 || hunk_lines.len() <= 3 {
                    for hl in &hunk_lines {
                        compressed.push_str(hl);
                        compressed.push('\n');
                    }
                }
            }
            compressed.push_str(line);
            compressed.push('\n');
            in_hunk = true;
            hunk_lines.clear();
        } else if in_hunk {
            hunk_lines.push(line);
        } else {
            compressed.push_str(line);
            compressed.push('\n');
        }
    }

    let compressed_bytes = compressed.len();
    if compressed_bytes >= original_bytes * 8 / 10 {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }

    let ccr_hash = CCR.lock().unwrap().store(input);
    serde_json::json!({
        "status": "compressed",
        "replacement": format!("/* Diff {}→{} bytes. <<ccr:{}>> */\n{}", original_bytes, compressed_bytes, ccr_hash, compressed),
        "original_bytes": original_bytes,
        "compressed_bytes": compressed_bytes,
        "ccr_hash": ccr_hash,
    }).to_string()
}

fn compress_log(input: &str) -> String {
    if input.lines().count() < 50 {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }
    let original_bytes = input.len();

    // Keep error/warning lines + context
    let mut selected: Vec<(usize, &str)> = Vec::new();
    for (i, line) in input.lines().enumerate() {
        let lower = line.to_lowercase();
        if lower.contains("error")
            || lower.contains("fail")
            || lower.contains("warn")
            || lower.contains("panic")
        {
            // Add context window
            let start = i.saturating_sub(2);
            let end = (i + 3).min(input.lines().count());
            for j in start..end {
                if let Some(l) = input.lines().nth(j) {
                    selected.push((j, l));
                }
            }
        }
    }
    selected.sort_by_key(|(i, _)| *i);
    selected.dedup_by_key(|(i, _)| *i);

    let compressed: String = selected
        .iter()
        .map(|(_, l)| *l)
        .collect::<Vec<_>>()
        .join("\n");
    let compressed_bytes = compressed.len();
    if compressed_bytes >= original_bytes * 8 / 10 || selected.is_empty() {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }

    let ccr_hash = CCR.lock().unwrap().store(input);
    serde_json::json!({
        "status": "compressed",
        "replacement": format!("/* Log {}→{} bytes. <<ccr:{}>> */\n{}", original_bytes, compressed_bytes, ccr_hash, compressed),
        "original_bytes": original_bytes,
        "compressed_bytes": compressed_bytes,
        "ccr_hash": ccr_hash,
    }).to_string()
}

fn compress_text(input: &str) -> String {
    if input.len() < 800 {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }
    let original_bytes = input.len();

    // Sentence-level extraction
    let sentences: Vec<&str> = input
        .split(|c| c == '.' || c == '!' || c == '?')
        .filter(|s| s.trim().len() > 2)
        .collect();
    if sentences.len() <= 3 {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }

    let keep = (sentences.len() / 2).max(3);
    let mut scored: Vec<(usize, f64)> = sentences
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut score = 0.0;
            let lower = s.to_lowercase();
            if lower.contains("error") || lower.contains("fail") {
                score += 0.5;
            }
            if lower.contains("warn") {
                score += 0.3;
            }
            score += (s.chars().filter(|c| c.is_ascii_digit()).count() as f64
                / s.len().max(1) as f64)
                * 0.2;
            score += (i as f64 / sentences.len() as f64) * 0.3;
            (i, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut indices: Vec<usize> = scored.iter().take(keep).map(|(i, _)| *i).collect();
    indices.sort();

    let compressed: String = indices
        .iter()
        .map(|&i| sentences[i])
        .collect::<Vec<_>>()
        .join(". ");
    let compressed_bytes = compressed.len();

    let ccr_hash = CCR.lock().unwrap().store(input);
    serde_json::json!({
        "status": "compressed",
        "replacement": format!("/* Text {}→{} bytes. <<ccr:{}>> */\n{}", original_bytes, compressed_bytes, ccr_hash, compressed),
        "original_bytes": original_bytes,
        "compressed_bytes": compressed_bytes,
        "ccr_hash": ccr_hash,
    }).to_string()
}

// ── Search compressor (content_type=4) ─────────────────────────────

fn compress_search(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < 20 {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }
    let original_bytes = input.len();

    // Group by file
    let mut file_groups: std::collections::BTreeMap<String, Vec<(u64, &str)>> =
        std::collections::BTreeMap::new();
    let mut match_count = 0usize;
    for line in &lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Quick parse: find first `:digit:` pattern
        if let Some(rest) = find_file_line(line) {
            let file = &line[..line.len() - rest.len() - 1];
            if let Some((num_str, content)) = rest.split_once(':') {
                if let Ok(line_num) = num_str.parse::<u64>() {
                    file_groups
                        .entry(file.to_string())
                        .or_default()
                        .push((line_num, content.trim()));
                    match_count += 1;
                }
            }
        }
    }

    if file_groups.is_empty() {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }

    // Keep top 10 files, top 3 matches each
    let mut file_list: Vec<(String, Vec<(u64, &str)>)> = file_groups.into_iter().collect();
    file_list.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    file_list.truncate(10);

    let mut compressed = String::new();
    let mut kept = 0usize;
    for (file, matches) in &file_list {
        let take = 3.min(matches.len());
        for (line_num, content) in matches.iter().take(take) {
            compressed.push_str(&format!("{}:{}:{}\n", file, line_num, content));
            kept += 1;
        }
        if matches.len() > take {
            compressed.push_str(&format!(
                "{}: … and {} more matches …\n",
                file,
                matches.len() - take
            ));
        }
    }

    let compressed_bytes = compressed.len();
    if compressed_bytes >= original_bytes * 8 / 10 {
        return serde_json::json!({"status":"unchanged"}).to_string();
    }

    let ccr_hash = CCR.lock().unwrap().store(input);
    serde_json::json!({
        "status": "compressed",
        "replacement": format!("/* Search {}/{} matches. <<ccr:{}>> */\n{}", kept, match_count, ccr_hash, compressed),
        "original_bytes": original_bytes,
        "compressed_bytes": compressed_bytes,
        "ccr_hash": ccr_hash,
    }).to_string()
}

/// Find `:digits:` separator in a line, returning the rest after the file path.
fn find_file_line(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        if bytes[i] == b':' && bytes[i + 1].is_ascii_digit() {
            // Look for the next `:` after digits
            let rest = &bytes[i + 1..];
            if let Some(j) = rest.iter().position(|&b| b == b':') {
                return Some(&line[i + 1 + j..]);
            }
        }
    }
    None
}

// ── CCR retrieval ──────────────────────────────────────────────────

/// Retrieve original content by CCR hash. Returns null if not found.
/// Caller must free with `headroom_free`.
#[no_mangle]
pub extern "C" fn headroom_retrieve(hash: *const c_char) -> *mut c_char {
    let hash = unsafe { cstr_to_str(hash) };
    match CCR.lock().unwrap().get(&hash) {
        Some(content) => str_to_cstring(&content),
        None => std::ptr::null_mut(),
    }
}

/// Retrieve CCR stats as JSON.
/// Caller must free with `headroom_free`.
#[no_mangle]
pub extern "C" fn headroom_ccr_stats() -> *mut c_char {
    let store = CCR.lock().unwrap();
    let json = serde_json::json!({
        "entries": store.entries.lock().unwrap().len(),
        "total_stored": *store.total_stored.lock().unwrap(),
        "hits": *store.hits.lock().unwrap(),
        "misses": *store.misses.lock().unwrap(),
    });
    str_to_cstring(&json.to_string())
}

// ── Memory management ──────────────────────────────────────────────

/// Free a string previously returned by any headroom_* function.
#[no_mangle]
pub extern "C" fn headroom_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(ptr);
    }
}
