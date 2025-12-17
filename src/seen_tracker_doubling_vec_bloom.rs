use crate::FoldError;
use bloomfilter::Bloom;

/// Doubling-level tracker with a Bloom front. Buffer cascades into sorted levels (1k, 2k, 4k, ...).
pub struct DoublingVecBloomTracker {
    bloom: Bloom<usize>,
    bloom_capacity: usize,
    base_capacity: usize,
    buffer: Vec<usize>,
    buffer_sorted: bool,
    levels: Vec<Vec<usize>>,
    total_seen_count: usize,
}

impl DoublingVecBloomTracker {
    pub fn new(bloom_capacity: usize, base_capacity: usize) -> Self {
        let bloom_capacity = bloom_capacity.max(1_000_000);
        Self {
            bloom: Bloom::new_for_fp_rate(bloom_capacity, 0.01),
            bloom_capacity,
            base_capacity: base_capacity.max(1_024),
            buffer: Vec::new(),
            buffer_sorted: false,
            levels: Vec::new(),
            total_seen_count: 0,
        }
    }

    pub fn contains(&mut self, id: &usize) -> bool {
        if !self.bloom.check(id) {
            return false;
        }
        if !self.buffer.is_empty() && !self.buffer_sorted {
            self.ensure_buffer_sorted();
        }
        if self.buffer.binary_search(id).is_ok() {
            return true;
        }
        for level in self.levels.iter().rev() {
            if level.binary_search(id).is_ok() {
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
        self.buffer.push(id);
        self.buffer_sorted = false;
        if self.buffer.len() >= self.base_capacity {
            let _ = self.flush_buffer();
        }
    }

    pub fn insert_batch(&mut self, ids: &[usize]) {
        for &id in ids {
            self.insert(id);
        }
    }

    pub fn flush(&mut self) -> Result<(), FoldError> {
        self.flush_buffer()
    }

    pub fn len(&self) -> usize {
        self.total_seen_count + self.buffer.len()
    }

    fn ensure_buffer_sorted(&mut self) {
        if self.buffer_sorted {
            return;
        }
        self.buffer.sort_unstable();
        self.buffer.dedup();
        self.buffer_sorted = true;
    }

    fn flush_buffer(&mut self) -> Result<(), FoldError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.ensure_buffer_sorted();
        let mut carry = std::mem::take(&mut self.buffer);
        self.buffer_sorted = false;

        let mut level = 0;
        loop {
            let capacity = self.capacity_for_level(level);
            if level >= self.levels.len() {
                self.levels.push(Vec::new());
            }

            if self.levels[level].is_empty() {
                self.levels[level] = carry;
                break;
            } else {
                let existing = std::mem::take(&mut self.levels[level]);
                carry = merge_sorted(existing, carry);
                if carry.len() <= capacity {
                    self.levels[level] = carry;
                    break;
                } else {
                    level += 1;
                }
            }
        }

        self.recompute_total();
        self.maybe_rebuild_bloom();
        Ok(())
    }

    fn recompute_total(&mut self) {
        let mut total = 0usize;
        for level in &self.levels {
            total = total.saturating_add(level.len());
        }
        self.total_seen_count = total;
    }

    fn capacity_for_level(&self, level: usize) -> usize {
        self.base_capacity << level
    }

    fn maybe_rebuild_bloom(&mut self) {
        let items = self.len();
        let fp = estimate_fp_rate(self.bloom_capacity, items, 0.01);
        if fp <= 0.02 {
            return;
        }
        let new_capacity = (self.bloom_capacity * 2).max(items.max(1_000_000));
        let mut bloom = Bloom::new_for_fp_rate(new_capacity, 0.01);
        if !self.buffer.is_empty() && !self.buffer_sorted {
            self.ensure_buffer_sorted();
        }
        for id in &self.buffer {
            bloom.set(id);
        }
        for level in &self.levels {
            for id in level {
                bloom.set(id);
            }
        }
        self.bloom = bloom;
        self.bloom_capacity = new_capacity;
    }
}

fn merge_sorted(a: Vec<usize>, b: Vec<usize>) -> Vec<usize> {
    let mut merged = Vec::with_capacity(a.len().saturating_add(b.len()));
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
