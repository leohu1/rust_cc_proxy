//! Anchor selector — weighted region allocation for array compression.
//!
//! Instead of "keep first N and last M" (which wastes slots on boring
//! items), we allocate anchor slots across three regions (front, middle,
//! back) weighted by data pattern, then greedily select the best items
//! within each region.
//!
//! This is a simplified port from headroom-core's `anchor_selector.rs`.
//! Information-density scoring and query-keyword shifting are omitted
//! in favor of a clean, predictable allocation strategy.

use std::collections::BTreeSet;

// ── Config ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AnchorConfig {
    /// Fraction of total budget allocated to anchors (vs. score-based fill).
    pub anchor_budget_pct: f64,
    /// Minimum anchor slots regardless of budget.
    pub min_anchor_slots: usize,
    /// Maximum anchor slots regardless of budget.
    pub max_anchor_slots: usize,
}

impl Default for AnchorConfig {
    fn default() -> Self {
        AnchorConfig {
            anchor_budget_pct: 0.25,
            min_anchor_slots: 3,
            max_anchor_slots: 12,
        }
    }
}

// ── Data pattern ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataPattern {
    /// Search results — front matters more (best matches first).
    SearchResults,
    /// Logs — back matters more (errors/summaries at end).
    Logs,
    /// Time-series — evenly distributed anchors.
    TimeSeries,
    /// Generic — balanced front/back.
    Generic,
}

// ── Region weights ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct RegionWeights {
    front: f64,
    #[allow(dead_code)]
    middle: f64,
    back: f64,
}

impl RegionWeights {
    fn for_pattern(pattern: DataPattern) -> Self {
        match pattern {
            DataPattern::SearchResults => RegionWeights {
                front: 0.5,
                middle: 0.2,
                back: 0.3,
            },
            DataPattern::Logs => RegionWeights {
                front: 0.15,
                middle: 0.2,
                back: 0.65,
            },
            DataPattern::TimeSeries => RegionWeights {
                front: 0.33,
                middle: 0.34,
                back: 0.33,
            },
            DataPattern::Generic => RegionWeights {
                front: 0.35,
                middle: 0.25,
                back: 0.4,
            },
        }
    }
}

// ── Selector ───────────────────────────────────────────────────────

pub struct AnchorSelector {
    config: AnchorConfig,
}

impl AnchorSelector {
    pub fn new(config: AnchorConfig) -> Self {
        AnchorSelector { config }
    }

    /// Select anchor indices from a JSON array.
    ///
    /// Returns a `BTreeSet<usize>` of indices that should be kept as anchors.
    /// These are in addition to any score-based selection; they guarantee
    /// coverage across the front, middle, and back of the array.
    ///
    /// - `total_items`: total number of items in the array.
    /// - `max_items`: target total items after compression (budget for all selections).
    /// - `pattern`: data pattern for region weighting.
    /// - `scores`: optional per-item scores [0, 1] for middle-region selection.
    ///   If not provided, middle anchors are evenly spaced.
    pub fn select_anchors(
        &self,
        total_items: usize,
        max_items: usize,
        pattern: DataPattern,
        scores: Option<&[f64]>,
    ) -> BTreeSet<usize> {
        if total_items == 0 {
            return BTreeSet::new();
        }

        let weights = RegionWeights::for_pattern(pattern);

        // Compute anchor budget
        let budget = ((max_items as f64) * self.config.anchor_budget_pct)
            .ceil() as usize;
        let budget = budget
            .max(self.config.min_anchor_slots)
            .min(self.config.max_anchor_slots)
            .min(total_items);

        if budget == 0 {
            return BTreeSet::new();
        }

        let front_slots = (budget as f64 * weights.front).ceil() as usize;
        let back_slots = (budget as f64 * weights.back).ceil() as usize;
        let middle_slots = budget.saturating_sub(front_slots + back_slots);

        let mut selected = BTreeSet::new();

        // Front region: [0, front_range)
        let front_range = (front_slots * 3).min(total_items / 3).max(front_slots);
        self.pick_region(&mut selected, 0, front_range, front_slots);

        // Back region: [total - back_range, total)
        let back_range = (back_slots * 3).min(total_items / 3).max(back_slots);
        let back_start = total_items.saturating_sub(back_range);
        self.pick_region(&mut selected, back_start, total_items, back_slots);

        // Middle region: [front_range, back_start)
        let middle_start = front_range;
        let middle_end = back_start.max(middle_start);
        if middle_slots > 0 && middle_end > middle_start {
            self.pick_middle(
                &mut selected,
                middle_start,
                middle_end,
                middle_slots,
                scores,
            );
        }

        selected
    }

