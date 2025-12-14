use crate::FoldError;
use bloomfilter::Bloom;
use hashbrown::HashSet as HbHashSet;
use nohash_hasher::BuildNoHashHasher;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

const DEFAULT_PER_SHARD_BUFFER: usize = 1024;
const DEFAULT_GLOBAL_BUFFER: usize = 16_384;

type FastSet<T> = HbHashSet<T, BuildNoHashHasher<T>>;

#[derive(Clone, Default, Debug)]
pub struct TrackerStats {
    pub shard_loads_disk: usize,
    pub shard_loads_new: usize,
    pub shard_hits: usize,
    pub evictions: usize,
    pub flushes_threshold: usize,
    pub flushes_explicit: usize,
    pub flushed_ids: usize,
    pub buffered_total: usize,
    pub max_buffered_per_shard: usize,
}

#[derive(Default, Debug)]
pub struct BatchResult {
    pub new: Vec<usize>,
    pub seen: Vec<usize>,
}

#[derive(Clone, Copy)]
enum FlushReason {
    Threshold,
    Explicit,
}

/// A single shard containing a portion of the seen IDs
struct Shard {
    id: usize,
    seen: FastSet<usize>,
    dirty: bool, // true if modified since last disk write
}

impl Shard {
    fn new(id: usize) -> Self {
        Self {
            id,
            seen: FastSet::default(),
            dirty: false,
        }
    }

    fn load_from_disk(path: &Path) -> Result<Self, FoldError> {
        let data = fs::read(path).map_err(|e| FoldError::Io(e))?;
        let seen_vec: Vec<usize> = bincode::decode_from_slice(&data, bincode::config::standard())
            .map_err(|e| FoldError::Deserialization(Box::new(e)))?
            .0;
        let seen: FastSet<usize> = seen_vec.into_iter().collect();

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| FoldError::Other("Invalid shard filename".to_string()))?;

        let id = file_name
            .strip_prefix("shard_")
            .and_then(|s| s.strip_suffix(".bin"))
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| FoldError::Other("Invalid shard ID in filename".to_string()))?;

        Ok(Self {
            id,
            seen,
            dirty: false,
        })
    }

    fn save_to_disk(&mut self, dir: &Path) -> Result<(), FoldError> {
        if !self.dirty {
            return Ok(());
        }

        fs::create_dir_all(dir).map_err(|e| FoldError::Io(e))?;

        let path = dir.join(format!("shard_{:08}.bin", self.id));
        // Persist as a Vec to avoid relying on HashSet serde support for the no-hash hasher.
        let data = bincode::encode_to_vec(
            &self.seen.iter().copied().collect::<Vec<_>>(),
            bincode::config::standard(),
        )
        .map_err(|e| FoldError::Serialization(Box::new(e)))?;

        fs::write(path, data).map_err(|e| FoldError::Io(e))?;
        self.dirty = false;
        Ok(())
    }
}

/// Tracks seen ortho IDs using a bloom filter for fast negative checks
/// and a sharded hashmap with LRU disk backing for memory efficiency
pub struct SeenTracker {
    bloom: Bloom<usize>,
    bloom_capacity: usize,

    // Sharding configuration
    num_shards: usize,
    max_shards_in_memory: usize,
    max_shards_ceiling: usize,

    // In-memory shards (LRU order - most recently used at end)
    loaded_shards: Vec<Shard>,

    // Directory for disk-backed shards
    shard_dir: PathBuf,

    // Track total count across all shards
    total_seen_count: usize,

    // Deferred bloom "maybe" buffers
    shard_buffers: HashMap<usize, Vec<usize>>,
    buffered_total: usize,
    per_shard_flush_threshold: usize,
    global_buffer_limit: usize,
    already_emitted: FastSet<usize>,

    // Instrumentation
    stats: TrackerStats,
}

impl SeenTracker {
    /// Create a new SeenTracker with default settings (for fresh start)
    /// Default: 1,000,000 bloom capacity, 64 shards, all in memory
    pub fn new(expected_items: usize) -> Self {
        let bloom_capacity = expected_items.max(1_000_000);
        let num_shards = 64;
        let max_shards_in_memory = num_shards; // all shards in memory for fresh start

        Self::with_config(bloom_capacity, num_shards, max_shards_in_memory)
    }

    /// Create a new SeenTracker with specific configuration (for checkpoint resume)
    pub fn with_config(
        bloom_capacity: usize,
        num_shards: usize,
        max_shards_in_memory: usize,
    ) -> Self {
        Self::with_path(
            "./fold_state/seen_shards",
            bloom_capacity,
            num_shards,
            max_shards_in_memory,
        )
    }

