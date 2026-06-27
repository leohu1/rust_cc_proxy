//! Build/test log compressor.
//!
//! Detects log format (pytest, cargo, npm, jest, generic) and compresses by:
//! 1. Classifying each line by level (ERROR/FAIL/WARN/INFO/DEBUG)
//! 2. Detecting stack traces
//! 3. Scoring lines: errors (100), warnings (30), stack traces (50), summaries (20)
//! 4. Keeping top-scored lines with context windows

use crate::compress::ccr;
use crate::compress::signals::{self, ImportanceContext};
use crate::compress::CompressionResult;

pub struct LogCompressor {
    max_lines: usize,
    min_lines_for_ccr: usize,
}

impl Default for LogCompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl LogCompressor {
    pub fn new() -> Self {
        LogCompressor {
            max_lines: 100,
            min_lines_for_ccr: 50,
        }
    }

    pub fn compress(&self, content: &str, ccr_store: &ccr::CcrStore) -> CompressionResult {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() < self.min_lines_for_ccr {
            return CompressionResult::Unchanged;
        }

        let format = detect_format(content);
        let original_bytes = content.len();

        // Score each line
        let scored: Vec<(usize, i32, &str)> = lines
            .iter()
            .enumerate()
            .map(|(i, line)| (i, score_line(line, &format), *line))
            .collect();

        // Select top lines
        let mut selected: Vec<usize> = Vec::new();
        let mut ranked: Vec<(usize, i32)> = scored.iter().map(|(i, s, _)| (*i, *s)).collect();
        ranked.sort_by_key(|(_, s)| -*s);

        let keep = self.max_lines.min(lines.len());
        for (idx, score) in ranked.iter().take(keep) {
            if *score > 0 {
                selected.push(*idx);
                // Add context window (±2 lines)
                let i = *idx;
                for ctx in 1..=2 {
                    if i >= ctx && !selected.contains(&(i - ctx)) {
                        selected.push(i - ctx);
                    }
                    if i + ctx < lines.len() && !selected.contains(&(i + ctx)) {
                        selected.push(i + ctx);
                    }
                }
            }
        }

        selected.sort();
        selected.dedup();

        if selected.len() >= lines.len() * 8 / 10 {
            return CompressionResult::Unchanged;
        }

        // Build compressed output
        let mut compressed = String::new();
        compressed.push_str(&format!(
            "/* {} log: {}/{} lines kept */\n",
            format.name(),
            selected.len(),
            lines.len()
        ));

        let mut last_idx = 0;
        for &idx in &selected {
            if idx > last_idx + 1 {
                compressed.push_str(&format!("… {} lines omitted …\n", idx - last_idx - 1));
            }
            compressed.push_str(lines[idx]);
            compressed.push('\n');
            last_idx = idx;
        }
        if last_idx < lines.len() - 1 {
            compressed.push_str(&format!(
                "… {} lines omitted …\n",
                lines.len() - last_idx - 1
            ));
        }

        let compressed_bytes = compressed.len();
        let ccr_hash = ccr_store.store(content);

        let final_output = format!(
            "/* Log: {}→{} bytes. <<ccr:{}>> */\n{}",
            original_bytes, compressed_bytes, ccr_hash, compressed
        );

        CompressionResult::Compressed {
            compressed_bytes: final_output.len(),
            replacement: final_output,
            ccr_hash,
            original_bytes,
        }
    }
}

#[derive(Debug, Clone)]
enum LogFormat {
    Pytest,
    Cargo,
    Npm,
    Jest,
    Make,
    Generic,
}

impl LogFormat {
    fn name(&self) -> &str {
        match self {
            LogFormat::Pytest => "pytest",
            LogFormat::Cargo => "cargo",
            LogFormat::Npm => "npm",
            LogFormat::Jest => "jest",
            LogFormat::Make => "make",
            LogFormat::Generic => "log",
        }
    }
}

fn detect_format(content: &str) -> LogFormat {
    let head = &content[..content.len().min(2000)];
    if head.contains("============================= test session starts") {
        LogFormat::Pytest
    } else if head.contains("Compiling") || head.contains("error[") || head.contains("warning[") {
        LogFormat::Cargo
    } else if head.contains("npm ERR!") || head.contains("npm WARN") {
        LogFormat::Npm
    } else if head.contains("FAIL ") || head.contains("PASS ") {
        LogFormat::Jest
    } else if head.contains("make[") || head.contains("Makefile:") {
        LogFormat::Make
    } else {
        LogFormat::Generic
    }
}

fn score_line(line: &str, _format: &LogFormat) -> i32 {
    let lower = line.to_lowercase();

    // Base score from unified keyword detector
    let signal = signals::score_line(line, ImportanceContext::Log);
    let base: i32 = (signal * 100.0) as i32;

    // Format-specific structural boosts
    let mut bonus = 0i32;

    // Stack traces — file references
    if line.starts_with("  at ")
        || line.starts_with("    at ")
        || line.contains(".rs:")
        || line.contains(".py:")
        || line.contains(".ts:")
        || line.contains(".js:")
    {
        bonus += 50;
    }

    // Test/build result summaries
    if lower.starts_with("test result:")
        || lower.starts_with("test summary:")
        || lower.starts_with("build finished")
    {
        bonus += 20;
    }

    // Info lines get a small bump
    if lower.starts_with("info") || lower.starts_with("compiling") {
        bonus += 5;
    }

    (base + bonus).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_pytest() {
        let log =
            "============================= test session starts =============================\n...";
        assert!(matches!(detect_format(log), LogFormat::Pytest));
    }

    #[test]
    fn test_detect_cargo() {
        let log = "   Compiling crate v0.1.0\nwarning: unused variable\n";
        assert!(matches!(detect_format(log), LogFormat::Cargo));
    }

    #[test]
    fn test_score_error_line() {
        // error keywords → signals priority 0.9 → base 90
        assert!(
            score_line("error: connection refused", &LogFormat::Generic) >= 80,
            "error lines should score high"
        );
        assert!(
            score_line("thread panicked at src/main.rs:42", &LogFormat::Generic) >= 80,
            "panic lines should score high"
        );
    }

    #[test]
    fn test_score_warning_line() {
        // warning keywords → signals priority 0.6 → base 60
        let score = score_line("warning: unused import", &LogFormat::Generic);
        assert!(score >= 50, "warning lines should score moderately, got {score}");
    }

    #[test]
    fn test_compress_small_log_unchanged() {
        let c = LogCompressor::new();
        let store = ccr::CcrStore::new(10);
        let result = c.compress("short log\njust a few lines\nthat's it", &store);
        assert!(matches!(result, CompressionResult::Unchanged));
    }
}
