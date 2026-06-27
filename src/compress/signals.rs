//! Unified line-importance detection framework.
//!
//! Replaces the scattered, hardcoded keyword scoring in each compressor
//! (diff, log, text, search) with a single `LineImportanceDetector` trait
//! and a `KeywordDetector` backed by keyword registries.
//!
//! ## Architecture
//! LineImportanceDetector (trait)
//!   └── KeywordDetector (concrete)
//!         ├── Error keywords (all contexts)
//!         ├── Warning keywords (Text/Search/Log)
//!         ├── Importance keywords (all contexts)
//!         └── Security keywords (Diff only)
//!
//! Each compressor passes an `ImportanceContext` so the detector can
//! activate/deactivate keyword categories that are context-appropriate.

// ── Types ──────────────────────────────────────────────────────────

/// Where a line came from — determines which keyword categories fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportanceContext {
    Text,
    Search,
    Diff,
    Log,
}

/// Why a line earned its priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportanceCategory {
    Error,
    Warning,
    Importance,
    Security,
}

/// Output of a single detector for a single line.
#[derive(Debug, Clone)]
pub struct ImportanceSignal {
    pub category: Option<ImportanceCategory>,
    /// 0.0 = drop first, 1.0 = keep at all costs
    pub priority: f32,
    /// 0.0 = no information, 1.0 = detector is sure
    pub confidence: f32,
}

impl ImportanceSignal {
    pub const fn neutral() -> Self {
        ImportanceSignal {
            category: None,
            priority: 0.0,
            confidence: 0.0,
        }
    }

    pub const fn matched(category: ImportanceCategory, priority: f32, confidence: f32) -> Self {
        ImportanceSignal {
            category: Some(category),
            priority,
            confidence,
        }
    }

    pub fn is_match(&self) -> bool {
        self.category.is_some()
    }
}

// ── Trait ──────────────────────────────────────────────────────────

/// Classifies a single line for importance.
///
/// Implementations must be `Send + Sync` so they can be shared across
/// threads behind an `Arc`.
pub trait LineImportanceDetector: Send + Sync {
    /// Score a line within a given context.
    fn score(&self, line: &str, ctx: ImportanceContext) -> ImportanceSignal;
}

// ── KeywordDetector ────────────────────────────────────────────────

/// Keywords per category. All matching is case-insensitive and
/// word-boundary checked (except `contains_indicators` for fast triage).
pub struct KeywordRegistry {
    pub error: Vec<&'static str>,
    pub warning: Vec<&'static str>,
    pub importance: Vec<&'static str>,
    pub security: Vec<&'static str>,
}

impl KeywordRegistry {
    /// Default Headroom keyword set, curated for dev-tool output.
    pub fn default_set() -> Self {
        KeywordRegistry {
            error: vec![
                "error", "fail", "fatal", "panic", "panicked", "crash", "exception",
                "timeout", "refused", "denied", "abort", "killed",
                "segfault", "corrupt", "invalid", "missing", "unknown",
                "unsupported", "unreachable", "overflow", "deadlock",
            ],
            warning: vec![
                "warn", "warning", "deprecated", "obsolete", "todo",
                "fixme", "hack", "workaround", "unstable", "experimental",
                "slow", "timeout", "retry", "fallback",
            ],
            importance: vec![
                "critical", "security", "vuln", "cve", "exploit",
                "injection", "xss", "csrf", "auth", "password",
                "secret", "token", "key", "certificate",
                "fn", "def", "class", "impl", "struct", "trait",
                "pub", "mod", "use", "import", "export",
            ],
            security: vec![
                "password", "secret", "token", "api_key", "private_key",
                "credential", "auth_token", "bearer", "access_key",
                "sql_inject", "xss", "csrf", "ssrf", "rce", "lfi",
                "hardcoded", "plaintext", "cleartext",
            ],
        }
    }

    /// Return all keywords active for a given context.
    fn active_for(&self, ctx: ImportanceContext) -> Vec<(&'static str, ImportanceCategory)> {
        let mut result: Vec<(&'static str, ImportanceCategory)> = Vec::new();

        // Error — fires in all contexts
        for kw in &self.error {
            result.push((kw, ImportanceCategory::Error));
        }

        // Warning — fires in Text, Search, Log; NOT in Diff
        if ctx != ImportanceContext::Diff {
            for kw in &self.warning {
                result.push((kw, ImportanceCategory::Warning));
            }
        }

        // Importance — fires in all contexts
        for kw in &self.importance {
            result.push((kw, ImportanceCategory::Importance));
        }

        // Security — fires in Diff only
        if ctx == ImportanceContext::Diff {
            for kw in &self.security {
                result.push((kw, ImportanceCategory::Security));
            }
        }

        result
    }
}

/// Keyword-based `LineImportanceDetector`.
///
/// Uses substring matching (case-insensitive) with word-boundary
/// post-filter. Fast enough for typical tool output line counts
/// (hundreds to low thousands). For very large datasets, the
/// headroom-core crate uses aho-corasick.
pub struct KeywordDetector {
    registry: KeywordRegistry,
}

impl KeywordDetector {
    pub fn new() -> Self {
        KeywordDetector {
            registry: KeywordRegistry::default_set(),
        }
    }

    pub fn with_registry(registry: KeywordRegistry) -> Self {
        KeywordDetector { registry }
    }