    pub fn with_path(
        path: &str,
        bloom_capacity: usize,
        num_shards: usize,
        max_shards_in_memory: usize,
    ) -> Self {
        let false_positive_rate = 0.01; // 1% FPR
        let bloom = Bloom::new_for_fp_rate(bloom_capacity, false_positive_rate);

        let shard_dir = PathBuf::from(path);

        // Clear old shard files on initialization
        if shard_dir.exists() {
            let _ = fs::remove_dir_all(&shard_dir);
        }
        let _ = fs::create_dir_all(&shard_dir);

        Self {
            bloom,
            bloom_capacity,
            num_shards,
            max_shards_in_memory,
            max_shards_ceiling: max_shards_in_memory,
            loaded_shards: Vec::new(),
            shard_dir,
            total_seen_count: 0,
            shard_buffers: HashMap::new(),
            buffered_total: 0,
            per_shard_flush_threshold: DEFAULT_PER_SHARD_BUFFER,
            global_buffer_limit: DEFAULT_GLOBAL_BUFFER,
            already_emitted: FastSet::default(),
            stats: TrackerStats::default(),
        }
    }

    fn enqueue_buffer(&mut self, shard_id: usize, id: usize) {
        let entry = self.shard_buffers.entry(shard_id).or_default();
        entry.push(id);
        self.buffered_total = self.buffered_total.saturating_add(1);
        if entry.len() > self.stats.max_buffered_per_shard {
            self.stats.max_buffered_per_shard = entry.len();
        }
    }

    fn should_flush_threshold(&self) -> bool {
        if self.buffered_total >= self.global_buffer_limit {
            return true;
        }
        self.shard_buffers
            .values()
            .any(|buf| buf.len() >= self.per_shard_flush_threshold)
    }

    fn flush_one(
        &mut self,
        reason: FlushReason,
        result: &mut BatchResult,
    ) -> Result<(), FoldError> {
        let shard_id = match self
            .shard_buffers
            .iter()
            .max_by_key(|(_, buf)| buf.len())
            .map(|(id, _)| *id)
        {
            Some(id) => id,
            None => return Ok(()),
        };

        let mut buffer = self.shard_buffers.remove(&shard_id).unwrap_or_default();
        self.buffered_total = self.buffered_total.saturating_sub(buffer.len());

        // Sort/dedup instead of a HashSet to reduce hashing overhead for batch flushes.
        buffer.sort_unstable();
        buffer.dedup();

        let mut items = Vec::with_capacity(buffer.len());
        for id in buffer {
            let already_emitted = self.already_emitted.remove(&id);
            items.push((id, already_emitted));
        }

        let mut new_ids = Vec::new();
        let mut processed = 0usize;
        {
            let shard = self.get_shard_mut(shard_id)?;
            shard.seen.reserve(items.len());
            for (id, already_emitted) in items {
                if shard.seen.insert(id) {
                    shard.dirty = true;
                    new_ids.push(id);
                    if !already_emitted {
                        result.new.push(id);
                    }
                } else if !already_emitted {
                    result.seen.push(id);
                }
                processed += 1;
            }
        }

        for id in new_ids {
            self.total_seen_count += 1;
            self.bloom.set(&id);
        }

        match reason {
            FlushReason::Threshold => self.stats.flushes_threshold += 1,
            FlushReason::Explicit => self.stats.flushes_explicit += 1,
        }
        self.stats.flushed_ids = self.stats.flushed_ids.saturating_add(processed);

        Ok(())
    }

    fn evict_lru(&mut self) -> Result<(), FoldError> {
        if let Some(shard) = self.loaded_shards.get_mut(0) {
            shard.save_to_disk(&self.shard_dir)?;
        }
        if !self.loaded_shards.is_empty() {
            self.loaded_shards.remove(0);
            self.stats.evictions += 1;
        }
        Ok(())
    }

    /// Calculate which shard an ID belongs to
    fn shard_id_for(&self, id: &usize) -> usize {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        (hasher.finish() as usize) % self.num_shards
    }

