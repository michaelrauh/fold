use nohash_hasher::BuildNoHashHasher;
use std::collections::HashSet;

/// Hybrid tracker: HashSet for fast front-door membership, periodically drained
/// into a single sorted Vec to keep memory predictable and lookups O(log n).
pub struct HashSetVecTracker {
    front: HashSet<usize, BuildNoHashHasher<usize>>,
    sorted: Vec<usize>,
    flush_limit: usize,
    total_seen_count: usize,
}

impl HashSetVecTracker {
    /// `flush_limit` controls when the HashSet is merged into the sorted Vec.
    pub fn new(flush_limit: usize) -> Self {
        Self {
            front: HashSet::with_hasher(BuildNoHashHasher::default()),
            sorted: Vec::new(),
            flush_limit: flush_limit.max(1),
            total_seen_count: 0,
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        self.front.contains(id) || self.sorted.binary_search(id).is_ok()
    }

    pub fn insert(&mut self, id: usize) {
        if self.contains(&id) {
            return;
        }
        self.front.insert(id);
        if self.front.len() >= self.flush_limit {
            let _ = self.flush();
        }
    }

    pub fn insert_batch(&mut self, ids: &[usize]) {
        for &id in ids {
            self.insert(id);
        }
    }

    pub fn flush(&mut self) -> Result<(), crate::FoldError> {
        if self.front.is_empty() {
            return Ok(());
        }
        let mut drained: Vec<usize> = self.front.drain().collect();
        drained.sort_unstable();
        drained.dedup();
        self.merge_into_sorted(drained);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.total_seen_count + self.front.len()
    }

    fn merge_into_sorted(&mut self, drained: Vec<usize>) {
        if self.sorted.is_empty() {
            self.total_seen_count = drained.len();
            self.sorted = drained;
            return;
        }

        let mut merged = Vec::with_capacity(self.sorted.len() + drained.len());
        let mut i = 0;
        let mut j = 0;
        let left = &self.sorted;
        let right = drained;

        while i < left.len() && j < right.len() {
            let a = left[i];
            let b = right[j];
            if a < b {
                merged.push(a);
                i += 1;
            } else if b < a {
                merged.push(b);
                j += 1;
            } else {
                merged.push(a);
                i += 1;
                j += 1;
            }
        }
        if i < left.len() {
            merged.extend_from_slice(&left[i..]);
        }
        if j < right.len() {
            merged.extend_from_slice(&right[j..]);
        }

        merged.shrink_to_fit();
        self.total_seen_count = merged.len();
        self.sorted = merged;
    }
}
