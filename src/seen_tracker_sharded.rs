use crate::FoldError;
use bloomfilter::Bloom;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[derive(Default, Debug)]
struct Shard {
    id: usize,
    seen: HashMap<usize, ()>,
    dirty: bool,
}

impl Shard {
    fn new(id: usize) -> Self {
        Self {
            id,
            seen: HashMap::new(),
            dirty: false,
        }
    }

    fn load_from_disk(path: &Path) -> Result<Self, FoldError> {
        let data = fs::read(path).map_err(FoldError::Io)?;
        let seen: HashMap<usize, ()> = bincode::decode_from_slice(&data, bincode::config::standard())
            .map_err(|e| FoldError::Deserialization(Box::new(e)))?
            .0;

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

        fs::create_dir_all(dir).map_err(FoldError::Io)?;

        let path = dir.join(format!("shard_{:08}.bin", self.id));
        let data =
            bincode::encode_to_vec(&self.seen, bincode::config::standard()).map_err(|e| {
                FoldError::Serialization(Box::new(e))
            })?;

        fs::write(path, data).map_err(FoldError::Io)?;
        self.dirty = false;
        Ok(())
    }
}

/// Legacy sharded tracker: Bloom filter + LRU-managed shards backed by disk.
/// Keeps a limited number of shards resident; evicted shards are persisted.
pub struct ShardedSeenTracker {
    bloom: Bloom<usize>,
    bloom_capacity: usize,

    num_shards: usize,
    max_shards_in_memory: usize,

    loaded_shards: Vec<Shard>, // LRU: most recently used at end
    shard_dir: PathBuf,

    total_seen_count: usize,
}

impl ShardedSeenTracker {
    /// Create a new tracker with default settings (fresh start).
    pub fn new(expected_items: usize) -> Self {
        let bloom_capacity = expected_items.max(1_000_000);
        let num_shards = 64;
        let max_shards_in_memory = num_shards;
        Self::with_config(bloom_capacity, num_shards, max_shards_in_memory)
    }

    /// Create with explicit sizing for bloom/shards.
    pub fn with_config(
        bloom_capacity: usize,
        num_shards: usize,
        max_shards_in_memory: usize,
    ) -> Self {
        Self::with_path("./fold_state/seen_shards", bloom_capacity, num_shards, max_shards_in_memory)
    }

    /// Create with explicit path (used by benches/tests to isolate state).
    pub fn with_path(
        path: &str,
        bloom_capacity: usize,
        num_shards: usize,
        max_shards_in_memory: usize,
    ) -> Self {
        let false_positive_rate = 0.01;
        let bloom = Bloom::new_for_fp_rate(bloom_capacity, false_positive_rate);
        let shard_dir = PathBuf::from(path);

        if !shard_dir.exists() {
            let _ = fs::create_dir_all(&shard_dir);
        }

        let mut tracker = Self {
            bloom,
            bloom_capacity,
            num_shards,
            max_shards_in_memory,
            loaded_shards: Vec::new(),
            shard_dir,
            total_seen_count: 0,
        };

        // Rebuild bloom/count from existing shards if present.
        if let Ok(entries) = fs::read_dir(&tracker.shard_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Ok(mut shard) = Shard::load_from_disk(&path) {
                        for id in shard.seen.keys() {
                            tracker.bloom.set(id);
                        }
                        tracker.total_seen_count =
                            tracker.total_seen_count.saturating_add(shard.seen.len());
                        shard.dirty = false;
                        tracker.loaded_shards.push(shard);
                    }
                }
            }
        }

        tracker
    }

    fn shard_id_for(&self, id: &usize) -> usize {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        (hasher.finish() as usize) % self.num_shards
    }

    fn get_shard_mut(&mut self, shard_id: usize) -> Result<&mut Shard, FoldError> {
        if let Some(pos) = self.loaded_shards.iter().position(|s| s.id == shard_id) {
            let shard = self.loaded_shards.remove(pos);
            self.loaded_shards.push(shard);
            return Ok(self.loaded_shards.last_mut().unwrap());
        }

        let shard_path = self.shard_dir.join(format!("shard_{:08}.bin", shard_id));
        let shard = if shard_path.exists() {
            Shard::load_from_disk(&shard_path)?
        } else {
            Shard::new(shard_id)
        };

        if self.loaded_shards.len() >= self.max_shards_in_memory {
            let mut lru = self.loaded_shards.remove(0);
            lru.save_to_disk(&self.shard_dir)?;
        }

        self.loaded_shards.push(shard);
        Ok(self.loaded_shards.last_mut().unwrap())
    }

    /// Check if an ID has been seen before.
    pub fn contains(&mut self, id: &usize) -> bool {
        if !self.bloom.check(id) {
            return false;
        }

        let shard_id = self.shard_id_for(id);
        if let Ok(shard) = self.get_shard_mut(shard_id) {
            return shard.seen.contains_key(id);
        }
        false
    }

    /// Insert an ID.
    pub fn insert(&mut self, id: usize) {
        self.bloom.set(&id);
        let shard_id = self.shard_id_for(&id);
        if let Ok(shard) = self.get_shard_mut(shard_id) {
            if shard.seen.insert(id, ()).is_none() {
                shard.dirty = true;
                self.total_seen_count = self.total_seen_count.saturating_add(1);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.total_seen_count
    }

    pub fn is_empty(&self) -> bool {
        self.total_seen_count == 0
    }

    pub fn flush(&mut self) -> Result<(), FoldError> {
        for shard in &mut self.loaded_shards {
            shard.save_to_disk(&self.shard_dir)?;
        }
        Ok(())
    }

    pub fn bloom_capacity(&self) -> usize {
        self.bloom_capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn basic_ops() {
        let mut tracker = ShardedSeenTracker::with_config(1000, 4, 4);
        assert!(tracker.is_empty());
        tracker.insert(1);
        assert!(tracker.contains(&1));
        assert!(!tracker.contains(&2));
        tracker.insert(2);
        assert_eq!(tracker.len(), 2);
    }

    #[test]
    fn persists_on_flush() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("shards");
        let path_str = path.to_str().unwrap();
        let mut tracker = ShardedSeenTracker::with_path(path_str, 1000, 4, 2);
        for i in 0..200 {
            tracker.insert(i);
        }
        tracker.flush().unwrap();
        drop(tracker);

        let mut tracker = ShardedSeenTracker::with_path(path_str, 1000, 4, 2);
        for i in 0..200 {
            assert!(tracker.contains(&i));
        }
    }

    #[test]
    fn evicts_and_reloads() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("shards");
        let path_str = path.to_str().unwrap();
        let mut tracker = ShardedSeenTracker::with_path(path_str, 1000, 8, 2);
        for i in 0..10_000 {
            tracker.insert(i);
        }
        tracker.flush().unwrap();
        for i in (0..10_000).step_by(997) {
            assert!(tracker.contains(&i));
        }
        let entries: Vec<_> = fs::read_dir(path).unwrap().collect();
        assert!(!entries.is_empty());
    }
}
