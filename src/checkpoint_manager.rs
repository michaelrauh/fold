use crate::{disk_backed_queue::DiskBackedQueue, interner::Interner, memory_config::MemoryConfig, seen_tracker::SeenTracker, FoldError};
use std::fs;
use std::path::Path;

/// Manages checkpoint save/load operations for the fold system
pub struct CheckpointManager {
    checkpoint_dir: String,
    temp_checkpoint_dir: String,
    results_temp: String,
    results_path: String,
}

impl CheckpointManager {
    /// Create a new CheckpointManager with default paths
    pub fn new() -> Self {
        Self {
            checkpoint_dir: "./fold_state/checkpoint".to_string(),
            temp_checkpoint_dir: "./fold_state/checkpoint_temp".to_string(),
            results_temp: "./fold_state/results_temp".to_string(),
            results_path: "./fold_state/results".to_string(),
        }
    }
    
    /// Create a new CheckpointManager with custom base directory
    pub fn with_base_dir(base_dir: &Path) -> Self {
        Self {
            checkpoint_dir: base_dir.join("checkpoint").to_string_lossy().to_string(),
            temp_checkpoint_dir: base_dir.join("checkpoint_temp").to_string_lossy().to_string(),
            results_temp: base_dir.join("results_temp").to_string_lossy().to_string(),
            results_path: base_dir.join("results").to_string_lossy().to_string(),
        }
    }
    
    /// Save a checkpoint atomically
    /// Note: bloom filter and seen set are NOT saved - they are reconstructed from results on load
    pub fn save(
        &self,
        interner: &Interner,
        results_queue: &mut DiskBackedQueue,
    ) -> Result<(), FoldError> {
        // Remove temp dir if it exists from a previous failed save
        if Path::new(&self.temp_checkpoint_dir).exists() {
            fs::remove_dir_all(&self.temp_checkpoint_dir).map_err(|e| FoldError::Io(e))?;
        }
        
        fs::create_dir_all(&self.temp_checkpoint_dir).map_err(|e| FoldError::Io(e))?;
        
        // Flush all results to disk
        results_queue.flush()?;
        
        // Save interner to temp location
        let interner_bytes = bincode::encode_to_vec(interner, bincode::config::standard())?;
        fs::write(format!("{}/interner.bin", self.temp_checkpoint_dir), interner_bytes)
            .map_err(|e| FoldError::Io(e))?;
        
        // Copy results queue directory to temp location
        let results_path = results_queue.base_path();
        let temp_results_backup = format!("{}/results_backup", self.temp_checkpoint_dir);
        if Path::new(&results_path).exists() {
            copy_dir_all(&results_path, &temp_results_backup)?;
        }
        
        // Atomically swap: remove old checkpoint, rename temp to checkpoint
        if Path::new(&self.checkpoint_dir).exists() {
            fs::remove_dir_all(&self.checkpoint_dir).map_err(|e| FoldError::Io(e))?;
        }
        fs::rename(&self.temp_checkpoint_dir, &self.checkpoint_dir).map_err(|e| FoldError::Io(e))?;
        
        println!("[fold] Checkpoint saved atomically");
        
        Ok(())
    }
    
    /// Load a checkpoint if it exists
    /// Reconstructs bloom filter and seen set from all results
    /// Uses provided memory configuration for optimal sizing
    pub fn load(&self, memory_config: &MemoryConfig) -> Result<Option<(Interner, DiskBackedQueue, SeenTracker)>, FoldError> {
        if !Path::new(&format!("{}/interner.bin", self.checkpoint_dir)).exists() {
            return Ok(None);
        }
        
        println!("[fold] Loading checkpoint...");
        
        // Load interner
        let interner_bytes = fs::read(format!("{}/interner.bin", self.checkpoint_dir))
            .map_err(|e| FoldError::Io(e))?;
        let (interner, _): (Interner, usize) = bincode::decode_from_slice(&interner_bytes, bincode::config::standard())?;
        
        // Three-queue strategy:
        // 1. Checkpoint backup (preserved, read-only)
        // 2. Temporary copy (consumed to rebuild state)
        // 3. New active queue (being built)
        
        let results_backup = format!("{}/results_backup", self.checkpoint_dir);
        
        // Remove temp if it exists from a previous failed load
        if Path::new(&self.results_temp).exists() {
            fs::remove_dir_all(&self.results_temp).map_err(|e| FoldError::Io(e))?;
        }
        
        // Copy checkpoint backup to temporary consumable location
        if Path::new(&results_backup).exists() {
            copy_dir_all(&results_backup, &self.results_temp)?;
        } else {
            fs::create_dir_all(&self.results_temp).map_err(|e| FoldError::Io(e))?;
        }
        
        // Create temporary queue to consume
        let mut temp_queue = DiskBackedQueue::new_from_path(&self.results_temp, memory_config.queue_buffer_size)?;
        let total_items = temp_queue.len();
        
        println!("[fold] Reconstructing bloom filter and seen set from {} results...", total_items);
        
        // Use memory configuration
        let bloom_capacity = memory_config.bloom_capacity;
        let num_shards = memory_config.num_shards;
        let max_shards_in_memory = memory_config.max_shards_in_memory;
        
        println!("[fold] Tracker config: bloom_capacity={}, num_shards={}, max_in_memory={}", 
                 bloom_capacity, num_shards, max_shards_in_memory);
        
        // Create tracker with calculated configuration
        let mut tracker = SeenTracker::with_config(bloom_capacity, num_shards, max_shards_in_memory);
        
        // Delete current results if exists
        if Path::new(&self.results_path).exists() {
            fs::remove_dir_all(&self.results_path).map_err(|e| FoldError::Io(e))?;
        }
        
        // Create new active results queue
        let mut new_results = DiskBackedQueue::new_from_path(&self.results_path, memory_config.queue_buffer_size)?;
        
        // Consume temp queue: rebuild bloom filter and seen set from ALL results
        let mut consumed = 0;
        while let Some(ortho) = temp_queue.pop()? {
            let ortho_id = ortho.id();
            tracker.insert(ortho_id);
            new_results.push(ortho)?;
            
            consumed += 1;
            if consumed % 10000 == 0 {
                println!("[fold] Consumed {}/{} results...", consumed, total_items);
            }
        }
        
        // Cleanup temp directory
        if Path::new(&self.results_temp).exists() {
            fs::remove_dir_all(&self.results_temp).map_err(|e| FoldError::Io(e))?;
        }
        
        println!("[fold] Checkpoint loaded - interner version: {}, results: {}, seen: {}", 
                 interner.version(), new_results.len(), tracker.len());
        
        Ok(Some((interner, new_results, tracker)))
    }
}

