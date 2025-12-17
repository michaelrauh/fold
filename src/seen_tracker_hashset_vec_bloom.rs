use bloomfilter::Bloom;
use nohash_hasher::BuildNoHashHasher;
use std::collections::HashSet;

/// Hybrid tracker with a Bloom front and HashSet + sorted Vec backing.
/// Bloom filters fast negatives; HashSet handles recent inserts; Vec is long-term sorted store.
pub struct HashSetVecBloomTracker {
    bloom: Bloom<usize>,
    bloom_capacity: usize,
    front: HashSet<usize, BuildNoHashHasher<usize>>,
    sorted: Vec<usize>,
    flush_limit: usize,
    total_seen_count: usize,
}

impl HashSetVecBloomTracker {
    pub fn new(bloom_capacity: usize, flush_limit: usize) -> Self {
        let bloom_capacity = bloom_capacity.max(1_000_000);
        Self {
            bloom: Bloom::new_for_fp_rate(bloom_capacity, 0.01),
            bloom_capacity,
            front: HashSet::with_hasher(BuildNoHashHasher::default()),
            sorted: Vec::new(),
            flush_limit: flush_limit.max(1),
            total_seen_count: 0,
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        if !self.bloom.check(id) {
            return false;
        }
        self.front.contains(id) || self.sorted.binary_search(id).is_ok()
    }

    pub fn insert(&mut self, id: usize) {
        if self.contains(&id) {
            return;
        }
        self.bloom.set(&id);
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
        self.maybe_rebuild_bloom();
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

    fn maybe_rebuild_bloom(&mut self) {
        let items = self.len();
        let fp = estimate_fp_rate(self.bloom_capacity, items, 0.01);
        if fp <= 0.02 {
            return;
        }
        let new_capacity = (self.bloom_capacity * 2).max(items.max(1_000_000));
        let mut bloom = Bloom::new_for_fp_rate(new_capacity, 0.01);
        for id in &self.sorted {
            bloom.set(id);
        }
        for id in &self.front {
            bloom.set(id);
        }
        self.bloom = bloom;
        self.bloom_capacity = new_capacity;
    }
}

fn estimate_fp_rate(capacity: usize, items: usize, target_fp: f64) -> f64 {
    if capacity == 0 || items == 0 {
        return 0.0;
    }
    let bits_per_item = (-target_fp.ln()) / (std::f64::consts::LN_2.powi(2));
    let m = bits_per_item * capacity as f64;
    let k = (m / capacity as f64) * std::f64::consts::LN_2;
    let exponent = (-k * items as f64 / m).exp();
    let fp_rate = (1.0 - exponent).powf(k);
    fp_rate.max(0.0)
}
