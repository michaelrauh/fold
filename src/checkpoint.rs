use crate::error::FoldError;
use crate::interner::Interner;
use crate::ortho::Ortho;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

/// Checkpoint state that can be saved and restored
#[derive(Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Index of the last completed file (0-based)
    pub last_completed_file_index: Option<usize>,
    /// Serialized interner state
    pub interner: Option<Interner>,
    /// Set of seen ortho IDs
    pub seen_ids: HashSet<usize>,
    /// Optimal ortho found so far
    pub optimal_ortho: Option<Ortho>,
    /// Frontier ortho IDs
    pub frontier: HashSet<usize>,
    /// Frontier orthos saved for next iteration (ID -> Ortho)
    pub frontier_orthos_saved: HashMap<usize, Ortho>,
    /// Timestamp when checkpoint was created
    pub timestamp: String,
}

impl Checkpoint {
    /// Create a new checkpoint from current state
    pub fn new(
        last_completed_file_index: Option<usize>,
        interner: Option<Interner>,
        seen_ids: HashSet<usize>,
        optimal_ortho: Option<Ortho>,
        frontier: HashSet<usize>,
        frontier_orthos_saved: HashMap<usize, Ortho>,
    ) -> Self {
        let timestamp = chrono::Utc::now().to_rfc3339();
        Self {
            last_completed_file_index,
            interner,
            seen_ids,
            optimal_ortho,
            frontier,
            frontier_orthos_saved,
            timestamp,
        }
    }

    /// Save checkpoint to a file
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), FoldError> {
        let file = File::create(path).map_err(|e| FoldError::Io(e))?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self)
            .map_err(|e| FoldError::Serialization(e.to_string()))?;
        Ok(())
    }

    /// Load checkpoint from a file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, FoldError> {
        let file = File::open(path).map_err(|e| FoldError::Io(e))?;
        let reader = BufReader::new(file);
        let checkpoint = serde_json::from_reader(reader)
            .map_err(|e| FoldError::Deserialization(e.to_string()))?;
        Ok(checkpoint)
    }
}

/// Manage checkpoint files in a directory
pub struct CheckpointManager {
    checkpoint_dir: PathBuf,
    checkpoint_file: PathBuf,
}

impl CheckpointManager {
    /// Create a new checkpoint manager for the given state directory
    pub fn new<P: AsRef<Path>>(state_dir: P) -> Result<Self, FoldError> {
        let checkpoint_dir = state_dir.as_ref().to_path_buf();
        
        // Ensure directory exists
        if !checkpoint_dir.exists() {
            fs::create_dir_all(&checkpoint_dir).map_err(|e| FoldError::Io(e))?;
        }
        
        let checkpoint_file = checkpoint_dir.join("checkpoint.json");
        
        Ok(Self {
            checkpoint_dir,
            checkpoint_file,
        })
    }

    /// Check if a checkpoint exists
    pub fn checkpoint_exists(&self) -> bool {
        self.checkpoint_file.exists()
    }

    /// Save a checkpoint
    pub fn save_checkpoint(&self, checkpoint: &Checkpoint) -> Result<(), FoldError> {
        checkpoint.save(&self.checkpoint_file)
    }

    /// Load the checkpoint if it exists
    pub fn load_checkpoint(&self) -> Result<Option<Checkpoint>, FoldError> {
        if self.checkpoint_exists() {
            Ok(Some(Checkpoint::load(&self.checkpoint_file)?))
        } else {
            Ok(None)
        }
    }