impl Default for CheckpointManager {
    fn default() -> Self {
        Self::new()
    }
}

fn copy_dir_all(src: &str, dst: &str) -> Result<(), FoldError> {
    fs::create_dir_all(dst).map_err(|e| FoldError::Io(e))?;
    
    for entry in fs::read_dir(src).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let path = entry.path();
        let dest_path = Path::new(dst).join(entry.file_name());
        
        if path.is_dir() {
            copy_dir_all(path.to_str().unwrap(), dest_path.to_str().unwrap())?;
        } else {
            fs::copy(&path, &dest_path).map_err(|e| FoldError::Io(e))?;
        }
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;

    #[test]
    fn test_save_and_load_checkpoint() {
        let temp_dir = tempfile::tempdir().unwrap();
        let fold_state = temp_dir.path().join("fold_state");
        fs::create_dir_all(&fold_state).unwrap();
        
        let manager = CheckpointManager::with_base_dir(&fold_state);
        let interner = Interner::from_text("hello world");
        let results_path = fold_state.join("results");
        let mut results_queue = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), 10).unwrap();
        
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        let id1 = ortho1.id();
        let id2 = ortho2.id();
        
        results_queue.push(ortho1).unwrap();
        results_queue.push(ortho2).unwrap();
        
        manager.save(&interner, &mut results_queue).unwrap();
        
        // Verify checkpoint files exist
        let checkpoint_interner = fold_state.join("checkpoint/interner.bin");
        let checkpoint_results = fold_state.join("checkpoint/results_backup");
        assert!(checkpoint_interner.exists());
        assert!(checkpoint_results.exists());
        
        // LOAD PHASE - simulate restart
        let memory_config = crate::memory_config::MemoryConfig::default_config();
        let result = manager.load(&memory_config).unwrap();
        assert!(result.is_some());
        
        let (loaded_interner, loaded_results, mut loaded_tracker) = result.unwrap();
        
        assert_eq!(loaded_interner.version(), interner.version());
        assert_eq!(loaded_results.len(), 2, "Should have 2 results from checkpoint");
        assert_eq!(loaded_tracker.len(), 2, "Tracker should have 2 IDs reconstructed from results");
        assert!(loaded_tracker.contains(&id1));
        assert!(loaded_tracker.contains(&id2));
    }

    #[test]
    fn test_load_nonexistent_checkpoint() {
        let temp_dir = tempfile::tempdir().unwrap();
        let fold_state = temp_dir.path().join("fold_state");
        
        let manager = CheckpointManager::with_base_dir(&fold_state);
        let memory_config = crate::memory_config::MemoryConfig::default_config();
        let result = manager.load(&memory_config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_checkpoint_rehydrates_bloom_from_all_results() {
        let temp_dir = tempfile::tempdir().unwrap();
        let fold_state = temp_dir.path().join("fold_state");
        fs::create_dir_all(&fold_state).unwrap();
        
        let manager = CheckpointManager::with_base_dir(&fold_state);
        let interner = Interner::from_text("test");
        let results_path = fold_state.join("results");
        let mut results_queue = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), 10).unwrap();
        
        // Add three orthos to results (bloom/seen will be reconstructed from these)
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        let ortho3 = Ortho::new(3);
        let id1 = ortho1.id();
        let id2 = ortho2.id();
        let id3 = ortho3.id();
        
        results_queue.push(ortho1).unwrap();
        results_queue.push(ortho2).unwrap();
        results_queue.push(ortho3).unwrap();
        
        // Save checkpoint (no tracker - reconstructed on load)
        manager.save(&interner, &mut results_queue).unwrap();
        
        // Load and verify bloom/seen are reconstructed from ALL results
        let memory_config = crate::memory_config::MemoryConfig::default_config();
        let result = manager.load(&memory_config);
        assert!(result.is_ok(), "Load should succeed: {:?}", result.err());
        let loaded = result.unwrap();
        assert!(loaded.is_some(), "Checkpoint should exist");
        
        let (_int, _res, mut loaded_tracker) = loaded.unwrap();
        
        // All three should be in the tracker (reconstructed from all results)
        assert_eq!(loaded_tracker.len(), 3, "Tracker should have 3 IDs reconstructed from all results");
        assert!(loaded_tracker.contains(&id1));
        assert!(loaded_tracker.contains(&id2));
        assert!(loaded_tracker.contains(&id3));
    }
}
