//! Pre-compression reformat and offload utilities.
//!
//! These run BEFORE the main lossy compressor, performing cheap,
//! lossless transformations that pack data denser without dropping
//! any information (reformat) or dropping obviously useless content
//! (offload). They make the downstream lossy compressor's job easier.

use serde_json::Value;

// ── JsonMinifier ───────────────────────────────────────────────────

/// Lossless JSON minification. Strips whitespace by re-serializing
/// in compact form. Returns the minified string on success.
///
/// Cost: O(n) parse + O(n) serialize. Cheap enough to run always.
pub fn json_minify(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if !(trimmed.starts_with('[') || trimmed.starts_with('{')) {
        return None;
    }
    let parsed: Value = serde_json::from_str(trimmed).ok()?;
    let compact = serde_json::to_string(&parsed).ok()?;
    if compact.len() < content.len() {
        Some(compact)
    } else {
        None
    }
}

// ── DiffNoise ──────────────────────────────────────────────────────

/// Drop low-value noise from unified diffs.
///
/// Removes:
/// 1. Lock-file hunks (`Cargo.lock`, `package-lock.json`, `yarn.lock`, etc.)
/// 2. Whitespace-only hunks (no semantic change)
///
/// Returns the cleaned diff, or `None` if nothing was removed.
pub fn diff_noise_strip(content: &str) -> Option<String> {
    let mut result = String::with_capacity(content.len());
    let mut current_file_is_noise = false;
    let mut current_hunk_lines: Vec<&str> = Vec::new();
    let mut in_hunk = false;
    let mut hunk_has_semantic_change = false;
    let mut removed_anything = false;

    for line in content.lines() {
        // File header detection
        if line.starts_with("diff --git ") {
            // Flush previous hunk
            if in_hunk && !hunk_has_semantic_change && !current_hunk_lines.is_empty() {
                removed_anything = true;
                // Skip noise hunk — don't write it
            } else if in_hunk {
                for hl in &current_hunk_lines {
                    result.push_str(hl);
                    result.push('\n');
                }
            }
            current_hunk_lines.clear();
            in_hunk = false;
            hunk_has_semantic_change = false;

            // Check if this is a lockfile or other noise file
            current_file_is_noise = is_lockfile_line(line);
            if !current_file_is_noise {
                result.push_str(line);
                result.push('\n');
            } else {
                removed_anything = true;
            }
        } else if current_file_is_noise && (line.starts_with("--- ") || line.starts_with("+++ ")) {
            // Skip lockfile header lines
            removed_anything = true;
        } else if current_file_is_noise {
            // Skip entire lockfile diff
            removed_anything = true;
        } else if line.starts_with("@@") {
            // Flush previous hunk
            if in_hunk && !hunk_has_semantic_change && !current_hunk_lines.is_empty() {
                removed_anything = true;
                // Skip noise hunk
            } else if in_hunk {
                for hl in &current_hunk_lines {
                    result.push_str(hl);
                    result.push('\n');
                }
            }
            current_hunk_lines.clear();
            current_hunk_lines.push(line);
            in_hunk = true;
            hunk_has_semantic_change = false;
        } else if in_hunk {
            // Check for semantic change (not just whitespace)
            if line.starts_with('+') || line.starts_with('-') {
                let content = &line[1..];
                if !content.trim().is_empty() {
                    // This is a real change, not whitespace-only
                    hunk_has_semantic_change = true;
                }
            }
            current_hunk_lines.push(line);
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    // Flush last hunk
    if in_hunk && !hunk_has_semantic_change && !current_hunk_lines.is_empty() {
        removed_anything = true;
    } else if in_hunk {
        for hl in &current_hunk_lines {
            result.push_str(hl);
            result.push('\n');
        }
    }

    if removed_anything {
        Some(result)
    } else {
        None
    }
}

fn is_lockfile_line(line: &str) -> bool {
    let lockfiles = [
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "Gemfile.lock",
        "poetry.lock",
        "Pipfile.lock",
        "composer.lock",
        "go.sum",
        "packages.lock.json",
    ];
    lockfiles.iter().any(|lf| line.contains(lf))
}

// ── Bloat estimation ───────────────────────────────────────────────

/// Quick structural bloat check before running expensive compression.
///
/// Returns `true` if the content is bloated enough to be worth compressing.
/// False negatives are acceptable (we may skip compression on borderline
/// content); false positives are not (we should not waste CPU on content
/// that won't compress well).
pub fn is_bloated(content: &str, content_type_hint: &str) -> bool {
    let len = content.len();
    let lines = content.lines().count();

    match content_type_hint {
        "json_array" => {
            // Large JSON arrays benefit from compression
            len > 2000 && lines > 15
        }
        "diff" => {
            // Diffs with many hunks are worth compressing
            len > 3000 && lines > 40
        }
        "log" => {
            // Logs with many lines are worth compressing
            len > 2000 && lines > 30
        }
        "search" => {
            // Search results with many matches
            len > 1000 && lines > 10
        }
        "text" => {
            // Long prose
            len > 1500
        }
        _ => {
            // Unknown: be conservative, only compress if clearly large
            len > 5000
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_minify_removes_whitespace() {
        let pretty = r#"{
            "key": "value",
            "nested": {
                "a": 1
            }
        }"#;
        let compact = json_minify(pretty).unwrap();
        assert!(compact.len() < pretty.len());
        assert!(
            !compact.contains("  "),
            "compact JSON should have minimal whitespace"
        );
        // Should still be valid JSON
        assert!(serde_json::from_str::<Value>(&compact).is_ok());
    }

    #[test]
    fn test_json_minify_already_compact() {
        let compact = r#"{"key":"value"}"#;
        assert!(json_minify(compact).is_none());
    }

    #[test]
    fn test_diff_noise_strip_lockfile() {
        let diff = "diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n@@ -1,3 +1,3 @@\n-old\n+new\ndiff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -10,3 +10,3 @@\n-old code\n+new code\n";
        let cleaned = diff_noise_strip(diff).unwrap();
        assert!(
            !cleaned.contains("Cargo.lock"),
            "lockfile should be stripped"
        );
        assert!(cleaned.contains("src/main.rs"), "real files should be kept");
    }

    #[test]
    fn test_diff_noise_strip_whitespace_only() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -5,3 +5,3 @@\n-  \n+  \ndiff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -10,3 +10,3 @@\n-old code\n+new code\n";
        let cleaned = diff_noise_strip(diff).unwrap();
        // Whitespace-only hunk should be gone, but the real change should remain
        assert!(cleaned.contains("old code"));
    }

    #[test]
    fn test_bloat_check() {
        // Diff with many lines and enough content
        let mut diff = String::from("diff --git a/file b/file\n");
        for _ in 0..50 {
            diff.push_str("@@ -1,10 +1,10 @@ some context here to add length\n-old line that was removed\n+new line that was added\n  unchanged context\n");
        }
        assert!(is_bloated(&diff, "diff"));
        // Short content not bloated
        assert!(!is_bloated("short", "log"));
        // Large unknown content
        assert!(is_bloated(&"x".repeat(6000), "unknown"));
    }
}