    /// Delete the checkpoint file
    pub fn clear_checkpoint(&self) -> Result<(), FoldError> {
        if self.checkpoint_exists() {
            fs::remove_file(&self.checkpoint_file).map_err(|e| FoldError::Io(e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;
    use std::env;

    fn temp_dir_for_test(test_name: &str) -> PathBuf {
        let mut path = env::temp_dir();
        path.push(format!("fold_checkpoint_test_{}", test_name));
        path
    }

    fn cleanup_temp_dir(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn test_checkpoint_new() {
        let checkpoint = Checkpoint::new(
            Some(0),
            None,
            HashSet::new(),
            None,
            HashSet::new(),
            HashMap::new(),
        );
        assert_eq!(checkpoint.last_completed_file_index, Some(0));
        assert!(!checkpoint.timestamp.is_empty());
    }

    #[test]
    fn test_checkpoint_save_and_load() {
        let temp_dir = temp_dir_for_test("save_and_load");
        cleanup_temp_dir(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        
        let checkpoint_path = temp_dir.join("test_checkpoint.json");
        
        // Create checkpoint with some state
        let mut seen_ids = HashSet::new();
        seen_ids.insert(1);
        seen_ids.insert(2);
        
        let mut frontier = HashSet::new();
        frontier.insert(3);
        
        let checkpoint = Checkpoint::new(
            Some(0),
            None,
            seen_ids.clone(),
            None,
            frontier.clone(),
            HashMap::new(),
        );
        
        // Save
        checkpoint.save(&checkpoint_path).expect("Should save checkpoint");
        assert!(checkpoint_path.exists());
        
        // Load
        let loaded = Checkpoint::load(&checkpoint_path).expect("Should load checkpoint");
        assert_eq!(loaded.last_completed_file_index, Some(0));
        assert_eq!(loaded.seen_ids, seen_ids);
        assert_eq!(loaded.frontier, frontier);
        
        cleanup_temp_dir(&temp_dir);
    }

    #[test]
    fn test_checkpoint_with_ortho() {
        let temp_dir = temp_dir_for_test("with_ortho");
        cleanup_temp_dir(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        
        let checkpoint_path = temp_dir.join("test_checkpoint.json");
        
        let ortho = Ortho::new(1);
        let checkpoint = Checkpoint::new(
            Some(0),
            None,
            HashSet::new(),
            Some(ortho.clone()),
            HashSet::new(),
            HashMap::new(),
        );
        
        checkpoint.save(&checkpoint_path).expect("Should save checkpoint");
        let loaded = Checkpoint::load(&checkpoint_path).expect("Should load checkpoint");
        
        assert!(loaded.optimal_ortho.is_some());
        let loaded_ortho = loaded.optimal_ortho.unwrap();
        assert_eq!(loaded_ortho.id(), ortho.id());
        
        cleanup_temp_dir(&temp_dir);
    }

    #[test]
    fn test_checkpoint_manager_new() {
        let temp_dir = temp_dir_for_test("manager_new");
        cleanup_temp_dir(&temp_dir);
        
        let manager = CheckpointManager::new(&temp_dir).expect("Should create manager");
        assert!(temp_dir.exists());
        assert!(!manager.checkpoint_exists());
        
        cleanup_temp_dir(&temp_dir);
    }

    #[test]
    fn test_checkpoint_manager_save_and_load() {
        let temp_dir = temp_dir_for_test("manager_save_and_load");
        cleanup_temp_dir(&temp_dir);
        
        let manager = CheckpointManager::new(&temp_dir).expect("Should create manager");
        
        let checkpoint = Checkpoint::new(
            Some(5),
            None,
            HashSet::new(),
            None,
            HashSet::new(),
            HashMap::new(),
        );
        
        manager.save_checkpoint(&checkpoint).expect("Should save checkpoint");
        assert!(manager.checkpoint_exists());
        
        let loaded = manager.load_checkpoint().expect("Should load checkpoint");
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().last_completed_file_index, Some(5));
        
        cleanup_temp_dir(&temp_dir);
    }

    #[test]
    fn test_checkpoint_manager_clear() {
        let temp_dir = temp_dir_for_test("manager_clear");
        cleanup_temp_dir(&temp_dir);
        
        let manager = CheckpointManager::new(&temp_dir).expect("Should create manager");
        
        let checkpoint = Checkpoint::new(
            Some(0),
            None,
            HashSet::new(),
            None,
            HashSet::new(),
            HashMap::new(),
        );
        
        manager.save_checkpoint(&checkpoint).expect("Should save checkpoint");
        assert!(manager.checkpoint_exists());
        
        manager.clear_checkpoint().expect("Should clear checkpoint");
        assert!(!manager.checkpoint_exists());
        
        cleanup_temp_dir(&temp_dir);
    }

    #[test]
    fn test_checkpoint_with_frontier_orthos() {
        let temp_dir = temp_dir_for_test("with_frontier_orthos");
        cleanup_temp_dir(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        
        let checkpoint_path = temp_dir.join("test_checkpoint.json");
        
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        
        let mut frontier_orthos = HashMap::new();
        frontier_orthos.insert(ortho1.id(), ortho1.clone());
        frontier_orthos.insert(ortho2.id(), ortho2.clone());
        
        let checkpoint = Checkpoint::new(
            Some(0),
            None,
            HashSet::new(),
            None,
            HashSet::new(),
            frontier_orthos.clone(),
        );
        
        checkpoint.save(&checkpoint_path).expect("Should save checkpoint");
        let loaded = Checkpoint::load(&checkpoint_path).expect("Should load checkpoint");
        
        assert_eq!(loaded.frontier_orthos_saved.len(), 2);
        assert!(loaded.frontier_orthos_saved.contains_key(&ortho1.id()));
        assert!(loaded.frontier_orthos_saved.contains_key(&ortho2.id()));
        
        cleanup_temp_dir(&temp_dir);
    }
}
