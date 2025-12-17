use crate::FoldError;
use bloomfilter::Bloom;

/// Pure in-memory segmented tracker: multiple sealed sorted segments plus an active buffer.
pub struct SegmentedRamSeenTracker {
    bloom: Bloom<usize>,
    active: Vec<usize>,
    active_sorted: bool,
    segment_limit: usize,
    segments: Vec<Vec<usize>>, // newest last
    total_seen_count: usize,
}

impl SegmentedRamSeenTracker {
    pub fn new(bloom_capacity: usize, segment_limit: usize) -> Self {
        let bloom_capacity = bloom_capacity.max(1_000_000);
        let bloom = Bloom::new_for_fp_rate(bloom_capacity, 0.01);
        Self {
            bloom,
            active: Vec::new(),
            active_sorted: false,
            segment_limit,
            segments: Vec::new(),
            total_seen_count: 0,
        }
    }

    fn ensure_active_sorted(&mut self) {
        if !self.active_sorted {
            self.active.sort_unstable();
            self.active.dedup();
            self.active_sorted = true;
        }
    }

    fn seal_segment(&mut self) {
        self.ensure_active_sorted();
        if self.active.is_empty() {
            return;
        }
        let mut segment = Vec::new();
        std::mem::swap(&mut segment, &mut self.active);
        self.active_sorted = false;
        self.total_seen_count = self.total_seen_count.saturating_add(segment.len());
        self.segments.push(segment);
    }

    pub fn contains(&mut self, id: &usize) -> bool {
        if !self.bloom.check(id) {
            return false;
        }
        if !self.active_sorted {
            self.ensure_active_sorted();
        }
        if self.active.binary_search(id).is_ok() {
            return true;
        }
        for seg in self.segments.iter().rev() {
            if seg.binary_search(id).is_ok() {
                return true;
            }
        }
        false
    }

    pub fn insert(&mut self, id: usize) {
        if self.contains(&id) {
            return;
        }
        self.bloom.set(&id);
        self.active.push(id);
        self.active_sorted = false;
        if self.active.len() >= self.segment_limit {
            self.seal_segment();
        }
    }

    pub fn flush(&mut self) -> Result<(), FoldError> {
        self.seal_segment();
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.total_seen_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmented_basic() {
        let mut tracker = SegmentedRamSeenTracker::new(1000, 4);
        for i in 0..10 {
            tracker.insert(i);
        }
        tracker.flush().unwrap();
        for i in 0..10 {
            assert!(tracker.contains(&i));
        }
    }
}