    /// Get or load a shard, managing LRU eviction
    fn get_shard_mut(&mut self, shard_id: usize) -> Result<&mut Shard, FoldError> {
        // Check if shard is already loaded
        if let Some(pos) = self.loaded_shards.iter().position(|s| s.id == shard_id) {
            // Move to end (most recently used)
            let shard = self.loaded_shards.remove(pos);
            self.loaded_shards.push(shard);
            self.stats.shard_hits += 1;
            return Ok(self.loaded_shards.last_mut().unwrap());
        }

        // Need to load from disk or create new
        let shard_path = self.shard_dir.join(format!("shard_{:08}.bin", shard_id));
        let shard = if shard_path.exists() {
            self.stats.shard_loads_disk += 1;
            Shard::load_from_disk(&shard_path)?
        } else {
            self.stats.shard_loads_new += 1;
            Shard::new(shard_id)
        };

        // If at capacity, evict least recently used (first in vec)
        if self.loaded_shards.len() >= self.max_shards_in_memory {
            self.evict_lru()?;
        }

        self.loaded_shards.push(shard);
        Ok(self.loaded_shards.last_mut().unwrap())
    }

    /// Check if an ID has been seen before
    pub fn contains(&mut self, id: &usize) -> bool {
        // Fast negative check with bloom filter
        if !self.bloom.check(id) {
            return false;
        }

        // Determine which shard
        let shard_id = self.shard_id_for(id);

        // Try to get the shard (will load from disk if needed, or create new if doesn't exist)
        // If the shard doesn't exist on disk, it will be created empty, so contains_key will return false
        if let Ok(shard) = self.get_shard_mut(shard_id) {
            return shard.seen.contains(id);
        }

        // If we can't load the shard for some reason, assume not seen
        false
    }

    /// Insert an ID into the tracker
    pub fn insert(&mut self, id: usize) {
        // Add to bloom filter
        self.bloom.set(&id);

        // Determine which shard
        let shard_id = self.shard_id_for(&id);

        // Get or load shard and insert
        if let Ok(shard) = self.get_shard_mut(shard_id) {
            if shard.seen.insert(id) {
                shard.dirty = true;
                self.total_seen_count += 1;
            }
        }
    }

    /// Get the number of items tracked
    pub fn len(&self) -> usize {
        self.total_seen_count
    }

    pub fn bloom_capacity(&self) -> usize {
        self.bloom_capacity
    }

    pub fn estimated_false_positive_rate(&self) -> f64 {
        estimate_fp_rate(self.bloom_capacity, self.total_seen_count, 0.01)
    }

    pub fn estimated_false_positive_rate_for_capacity(&self, capacity: usize) -> f64 {
        estimate_fp_rate(capacity, self.total_seen_count, 0.01)
    }

    pub fn buffered_total(&self) -> usize {
        self.buffered_total
    }

    pub fn stats_snapshot(&self) -> TrackerStats {
        let mut stats = self.stats.clone();
        stats.buffered_total = self.buffered_total;
        stats.max_buffered_per_shard = self
            .shard_buffers
            .values()
            .map(|v| v.len())
            .max()
            .unwrap_or(0);
        stats
    }

    /// Batch check: bloom negatives return immediately as new; bloom positives are buffered
    /// and resolved on threshold breach or when flush=true. Only one shard buffer is drained
    /// per call to keep work incremental.
    pub fn check_batch(&mut self, ids: &[usize], flush: bool) -> Result<BatchResult, FoldError> {
        let mut result = BatchResult::default();

        for id in ids {
            let shard_id = self.shard_id_for(id);
            if !self.bloom.check(id) {
                // Definite miss: set bloom, return as new, and buffer for persistence.
                self.bloom.set(id);
                self.enqueue_buffer(shard_id, *id);
                self.already_emitted.insert(*id);
                result.new.push(*id);
            } else {
                self.enqueue_buffer(shard_id, *id);
            }
        }

        if self.should_flush_threshold() {
            self.flush_one(FlushReason::Threshold, &mut result)?;
        }

        if flush {
            self.flush_one(FlushReason::Explicit, &mut result)?;
        }

        Ok(result)
    }

    pub fn flush_pending(&mut self) -> Result<BatchResult, FoldError> {
        self.check_batch(&[], true)
    }

    pub fn rebuild_bloom(&mut self, new_capacity: usize) -> Result<f64, FoldError> {
        if new_capacity <= self.bloom_capacity {
            return Ok(self.estimated_false_positive_rate());
        }

        // Flush loaded shards so disk contains current state
        self.flush()?;

        let false_positive_rate = 0.01;
        let mut new_bloom = Bloom::new_for_fp_rate(new_capacity, false_positive_rate);

        let mut loaded_ids = HashSet::new();
        for shard in &self.loaded_shards {
            loaded_ids.insert(shard.id);
            for id in shard.seen.iter() {
                new_bloom.set(id);
            }
        }

        if self.shard_dir.exists() {
            for entry in fs::read_dir(&self.shard_dir).map_err(FoldError::Io)? {
                let entry = entry.map_err(FoldError::Io)?;
                if entry.path().is_file() {
                    if let Some(name) = entry.file_name().to_str() {
                        if name.starts_with("shard_") && name.ends_with(".bin") {
                            if let Some(id) = name
                                .trim_start_matches("shard_")
                                .trim_end_matches(".bin")
                                .parse::<usize>()
                                .ok()
                            {
                                if loaded_ids.contains(&id) {
                                    continue;
                                }
                            }
                            let shard = Shard::load_from_disk(&entry.path())?;
                            for id in shard.seen.iter() {
                                new_bloom.set(id);
                            }
                        }
                    }
                }
            }
        }

        self.bloom = new_bloom;
        self.bloom_capacity = new_capacity;

        Ok(self.estimated_false_positive_rate())
    }

