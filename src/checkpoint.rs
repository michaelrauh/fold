use crate::error::FoldError;
use crate::interner::Interner;
use crate::ortho::Ortho;
use bincode::{Decode, Encode};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

/// Checkpoint state that can be saved and restored
#[derive(Debug)]
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
    /// Number of orthos processed when checkpoint was created
    pub processed_count: usize,
}

// Manual bincode implementation for Checkpoint
impl Encode for Checkpoint {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), bincode::error::EncodeError> {
        self.last_completed_file_index.encode(encoder)?;
        self.interner.encode(encoder)?;
        // Convert HashSet to Vec for encoding
        let seen_ids_vec: Vec<usize> = self.seen_ids.iter().copied().collect();
        seen_ids_vec.encode(encoder)?;
        self.optimal_ortho.encode(encoder)?;
        let frontier_vec: Vec<usize> = self.frontier.iter().copied().collect();
        frontier_vec.encode(encoder)?;
        // Convert HashMap to Vec of tuples
        let frontier_orthos_vec: Vec<(usize, Ortho)> = self.frontier_orthos_saved.iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        frontier_orthos_vec.encode(encoder)?;
        self.timestamp.encode(encoder)?;
        self.processed_count.encode(encoder)?;
        Ok(())
    }
}

impl<Context> Decode<Context> for Checkpoint {
    fn decode<D: bincode::de::Decoder>(decoder: &mut D) -> Result<Self, bincode::error::DecodeError> {
        let last_completed_file_index = Option::<usize>::decode(decoder)?;
        let interner = Option::<Interner>::decode(decoder)?;
        let seen_ids_vec = Vec::<usize>::decode(decoder)?;
        let seen_ids: HashSet<usize> = seen_ids_vec.into_iter().collect();
        let optimal_ortho = Option::<Ortho>::decode(decoder)?;
        let frontier_vec = Vec::<usize>::decode(decoder)?;
        let frontier: HashSet<usize> = frontier_vec.into_iter().collect();
        let frontier_orthos_vec = Vec::<(usize, Ortho)>::decode(decoder)?;
        let frontier_orthos_saved: HashMap<usize, Ortho> = frontier_orthos_vec.into_iter().collect();
        let timestamp = String::decode(decoder)?;
        let processed_count = usize::decode(decoder)?;
        
        Ok(Checkpoint {
            last_completed_file_index,
            interner,
            seen_ids,
            optimal_ortho,
            frontier,
            frontier_orthos_saved,
            timestamp,
            processed_count,
        })
    }
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
        processed_count: usize,
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
            processed_count,
        }
    }

    /// Save checkpoint to a file using bincode
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), FoldError> {
        let file = File::create(path).map_err(|e| FoldError::Io(e))?;
        let mut writer = BufWriter::new(file);
        let config = bincode::config::standard();
        bincode::encode_into_std_write(self, &mut writer, config)
            .map_err(|e| FoldError::Serialization(e.to_string()))?;
        Ok(())
    }

    /// Load checkpoint from a file using bincode
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, FoldError> {
        let file = File::open(path).map_err(|e| FoldError::Io(e))?;
        let mut reader = BufReader::new(file);
        let config = bincode::config::standard();
        let checkpoint = bincode::decode_from_std_read(&mut reader, config)
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
    /// Get the checkpoint filename (for use in naming)
    pub fn checkpoint_filename() -> &'static str {
        "checkpoint.bin"
    }
}

impl CheckpointManager {
    /// Create a new checkpoint manager for the given state directory
    pub fn new<P: AsRef<Path>>(state_dir: P) -> Result<Self, FoldError> {
        let checkpoint_dir = state_dir.as_ref().to_path_buf();
        
        // Ensure directory exists
        if !checkpoint_dir.exists() {
            fs::create_dir_all(&checkpoint_dir).map_err(|e| FoldError::Io(e))?;
        }
        
        let checkpoint_file = checkpoint_dir.join(Self::checkpoint_filename());
        
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
            0, // processed_count
        );
        assert_eq!(checkpoint.last_completed_file_index, Some(0));
        assert!(!checkpoint.timestamp.is_empty());
    }

    #[test]
    fn test_checkpoint_save_and_load() {
        let temp_dir = temp_dir_for_test("save_and_load");
        cleanup_temp_dir(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        
        let checkpoint_path = temp_dir.join("test_checkpoint.bin");
        
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
            0, // processed_count
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
        
        let checkpoint_path = temp_dir.join("test_checkpoint.bin");
        
        let ortho = Ortho::new(1);
        let checkpoint = Checkpoint::new(
            Some(0),
            None,
            HashSet::new(),
            Some(ortho.clone()),
            HashSet::new(),
            HashMap::new(),
            0, // processed_count
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
            0, // processed_count
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
            0, // processed_count
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
        
        let checkpoint_path = temp_dir.join("test_checkpoint.bin");
        
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
            0, // processed_count
        );
        
        checkpoint.save(&checkpoint_path).expect("Should save checkpoint");
        let loaded = Checkpoint::load(&checkpoint_path).expect("Should load checkpoint");
        
        assert_eq!(loaded.frontier_orthos_saved.len(), 2);
        assert!(loaded.frontier_orthos_saved.contains_key(&ortho1.id()));
        assert!(loaded.frontier_orthos_saved.contains_key(&ortho2.id()));
        
        cleanup_temp_dir(&temp_dir);
    }
}
