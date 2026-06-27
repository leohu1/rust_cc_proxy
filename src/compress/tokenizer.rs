//! Token counting with tiktoken-rs for accurate counts.
//!
//! Uses OpenAI's `o200k_base` BPE tokenizer (the most recent and broadly
//! applicable model). Falls back to character-based estimation if the
//! tokenizer data files can't be loaded (e.g., in restricted environments
//! or first-run download failures).

use std::sync::OnceLock;

static TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();

/// Which counting backend is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// `tiktoken-rs` o200k_base BPE tokenizer.
    Tiktoken,
    /// Simple character-based heuristic (~4 chars/token ASCII, ~2 CJK).
    Estimating,
}

/// Thread-safe token counter.
///
/// Created once at startup via `Tokenizer::get()`. On first access, tries
/// to load the tiktoken model; falls back silently to estimating.
pub struct Tokenizer {
    backend: Backend,
    bpe: Option<tiktoken_rs::CoreBPE>,
}

impl std::fmt::Debug for Tokenizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokenizer")
            .field("backend", &self.backend)
            .field(
                "bpe",
                &if self.bpe.is_some() {
                    "Some(CoreBPE)"
                } else {
                    "None"
                },
            )
            .finish()
    }
}

impl Tokenizer {
    /// Get the global tokenizer instance, initializing it on first call.
    pub fn get() -> &'static Tokenizer {
        TOKENIZER.get_or_init(|| match tiktoken_rs::o200k_base() {
            Ok(bpe) => {
                tracing::info!("Tokenizer: tiktoken-rs o200k_base loaded successfully");
                Tokenizer {
                    backend: Backend::Tiktoken,
                    bpe: Some(bpe),
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Tokenizer: tiktoken-rs failed to load o200k_base ({e}), \
                         falling back to character-based estimation"
                );
                Tokenizer {
                    backend: Backend::Estimating,
                    bpe: None,
                }
            }
        })
    }

    /// Which backend is active.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Count tokens in the given text.
    ///
    /// Returns the token count. For empty text, returns 0.
    /// Uses tiktoken if available, otherwise falls back to character estimation.
    pub fn count(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        match &self.bpe {
            Some(bpe) => bpe.encode_ordinary(text).len(),
            None => estimate_tokens_fallback(text),
        }
    }
}

/// Character-based token estimation fallback.
///
/// ASCII / Latin: ~4 chars per token (code, JSON, English prose).
/// CJK / wide chars: ~2 chars per token (Chinese, Japanese, Korean).
fn estimate_tokens_fallback(text: &str) -> usize {
    let ascii_count = text.chars().filter(|c| c.is_ascii()).count();
    let wide_count = text.chars().count() - ascii_count;
    (ascii_count / 4) + (wide_count / 2)
}

/// Convenience: count tokens in text using the global tokenizer.
pub fn count_tokens(text: &str) -> usize {
    Tokenizer::get().count(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenizer_init() {
        let t = Tokenizer::get();
        // Should always succeed (either tiktoken or fallback)
        let count = t.count("hello world");
        assert!(count > 0);
        assert!(count < 20);
    }

    #[test]
    fn test_empty_text() {
        assert_eq!(Tokenizer::get().count(""), 0);
    }

    #[test]
    fn test_count_english() {
        let t = Tokenizer::get();
        let text = "This is a simple English sentence for token counting.";
        let count = t.count(text);
        // Roughly 10-15 tokens for this sentence
        assert!(count >= 8, "expected >=8 tokens, got {count}");
        assert!(count <= 30, "expected <=30 tokens, got {count}");
    }

    #[test]
    fn test_count_json() {
        let t = Tokenizer::get();
        let json = r#"{"key":"value","items":[1,2,3,4,5]}"#;
        let count = t.count(json);
        assert!(count > 0);
        // Should be fewer than chars/4 (which would be very rough)
        assert!(count < json.len());
    }

    #[test]
    fn test_fallback_estimate() {
        let ascii = "hello world this is a test with many english words";
        let count = estimate_tokens_fallback(ascii);
        assert!(count > 0);
        assert!(count < ascii.len());

        // Chinese text should use different ratio
        let cjk = "这是一个中文测试句子";
        let count_cjk = estimate_tokens_fallback(cjk);
        assert!(count_cjk > 0);
    }

    #[test]
    fn test_consistent_counts() {
        let t = Tokenizer::get();
        let text = "Hello world. This is a test.";
        let c1 = t.count(text);
        let c2 = t.count(text);
        assert_eq!(c1, c2, "token counts must be deterministic");
    }
}
