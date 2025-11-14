use bloomfilter::Bloom;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use crate::FoldError;

/// A single shard containing a portion of the seen IDs
struct Shard {
    id: usize,
    seen: HashMap<usize, ()>,
    dirty: bool, // true if modified since last disk write
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
        let data = fs::read(path).map_err(|e| FoldError::Io(e))?;
        let seen: HashMap<usize, ()> = bincode::decode_from_slice(&data, bincode::config::standard())
            .map_err(|e| FoldError::Deserialization(Box::new(e)))?
            .0;
        
        let file_name = path.file_name()
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
        let data = bincode::encode_to_vec(&self.seen, bincode::config::standard())
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
    
    // Sharding configuration
    num_shards: usize,
    max_shards_in_memory: usize,
    
    // In-memory shards (LRU order - most recently used at end)
    loaded_shards: Vec<Shard>,
    
    // Directory for disk-backed shards
    shard_dir: PathBuf,
    
    // Track total count across all shards
    total_seen_count: usize,
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
    pub fn with_config(bloom_capacity: usize, num_shards: usize, max_shards_in_memory: usize) -> Self {
        let false_positive_rate = 0.01; // 1% FPR
        let bloom = Bloom::new_for_fp_rate(bloom_capacity, false_positive_rate);
        
        let shard_dir = PathBuf::from("./fold_state/seen_shards");
        
        // Clear old shard files on initialization
        if shard_dir.exists() {
            let _ = fs::remove_dir_all(&shard_dir);
        }
        let _ = fs::create_dir_all(&shard_dir);
        
        Self {
            bloom,
            num_shards,
            max_shards_in_memory,
            loaded_shards: Vec::new(),
            shard_dir,
            total_seen_count: 0,
        }
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
            return Ok(self.loaded_shards.last_mut().unwrap());
        }
        
        // Need to load from disk or create new
        let shard_path = self.shard_dir.join(format!("shard_{:08}.bin", shard_id));
        let shard = if shard_path.exists() {
            Shard::load_from_disk(&shard_path)?
        } else {
            Shard::new(shard_id)
        };
        
        // If at capacity, evict least recently used (first in vec)
        if self.loaded_shards.len() >= self.max_shards_in_memory {
            let mut lru_shard = self.loaded_shards.remove(0);
            lru_shard.save_to_disk(&self.shard_dir)?;
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
            return shard.seen.contains_key(id);
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
            if !shard.seen.contains_key(&id) {
                shard.seen.insert(id, ());
                shard.dirty = true;
                self.total_seen_count += 1;
            }
        }
    }
    
    /// Get the number of items tracked
    pub fn len(&self) -> usize {
        self.total_seen_count
    }
    
    /// Check if the tracker is empty
    pub fn is_empty(&self) -> bool {
        self.total_seen_count == 0
    }
    
    /// Flush all dirty shards to disk
    pub fn flush(&mut self) -> Result<(), FoldError> {
        for shard in &mut self.loaded_shards {
            shard.save_to_disk(&self.shard_dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    fn test_seen_tracker_disk_persistence() {
        let test_dir = PathBuf::from("./test_seen_tracker");
        let _ = fs::remove_dir_all(&test_dir);
        
        let mut tracker = SeenTracker::with_config(1000, 4, 2);
        tracker.shard_dir = test_dir.clone();
        let _ = fs::create_dir_all(&test_dir);
        
        // Insert many items to force disk writes
        for i in 0..200 {
            tracker.insert(i);
        }
        
        // Flush to ensure everything is written
        tracker.flush().unwrap();
        
        // Verify shard files exist
        assert!(test_dir.exists());
        
        // Verify all items are still found
        for i in 0..200 {
            assert!(tracker.contains(&i), "Item {} should be found after flush", i);
        }
        
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
}
