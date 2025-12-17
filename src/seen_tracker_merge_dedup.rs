use crate::{seen_tracker::BatchResult, FoldError};
use bloomfilter::Bloom;

/// Deferred-dedup tracker: accumulates buffers and deduplicates only on merge/flush.
/// Membership checks ignore the in-flight buffer; duplicates are eliminated when flushing.
pub struct MergeDedupTracker {
    bloom: Bloom<usize>,
    buffer: Vec<usize>,
    committed: Vec<usize>,
    buffer_limit: usize,
    total_seen_count: usize,
}

impl MergeDedupTracker {
    pub fn new(expected_items: usize, buffer_limit: usize) -> Self {
        let bloom_capacity = expected_items.max(1_000_000);
        let bloom = Bloom::new_for_fp_rate(bloom_capacity, 0.01);
        Self {
            bloom,
            buffer: Vec::new(),
            committed: Vec::new(),
            buffer_limit,
            total_seen_count: 0,
        }
    }

    /// Stage a batch without immediate classification.
    pub fn stage_batch(&mut self, ids: &[usize]) {
        self.buffer.extend_from_slice(ids);
        if self.buffer.len() >= self.buffer_limit {
            let _ = self.flush();
        }
    }

    pub fn flush(&mut self) -> Result<(), FoldError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.buffer.sort_unstable();
        self.buffer.dedup();
        self.committed =
            merge_sorted(std::mem::take(&mut self.buffer), std::mem::take(&mut self.committed));
        for id in &self.committed {
            self.bloom.set(id);
        }
        self.total_seen_count = self.committed.len();
        Ok(())
    }

    /// Flush and return classification of staged IDs vs committed set.
    pub fn flush_with_result(&mut self) -> Result<BatchResult, FoldError> {
        if self.buffer.is_empty() {
            return Ok(BatchResult::default());
        }
        self.buffer.sort_unstable();
        self.buffer.dedup();

        let mut new = Vec::new();
        let mut seen = Vec::new();

        let mut merged = Vec::with_capacity(self.buffer.len() + self.committed.len());
        let mut i = 0;
        let mut j = 0;
        while i < self.buffer.len() && j < self.committed.len() {
            let a = self.buffer[i];
            let b = self.committed[j];
            if a < b {
                new.push(a);
                merged.push(a);
                i += 1;
            } else if b < a {
                merged.push(b);
                j += 1;
            } else {
                seen.push(a);
                merged.push(a);
                i += 1;
                j += 1;
            }
        }
        if i < self.buffer.len() {
            new.extend_from_slice(&self.buffer[i..]);
            merged.extend_from_slice(&self.buffer[i..]);
        }
        if j < self.committed.len() {
            merged.extend_from_slice(&self.committed[j..]);
        }

        self.buffer.clear();
        self.committed = merged;
        for id in &self.committed {
            self.bloom.set(id);
        }
        self.total_seen_count = self.committed.len();

        Ok(BatchResult { new, seen })
    }

    pub fn contains(&self, id: &usize) -> bool {
        if !self.bloom.check(id) {
            return false;
        }
        self.committed.binary_search(id).is_ok()
    }

    pub fn len(&self) -> usize {
        self.total_seen_count
    }
}

fn merge_sorted(mut a: Vec<usize>, mut b: Vec<usize>) -> Vec<usize> {
    a.sort_unstable();
    a.dedup();
    b.sort_unstable();
    b.dedup();
    let mut merged = Vec::with_capacity(a.len() + b.len());
    let mut i = 0;
    let mut j = 0;
    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            merged.push(a[i]);
            i += 1;
        } else if b[j] < a[i] {
            merged.push(b[j]);
            j += 1;
        } else {
            merged.push(a[i]);
            i += 1;
            j += 1;
        }
    }
    if i < a.len() {
        merged.extend_from_slice(&a[i..]);
    }
    if j < b.len() {
        merged.extend_from_slice(&b[j..]);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_dedup_basic() {
        let mut tracker = MergeDedupTracker::new(1000, 10);
        tracker.stage_batch(&[1, 2, 2, 3]);
        tracker.flush().unwrap();
        assert!(tracker.contains(&1));
        assert!(tracker.contains(&2));
        assert!(tracker.contains(&3));
        assert_eq!(tracker.len(), 3);
    }
}