    /// Current cap on how many shards can be resident simultaneously.
    pub fn max_shards_in_memory(&self) -> usize {
        self.max_shards_in_memory
    }

    /// Reduce the shard residency limit and evict least-recently-used shards to match.
    /// Returns the number of shards evicted.
    pub fn shrink_shards_in_memory(&mut self, new_limit: usize) -> usize {
        if new_limit == 0 {
            return 0;
        }

        let target = new_limit.min(self.num_shards).max(1);
        if target >= self.max_shards_in_memory {
            self.max_shards_in_memory = target;
            return 0;
        }

        self.max_shards_in_memory = target;
        let mut evicted = 0;
        while self.loaded_shards.len() > self.max_shards_in_memory {
            let mut shard = self.loaded_shards.remove(0);
            let _ = shard.save_to_disk(&self.shard_dir);
            evicted += 1;
        }

        evicted
    }

    /// Increase the shard residency limit (up to the original ceiling). Returns the increment applied.
    pub fn grow_shards_in_memory(&mut self, new_limit: usize) -> usize {
        let target = new_limit.min(self.max_shards_ceiling).max(1);
        if target <= self.max_shards_in_memory {
            return 0;
        }

        let increment = target - self.max_shards_in_memory;
        self.max_shards_in_memory = target;
        increment
    }

    pub fn max_shards_ceiling(&self) -> usize {
        self.max_shards_ceiling
    }

    /// Check if the tracker is empty
    pub fn is_empty(&self) -> bool {
        self.total_seen_count == 0
    }

    /// Flush all dirty shards to disk
    pub fn flush(&mut self) -> Result<(), FoldError> {
        while self.buffered_total > 0 {
            let mut dummy = BatchResult::default();
            self.flush_one(FlushReason::Explicit, &mut dummy)?;
        }
        Ok(())
    }
}

