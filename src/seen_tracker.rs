use crate::FoldError;
use fixedbitset::FixedBitSet;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Write, Read};
use std::path::PathBuf;

const BLOOM_SIZE: usize = 16_000_000_000; // 16 billion bits = ~1.9 GB (increased from 4B for lower false positive rate)
const SHARD_COUNT: usize = 256; // Number of disk shards
const MEMORY_CACHE_SIZE: usize = 100_000; // Keep recent IDs in memory per shard (was 10k, increased for better cache hit rate)

/// A probabilistic set that uses a bloom filter backed by sharded disk storage
pub struct SeenTracker {
    /// Bloom filter for fast probabilistic membership check
    bloom: FixedBitSet,
    /// Directory for disk shards
    shard_dir: PathBuf,
    /// In-memory cache for recently seen IDs (per shard)
    memory_cache: Vec<HashSet<usize>>,
    /// Dirty flags for shards that need to be written
    dirty_shards: Vec<bool>,
    /// Statistics
    bloom_hits: usize,      // Bloom filter said "definitely not"
    bloom_misses: usize,    // Bloom filter said "maybe" - had to check disk/memory
    disk_checks: usize,     // Actually had to read from disk
}

impl SeenTracker {
    pub fn new() -> Result<Self, FoldError> {
        let state_dir = std::env::var("FOLD_STATE_DIR")
            .unwrap_or_else(|_| "./fold_state".to_string());
        let shard_dir = PathBuf::from(&state_dir).join("seen_shards");
        Self::new_with_dir(shard_dir)
    }

    /// Create a new tracker with a specific directory (useful for tests)
    pub fn new_with_dir(shard_dir: PathBuf) -> Result<Self, FoldError> {
        // Create directory if it doesn't exist
        std::fs::create_dir_all(&shard_dir)
            .map_err(|e| FoldError::Io(e))?;

        let mut memory_cache = Vec::with_capacity(SHARD_COUNT);
        let mut dirty_shards = Vec::with_capacity(SHARD_COUNT);
        
        for _ in 0..SHARD_COUNT {
            memory_cache.push(HashSet::new());
            dirty_shards.push(false);
        }

        Ok(SeenTracker {
            bloom: FixedBitSet::with_capacity(BLOOM_SIZE),
            shard_dir,
            memory_cache,
            dirty_shards,
            bloom_hits: 0,
            bloom_misses: 0,
            disk_checks: 0,
        })
    }

    /// Load existing state from disk
    pub fn load() -> Result<Self, FoldError> {
        let state_dir = std::env::var("FOLD_STATE_DIR")
            .unwrap_or_else(|_| "./fold_state".to_string());
        let shard_dir = PathBuf::from(&state_dir).join("seen_shards");
        
        if !shard_dir.exists() {
            return Self::new();
        }

        let mut tracker = Self::new()?;
        
        // Load bloom filter if it exists
        let bloom_path = shard_dir.join("bloom.bin");
        if bloom_path.exists() {
            let mut file = File::open(&bloom_path)
                .map_err(|e| FoldError::Io(e))?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .map_err(|e| FoldError::Io(e))?;
            
            // Deserialize bloom filter
            if buffer.len() == (BLOOM_SIZE + 7) / 8 {
                for (i, byte) in buffer.iter().enumerate() {
                    for bit in 0..8 {
                        let bit_index = i * 8 + bit;
                        if bit_index < BLOOM_SIZE && (byte & (1 << bit)) != 0 {
                            tracker.bloom.insert(bit_index);
                        }
                    }
                }
            }
        }

        Ok(tracker)
    }

    /// Save bloom filter to disk
    pub fn save(&mut self) -> Result<(), FoldError> {
        // Flush all dirty shards
        for shard_id in 0..SHARD_COUNT {
            if self.dirty_shards[shard_id] {
                self.flush_shard(shard_id)?;
            }
        }

        // Save bloom filter
        let bloom_path = self.shard_dir.join("bloom.bin");
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&bloom_path)
            .map_err(|e| FoldError::Io(e))?;

        // Serialize bloom filter to bytes
        let byte_count = (BLOOM_SIZE + 7) / 8;
        let mut buffer = vec![0u8; byte_count];
        
        for bit_index in self.bloom.ones() {
            let byte_index = bit_index / 8;
            let bit_offset = bit_index % 8;
            if byte_index < buffer.len() {
                buffer[byte_index] |= 1 << bit_offset;
            }
        }

        file.write_all(&buffer)
            .map_err(|e| FoldError::Io(e))?;

