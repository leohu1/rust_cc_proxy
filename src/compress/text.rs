//! Extractive prose / plain-text compressor.
//!
//! Splits text into sentences, scores each by relevance + salience,
//! keeps top sentences in original order. Used for long user messages
//! or concatenated text tool outputs.

use crate::compress::ccr;
use crate::compress::CompressionResult;

pub struct TextCompressor {
    min_chars: usize,
    target_ratio: f64,
}

impl Default for TextCompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl TextCompressor {
    pub fn new() -> Self {
        TextCompressor {
            min_chars: 800,
            target_ratio: 0.5,
        }
    }

    pub fn compress(&self, content: &str, ccr_store: &ccr::CcrStore) -> CompressionResult {
        if content.len() < self.min_chars {
            return CompressionResult::Unchanged;
        }

        let sentences = split_sentences(content);
        if sentences.len() <= 3 {
            return CompressionResult::Unchanged;
        }

        let original_bytes = content.len();

        // Score each sentence
        let scored: Vec<(usize, f64)> = sentences
            .iter()
            .enumerate()
            .map(|(i, s)| (i, score_sentence(s, i, sentences.len())))
            .collect();

        // Select top sentences
        let keep = (sentences.len() as f64 * self.target_ratio).ceil() as usize;
        let keep = keep.max(3).min(sentences.len());
        let mut ranked = scored.clone();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut selected: Vec<usize> = ranked.iter().take(keep).map(|(i, _)| *i).collect();
        selected.sort();

        // Build compressed text
        let compressed: String = selected
            .iter()
            .map(|&i| sentences[i].clone())
            .collect::<Vec<_>>()
            .join(" ");

        let compressed_bytes = compressed.len();
        if compressed_bytes >= original_bytes * 8 / 10 {
            return CompressionResult::Unchanged;
        }

        let ccr_hash = ccr_store.store(content);

        let final_output = format!(
            "/* Text: {}/{} sentences, {}→{} bytes. <<ccr:{}>> */\n{}",
            selected.len(),
            sentences.len(),
            original_bytes,
            compressed_bytes,
            ccr_hash,
            compressed
        );

        CompressionResult::Compressed {
            compressed_bytes: final_output.len(),
            replacement: final_output,
            ccr_hash,
            original_bytes,
        }
    }
}

/// Split text into sentences on `.`, `!`, `?`, `\n` boundaries.
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        current.push(ch);
        if ch == '.' || ch == '!' || ch == '?' || ch == '\n' {
            let trimmed = current.trim();
            if trimmed.len() > 1 {
                sentences.push(trimmed.to_string());
            }
            current = String::new();
        }
    }
    let trimmed = current.trim();
    if trimmed.len() > 1 {
        sentences.push(trimmed.to_string());
    }

    sentences
}

/// Score a sentence for importance.
fn score_sentence(sentence: &str, position: usize, total: usize) -> f64 {
    let mut score = 0.0;

    // Recency bonus: later sentences more important (common in tool outputs)
    score += (position as f64 / total as f64) * 0.3;

    // Salience: error keywords, digits, caps, identifiers
    let lower = sentence.to_lowercase();
    if lower.contains("error") || lower.contains("fail") || lower.contains("panic") {
        score += 0.5;
    }
    if lower.contains("warn") || lower.contains("warning") {
        score += 0.3;
    }
    if lower.contains("result") || lower.contains("total") || lower.contains("summary") {
        score += 0.3;
    }

    // Digit density: lines with numbers often carry data
    let digit_count = sentence.chars().filter(|c| c.is_ascii_digit()).count();
    score += (digit_count as f64 / sentence.len().max(1) as f64) * 0.2;

    // ALLCAPS words signal importance
    let caps_count = sentence
        .split_whitespace()
        .filter(|w| w.len() > 1 && w.chars().all(|c| c.is_uppercase()))
        .count();
    score += (caps_count as f64) * 0.1;

    score.min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_sentences() {
        let text = "First sentence. Second sentence! Third sentence?";
        let s = split_sentences(text);
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn test_compress_short_text_unchanged() {
        let c = TextCompressor::new();
        let store = ccr::CcrStore::new(10);
        let result = c.compress("short text", &store);
        assert!(matches!(result, CompressionResult::Unchanged));
    }

    #[test]
    fn test_score_error_sentence() {
        let score = score_sentence("ERROR: critical failure detected in module.", 0, 10);
        assert!(score > 0.4, "error sentence should score high, got {score}");
    }
}