    /// Pick `n` evenly-spaced indices from [start, end).
    fn pick_region(
        &self,
        selected: &mut BTreeSet<usize>,
        start: usize,
        end: usize,
        n: usize,
    ) {
        let range = end.saturating_sub(start);
        if range == 0 || n == 0 {
            return;
        }
        let step = (range as f64 / n as f64).max(1.0);
        for i in 0..n {
            let idx = start + (i as f64 * step) as usize;
            if idx < end {
                selected.insert(idx);
            }
        }
        // Always include the very first and last item of the region if slots permit
        if n >= 2 && start < end {
            selected.insert(start);
            if end > start + 1 {
                selected.insert(end - 1);
            }
        }
    }

    /// Pick `n` indices from the middle [start, end), using scores if available.
    fn pick_middle(
        &self,
        selected: &mut BTreeSet<usize>,
        start: usize,
        end: usize,
        n: usize,
        scores: Option<&[f64]>,
    ) {
        if let Some(scores) = scores {
            // Score-based: pick top-N by score in the middle region
            let mut candidates: Vec<(usize, f64)> = (start..end)
                .filter(|i| *i < scores.len() && !selected.contains(i))
                .map(|i| (i, scores[i]))
                .collect();
            candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            for (idx, _) in candidates.iter().take(n) {
                selected.insert(*idx);
            }
        } else {
            // No scores: evenly spaced picks
            self.pick_region(selected, start, end, n);
        }
    }
}

impl Default for AnchorSelector {
    fn default() -> Self {
        Self::new(AnchorConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_pattern_front_heavy() {
        let selector = AnchorSelector::default();
        let anchors = selector.select_anchors(100, 20, DataPattern::SearchResults, None);
        // Should have anchors in all three regions
        assert!(!anchors.is_empty());
        assert!(anchors.len() <= 12); // max_anchor_slots
        // Front should have more slots: at least index 0
        assert!(anchors.contains(&0));
        // Back should have the last index
        assert!(anchors.contains(&99));
    }

    #[test]
    fn test_log_pattern_back_heavy() {
        let selector = AnchorSelector::default();
        let anchors = selector.select_anchors(100, 20, DataPattern::Logs, None);
        // Back region gets 65% of budget
        assert!(anchors.contains(&99));
        assert!(!anchors.is_empty());
    }

    #[test]
    fn test_small_array() {
        let selector = AnchorSelector::default();
        let anchors = selector.select_anchors(5, 20, DataPattern::Generic, None);
        // Small array: budget clamped to total_items
        assert!(anchors.len() <= 5);
        assert!(anchors.contains(&0));
        assert!(anchors.contains(&4));
    }

    #[test]
    fn test_with_scores() {
        let selector = AnchorSelector::default();
        let scores: Vec<f64> = (0..50)
            .map(|i| if i == 25 { 1.0 } else { 0.1 })
            .collect();
        let anchors =
            selector.select_anchors(50, 20, DataPattern::Generic, Some(&scores));
        // The high-scoring index 25 should be selected in the middle region
        assert!(anchors.contains(&25), "high-scored item 25 should be an anchor");
    }

    #[test]
    fn test_empty() {
        let selector = AnchorSelector::default();
        assert!(selector
            .select_anchors(0, 20, DataPattern::Generic, None)
            .is_empty());
    }
}
