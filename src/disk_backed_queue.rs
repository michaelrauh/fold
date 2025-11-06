use crate::ortho::Ortho;
use crate::error::FoldError;
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use bincode::{Decode, Encode};

/// A disk-backed FIFO queue for Ortho structures that keeps a limited number of items in memory
/// and spills overflow to disk files. This prevents OOM errors when processing millions of orthos.
pub struct DiskBackedQueue {
    memory_buffer: VecDeque<Ortho>,
    buffer_size_limit: usize,
    disk_files: VecDeque<PathBuf>,
    temp_dir: PathBuf,
    file_counter: usize,
}

impl DiskBackedQueue {
    /// Create a new disk-backed queue with specified memory buffer size and temp directory.
    pub fn new(buffer_size_limit: usize, temp_dir: PathBuf) -> Result<Self, FoldError> {
        // Create temp directory if it doesn't exist
        if !temp_dir.exists() {
            fs::create_dir_all(&temp_dir).map_err(|e| FoldError::Io(e))?;
        }
        
        Ok(DiskBackedQueue {
            memory_buffer: VecDeque::new(),
            buffer_size_limit,
            disk_files: VecDeque::new(),
            temp_dir,
            file_counter: 0,
        })
    }
    
    /// Push an ortho to the queue. If memory buffer is full, flush oldest items to disk.
    pub fn push(&mut self, ortho: Ortho) -> Result<(), FoldError> {
        self.memory_buffer.push_back(ortho);
        
        // If buffer exceeds limit, flush oldest half to disk
        if self.memory_buffer.len() > self.buffer_size_limit {
            self.flush_to_disk()?;
        }
        
        Ok(())
    }
    
    /// Pop an ortho from the front of the queue. Reloads from disk to maintain FIFO order.
    pub fn pop(&mut self) -> Result<Option<Ortho>, FoldError> {
        // If memory is empty, reload from disk
        if self.memory_buffer.is_empty() {
            if !self.disk_files.is_empty() {
                self.reload_from_disk()?;
            }
            return Ok(self.memory_buffer.pop_front());
        }
        
        // Memory not empty - check if disk has older items
        // If so, reload ALL disk files before continuing
        if !self.disk_files.is_empty() {
            // Save current memory temporarily
            let mut temp = VecDeque::new();
            std::mem::swap(&mut temp, &mut self.memory_buffer);
            
            // Reload ALL disk files
            while !self.disk_files.is_empty() {
                self.reload_from_disk()?;
            }
            
            // Append saved memory items (they're newer than all disk items)
            self.memory_buffer.extend(temp);
        }
        
        // Pop from memory
        Ok(self.memory_buffer.pop_front())
    }
    
    /// Returns the total number of items in the queue (memory + disk).
    pub fn len(&self) -> usize {
        self.memory_buffer.len() + self.disk_file_item_count()
    }
    
