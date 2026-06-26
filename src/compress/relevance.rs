//! BM25 keyword relevance scorer.
//!
//! Scores items against a query context using BM25 with standard parameters
//! (k1=1.5, b=0.75). Used by JSON array compression to decide which items
//! are most relevant to keep.

use std::collections::{HashMap, HashSet};

pub struct BM25Scorer {
    k1: f64,
    b: f64,
}

impl Default for BM25Scorer {
    fn default() -> Self {
        BM25Scorer { k1: 1.5, b: 0.75 }
    }
}

impl BM25Scorer {
    /// Score each item against the query context. Returns normalized scores [0, 1].
    pub fn score_items(&self, items: &[&str], query: &str) -> Vec<f64> {
        if items.is_empty() || query.is_empty() {
            return vec![0.0; items.len()];
        }

        let query_tokens: Vec<String> = tokenize(query);
        let item_tokens: Vec<Vec<String>> = items.iter().map(|s| tokenize(s)).collect();

        // Compute average document length
        let avgdl: f64 = item_tokens.iter().map(|t| t.len() as f64).sum::<f64>()
            / (item_tokens.len() as f64).max(1.0);

        // Compute IDF for query terms
        let n = item_tokens.len() as f64;
        let mut idf: HashMap<&str, f64> = HashMap::new();
        for qt in &query_tokens {
            let df = item_tokens.iter().filter(|t| t.contains(qt)).count() as f64;
            idf.insert(qt.as_str(), ((n - df + 0.5) / (df + 0.5) + 1.0).ln());
        }

        // Score each item
        let mut scores: Vec<f64> = item_tokens
            .iter()
            .map(|tokens| {
                let dl = tokens.len() as f64;
                let mut score = 0.0;
                let mut term_freqs: HashMap<&str, usize> = HashMap::new();
                for t in tokens {
                    *term_freqs.entry(t.as_str()).or_default() += 1;
                }
                for (term, &tf) in &term_freqs {
                    let term_str: &str = term;
                    if let Some(&idf_val) = idf.get(term_str) {
                        let tf = tf as f64;
                        score += idf_val * (tf * (self.k1 + 1.0))
                            / (tf + self.k1 * (1.0 - self.b + self.b * dl / avgdl.max(1.0)));
                    }
                }
                // Long-token bonus: +0.3 for items with UUIDs or long IDs
                let has_long = tokens.iter().any(|t| t.len() >= 8);
                if has_long {
                    score += 0.3;
                }
                score
            })
            .collect();

        // Normalize to [0, 1]
        let max = scores.iter().cloned().fold(0.0, f64::max);
        if max > 0.0 {
            for s in &mut scores {
                *s = (*s / max).min(1.0);
            }
        }

        scores
    }
}

/// Tokenize: lowercase, split on whitespace + punctuation, filter short tokens.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == ',' || c == ':' || c == ';') {
        let trimmed = word.trim_matches(|c: char| {
            c == '"'
                || c == '\''
                || c == '{'
                || c == '}'
                || c == '['
                || c == ']'
                || c == '.'
                || c == '!'
                || c == '?'
        });
        if trimmed.len() >= 2 {
            tokens.push(trimmed.to_lowercase());
        }
    }
    tokens
}

/// Keep top-N items by BM25 score, preserving original order.
pub fn select_top_by_relevance(
    items: &[serde_json::Value],
    scores: &[f64],
    n: usize,
    always_keep_first: usize,
    always_keep_last: usize,
) -> Vec<(usize, serde_json::Value)> {
    let total = items.len();
    let n = n.min(total);

    // Build scored indices for middle items
    let middle_start = always_keep_first;
    let middle_end = total.saturating_sub(always_keep_last);
    let mut middle: Vec<(usize, f64)> =
        (middle_start..middle_end).map(|i| (i, scores[i])).collect();
    middle.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Select: first N always kept, top N from middle by score, last N always kept
    let mut selected_indices: HashSet<usize> = HashSet::new();
    for i in 0..always_keep_first.min(total) {
        selected_indices.insert(i);
    }
    for i in total.saturating_sub(always_keep_last)..total {
        selected_indices.insert(i);
    }
    let middle_n = n.saturating_sub(selected_indices.len());
    for (idx, _) in middle.iter().take(middle_n) {
        selected_indices.insert(*idx);
    }

    let mut result: Vec<(usize, serde_json::Value)> = Vec::new();
    for i in 0..total {
        if selected_indices.contains(&i) {
            result.push((i, items[i].clone()));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_basic() {
        let tokens = tokenize("Hello World! This is a test.");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"this".to_string()));
        assert!(tokens.contains(&"test".to_string()));
    }

    #[test]
    fn test_bm25_scores_items() {
        let scorer = BM25Scorer::default();
        let items = [
            "error: connection refused on port 8080",
            "info: server started successfully",
            "error: database timeout after 30s",
            "debug: request processed in 5ms",
            "warn: memory usage at 85%",
        ];
        let scores = scorer.score_items(&items, "error connection database");
        // Items containing "error" should score higher
        assert!(scores[0] > 0.1, "item[0] should have score for 'error'");
        assert!(scores[2] > 0.1, "item[2] should have score for 'error'");
    }
}
