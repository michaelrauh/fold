use crate::FoldError;
use crate::seen_tracker_hashset_doubling::HashSetDoublingTracker;

pub const DEFAULT_GLOBAL_BUFFER: usize = 16_384;

#[derive(Clone, Default, Debug)]
pub struct TrackerStats {
    pub tier_count: usize,
    pub top_tiers: Vec<usize>,
    pub merge_count: u64,
    pub merge_keys_total: u64,
    pub avg_probe_depth: f64,
    pub bytes_est: usize,
}

#[derive(Default, Debug)]
pub struct BatchResult {
    pub new: Vec<usize>,
    pub seen: Vec<usize>,
}

/// Production seen tracker: in-memory HashSet + doubling tiers (base 16k).
pub struct SeenTracker {
    inner: HashSetDoublingTracker,
}

impl SeenTracker {
    pub fn new(_expected_items: usize) -> Self {
        Self::with_path("./fold_state/seen_shards", DEFAULT_GLOBAL_BUFFER)
    }

    pub fn with_config(_bloom_capacity: usize) -> Self {
        Self::with_path("./fold_state/seen_shards", DEFAULT_GLOBAL_BUFFER)
    }

    pub fn with_path(_path: &str, _bloom_capacity: usize) -> Self {
        Self {
            inner: HashSetDoublingTracker::new(DEFAULT_GLOBAL_BUFFER),
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        self.inner.contains(id)
    }

    pub fn insert(&mut self, id: usize) {
        self.inner.insert(id);
    }

    pub fn insert_batch(&mut self, ids: &[usize]) {
        self.inner.insert_batch(ids);
    }

    /// Classify a batch: returns which IDs were new vs already present.
    pub fn check_batch(&mut self, ids: &[usize], _flush: bool) -> Result<BatchResult, FoldError> {
        let mut result = BatchResult::default();
        for &id in ids {
            if self.inner.contains_sampled(&id) {
                result.seen.push(id);
            } else {
                self.inner.insert(id);
                result.new.push(id);
            }
        }
        Ok(result)
    }

    pub fn flush_pending(&mut self) -> Result<BatchResult, FoldError> {
        Ok(BatchResult::default())
    }

    pub fn flush(&mut self) -> Result<(), FoldError> {
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn buffered_total(&self) -> usize {
        0
    }

    pub fn stats_snapshot(&self) -> TrackerStats {
        self.inner.stats_snapshot()
    }

    pub fn estimated_false_positive_rate(&self) -> f64 {
        0.0
    }

    pub fn estimated_false_positive_rate_for_capacity(&self, _capacity: usize) -> f64 {
        0.0
    }

    pub fn rebuild_bloom(&mut self, _new_capacity: usize) -> Result<f64, FoldError> {
        Ok(0.0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_contains() {
        let mut tracker = SeenTracker::new(100);
        assert!(!tracker.contains(&1));
        tracker.insert(1);
        assert!(tracker.contains(&1));
    }
}