    /// Returns true if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.memory_buffer.is_empty() && self.disk_files.is_empty()
    }
    
    /// Flush oldest half of memory buffer to disk.
    /// Invariant: Disk files contain items OLDER than everything in memory.
    fn flush_to_disk(&mut self) -> Result<(), FoldError> {
        let flush_count = self.memory_buffer.len() / 2;
        if flush_count == 0 {
            return Ok(());
        }
        
        // Create new disk file
        let file_path = self.temp_dir.join(format!("queue_{}.bin", self.file_counter));
        self.file_counter += 1;
        
        let file = File::create(&file_path).map_err(|e| FoldError::Io(e))?;
        let mut writer = BufWriter::new(file);
        
        // Write oldest items from front of memory to file
        let config = bincode::config::standard();
        for _ in 0..flush_count {
            if let Some(ortho) = self.memory_buffer.pop_front() {
                bincode::encode_into_std_write(&ortho, &mut writer, config)
                    .map_err(|e| FoldError::Serialization(e.to_string()))?;
            }
        }
        
        writer.flush().map_err(|e| FoldError::Io(e))?;
        // Add to BACK of disk files (newest disk file with older items than current memory)
        self.disk_files.push_back(file_path);
        
        Ok(())
    }
    
    /// Reload orthos from the oldest disk file.
    fn reload_from_disk(&mut self) -> Result<(), FoldError> {
        // Get oldest disk file
        if let Some(file_path) = self.disk_files.pop_front() {
            let file = File::open(&file_path).map_err(|e| FoldError::Io(e))?;
            let mut reader = BufReader::new(file);
            
            let config = bincode::config::standard();
            
            // Read all orthos and add to back of memory
            loop {
                match bincode::decode_from_std_read::<Ortho, _, _>(&mut reader, config) {
                    Ok(ortho) => self.memory_buffer.push_back(ortho),
                    Err(_) => break, // End of file or error
                }
            }
            
            // Delete the file after reading
            fs::remove_file(&file_path).map_err(|e| FoldError::Io(e))?;
        }
        
        Ok(())
    }
    
    /// Estimate the number of items stored in disk files.
    /// This is an approximation based on the number of files.
    fn disk_file_item_count(&self) -> usize {
        // Each disk file contains approximately buffer_size_limit / 2 items
        self.disk_files.len() * (self.buffer_size_limit / 2)
    }
}

