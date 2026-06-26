//! Unified-diff compressor.
//!
//! Compresses git diff output by:
//! 1. Parsing into files + hunks
//! 2. Capping file count (keep heaviest by change count)
//! 3. Capping hunks per file (keep first + last + highest-change middle hunks)
//! 4. Trimming context lines around +/- changes
//! 5. Storing original in CCR if savings > 20%

use crate::compress::ccr;
use crate::compress::CompressionResult;

pub struct DiffCompressor {
    max_files: usize,
    max_hunks_per_file: usize,
    max_context: usize,
    min_lines_for_ccr: usize,
}

impl DiffCompressor {
    pub fn new() -> Self {
        DiffCompressor {
            max_files: 5,
            max_hunks_per_file: 3,
            max_context: 2,
            min_lines_for_ccr: 30,
        }
    }

    pub fn compress(&self, content: &str, ccr_store: &ccr::CcrStore) -> CompressionResult {
        if content.lines().count() < self.min_lines_for_ccr {
            return CompressionResult::Unchanged;
        }

        let files = parse_diff(content);
        if files.is_empty() {
            return CompressionResult::Skipped;
        }

        let original_bytes = content.len();

        // Cap files: keep heaviest by total changes
        let mut ranked: Vec<(usize, &DiffFile)> = files.iter().enumerate().collect();
        ranked.sort_by_key(|(_, f)| -(f.total_changes() as i64));
        let selected: Vec<usize> = ranked
            .iter()
            .take(self.max_files)
            .map(|(i, _)| *i)
            .collect();

        let mut compressed = String::new();
        let total_files = files.len();

        for (idx, file) in files.iter().enumerate() {
            if !selected.contains(&idx) {
                if idx == selected.iter().max().unwrap_or(&0) + 1 && selected.len() < files.len() {
                    compressed.push_str(&format!(
                        "… {} more files omitted …\n",
                        files.len() - selected.len()
                    ));
                }
                continue;
            }

            compressed.push_str(&format!(
                "diff --git {}\n--- {}\n+++ {}\n",
                file.path, file.old_path, file.new_path
            ));

            // Cap hunks: first + last + top middle
            let hunk_count = file.hunks.len();
            let mut hunk_indices: Vec<usize> = Vec::new();

            if hunk_count <= self.max_hunks_per_file {
                hunk_indices = (0..hunk_count).collect();
            } else {
                hunk_indices.push(0); // first
                                      // Middle hunks by change count
                let mut middle: Vec<(usize, usize)> = (1..hunk_count - 1)
                    .map(|i| (i, file.hunks[i].change_lines()))
                    .collect();
                middle.sort_by_key(|(_, c)| -(*c as i64));
                let mid_keep = (self.max_hunks_per_file - 2).min(middle.len());
                for (i, _) in middle.iter().take(mid_keep) {
                    hunk_indices.push(*i);
                }
                hunk_indices.push(hunk_count - 1); // last
                hunk_indices.sort();
            }

            let mut last_idx = 0;
            for &hi in &hunk_indices {
                if hi > last_idx + 1 {
                    compressed.push_str(&format!("… {} hunks omitted …\n", hi - last_idx - 1));
                }
                let hunk = &file.hunks[hi];
                compressed.push_str(&format!(
                    "@@ -{},{} +{},{} @@ {}\n",
                    hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count, hunk.section,
                ));
                let trimmed = trim_context(&hunk.lines, self.max_context);
                compressed.push_str(&trimmed);
                last_idx = hi;
            }
        }

        let compressed_bytes = compressed.len();

        // Only return compressed if meaningful savings
        if compressed_bytes >= original_bytes * 8 / 10 {
            return CompressionResult::Unchanged;
        }

        let ccr_hash = ccr_store.store(content);

        let final_output = format!(
            "/* Diff: {total_files} files, {}→{} bytes. <<ccr:{}>> */\n{}",
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

// ── Diff parsing ─────────────────────────────────────────────────

struct DiffFile {
    path: String,
    old_path: String,
    new_path: String,
    hunks: Vec<Hunk>,
}

impl DiffFile {
    fn total_changes(&self) -> usize {
        self.hunks.iter().map(|h| h.change_lines()).sum()
    }
}

struct Hunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    section: String,
    lines: Vec<String>,
}

impl Hunk {
    fn change_lines(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.starts_with('+') || l.starts_with('-'))
            .count()
    }
}

fn parse_diff(content: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut current_file: Option<DiffFile> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in content.lines() {
        if line.starts_with("diff --git ") {
            if let Some(mut f) = current_file.take() {
                if let Some(h) = current_hunk.take() {
                    f.hunks.push(h);
                }
                files.push(f);
            }
            let path = line
                .strip_prefix("diff --git ")
                .unwrap_or("unknown")
                .trim()
                .to_string();
            current_file = Some(DiffFile {
                path,
                old_path: String::new(),
                new_path: String::new(),
                hunks: Vec::new(),
            });
        } else if line.starts_with("--- ") {
            if let Some(ref mut f) = current_file {
                f.old_path = line.strip_prefix("--- ").unwrap_or("").to_string();
            }
        } else if line.starts_with("+++ ") {
            if let Some(ref mut f) = current_file {
                f.new_path = line.strip_prefix("+++ ").unwrap_or("").to_string();
            }
        } else if line.starts_with("@@") {
            if let Some(ref mut f) = current_file {
                if let Some(h) = current_hunk.take() {
                    f.hunks.push(h);
                }
            }
            current_hunk = Some(parse_hunk_header(line));
        } else if let Some(ref mut h) = current_hunk {
            h.lines.push(line.to_string());
        }
    }

    if let Some(mut f) = current_file {
        if let Some(h) = current_hunk {
            f.hunks.push(h);
        }
        files.push(f);
    }

    files
}

fn parse_hunk_header(line: &str) -> Hunk {
    // @@ -old_start,old_count +new_start,new_count @@ section
    let mut old_start = 1;
    let mut old_count = 1;
    let mut new_start = 1;
    let mut new_count = 1;
    let mut section = String::new();

    if let Some(inner) = line.strip_prefix("@@").and_then(|s| s.split("@@").next()) {
        let parts: Vec<&str> = inner.trim().split_whitespace().collect();
        if parts.len() >= 2 {
            if let Some(olds) = parts[0].strip_prefix('-') {
                let op: Vec<&str> = olds.split(',').collect();
                old_start = op.first().and_then(|v| v.parse().ok()).unwrap_or(1);
                old_count = op.get(1).and_then(|v| v.parse().ok()).unwrap_or(1);
            }
            if let Some(news) = parts[1].strip_prefix('+') {
                let np: Vec<&str> = news.split(',').collect();
                new_start = np.first().and_then(|v| v.parse().ok()).unwrap_or(1);
                new_count = np.get(1).and_then(|v| v.parse().ok()).unwrap_or(1);
            }
        }
        if parts.len() > 2 {
            section = parts[2..].join(" ");
        }
    }

    Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        section,
        lines: Vec::new(),
    }
}