    /// Fast triage: does the text contain any error indicator?
    /// Substring-only, no word-boundary check.
    pub fn contains_error_indicator(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        self.registry
            .error
            .iter()
            .any(|kw| lower.contains(*kw))
    }
}

impl Default for KeywordDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LineImportanceDetector for KeywordDetector {
    fn score(&self, line: &str, ctx: ImportanceContext) -> ImportanceSignal {
        let lower = line.to_lowercase();
        let keywords = self.registry.active_for(ctx);

        let mut best_priority: f32 = 0.0;
        let mut best_category: Option<ImportanceCategory> = None;

        for (kw, cat) in &keywords {
            if word_boundary_match(&lower, kw) {
                let priority = category_priority(*cat);
                if priority > best_priority {
                    best_priority = priority;
                    best_category = Some(*cat);
                }
            }
        }

        // Additional structural boosts
        let mut boost = 0.0;

        // ALLCAPS words signal importance
        let caps_count = line
            .split_whitespace()
            .filter(|w| w.len() > 1 && w.chars().all(|c| c.is_uppercase()))
            .count();
        boost += (caps_count as f32) * 0.03;

        // Digit density: lines with numbers often carry data
        let digit_count = line.chars().filter(|c| c.is_ascii_digit()).count();
        if digit_count > 0 {
            boost += (digit_count as f32 / line.len().max(1) as f32) * 0.1;
        }

        let priority = (best_priority + boost).min(1.0);

        if let Some(cat) = best_category {
            ImportanceSignal::matched(cat, priority, 0.7)
        } else if priority > 0.05 {
            ImportanceSignal {
                category: None,
                priority,
                confidence: 0.3,
            }
        } else {
            ImportanceSignal::neutral()
        }
    }
}

/// Per-category base priority.
fn category_priority(cat: ImportanceCategory) -> f32 {
    match cat {
        ImportanceCategory::Error => 0.9,
        ImportanceCategory::Security => 0.85,
        ImportanceCategory::Warning => 0.6,
        ImportanceCategory::Importance => 0.45,
    }
}

/// Case-insensitive word-boundary match.
///
/// A "word boundary" is: start-of-string, whitespace, or punctuation
/// before the keyword, and end-of-string, whitespace, or punctuation
/// after. This prevents "error" matching inside "terrorism".
fn word_boundary_match(text: &str, keyword: &str) -> bool {
    if let Some(pos) = text.find(keyword) {
        let before_ok = pos == 0 || {
            let ch = text.as_bytes()[pos - 1] as char;
            !ch.is_alphanumeric() && ch != '_'
        };
        let after_pos = pos + keyword.len();
        let after_ok = after_pos >= text.len() || {
            let ch = text.as_bytes()[after_pos] as char;
            !ch.is_alphanumeric() && ch != '_'
        };
        before_ok && after_ok
    } else {
        false
    }
}

// ── Convenience ────────────────────────────────────────────────────

/// Global keyword detector instance.
static DETECTOR: std::sync::LazyLock<KeywordDetector> =
    std::sync::LazyLock::new(KeywordDetector::new);

/// Quick line scoring — returns priority in [0.0, 1.0].
/// Convenience for compressors that just need a scalar score.
pub fn score_line(line: &str, ctx: ImportanceContext) -> f32 {
    DETECTOR.score(line, ctx).priority
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_detection() {
        let signal = KeywordDetector::new().score(
            "error: connection refused on port 8080",
            ImportanceContext::Log,
        );
        assert!(signal.is_match());
        assert_eq!(signal.category, Some(ImportanceCategory::Error));
        assert!(signal.priority > 0.8);
    }

    #[test]
    fn test_warning_not_in_diff() {
        // "warning" in Diff context shouldn't fire as Warning
        let text = "warning: deprecated API usage";
        let signal_log = KeywordDetector::new().score(text, ImportanceContext::Log);
        let signal_diff = KeywordDetector::new().score(text, ImportanceContext::Diff);

        // In Log context, "warning" fires
        assert!(signal_log.priority > 0.3);
        // In Diff context, "warning" is NOT active (but "deprecated" isn't a keyword)
        // The signal will be neutral or very low
        assert!(signal_diff.priority < 0.5);
    }

    #[test]
    fn test_security_in_diff() {
        let signal = KeywordDetector::new().score(
            "-password: admin123",
            ImportanceContext::Diff,
        );
        // "password" is in the security keyword list which fires in Diff
        assert!(signal.is_match());
    }

    #[test]
    fn test_word_boundary() {
        // "error" inside "terrorism" should NOT match
        assert!(!word_boundary_match("terrorism", "error"));
        // "error" as a standalone word SHOULD match
        assert!(word_boundary_match("error: something failed", "error"));
        assert!(word_boundary_match("an error occurred", "error"));
        assert!(word_boundary_match("  error  ", "error"));
    }

    #[test]
    fn test_neutral_signal() {
        let signal = KeywordDetector::new().score(
            "the quick brown fox jumps over the lazy dog",
            ImportanceContext::Text,
        );
        assert!(!signal.is_match());
        assert!(signal.priority < 0.1);
    }

    #[test]
    fn test_context_specificity() {
        let det = KeywordDetector::new();

        // Error fires everywhere
        let s1 = det.score("error: fail", ImportanceContext::Log);
        let s2 = det.score("error: fail", ImportanceContext::Diff);
        assert!(s1.priority > 0.8 && s2.priority > 0.8);

        // "fn " fires as Importance in all contexts
        let s3 = det.score("fn main() {", ImportanceContext::Diff);
        assert!(s3.priority > 0.3);
    }

    #[test]
    fn test_contains_error_indicator() {
        let det = KeywordDetector::new();
        assert!(det.contains_error_indicator("something error happened"));
        assert!(!det.contains_error_indicator("all systems nominal"));
    }
}
