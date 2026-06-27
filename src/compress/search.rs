//! Search result compressor — grep/ripgrep output compression.
//!
//! Parses lines like `file:line:content`, scores by relevance, caps
//! matches per file and total files, and produces a compressed
//! summary with a CCR marker for full retrieval.
//!
//! ## Input formats supported
//!
//! - `file:line:content` (standard grep -n)
//! - `file:line:col:content` (ripgrep with --column)
//! - `file-line-content` (alternative separator)
//! - Windows paths: `C:\path\to\file:line:content`

use crate::compress::adaptive_sizer;
use crate::compress::ccr::CcrStore;
use crate::compress::signals::{self, ImportanceContext};
use crate::compress::CompressionResult;

// ── Types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SearchMatch {
    #[allow(dead_code)]
    file: String,
    line_number: u64,
    content: String,
    score: f32,
}

#[derive(Debug, Clone)]
struct FileMatches {
    file: String,
    matches: Vec<SearchMatch>,
}

impl FileMatches {
    fn total_score(&self) -> f32 {
        self.matches.iter().map(|m| m.score).sum()
    }
}

// ── Configuration ─────────────────────────────────────────────────

pub struct SearchCompressor {
    min_chars: usize,
    max_files: usize,
    max_matches_per_file: usize,
    max_total_matches: usize,
    always_keep_first: bool,
    always_keep_last: bool,
}

impl Default for SearchCompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchCompressor {
    pub fn new() -> Self {
        SearchCompressor {
            min_chars: 500,
            max_files: 15,
            max_matches_per_file: 5,
            max_total_matches: 30,
            always_keep_first: true,
            always_keep_last: true,
        }
    }