fn estimate_fp_rate(capacity: usize, items: usize, target_fp: f64) -> f64 {
    if capacity == 0 {
        return 1.0;
    }
    if items == 0 {
        return 0.0;
    }

    let bits_per_item = (-target_fp.ln()) / (std::f64::consts::LN_2.powi(2));
    let m = bits_per_item * capacity as f64;
    let k = (m / capacity as f64) * std::f64::consts::LN_2;

    let exponent = (-k * items as f64 / m).exp();
    let fp_rate = (1.0 - exponent).powf(k);
    fp_rate.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn test_seen_tracker_basic() {
        let mut tracker = SeenTracker::new(100);

        assert!(!tracker.contains(&1));
        assert!(!tracker.contains(&2));

        tracker.insert(1);
        assert!(tracker.contains(&1));
        assert!(!tracker.contains(&2));

        tracker.insert(2);
        assert!(tracker.contains(&1));
        assert!(tracker.contains(&2));
        assert!(!tracker.contains(&3));
    }

    #[test]
    fn test_seen_tracker_len() {
        let mut tracker = SeenTracker::new(100);
        assert_eq!(tracker.len(), 0);
        assert!(tracker.is_empty());

        tracker.insert(1);
        assert_eq!(tracker.len(), 1);
        assert!(!tracker.is_empty());

        tracker.insert(2);
        assert_eq!(tracker.len(), 2);

        // Duplicate insert shouldn't increase count
        tracker.insert(1);
        assert_eq!(tracker.len(), 2);
    }

    #[test]
    fn test_seen_tracker_sharding() {
        let test_dir = PathBuf::from("./test_seen_sharding");
        let _ = fs::remove_dir_all(&test_dir);

        let mut tracker = SeenTracker::with_config(1000, 4, 2);
        tracker.shard_dir = test_dir.clone();
        let _ = fs::create_dir_all(&test_dir);

        // Insert items across multiple shards
        for i in 0..100 {
            tracker.insert(i);
        }

        // Verify all items are found
        for i in 0..100 {
            assert!(tracker.contains(&i), "Item {} should be found", i);
        }

        assert_eq!(tracker.len(), 100);

        // Cleanup
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_seen_tracker_persists_evicted_shards() {
        let test_dir = PathBuf::from("./test_seen_tracker");
        let _ = fs::remove_dir_all(&test_dir);

        // Allow only 2 shards to stay resident so eviction kicks in.
        let mut tracker = SeenTracker::with_config(1000, 8, 2);
        tracker.shard_dir = test_dir.clone();
        let _ = fs::create_dir_all(&test_dir);

        // Insert many items spread across shards to force evictions to disk.
        for i in 0..500 {
            tracker.insert(i);
        }

        // Further reduce the cap to guarantee additional evictions.
        tracker.shrink_shards_in_memory(1);

        // Verify shard files exist because evicted shards are persisted.
        let shard_files: Vec<_> = fs::read_dir(&test_dir).unwrap().collect();
        assert!(
            !shard_files.is_empty(),
            "eviction should persist shards to disk"
        );

        // Verify an early item (likely from an evicted shard) can still be found.
        assert!(
            tracker.contains(&0),
            "Evicted shard should reload from disk"
        );

        // Cleanup
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_bloom_filter_false_negatives() {
        // Bloom filters should never have false negatives
        let mut tracker = SeenTracker::new(1000);

        for i in 0..100 {
            tracker.insert(i);
        }

        for i in 0..100 {
            assert!(tracker.contains(&i), "False negative for {}", i);
        }
    }

    #[test]
    fn test_bloom_rebuild_increases_capacity_and_reduces_fp() {
        let base = std::env::temp_dir().join(format!("seen_tracker_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);

        let mut tracker = SeenTracker::with_path(
            base.to_str().unwrap(),
            1_000, // intentionally small to force higher FP
            8,
            4,
        );

        for i in 0..10_000 {
            tracker.insert(i);
        }

        let fp_before = tracker.estimated_false_positive_rate();
        let capacity_before = tracker.bloom_capacity();

        let fp_after = tracker
            .rebuild_bloom(capacity_before * 4)
            .expect("rebuild succeeds");

        assert!(
            tracker.bloom_capacity() > capacity_before,
            "capacity should grow"
        );
        assert!(
            fp_after <= fp_before,
            "false positive rate should not increase after rebuild (before {:.4}, after {:.4})",
            fp_before,
            fp_after
        );

        // Spot-check membership survived rebuild
        assert!(tracker.contains(&1234));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn batch_check_buffers_and_flushes_once() {
        let mut tracker = SeenTracker::with_config(100, 4, 2);
        let ids = vec![1usize, 2, 3];

        let result = tracker.check_batch(&ids, false).unwrap();
        assert_eq!(result.new.len(), 3);
        assert_eq!(result.seen.len(), 0);
        assert_eq!(tracker.buffered_total(), 3);
        assert_eq!(tracker.len(), 0, "deferred inserts should not count yet");

        let flush_result = tracker.flush_pending().unwrap();
        assert!(
            flush_result.new.is_empty(),
            "flush should not re-emit already returned items"
        );
        assert!(flush_result.seen.is_empty());

        if tracker.buffered_total() > 0 {
            let second_flush = tracker.flush_pending().unwrap();
            assert!(second_flush.new.is_empty());
            assert!(second_flush.seen.is_empty());
        }
        assert_eq!(tracker.buffered_total(), 0);
        assert_eq!(tracker.stats_snapshot().flushed_ids, 3);
        assert_eq!(tracker.len(), 3, "flush should persist buffered inserts");

        // Subsequent checks should classify as seen
        let result_again = tracker.check_batch(&ids, true).unwrap();
        assert!(result_again.new.is_empty());
        let mut seen_total = result_again.seen.len();
        while tracker.buffered_total() > 0 {
            let flush_seen = tracker.flush_pending().unwrap();
            seen_total += flush_seen.seen.len();
        }
        assert_eq!(seen_total, 3);
    }

    #[test]
    fn threshold_flushes_one_shard_at_a_time() {
        let mut tracker = SeenTracker::with_config(100, 8, 2);
        tracker.per_shard_flush_threshold = 1;
        tracker.global_buffer_limit = 1;

        let res = tracker.check_batch(&[42], false).unwrap();
        assert_eq!(res.new, vec![42]);
        assert_eq!(
            tracker.buffered_total(),
            0,
            "threshold flush should drain immediately"
        );
        assert!(
            tracker.contains(&42),
            "item should be persisted after auto-flush"
        );
    }
}
