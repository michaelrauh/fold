use crate::FoldError;
use bloomfilter::Bloom;
use nohash_hasher::BuildNoHashHasher;
use std::collections::HashSet;

struct EytzingerTier {
    bloom: Bloom<usize>,
    sorted: Vec<usize>,
    eytzinger: Vec<usize>,
}

struct EytzingerTierNoBloom {
    sorted: Vec<usize>,
    eytzinger: Vec<usize>,
}

struct SortedVecBloomTier {
    bloom: Bloom<usize>,
    sorted: Vec<usize>,
}

pub struct EytzingerBloomTracker {
    front: HashSet<usize, BuildNoHashHasher<usize>>,
    base_capacity: usize,
    levels: Vec<EytzingerTier>,
    total_seen_count: usize,
}

impl EytzingerBloomTracker {
    pub fn new(base_capacity: usize) -> Self {
        Self {
            front: HashSet::with_hasher(BuildNoHashHasher::default()),
            base_capacity: base_capacity.max(16_384),
            levels: Vec::new(),
            total_seen_count: 0,
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        if self.front.contains(id) {
            return true;
        }
        for tier in self.levels.iter().rev() {
            if !tier.bloom.check(id) {
                continue;
            }
            if eytzinger_contains(&tier.eytzinger, *id) {
                return true;
            }
        }
        false
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

    pub fn flush(&mut self) -> Result<(), FoldError> {
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
                self.levels.push(build_tier(carry, 0.002));
                break;
            }

            if self.levels[level].sorted.is_empty() {
                self.levels[level] = build_tier(carry, 0.002);
                break;
            } else {
                let existing_sorted = std::mem::take(&mut self.levels[level].sorted);
                let merged = merge_sorted(existing_sorted, carry);
                if merged.len() <= capacity {
                    self.levels[level] = build_tier(merged, 0.002);
                    break;
                } else {
                    carry = merged;
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

    fn recompute_total(&mut self) {
        let mut total = 0usize;
        for tier in &self.levels {
            total = total.saturating_add(tier.sorted.len());
        }
        self.total_seen_count = total;
    }

    fn capacity_for_level(&self, level: usize) -> usize {
        self.base_capacity << level
    }
}

/// Same tiering but no Bloom; still uses Eytzinger layout.
pub struct EytzingerNoBloomTracker {
    front: HashSet<usize, BuildNoHashHasher<usize>>,
    base_capacity: usize,
    levels: Vec<EytzingerTierNoBloom>,
    total_seen_count: usize,
}

impl EytzingerNoBloomTracker {
    pub fn new(base_capacity: usize) -> Self {
        Self {
            front: HashSet::with_hasher(BuildNoHashHasher::default()),
            base_capacity: base_capacity.max(16_384),
            levels: Vec::new(),
            total_seen_count: 0,
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        if self.front.contains(id) {
            return true;
        }
        for tier in self.levels.iter().rev() {
            if eytzinger_contains(&tier.eytzinger, *id) {
                return true;
            }
        }
        false
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

    pub fn flush(&mut self) -> Result<(), FoldError> {
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
                self.levels.push(build_tier_no_bloom(carry));
                break;
            }
            if self.levels[level].sorted.is_empty() {
                self.levels[level] = build_tier_no_bloom(carry);
                break;
            } else {
                let existing_sorted = std::mem::take(&mut self.levels[level].sorted);
                let merged = merge_sorted(existing_sorted, carry);
                if merged.len() <= capacity {
                    self.levels[level] = build_tier_no_bloom(merged);
                    break;
                } else {
                    carry = merged;
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

    fn recompute_total(&mut self) {
        let mut total = 0usize;
        for tier in &self.levels {
            total = total.saturating_add(tier.sorted.len());
        }
        self.total_seen_count = total;
    }

    fn capacity_for_level(&self, level: usize) -> usize {
        self.base_capacity << level
    }
}

/// Sorted vec with Bloom (0.2% FP), no Eytzinger layout.
pub struct SortedVecBloomTracker {
    front: HashSet<usize, BuildNoHashHasher<usize>>,
    base_capacity: usize,
    levels: Vec<SortedVecBloomTier>,
    total_seen_count: usize,
}

impl SortedVecBloomTracker {
    pub fn new(base_capacity: usize) -> Self {
        Self {
            front: HashSet::with_hasher(BuildNoHashHasher::default()),
            base_capacity: base_capacity.max(16_384),
            levels: Vec::new(),
            total_seen_count: 0,
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        if self.front.contains(id) {
            return true;
        }
        for tier in self.levels.iter().rev() {
            if !tier.bloom.check(id) {
                continue;
            }
            if tier.sorted.binary_search(id).is_ok() {
                return true;
            }
        }
        false
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

    pub fn flush(&mut self) -> Result<(), FoldError> {
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
                self.levels.push(build_tier_sorted_bloom(carry, 0.002));
                break;
            }

            if self.levels[level].sorted.is_empty() {
                self.levels[level] = build_tier_sorted_bloom(carry, 0.002);
                break;
            } else {
                let existing_sorted = std::mem::take(&mut self.levels[level].sorted);
                let merged = merge_sorted(existing_sorted, carry);
                if merged.len() <= capacity {
                    self.levels[level] = build_tier_sorted_bloom(merged, 0.002);
                    break;
                } else {
                    carry = merged;
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

    fn recompute_total(&mut self) {
        let mut total = 0usize;
        for tier in &self.levels {
            total = total.saturating_add(tier.sorted.len());
        }
        self.total_seen_count = total;
    }

    fn capacity_for_level(&self, level: usize) -> usize {
        self.base_capacity << level
    }
}

fn build_tier(mut sorted: Vec<usize>, fp_rate: f64) -> EytzingerTier {
    let bloom = Bloom::new_for_fp_rate(sorted.len().max(1_000_000), fp_rate);
    let mut bloom = bloom;
    for id in &sorted {
        bloom.set(id);
    }
    let eytz = to_eytzinger(&sorted);
    sorted.shrink_to_fit();
    EytzingerTier { bloom, sorted, eytzinger: eytz }
}

fn build_tier_no_bloom(mut sorted: Vec<usize>) -> EytzingerTierNoBloom {
    let eytz = to_eytzinger(&sorted);
    sorted.shrink_to_fit();
    EytzingerTierNoBloom { sorted, eytzinger: eytz }
}

fn build_tier_sorted_bloom(mut sorted: Vec<usize>, fp_rate: f64) -> SortedVecBloomTier {
    let bloom = Bloom::new_for_fp_rate(sorted.len().max(1_000_000), fp_rate);
    let mut bloom = bloom;
    for id in &sorted {
        bloom.set(id);
    }
    sorted.shrink_to_fit();
    SortedVecBloomTier { bloom, sorted }
}

fn to_eytzinger(sorted: &[usize]) -> Vec<usize> {
    let n = sorted.len();
    if n == 0 {
        return Vec::new();
    }
    let mut eytz = vec![0usize; n];
    let mut stack = Vec::new();
    stack.push((0usize, 0usize, n)); // (dest_index, lo, hi)
    while let Some((idx, lo, hi)) = stack.pop() {
        if idx >= n || lo >= hi {
            continue;
        }
        let mid = (lo + hi) / 2;
        eytz[idx] = sorted[mid];
        let left_idx = 2 * idx + 1;
        let right_idx = 2 * idx + 2;
        if right_idx < n {
            stack.push((right_idx, mid + 1, hi));
        }
        if left_idx < n {
            stack.push((left_idx, lo, mid));
        }
    }
    eytz
}

fn eytzinger_contains(data: &[usize], key: usize) -> bool {
    let mut idx = 0usize;
    while idx < data.len() {
        let v = data[idx];
        if key < v {
            idx = 2 * idx + 1;
        } else if key > v {
            idx = 2 * idx + 2;
        } else {
            return true;
        }
    }
    false
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