impl Drop for DiskBackedQueue {
    fn drop(&mut self) {
        // Clean up any remaining disk files
        for file_path in &self.disk_files {
            let _ = fs::remove_file(file_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;
    use std::env;
    
    fn temp_dir_for_test(test_name: &str) -> PathBuf {
        let mut path = env::temp_dir();
        path.push(format!("fold_test_{}", test_name));
        path
    }
    
    fn cleanup_temp_dir(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }
    
    #[test]
    fn test_new_creates_temp_directory() {
        let temp_dir = temp_dir_for_test("new_creates_temp_directory");
        cleanup_temp_dir(&temp_dir);
        
        let queue = DiskBackedQueue::new(10, temp_dir.clone());
        assert!(queue.is_ok());
        assert!(temp_dir.exists());
        
        cleanup_temp_dir(&temp_dir);
    }
    
    #[test]
    fn test_push_and_pop_basic() {
        let temp_dir = temp_dir_for_test("push_and_pop_basic");
        cleanup_temp_dir(&temp_dir);
        
        let mut queue = DiskBackedQueue::new(10, temp_dir.clone()).unwrap();
        
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        
        queue.push(ortho1.clone()).unwrap();
        queue.push(ortho2.clone()).unwrap();
        
        assert_eq!(queue.len(), 2);
        assert!(!queue.is_empty());
        
        let popped1 = queue.pop().unwrap().unwrap();
        assert_eq!(popped1.id(), ortho1.id());
        
        let popped2 = queue.pop().unwrap().unwrap();
        assert_eq!(popped2.id(), ortho2.id());
        
        assert_eq!(queue.len(), 0);
        assert!(queue.is_empty());
        
        cleanup_temp_dir(&temp_dir);
    }
    
    #[test]
    fn test_fifo_order() {
        let temp_dir = temp_dir_for_test("fifo_order");
        cleanup_temp_dir(&temp_dir);
        
        let mut queue = DiskBackedQueue::new(10, temp_dir.clone()).unwrap();
        
        let orthos: Vec<Ortho> = (0..5).map(|i| {
            let o = Ortho::new(1);
            o.add(i, 1)[0].clone()
        }).collect();
        
        for ortho in &orthos {
            queue.push(ortho.clone()).unwrap();
        }
        
        for ortho in &orthos {
            let popped = queue.pop().unwrap().unwrap();
            assert_eq!(popped.id(), ortho.id());
        }
        
        cleanup_temp_dir(&temp_dir);
    }
    
    #[test]
    fn test_disk_overflow() {
        let temp_dir = temp_dir_for_test("disk_overflow");
        cleanup_temp_dir(&temp_dir);
        
        let buffer_limit = 10;
        let mut queue = DiskBackedQueue::new(buffer_limit, temp_dir.clone()).unwrap();
        
        // Push more items than buffer limit to trigger disk overflow
        let total_items = 25;
        let orthos: Vec<Ortho> = (0..total_items).map(|i| {
            let o = Ortho::new(1);
            o.add(i, 1)[0].clone()
        }).collect();
        
        for ortho in &orthos {
            queue.push(ortho.clone()).unwrap();
        }
        
        // Check that some items were flushed to disk
        assert!(queue.disk_files.len() > 0, "Items should have been flushed to disk");
        
        // Verify all items can be popped in correct order
        for ortho in &orthos {
            let popped = queue.pop().unwrap().unwrap();
            assert_eq!(popped.payload(), ortho.payload(), "Payloads should match");
            assert_eq!(popped.dims(), ortho.dims(), "Dims should match");
        }
        
        assert!(queue.is_empty());
        
        cleanup_temp_dir(&temp_dir);
    }
    
    #[test]
    fn test_pop_empty_queue() {
        let temp_dir = temp_dir_for_test("pop_empty_queue");
        cleanup_temp_dir(&temp_dir);
        
        let mut queue = DiskBackedQueue::new(10, temp_dir.clone()).unwrap();
        
        let result = queue.pop().unwrap();
        assert!(result.is_none());
        
        cleanup_temp_dir(&temp_dir);
    }
    
    #[test]
    fn test_len_with_disk_and_memory() {
        let temp_dir = temp_dir_for_test("len_with_disk_and_memory");
        cleanup_temp_dir(&temp_dir);
        
        let buffer_limit = 10;
        let mut queue = DiskBackedQueue::new(buffer_limit, temp_dir.clone()).unwrap();
        
        // Push items to trigger disk overflow
        for i in 0..20 {
            let o = Ortho::new(1);
            let ortho = o.add(i, 1)[0].clone();
            queue.push(ortho).unwrap();
        }
        
        // len() should account for both memory and disk
        assert!(queue.len() >= 20, "Queue length should be at least 20");
        
        cleanup_temp_dir(&temp_dir);
    }
    
    #[test]
    fn test_multiple_disk_files() {
        let temp_dir = temp_dir_for_test("multiple_disk_files");
        cleanup_temp_dir(&temp_dir);
        
        let buffer_limit = 10;
        let mut queue = DiskBackedQueue::new(buffer_limit, temp_dir.clone()).unwrap();
        
        // Push enough items to create multiple disk files
        let total_items = 50;
        let orthos: Vec<Ortho> = (0..total_items).map(|i| {
            let o = Ortho::new(1);
            o.add(i, 1)[0].clone()
        }).collect();
        
        for ortho in &orthos {
            queue.push(ortho.clone()).unwrap();
        }
        
        // Should have created multiple disk files
        assert!(queue.disk_files.len() > 1, "Should have created multiple disk files");
        
        // Verify all items are retrievable in order
        for ortho in &orthos {
            let popped = queue.pop().unwrap().unwrap();
            assert_eq!(popped.payload(), ortho.payload(), "Payloads should match");
            assert_eq!(popped.dims(), ortho.dims(), "Dims should match");
        }
        
        assert!(queue.is_empty());
        
        cleanup_temp_dir(&temp_dir);
    }
    
    #[test]
    fn test_cleanup_on_drop() {
        let temp_dir = temp_dir_for_test("cleanup_on_drop");
        cleanup_temp_dir(&temp_dir);
        
        {
            let mut queue = DiskBackedQueue::new(10, temp_dir.clone()).unwrap();
            
            // Create some disk files
            for i in 0..25 {
                let o = Ortho::new(1);
                let ortho = o.add(i, 1)[0].clone();
                queue.push(ortho).unwrap();
            }
            
            assert!(queue.disk_files.len() > 0);
        } // queue dropped here
        
        // Check that disk files were cleaned up
        let files_remaining = fs::read_dir(&temp_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        
        assert_eq!(files_remaining, 0, "Disk files should be cleaned up on drop");
        
        cleanup_temp_dir(&temp_dir);
    }
}