        Ok(())
    }

    /// Check if an ID has been seen before
    pub fn contains(&mut self, id: usize) -> Result<bool, FoldError> {
        // First check bloom filter (fast)
        let hash1 = self.hash1(id);
        let hash2 = self.hash2(id);
        let hash3 = self.hash3(id);

        if !self.bloom.contains(hash1) || !self.bloom.contains(hash2) || !self.bloom.contains(hash3) {
            // Definitely not seen - bloom filter says no
            self.bloom_hits += 1;
            return Ok(false);
        }

        // Bloom filter says "maybe" - must check actual storage
        self.bloom_misses += 1;

        // Check actual storage
        let shard_id = self.get_shard_id(id);
        
        // Check memory cache first
        if self.memory_cache[shard_id].contains(&id) {
            return Ok(true);
        }

        // Check disk
        self.disk_checks += 1;
        self.contains_on_disk(shard_id, id)
    }

    /// Insert an ID into the tracker
    pub fn insert(&mut self, id: usize) -> Result<(), FoldError> {
        // Add to bloom filter
        let hash1 = self.hash1(id);
        let hash2 = self.hash2(id);
        let hash3 = self.hash3(id);
        
        self.bloom.insert(hash1);
        self.bloom.insert(hash2);
        self.bloom.insert(hash3);

        // Add to memory cache
        let shard_id = self.get_shard_id(id);
        self.memory_cache[shard_id].insert(id);
        self.dirty_shards[shard_id] = true;

        // Flush shard if cache is too large
        if self.memory_cache[shard_id].len() >= MEMORY_CACHE_SIZE {
            self.flush_shard(shard_id)?;
        }

        Ok(())
    }

    /// Get the number of unique IDs tracked (approximate)
    pub fn len(&self) -> usize {
        // Count bits set in bloom filter / 3 (we use 3 hash functions)
        // This is an approximation
        self.bloom.count_ones(..) / 3
    }

    /// Get bloom filter statistics
    pub fn get_stats(&self) -> (usize, usize, usize) {
        (self.bloom_hits, self.bloom_misses, self.disk_checks)
    }



    // Hash functions for bloom filter
    fn hash1(&self, id: usize) -> usize {
        (id.wrapping_mul(2654435761)) % BLOOM_SIZE
    }

    fn hash2(&self, id: usize) -> usize {
        (id.wrapping_mul(2246822519).wrapping_add(1)) % BLOOM_SIZE
    }

    fn hash3(&self, id: usize) -> usize {
        (id.wrapping_mul(3266489917).wrapping_add(2)) % BLOOM_SIZE
    }

    fn get_shard_id(&self, id: usize) -> usize {
        id % SHARD_COUNT
    }

    fn shard_path(&self, shard_id: usize) -> PathBuf {
        self.shard_dir.join(format!("shard_{:03}.bin", shard_id))
    }

    fn contains_on_disk(&self, shard_id: usize, id: usize) -> Result<bool, FoldError> {
        let path = self.shard_path(shard_id);
        
        if !path.exists() {
            return Ok(false);
        }

        let file = File::open(&path)
            .map_err(|e| FoldError::Io(e))?;
        let mut reader = BufReader::new(file);

        // Read the shard file (sequence of usize values)
        let mut buffer = [0u8; 8];
        loop {
            match reader.read_exact(&mut buffer) {
                Ok(_) => {
                    let stored_id = usize::from_le_bytes(buffer);
                    if stored_id == id {
                        return Ok(true);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(FoldError::Io(e)),
            }
        }

        Ok(false)
    }

    fn flush_shard(&mut self, shard_id: usize) -> Result<(), FoldError> {
        if self.memory_cache[shard_id].is_empty() {
            self.dirty_shards[shard_id] = false;
            return Ok(());
        }

        let path = self.shard_path(shard_id);
        
        // Read existing IDs
        let mut all_ids = HashSet::new();
        if path.exists() {
            let file = File::open(&path)
                .map_err(|e| FoldError::Io(e))?;
            let mut reader = BufReader::new(file);
            let mut buffer = [0u8; 8];
            
            while reader.read_exact(&mut buffer).is_ok() {
                all_ids.insert(usize::from_le_bytes(buffer));
            }
        }

        // Merge with memory cache
        all_ids.extend(self.memory_cache[shard_id].iter().copied());

        // Write back to disk
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| FoldError::Io(e))?;
        
        let mut writer = BufWriter::new(file);
        
        for id in all_ids.iter() {
            writer.write_all(&id.to_le_bytes())
                .map_err(|e| FoldError::Io(e))?;
        }
        
        writer.flush()
            .map_err(|e| FoldError::Io(e))?;

        // Clear memory cache for this shard
        self.memory_cache[shard_id].clear();
        self.dirty_shards[shard_id] = false;

        Ok(())
    }
}

impl Drop for SeenTracker {
    fn drop(&mut self) {
        // Try to save state on drop
        let _ = self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn create_test_tracker() -> SeenTracker {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let temp_dir = std::env::temp_dir().join(format!("fold_seen_test_{}", id));
        SeenTracker::new_with_dir(temp_dir).unwrap()
    }

    #[test]
    fn test_basic_insert_and_contains() {
        let mut tracker = create_test_tracker();
        
        assert!(!tracker.contains(12345).unwrap());
        tracker.insert(12345).unwrap();
        assert!(tracker.contains(12345).unwrap());
    }

    #[test]
    fn test_multiple_inserts() {
        let mut tracker = create_test_tracker();
        
        for i in 0..1000 {
            tracker.insert(i).unwrap();
        }
        
        for i in 0..1000 {
            assert!(tracker.contains(i).unwrap());
        }
        
        assert!(!tracker.contains(1001).unwrap());
    }

    #[test]
    fn test_shard_distribution() {
        let mut tracker = create_test_tracker();
        
        // Insert many items to test shard flushing
        for i in 0..MEMORY_CACHE_SIZE * 2 {
            tracker.insert(i).unwrap();
        }
        
        // Verify all are present
        for i in 0..MEMORY_CACHE_SIZE * 2 {
            assert!(tracker.contains(i).unwrap());
        }
    }
}