use crate::ortho::Ortho;
use crate::FoldError;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Write, Read};
use std::path::PathBuf;

const MEMORY_THRESHOLD: usize = 1_000;

/// A queue that keeps items in memory up to MEMORY_THRESHOLD, then spills to disk
pub struct DiskQueue {
    /// In-memory buffer
    memory: VecDeque<Ortho>,
    /// Disk file for overflow items
    disk_file: Option<PathBuf>,
    /// Writer for appending to disk
    disk_writer: Option<BufWriter<File>>,
    /// Reader for reading from disk
    disk_reader: Option<BufReader<File>>,
    /// Number of items currently on disk
    disk_count: usize,
    /// Total items ever written to disk (for unique file naming)
    disk_generation: usize,
}

impl DiskQueue {
    pub fn new() -> Self {
        DiskQueue {
            memory: VecDeque::new(),
            disk_file: None,
            disk_writer: None,
            disk_reader: None,
            disk_count: 0,
            disk_generation: 0,
        }
    }

    /// Create a DiskQueue that writes to a persistent storage file (for ortho logging)
    pub fn new_persistent() -> Result<Self, FoldError> {
        let state_dir = std::env::var("FOLD_STATE_DIR")
            .unwrap_or_else(|_| "./fold_state".to_string());
        let storage_path = PathBuf::from(&state_dir).join("ortho_storage.bin");
        
        // Create directory if it doesn't exist
        if let Some(parent) = storage_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| FoldError::Io(e))?;
        }

        // Open file for appending
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&storage_path)
            .map_err(|e| FoldError::Io(e))?;

        let queue = DiskQueue {
            memory: VecDeque::new(),
            disk_file: Some(storage_path),
            disk_writer: Some(BufWriter::new(file)),
            disk_reader: None,
            disk_count: 0,
            disk_generation: 0,
        };

        // Always write to disk for persistent storage
        Ok(queue)
    }

    /// Push an item to the back of the queue
    pub fn push_back(&mut self, ortho: Ortho) -> Result<(), FoldError> {
        // If we're at capacity and have no disk file yet, create one
        if self.memory.len() >= MEMORY_THRESHOLD && self.disk_writer.is_none() {
            self.create_disk_file()?;
        }

        // If we have a disk file, write to it
        if let Some(ref mut writer) = self.disk_writer {
            let encoded = bincode::encode_to_vec(&ortho, bincode::config::standard())
                .map_err(|e| FoldError::Serialization(Box::new(e)))?;
            
            // Write length prefix then data
            let len = encoded.len() as u32;
            writer.write_all(&len.to_le_bytes())
                .map_err(|e| FoldError::Io(e))?;
            writer.write_all(&encoded)
                .map_err(|e| FoldError::Io(e))?;
            
            self.disk_count += 1;
        } else {
            // Still room in memory
            self.memory.push_back(ortho);
        }

        Ok(())
    }

    /// Pop an item from the front of the queue
    pub fn pop_front(&mut self) -> Result<Option<Ortho>, FoldError> {
        // First try memory
        if let Some(ortho) = self.memory.pop_front() {
            return Ok(Some(ortho));
        }

        // If memory is empty, try to load from disk
        if self.disk_count > 0 {
            self.load_from_disk()?;
            return Ok(self.memory.pop_front());
        }

        // Queue is empty
        Ok(None)
    }

    /// Check if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.memory.is_empty() && self.disk_count == 0
    }

    /// Get the total number of items in the queue
    pub fn len(&self) -> usize {
        self.memory.len() + self.disk_count
    }

    /// Get statistics about memory vs disk usage
    pub fn get_stats(&self) -> (usize, usize) {
        (self.memory.len(), self.disk_count)
    }

    /// Create a new disk file for spillover
    fn create_disk_file(&mut self) -> Result<(), FoldError> {
        let state_dir = std::env::var("FOLD_STATE_DIR")
            .unwrap_or_else(|_| "./fold_state".to_string());
        let disk_dir = PathBuf::from(&state_dir).join("queue_spill");
        
        // Create directory if it doesn't exist
        std::fs::create_dir_all(&disk_dir)
            .map_err(|e| FoldError::Io(e))?;

        // Create unique filename
        self.disk_generation += 1;
        let filename = format!("queue_spill_{}.bin", self.disk_generation);
        let path = disk_dir.join(filename);

        // Open file for writing
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|e| FoldError::Io(e))?;

        self.disk_file = Some(path);
        self.disk_writer = Some(BufWriter::new(file));
        self.disk_count = 0;

        Ok(())
    }

    /// Load a batch of items from disk into memory
    fn load_from_disk(&mut self) -> Result<(), FoldError> {
        // If we don't have a reader yet, create one
        if self.disk_reader.is_none() && self.disk_file.is_some() {
            // Flush and close writer
            if let Some(mut writer) = self.disk_writer.take() {
                writer.flush().map_err(|e| FoldError::Io(e))?;
            }

            // Open reader
            let file = File::open(self.disk_file.as_ref().unwrap())
                .map_err(|e| FoldError::Io(e))?;
            self.disk_reader = Some(BufReader::new(file));
        }

        // Read items from disk up to MEMORY_THRESHOLD / 2 (to reduce I/O frequency)
        let batch_size = MEMORY_THRESHOLD / 2;
        let reader = self.disk_reader.as_mut()
            .ok_or_else(|| FoldError::Queue("No disk reader".to_string()))?;

        for _ in 0..batch_size {
            if self.disk_count == 0 {
                break;
            }

            // Read length prefix
            let mut len_bytes = [0u8; 4];
            use std::io::Read;
            if reader.read_exact(&mut len_bytes).is_err() {
                break;
            }
            let len = u32::from_le_bytes(len_bytes) as usize;

            // Read data
            let mut buffer = vec![0u8; len];
            reader.read_exact(&mut buffer)
                .map_err(|e| FoldError::Io(e))?;

            // Deserialize
            let (ortho, _): (Ortho, _) = bincode::decode_from_slice(&buffer, bincode::config::standard())
                .map_err(|e| FoldError::Deserialization(Box::new(e)))?;

            self.memory.push_back(ortho);
            self.disk_count -= 1;
        }

        // If we've exhausted the disk, clean up
        if self.disk_count == 0 {
            self.cleanup_disk()?;
        }

        Ok(())
    }

    /// Clean up disk resources
    fn cleanup_disk(&mut self) -> Result<(), FoldError> {
        self.disk_reader = None;
        self.disk_writer = None;

        if let Some(path) = self.disk_file.take() {
            // Try to delete the file, but don't fail if it doesn't exist
            let _ = std::fs::remove_file(&path);
        }

        Ok(())
    }

    /// Flush any buffered writes to disk
    pub fn flush(&mut self) -> Result<(), FoldError> {
        if let Some(ref mut writer) = self.disk_writer {
            writer.flush().map_err(|e| FoldError::Io(e))?;
        }
        Ok(())
    }

    /// Read all orthos from the persistent storage file
    pub fn read_all_from_storage() -> Result<Vec<Ortho>, FoldError> {
        let state_dir = std::env::var("FOLD_STATE_DIR")
            .unwrap_or_else(|_| "./fold_state".to_string());
        let storage_path = PathBuf::from(&state_dir).join("ortho_storage.bin");
        
        if !storage_path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&storage_path).map_err(|e| FoldError::Io(e))?;
        let mut reader = BufReader::new(file);
        let mut orthos = Vec::new();

        loop {
            // Read length prefix
            let mut len_bytes = [0u8; 4];
            match reader.read_exact(&mut len_bytes) {
                Ok(_) => {},
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(FoldError::Io(e)),
            }
            let len = u32::from_le_bytes(len_bytes) as usize;

            // Read data
            let mut buffer = vec![0u8; len];
            reader.read_exact(&mut buffer)
                .map_err(|e| FoldError::Io(e))?;

            // Deserialize
            let (ortho, _): (Ortho, _) = bincode::decode_from_slice(&buffer, bincode::config::standard())
                .map_err(|e| FoldError::Deserialization(Box::new(e)))?;

            orthos.push(ortho);
        }

        Ok(orthos)
    }

    /// Count orthos in persistent storage without loading them all
    pub fn count_storage() -> Result<usize, FoldError> {
        let state_dir = std::env::var("FOLD_STATE_DIR")
            .unwrap_or_else(|_| "./fold_state".to_string());
        let storage_path = PathBuf::from(&state_dir).join("ortho_storage.bin");
        
        if !storage_path.exists() {
            return Ok(0);
        }

        let file = File::open(&storage_path).map_err(|e| FoldError::Io(e))?;
        let mut reader = BufReader::new(file);
        let mut count = 0;

        loop {
            // Read length prefix
            let mut len_bytes = [0u8; 4];
            match reader.read_exact(&mut len_bytes) {
                Ok(_) => {},
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(FoldError::Io(e)),
            }
            let len = u32::from_le_bytes(len_bytes) as usize;

            // Skip the data
            let mut buffer = vec![0u8; len];
            reader.read_exact(&mut buffer)
                .map_err(|e| FoldError::Io(e))?;

            count += 1;
        }

        Ok(count)
    }
}

