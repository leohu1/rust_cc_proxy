//! Adaptive sizer — Kneedle-based optimal keep count.
//!
//! Instead of hardcoding how many items/lines to keep (e.g. "max 10 items"),
//! we compute the information-saturation point: how many items before adding
//! more stops contributing new information.
//!
//! ## Algorithm
//!
//! 1. Compute cumulative unique bigram coverage over items in original order.
//! 2. Find the "knee" (elbow) of the curve — the point where adding items
//!    yields diminishing returns on unique bigrams.
//! 3. Apply a bias multiplier and clamp to [min_k, max_k].
//!
//! This is a simplified port from headroom-core's `adaptive_sizer.rs`.
//! Tier 3 (zlib validation) is omitted; Tier 2 (Kneedle on bigrams) is
//! the primary signal and works well in practice.

/// Compute the optimal number of items to keep from an ordered list.
///
/// - `items`: item strings in their original order.
/// - `bias`: multiplier on the knee point (1.0 = unbiased, >1.0 keeps more).
/// - `min_k`: floor — never keep fewer than this.
/// - `max_k`: ceiling — never keep more than this. If `None`, uses `items.len()`.
pub fn compute_optimal_k(items: &[&str], bias: f64, min_k: usize, max_k: Option<usize>) -> usize {
    let n = items.len();
    if n == 0 {
        return 0;
    }

    // Tier 1: Fast path for small arrays
    if n <= 8 {
        return n;
    }

    let max_k = max_k.unwrap_or(n).min(n);

    // Tier 2: Kneedle on unique-bigram coverage curve
    let curve = compute_unique_bigram_curve(items);

    if let Some(knee) = find_knee(&curve) {
        let k = ((knee as f64) * bias).ceil() as usize;
        return k.clamp(min_k, max_k);
    }

    // No clear saturation → keep a balanced fraction
    let fallback = (n as f64 * 0.25).ceil() as usize;
    fallback.clamp(min_k, max_k)
}

/// Build cumulative unique bigram count for items in order.
///
/// For each item, extracts whitespace-split word bigrams and tracks the
/// running total of unique bigrams seen so far. Returns a vector where
/// `curve[i]` = unique bigrams after processing items 0..=i.
pub fn compute_unique_bigram_curve(items: &[&str]) -> Vec<usize> {
    let mut seen = std::collections::HashSet::new();
    let mut curve = Vec::with_capacity(items.len());

    for item in items {
        let words: Vec<&str> = item.split_whitespace().collect();
        for pair in words.windows(2) {
            let bigram = (pair[0].to_lowercase(), pair[1].to_lowercase());
            seen.insert(bigram);
        }
        // Single-word items: treat as singleton bigram
        if words.len() == 1 {
            seen.insert((words[0].to_lowercase(), String::new()));
        }
        curve.push(seen.len());
    }

    curve
}

/// Find the knee (elbow) of a monotonically increasing curve.
///
/// Normalizes x (index) and y (value) to [0, 1], then finds the point
/// with maximum perpendicular distance from the diagonal line y=x.
/// Returns the index (0-based) of the knee point, or `None` if the
/// deviation is too small to call a knee.
pub fn find_knee(curve: &[usize]) -> Option<usize> {
    let n = curve.len();
    if n < 3 {
        return None;
    }

    let max_y = *curve.last().unwrap_or(&1) as f64;
    if max_y < 1.0 {
        return None;
    }

    // Flat curve: no growth → no knee
    let first_y = curve.first().copied().unwrap_or(0) as f64;
    if max_y - first_y < max_y * 0.1 {
        return None;
    }

    let mut best_idx = 0usize;
    let mut best_dev = 0.0f64;

    for (i, &y) in curve.iter().enumerate() {
        let x_norm = i as f64 / (n - 1) as f64;
        let y_norm = y as f64 / max_y;

        // Perpendicular distance from point (x_norm, y_norm) to line y=x
        let dev = (y_norm - x_norm).abs() / std::f64::consts::SQRT_2;

        if dev > best_dev {
            best_dev = dev;
            best_idx = i;
        }
    }

    // Require minimum deviation of 0.05 to call a knee
    if best_dev > 0.05 {
        Some(best_idx + 1) // +1 because the knee is "keep at least this many"
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        assert_eq!(compute_optimal_k(&[], 1.0, 3, None), 0);
    }

    #[test]
    fn test_small_array() {
        let strings: Vec<String> = (0..5).map(|i| format!("item {i}")).collect();
        let items: Vec<&str> = strings.iter().map(|s| s.as_str()).collect();
        let k = compute_optimal_k(&items, 1.0, 3, None);
        assert_eq!(k, 5); // n <= 8, keep all
    }

    #[test]
    fn test_redundant_items() {
        // Highly redundant data — should detect low knee
        let items: Vec<String> = (0..50)
            .map(|i| format!("redundant content repeated item {i}"))
            .collect();
        let item_refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
        let k = compute_optimal_k(&item_refs, 1.0, 5, Some(30));
        // Redundant data → knee should be low
        assert!(k < 30, "redundant data should have low knee, got {k}");
        assert!(k >= 5);
    }

    #[test]
    fn test_diverse_items() {
        // Highly diverse data — should detect high knee
        let mut items: Vec<String> = Vec::new();
        for i in 0..100 {
            items.push(format!(
                "unique term {i} diverse content {i} specific data {i}"
            ));
        }
        let item_refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
        let k = compute_optimal_k(&item_refs, 1.0, 5, Some(50));
        // Diverse data → knee should be higher
        assert!(k > 5, "diverse data should have higher knee, got {k}");
    }

    #[test]
    fn test_find_knee_flat() {
        // Flat curve → no knee
        let curve = vec![10, 10, 10, 10, 10];
        assert!(find_knee(&curve).is_none());
    }

    #[test]
    fn test_find_knee_obvious() {
        // Fast growth then flat → clear knee at index 2
        let curve = vec![10, 50, 95, 100, 100, 100];
        let knee = find_knee(&curve).unwrap();
        assert!(knee >= 2 && knee <= 4, "knee at {knee}");
    }
}