fn trim_context(lines: &[String], max_context: usize) -> String {
    let mut result = String::new();
    let mut context_buf: Vec<&str> = Vec::new();

    for line in lines {
        let is_change = line.starts_with('+') || line.starts_with('-');
        let is_context = line.starts_with(' ');

        if is_change {
            // Flush context buffer (trimmed)
            let keep = if context_buf.len() > max_context * 2 {
                let front: Vec<&str> = context_buf[..max_context].to_vec();
                let back: Vec<&str> = context_buf[context_buf.len() - max_context..].to_vec();
                result.push_str(&format!(
                    "… {} context lines …\n",
                    context_buf.len() - max_context * 2
                ));
                [front, back].concat()
            } else {
                context_buf.clone()
            };
            for l in &keep {
                result.push_str(l);
                result.push('\n');
            }
            context_buf.clear();
            result.push_str(line);
            result.push('\n');
        } else if is_context {
            context_buf.push(line);
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_diff() {
        let diff = "diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,3 @@
 context line
-old line
+new line
 context line
@@ -10,2 +10,2 @@ section name
 context
-old2
+new2";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[0].change_lines(), 2);
    }

    #[test]
    fn test_compress_small_diff_unchanged() {
        let c = DiffCompressor::new();
        let store = ccr::CcrStore::new(10);
        let result = c.compress("small diff\none line", &store);
        assert!(matches!(result, CompressionResult::Unchanged));
    }
}