impl Drop for DiskQueue {
    fn drop(&mut self) {
        // Flush before cleanup
        let _ = self.flush();
        // Clean up disk file on drop (only for temporary queue files, not persistent storage)
        let _ = self.cleanup_disk();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_only() {
        let mut queue = DiskQueue::new();
        
        for i in 0..100 {
            let ortho = Ortho::new(i);
            queue.push_back(ortho).unwrap();
        }

        assert_eq!(queue.len(), 100);
        assert_eq!(queue.disk_count, 0);

        for i in 0..100 {
            let ortho = queue.pop_front().unwrap().unwrap();
            assert_eq!(ortho.version(), i);
        }

        assert!(queue.is_empty());
    }

    #[test]
    fn test_disk_spillover() {
        let mut queue = DiskQueue::new();
        
        // Add more than MEMORY_THRESHOLD items
        for i in 0..(MEMORY_THRESHOLD + 1000) {
            let ortho = Ortho::new(i);
            queue.push_back(ortho).unwrap();
        }

        // Should have spilled to disk
        assert!(queue.disk_count > 0);
        assert_eq!(queue.len(), MEMORY_THRESHOLD + 1000);

        // Pop all items and verify order
        for i in 0..(MEMORY_THRESHOLD + 1000) {
            let ortho = queue.pop_front().unwrap().unwrap();
            assert_eq!(ortho.version(), i);
        }

        assert!(queue.is_empty());
    }
}
