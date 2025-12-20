use crate::{FoldError, seen_tracker::TrackerStats};
use nohash_hasher::BuildNoHashHasher;
use std::collections::HashSet;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const DISK_MERGE_MIN_BYTES: usize = 32 * 1024 * 1024; // start streaming once merged run would exceed ~32MB

/// HashSet front for fast negative/duplicate checks, cascading into doubling-sized
/// sorted levels (1k, 2k, 4k, ...) to keep merges predictable in RAM.
pub struct HashSetDoublingTracker {
    front: HashSet<usize, BuildNoHashHasher<usize>>,
    base_capacity: usize,
    levels: Vec<Vec<usize>>,
    total_seen_count: usize, // counts IDs in levels only; front is added in len()
    merge_dir: PathBuf,
    disk_merge_min_bytes: usize,

    merge_count: u64,
    merge_keys_total: u64,
    probe_steps_sum: u64,
    probe_samples: u64,
    op_counter: u64,
    hit_count: u64,
    lookup_count: u64,
}

impl HashSetDoublingTracker {
    pub fn new(base_capacity: usize) -> Self {
        Self::with_merge_dir_default(base_capacity, std::env::temp_dir())
    }

    pub fn with_merge_dir_default<P: AsRef<Path>>(base_capacity: usize, merge_dir: P) -> Self {
        Self::with_merge_dir(base_capacity, merge_dir, DISK_MERGE_MIN_BYTES)
    }

    pub fn with_merge_dir<P: AsRef<Path>>(
        base_capacity: usize,
        merge_dir: P,
        disk_merge_min_bytes: usize,
    ) -> Self {
        let merge_dir = merge_dir.as_ref().to_path_buf();
        let _ = fs::create_dir_all(&merge_dir);
        Self {
            front: HashSet::with_hasher(BuildNoHashHasher::default()),
            base_capacity: base_capacity.max(1_024),
            levels: Vec::new(),
            total_seen_count: 0,
            merge_dir,
            disk_merge_min_bytes: disk_merge_min_bytes.max(DISK_MERGE_MIN_BYTES),
            merge_count: 0,
            merge_keys_total: 0,
            probe_steps_sum: 0,
            probe_samples: 0,
            op_counter: 0,
            hit_count: 0,
            lookup_count: 0,
        }
    }

    pub fn contains(&mut self, id: &usize) -> bool {
        self.contains_internal(id, false).0
    }

    pub fn contains_sampled(&mut self, id: &usize) -> bool {
        self.contains_internal(id, true).0
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
                self.levels.push(Vec::new());
            }

            if self.levels[level].is_empty() {
                self.levels[level] = carry;
                break;
            } else {
                let existing = std::mem::take(&mut self.levels[level]);
                self.merge_count = self.merge_count.saturating_add(1);
                let merged = self.merge_sorted_runs(existing, carry)?;
                self.merge_keys_total = self.merge_keys_total.saturating_add(merged.len() as u64);
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
        let tiers = self.levels.iter().map(|l| l.len()).collect::<Vec<_>>();
        let avg_probe_depth = if self.probe_samples > 0 {
            self.probe_steps_sum as f64 / self.probe_samples as f64
        } else {
            0.0
        };
        let bytes_est = self.len().saturating_mul(std::mem::size_of::<usize>());
        TrackerStats {
            tier_count,
            top_tiers,
            tiers,
            front_len: self.front.len(),
            total_len: self.len(),
            merge_count: self.merge_count,
            merge_keys_total: self.merge_keys_total,
            avg_probe_depth,
            bytes_est,
            hit_count: self.hit_count,
            lookup_count: self.lookup_count,
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

    fn merge_sorted_runs(&self, a: Vec<usize>, b: Vec<usize>) -> Result<Vec<usize>, FoldError> {
        let total_len = a.len().saturating_add(b.len());
        if self.should_disk_merge(total_len) {
            self.merge_sorted_disk(a, b)
        } else {
            Ok(merge_sorted(a, b))
        }
    }

    fn merge_sorted_disk(&self, a: Vec<usize>, b: Vec<usize>) -> Result<Vec<usize>, FoldError> {
        let mut file = tempfile::Builder::new()
            .prefix("seen_merge_")
            .tempfile_in(&self.merge_dir)
            .map_err(FoldError::Io)?;
        let mut writer = BufWriter::new(file.as_file_mut());
        let mut a_idx = 0;
        let mut b_idx = 0;
        let mut last_written: Option<usize> = None;
        let mut merged_len = 0usize;

        while a_idx < a.len() && b_idx < b.len() {
            let next = if a[a_idx] < b[b_idx] {
                let v = a[a_idx];
                a_idx += 1;
                v
            } else if b[b_idx] < a[a_idx] {
                let v = b[b_idx];
                b_idx += 1;
                v
            } else {
                let v = a[a_idx];
                a_idx += 1;
                b_idx += 1;
                v
            };
            if last_written.map_or(true, |last| last != next) {
                writer
                    .write_all(&next.to_le_bytes())
                    .map_err(FoldError::Io)?;
                last_written = Some(next);
                merged_len = merged_len.saturating_add(1);
            }
        }

        for remaining in [&a[a_idx..], &b[b_idx..]] {
            for &value in remaining {
                if last_written.map_or(true, |last| last != value) {
                    writer
                        .write_all(&value.to_le_bytes())
                        .map_err(FoldError::Io)?;
                    last_written = Some(value);
                    merged_len = merged_len.saturating_add(1);
                }
            }
        }

        writer.flush().map_err(FoldError::Io)?;
        drop(writer);
        file.as_file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(FoldError::Io)?;

        // Drop the input buffers before we allocate the new run to avoid peak usage spikes.
        drop(a);
        drop(b);

        let mut reader = BufReader::new(file.as_file_mut());
        let mut merged = Vec::with_capacity(merged_len);
        let mut buf = [0u8; std::mem::size_of::<usize>()];
        for _ in 0..merged_len {
            reader.read_exact(&mut buf).map_err(FoldError::Io)?;
            merged.push(usize::from_le_bytes(buf));
        }
        Ok(merged)
    }

    fn should_disk_merge(&self, total_len: usize) -> bool {
        total_len.saturating_mul(std::mem::size_of::<usize>()) >= self.disk_merge_min_bytes
    }

    fn contains_internal(&mut self, id: &usize, allow_sample: bool) -> (bool, u64) {
        self.lookup_count = self.lookup_count.saturating_add(1);
        self.op_counter = self.op_counter.wrapping_add(1);
        let sample = allow_sample && (self.op_counter & 0x3FF == 0); // ~every 1024 lookups
        let (present, steps) = self.contains_with_steps(id, sample);
        if present {
            self.hit_count = self.hit_count.saturating_add(1);
        }
        if sample {
            self.probe_samples = self.probe_samples.saturating_add(1);
            self.probe_steps_sum = self.probe_steps_sum.saturating_add(steps);
        }
        (present, steps)
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
