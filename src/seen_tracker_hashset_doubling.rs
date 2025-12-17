use crate::seen_tracker::TrackerStats;
use nohash_hasher::BuildNoHashHasher;
use std::collections::HashSet;

/// HashSet front for fast negative/duplicate checks, cascading into doubling-sized
/// sorted levels (1k, 2k, 4k, ...) to keep merges predictable in RAM.
pub struct HashSetDoublingTracker {
    front: HashSet<usize, BuildNoHashHasher<usize>>,
    base_capacity: usize,
    levels: Vec<Vec<usize>>,
    total_seen_count: usize, // counts IDs in levels only; front is added in len()

    merge_count: u64,
    merge_keys_total: u64,
    probe_steps_sum: u64,
    probe_samples: u64,
    op_counter: u64,
}

impl HashSetDoublingTracker {
    pub fn new(base_capacity: usize) -> Self {
        Self {
            front: HashSet::with_hasher(BuildNoHashHasher::default()),
            base_capacity: base_capacity.max(1_024),
            levels: Vec::new(),
            total_seen_count: 0,
            merge_count: 0,
            merge_keys_total: 0,
            probe_steps_sum: 0,
            probe_samples: 0,
            op_counter: 0,
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        self.contains_with_steps(id, false).0
    }

    pub fn contains_sampled(&mut self, id: &usize) -> bool {
        self.op_counter = self.op_counter.wrapping_add(1);
        let sample = self.op_counter & 0x3FF == 0; // sample roughly every 1024 lookups
        let (present, steps) = self.contains_with_steps(id, sample);
        if sample {
            self.probe_samples = self.probe_samples.saturating_add(1);
            self.probe_steps_sum = self.probe_steps_sum.saturating_add(steps);
        }
        present
    }

    pub fn insert(&mut self, id: usize) {
        if self.contains(&id) {
            return;
        }
        self.front.insert(id);
        if self.front.len() >= self.base_capacity {
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
        let mut carry: Vec<usize> = self.front.drain().collect();
        carry.sort_unstable();
        carry.dedup();

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
                self.merge_count = self.merge_count.saturating_add(1);
                let merged = merge_sorted(existing, carry);
                self.merge_keys_total = self
                    .merge_keys_total
                    .saturating_add(merged.len() as u64);
                carry = merged;
                if carry.len() <= capacity {
                    self.levels[level] = carry;
                    break;
                } else {
                    level += 1;
                }
            }
        }

        self.recompute_total();
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.total_seen_count + self.front.len()
    }

    pub fn stats_snapshot(&self) -> TrackerStats {
        let tier_count = self.levels.len();
        let mut top_tiers = Vec::new();
        for len in self.levels.iter().rev().take(3) {
            top_tiers.push(len.len());
        }
        let avg_probe_depth = if self.probe_samples > 0 {
            self.probe_steps_sum as f64 / self.probe_samples as f64
        } else {
            0.0
        };
        let bytes_est = self.len().saturating_mul(8);
        TrackerStats {
            tier_count,
            top_tiers,
            merge_count: self.merge_count,
            merge_keys_total: self.merge_keys_total,
            avg_probe_depth,
            bytes_est,
        }
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

    fn contains_with_steps(&self, id: &usize, sample: bool) -> (bool, u64) {
        let mut steps = 0u64;
        if sample {
            steps = steps.saturating_add(1);
        }
        if self.front.contains(id) {
            return (true, steps);
        }
        for level in self.levels.iter().rev() {
            if sample {
                steps = steps.saturating_add(1);
            }
            if level.binary_search(id).is_ok() {
                return (true, steps);
            }
        }
        (false, steps)
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