    /// Compress search results. Returns `Unchanged` if content is too
    /// small or doesn't parse as search output.
    pub fn compress(&self, content: &str, ccr_store: &CcrStore) -> CompressionResult {
        if content.len() < self.min_chars {
            return CompressionResult::Unchanged;
        }

        // Parse into file-groups
        let mut files = self.parse_search_results(content);
        if files.is_empty() {
            return CompressionResult::Unchanged;
        }

        let original_match_count: usize = files.iter().map(|f| f.matches.len()).sum();
        if original_match_count == 0 {
            return CompressionResult::Unchanged;
        }

        // Score matches
        self.score_matches(&mut files);

        // Select top files + top matches per file
        let selected = self.select_matches(&files);

        // Format output
        let (compressed, summaries) = self.format_output(&selected, &files);

        let original_bytes = content.len();
        let compressed_bytes = compressed.len();

        // Don't bother if savings are negligible
        if compressed_bytes >= original_bytes * 85 / 100 {
            return CompressionResult::Unchanged;
        }

        let ccr_hash = ccr_store.store(content);

        let replacement = format!(
            "/* Search: {}/{} matches across {}/{} files, {}→{} bytes. <<ccr:{}>> */\n{}{}",
            selected.iter().map(|f| f.matches.len()).sum::<usize>(),
            original_match_count,
            selected.len(),
            files.len(),
            original_bytes,
            compressed_bytes,
            ccr_hash,
            compressed,
            if !summaries.is_empty() {
                format!(
                    "\n/* File summaries: {} */",
                    summaries
                        .iter()
                        .map(|(f, s)| format!("{f}: {s}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            } else {
                String::new()
            },
        );

        CompressionResult::Compressed {
            compressed_bytes: replacement.len(),
            replacement,
            ccr_hash,
            original_bytes,
        }
    }

    // ── Stage 1: Parse ──────────────────────────────────────────

    fn parse_search_results(&self, content: &str) -> Vec<FileMatches> {
        let mut file_map: std::collections::BTreeMap<String, Vec<SearchMatch>> =
            std::collections::BTreeMap::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some((file, line_num, match_content)) = parse_match_line(line) {
                file_map
                    .entry(file.to_string())
                    .or_default()
                    .push(SearchMatch {
                        file: file.to_string(),
                        line_number: line_num,
                        content: match_content.to_string(),
                        score: 0.0,
                    });
            }
        }

        file_map
            .into_iter()
            .map(|(file, matches)| FileMatches { file, matches })
            .collect()
    }

    // ── Stage 2: Score ─────────────────────────────────────────

    fn score_matches(&self, files: &mut [FileMatches]) {
        for file_matches in files.iter_mut() {
            let total = file_matches.matches.len();
            for m in file_matches.matches.iter_mut() {
                m.score = score_line(&m.content, m.line_number, total);
            }
        }
    }

    // ── Stage 3: Select ────────────────────────────────────────

    fn select_matches(&self, files: &[FileMatches]) -> Vec<FileMatches> {
        let mut sorted_files: Vec<&FileMatches> = files.iter().collect();
        sorted_files.sort_by(|a, b| {
            b.total_score()
                .partial_cmp(&a.total_score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Cap total files
        let max_files = self.max_files.min(sorted_files.len());
        let selected_files = &sorted_files[..max_files];

        // Adaptive sizing: determine optimal total match count
        let all_match_strs: Vec<String> = selected_files
            .iter()
            .flat_map(|fm| fm.matches.iter().map(|m| m.content.clone()))
            .collect();
        let match_refs: Vec<&str> = all_match_strs.iter().map(|s| s.as_str()).collect();
        let adaptive_total =
            adaptive_sizer::compute_optimal_k(&match_refs, 1.0, 5, Some(self.max_total_matches));

        // Within each file, select top matches
        let mut result: Vec<FileMatches> = Vec::new();
        let mut global_count = 0usize;

        for fm in selected_files {
            let mut ranked: Vec<&SearchMatch> = fm.matches.iter().collect();
            ranked.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            let per_file = self.max_matches_per_file.min(ranked.len());
            let remaining_global = adaptive_total.saturating_sub(global_count);
            let take = per_file.min(remaining_global);
            if take == 0 {
                break;
            }

            let mut selected_matches: Vec<SearchMatch> = Vec::new();
            let total = ranked.len();

            // Always keep first if configured
            if self.always_keep_first && take >= 2 && total > take {
                if let Some(first) = fm.matches.first() {
                    if !selected_matches
                        .iter()
                        .any(|s| s.line_number == first.line_number)
                    {
                        selected_matches.push(first.clone());
                    }
                }
            }

            // Top by score (skip already selected)
            for m in ranked.iter() {
                if selected_matches.len() >= take {
                    break;
                }
                if selected_matches
                    .iter()
                    .any(|s| s.line_number == m.line_number)
                {
                    continue;
                }
                selected_matches.push((*m).clone());
            }

            // Always keep last if configured
            if self.always_keep_last && take >= 2 && total > take {
                if let Some(last) = fm.matches.last() {
                    if !selected_matches
                        .iter()
                        .any(|s| s.line_number == last.line_number)
                    {
                        if selected_matches.len() >= take {
                            // Replace the lowest-scored selected match
                            if let Some(min_idx) = selected_matches
                                .iter()
                                .enumerate()
                                .min_by(|(_, a), (_, b)| {
                                    a.score
                                        .partial_cmp(&b.score)
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                })
                                .map(|(i, _)| i)
                            {
                                selected_matches[min_idx] = last.clone();
                            }
                        } else {
                            selected_matches.push(last.clone());
                        }
                    }
                }
            }

            // Sort back by line number
            selected_matches.sort_by_key(|m| m.line_number);

            global_count += selected_matches.len();
            result.push(FileMatches {
                file: fm.file.clone(),
                matches: selected_matches,
            });
        }

        result
    }

    // ── Stage 4: Format ───────────────────────────────────────

    fn format_output(
        &self,
        selected: &[FileMatches],
        original: &[FileMatches],
    ) -> (String, std::collections::BTreeMap<String, String>) {
        let mut output = String::new();
        let mut summaries: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();

        // Build a lookup from file name to original match count
        let orig_counts: std::collections::BTreeMap<&str, usize> = original
            .iter()
            .map(|fm| (fm.file.as_str(), fm.matches.len()))
            .collect();

        for fm in selected {
            let orig_count = orig_counts
                .get(fm.file.as_str())
                .copied()
                .unwrap_or(fm.matches.len());
            let omitted = orig_count.saturating_sub(fm.matches.len());

            if omitted > 0 {
                summaries.insert(
                    fm.file.clone(),
                    format!("{}/{} matches shown", fm.matches.len(), orig_count),
                );
            }

            for m in &fm.matches {
                output.push_str(&format!("{}:{}:{}\n", fm.file, m.line_number, m.content));
            }

            if omitted > 0 {
                output.push_str(&format!("{}: … and {} more matches …\n", fm.file, omitted));
            }
        }

        (output, summaries)
    }
}

// ── Match line parser ────────────────────────────────────────────

/// Parse a single grep/ripgrep output line.
///
/// Supports:
/// - `file:line:text` (standard grep -n)
/// - `file:line:col:text` (ripgrep --column)
/// - `file-line-text` (alternative separator)
/// - Windows paths with drive letters
///
/// Returns `(file, line_number, content)` or `None` if the line
/// doesn't look like a search result.
fn parse_match_line(line: &str) -> Option<(&str, u64, &str)> {
    // Strategy: find the rightmost `:digits:` or `-digits-` separator
    // that splits the file path from the content.

    let bytes = line.as_bytes();
    let len = bytes.len();

    // Find the FIRST valid `:digits:` or `-digits-` separator
    let mut best_sep: Option<(usize, usize, u8)> = None; // (sep_start, sep_end, sep_byte)

    let mut i = 0;
    while i < len {
        // Look for ':' or '-' that might start a line-number segment
        if bytes[i] == b':' || bytes[i] == b'-' {
            let sep_byte = bytes[i];
            let sep_start = i;
            i += 1;

            // Try to parse digits
            let num_start = i;
            while i < len && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let num_end = i;

            if num_end > num_start {
                // Check what comes after the number — need the same separator
                if i < len && bytes[i] == sep_byte {
                    // It's `sep<digits>sep` — keep the FIRST match only
                    if best_sep.is_none() {
                        best_sep = Some((sep_start, num_end + 1, sep_byte));
                    }
                } else if i < len && bytes[i] == b':' && sep_byte == b':' {
                    // `:<digits>:` mixed separator — keep first
                    if best_sep.is_none() {
                        best_sep = Some((sep_start, num_end + 1, sep_byte));
                    }
                }
            }
            continue;
        }
        i += 1;
    }

    let (sep_start, sep_end, _sep_byte) = best_sep?;

    let file = &line[..sep_start];
    let file = file.trim();
    if file.is_empty() || file.len() > 500 {
        return None;
    }

    let num_str = &line[sep_start + 1..sep_end - 1];
    let line_num: u64 = num_str.parse().ok()?;

    // Content starts after the second separator
    let content_start = (sep_end).min(len);
    let content = line[content_start..].trim();
    if content.is_empty() {
        return None;
    }

    Some((file, line_num, content))
}

/// Score a matched line for priority. Uses unified keyword detector
/// plus structural boosts for search context.
fn score_line(content: &str, _line_number: u64, total_in_file: usize) -> f32 {
    let base = signals::score_line(content, ImportanceContext::Search);

    // Structural boost: definitions matter more than references
    let mut boost = 0.0;
    let lower = content.to_lowercase();
    if lower.contains("fn ") || lower.contains("def ") || lower.contains("class ") {
        boost += 0.15;
    }
    if lower.contains("impl ") || lower.contains("struct ") || lower.contains("trait ") {
        boost += 0.15;
    }

    // Slight penalty for very dense files
    if total_in_file > 20 {
        boost -= 0.05;
    }

    (base + boost).clamp(0.0, 1.0)
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_standard_grep() {
        let result = parse_match_line("src/main.rs:42:    let x = 1;");
        let (file, line_num, content) = result.unwrap();
        assert_eq!(file, "src/main.rs");
        assert_eq!(line_num, 42);
        assert!(content.contains("let x = 1"));
    }

    #[test]
    fn test_parse_ripgrep_with_column() {
        let result = parse_match_line("src/lib.rs:10:5:fn main() {");
        let (file, line_num, content) = result.unwrap();
        assert_eq!(file, "src/lib.rs");
        assert_eq!(line_num, 10);
        // After the first `:digits:`, everything else is content (includes col number)
        assert!(content.contains("fn main"), "got: {content}");
    }

    #[test]
    fn test_parse_dash_separator() {
        let result = parse_match_line("src/mod-123-Hello world");
        assert!(result.is_some());
    }

    #[test]
    fn test_parse_windows_path() {
        let result = parse_match_line(r"C:\Users\foo\src\main.rs:15:some content");
        let (file, line_num, content) = result.unwrap();
        assert_eq!(file, r"C:\Users\foo\src\main.rs");
        assert_eq!(line_num, 15);
        assert_eq!(content, "some content");
    }

    #[test]
    fn test_parse_non_match() {
        assert!(parse_match_line("just a regular sentence").is_none());
        assert!(parse_match_line("").is_none());
        assert!(parse_match_line("no colon here").is_none());
    }

    #[test]
    fn test_score_error_line() {
        let score = score_line("error: connection refused on port 8080", 10, 5);
        assert!(score > 0.4, "error lines should score high, got {score}");
    }

    #[test]
    fn test_score_warning_line() {
        let score = score_line("warning: deprecated API usage detected", 20, 10);
        assert!(score > 0.2, "warnings should score moderately, got {score}");
    }

    #[test]
    fn test_score_normal_line() {
        let score = score_line("some regular content here", 30, 100);
        assert!(score < 0.3, "normal lines should score low, got {score}");
    }

    #[test]
    fn test_compress_small_content_unchanged() {
        let c = SearchCompressor::new();
        let store = CcrStore::new(10);
        let result = c.compress("short text", &store);
        assert!(matches!(result, CompressionResult::Unchanged));
    }

    #[test]
    fn test_compress_search_output() {
        let c = SearchCompressor::new();

        // Build realistic grep-like output
        let mut content = String::new();
        for file_idx in 0..30 {
            for line_idx in 0..50 {
                let file = format!("src/module_{file_idx}.rs");
                if line_idx % 10 == 0 {
                    content.push_str(&format!(
                        "{file}:{line_idx}:ERROR: critical issue at module_{file_idx}\n"
                    ));
                } else {
                    content.push_str(&format!(
                        "{file}:{line_idx}:normal operation line {line_idx}\n"
                    ));
                }
            }
        }

        let store = CcrStore::new(100);
        match c.compress(&content, &store) {
            CompressionResult::Compressed {
                compressed_bytes,
                original_bytes,
                replacement,
                ..
            } => {
                assert!(
                    compressed_bytes < original_bytes,
                    "compressed ({compressed_bytes}) < original ({original_bytes})"
                );
                assert!(replacement.contains("<<ccr:"), "should contain CCR marker");
                assert!(
                    replacement.contains("Search:"),
                    "should contain search header"
                );
                // Error lines should be preserved
                assert!(replacement.contains("ERROR"), "should preserve error lines");
            }
            other => panic!("expected Compressed, got {other:?}"),
        }
    }

    #[test]
    fn test_compress_caps_files() {
        let c = SearchCompressor::new();

        let mut content = String::new();
        // 50 files, each with 20 matches — should be capped
        for file_idx in 0..50 {
            for line_idx in 0..20 {
                content.push_str(&format!(
                    "file_{file_idx}.rs:{line_idx}: match at {file_idx}:{line_idx}\n"
                ));
            }
        }

        let store = CcrStore::new(200);
        match c.compress(&content, &store) {
            CompressionResult::Compressed { replacement, .. } => {
                // Count actual match lines (those with 3 colon-separated segments,
                // excluding omission markers and header lines)
                let match_lines = replacement
                    .lines()
                    .filter(|l| {
                        l.trim().contains(".rs:")
                            && !l.contains("more matches")
                            && !l.contains("Search:")
                            && !l.contains("<<ccr:")
                    })
                    .count();
                // With max_total_matches=30, we should have ~30 actual matches
                // (allow a small margin for always_keep_first/last overfill)
                assert!(match_lines <= 35, "should cap matches, got {match_lines}");
            }
            _ => {}
        }
    }

    #[test]
    fn test_ccr_round_trip() {
        let c = SearchCompressor::new();
        let store = CcrStore::new(100);

        let mut content = String::new();
        for file_idx in 0..10 {
            for line_idx in 0..30 {
                content.push_str(&format!(
                    "file_{file_idx}.txt:{line_idx}: result {file_idx}-{line_idx}\n"
                ));
            }
        }

        match c.compress(&content, &store) {
            CompressionResult::Compressed { ccr_hash, .. } => {
                let retrieved = store.get(&ccr_hash).unwrap();
                assert_eq!(retrieved, content);
            }
            other => panic!("expected Compressed, got {other:?}"),
        }
    }
}
