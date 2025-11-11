use crate::ortho::Ortho;
use crate::FoldError;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;

pub struct DiskBackedQueue {
    buffer: Vec<Ortho>,
    buffer_size: usize,
    disk_path: PathBuf,
    disk_file_counter: usize,
    disk_files: Vec<PathBuf>,
    disk_count: usize,
}

impl DiskBackedQueue {
    pub fn new(buffer_size: usize) -> Result<Self, FoldError> {
        let disk_path = PathBuf::from("./fold_state/queue");
        
        if disk_path.exists() {
            fs::remove_dir_all(&disk_path).map_err(FoldError::Io)?;
        }
        fs::create_dir_all(&disk_path).map_err(FoldError::Io)?;
        
        Ok(Self {
            buffer: Vec::with_capacity(buffer_size),
            buffer_size,
            disk_path,
            disk_file_counter: 0,
            disk_files: Vec::new(),
            disk_count: 0,
        })
    }
    
    pub fn new_from_path(path: &str, buffer_size: usize) -> Result<Self, FoldError> {
        let disk_path = PathBuf::from(path);
        
        if !disk_path.exists() {
            fs::create_dir_all(&disk_path).map_err(FoldError::Io)?;
        }
        
        // Find existing disk files
        let mut disk_files = Vec::new();
        let mut disk_count = 0;
        let mut max_counter = 0;
        
        if disk_path.is_dir() {
            for entry in fs::read_dir(&disk_path).map_err(FoldError::Io)? {
                let entry = entry.map_err(FoldError::Io)?;
                let path = entry.path();
                
                if path.is_file() && path.extension().map_or(false, |ext| ext == "bin") {
                    // Count items in this file
                    let file = File::open(&path).map_err(FoldError::Io)?;
                    let mut reader = BufReader::new(file);
                    let config = bincode::config::standard();
                    
                    let mut count = 0;
                    loop {
                        match bincode::decode_from_std_read::<Ortho, _, _>(&mut reader, config) {
                            Ok(_) => count += 1,
                            Err(_) => break,
                        }
                    }
                    
                    disk_count += count;
                    disk_files.push(path.clone());
                    
                    // Extract counter from filename
                    if let Some(stem) = path.file_stem() {
                        if let Some(stem_str) = stem.to_str() {
                            if let Some(counter_str) = stem_str.strip_prefix("queue_") {
                                if let Ok(counter) = counter_str.parse::<usize>() {
                                    max_counter = max_counter.max(counter);
                                }
                            }
                        }
                    }
                }
            }
        }
        
        // Sort files by name - oldest files at front (lowest counter values)
        disk_files.sort();
        
        Ok(Self {
            buffer: Vec::with_capacity(buffer_size),
            buffer_size,
            disk_path,
            disk_file_counter: max_counter + 1,
            disk_files,
            disk_count,
        })
    }
    
    pub fn push(&mut self, ortho: Ortho) -> Result<(), FoldError> {
        self.buffer.push(ortho);
        
        if self.buffer.len() >= self.buffer_size {
            // Spill ALL to disk, not just half
            // This way buffer is always either empty or has items newer than anything on disk
            self.spill_to_disk()?;
        }
        
        Ok(())
    }
    
    pub fn pop(&mut self) -> Result<Option<Ortho>, FoldError> {
        // If buffer is empty AND we have disk files, load from disk
        if self.buffer.is_empty() && !self.disk_files.is_empty() {
            self.load_from_disk()?;
        }
        
        if self.buffer.is_empty() {
            Ok(None)
        } else {
            Ok(Some(self.buffer.remove(0)))
        }
    }
    
    pub fn len(&self) -> usize {
        self.buffer.len() + self.disk_count
    }
    
    pub fn base_path(&self) -> String {
        self.disk_path.to_string_lossy().to_string()
    }
    
    /// Scan all items in queue without consuming them
    pub fn scan<F>(&self, mut f: F) -> Result<(), FoldError>
    where
        F: FnMut(&Ortho),
    {
        // Scan items in buffer
        for ortho in &self.buffer {
            f(ortho);
        }
        
        // Scan items in disk files
        for file_path in &self.disk_files {
            let file = File::open(file_path).map_err(FoldError::Io)?;
            let mut reader = BufReader::new(file);
            let config = bincode::config::standard();
            
            loop {
                match bincode::decode_from_std_read::<Ortho, _, _>(&mut reader, config) {
                    Ok(ortho) => f(&ortho),
                    Err(_) => break,
                }
            }
        }
        
        Ok(())
    }
    
    pub fn flush(&mut self) -> Result<(), FoldError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        
        let file_path = self.disk_path.join(format!("queue_{:08}.bin", self.disk_file_counter));
        self.disk_file_counter += 1;
        
        let spill_count = self.buffer.len();
        // println!("[disk_backed_queue] Flushing {} orthos to disk: {}", spill_count, file_path.display());
        
        let file = File::create(&file_path).map_err(FoldError::Io)?;
        let mut writer = BufWriter::new(file);
        
        let config = bincode::config::standard();
        for ortho in &self.buffer {
            bincode::encode_into_std_write(ortho, &mut writer, config)?;
        }
        writer.flush().map_err(FoldError::Io)?;
        
        self.disk_files.push(file_path);
        self.disk_count += spill_count;
        self.buffer.clear();
        
        Ok(())
    }
    
    fn spill_to_disk(&mut self) -> Result<(), FoldError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        
        let half = self.buffer_size / 2;
        // Spill the NEWEST half (from the end) to disk
        // Keep OLDEST half (at front) in buffer ready to pop
        let split_point = self.buffer.len().saturating_sub(half);
        let to_spill: Vec<Ortho> = self.buffer.drain(split_point..).collect();
        
        let spill_count = to_spill.len();
        let file_path = self.disk_path.join(format!("queue_{:08}.bin", self.disk_file_counter));
        self.disk_file_counter += 1;
        
        // println!("[disk_backed_queue] Spilling {} orthos to disk: {}", spill_count, file_path.display());
        
        let file = File::create(&file_path).map_err(FoldError::Io)?;
        let mut writer = BufWriter::new(file);
        
        let config = bincode::config::standard();
        for ortho in to_spill {
            bincode::encode_into_std_write(&ortho, &mut writer, config)?;
        }
        writer.flush().map_err(FoldError::Io)?;
        
        // Newest file goes to end of list
        self.disk_files.push(file_path);
        self.disk_count += spill_count;
        
        Ok(())
    }
    
    fn load_from_disk(&mut self) -> Result<(), FoldError> {
        if self.disk_files.is_empty() {
            return Ok(());
        }
        
        // Load from the END (newest file with items we spilled most recently)
        let file_path = self.disk_files.pop().unwrap();
        
        // println!("[disk_backed_queue] Loading orthos from disk: {}", file_path.display());
        
        let file = File::open(&file_path).map_err(FoldError::Io)?;
        let mut reader = BufReader::new(file);
        
        let config = bincode::config::standard();
        let mut loaded = Vec::new();
        
        loop {
            match bincode::decode_from_std_read::<Ortho, _, _>(&mut reader, config) {
                Ok(ortho) => {
                    loaded.push(ortho);
                }
                Err(_) => break,
            }
        }
        
        // println!("[disk_backed_queue] Loaded {} orthos from disk", loaded.len());
        
        self.disk_count -= loaded.len();
        // Loaded items are newer, they go to the end
        self.buffer.append(&mut loaded);
        
        fs::remove_file(&file_path).map_err(FoldError::Io)?;
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_push_pop_no_disk() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_path = temp_dir.path().join("queue");
        let mut queue = DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 10).unwrap();
        
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        
        queue.push(ortho1.clone()).unwrap();
        queue.push(ortho2.clone()).unwrap();
        
        assert_eq!(queue.pop().unwrap().unwrap().version(), 1);
        assert_eq!(queue.pop().unwrap().unwrap().version(), 2);
        assert!(queue.pop().unwrap().is_none());
    }
    
    #[test]
    fn test_spill_and_load() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_path = temp_dir.path().join("queue");
        let mut queue = DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 4).unwrap();
        
        for v in 1..=5 {
            queue.push(Ortho::new(v)).unwrap();
        }
        
        // Pop all items - order doesn't matter, just that we get all 5
        let mut results = Vec::new();
        for _ in 0..5 {
            results.push(queue.pop().unwrap().unwrap().version());
        }
        
        results.sort();
        assert_eq!(results, vec![1, 2, 3, 4, 5]);
        assert!(queue.pop().unwrap().is_none());
    }
    
    #[test]
    fn test_len_with_disk() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_path = temp_dir.path().join("queue");
        let mut queue = DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 4).unwrap();
        
        assert_eq!(queue.len(), 0);
        
        queue.push(Ortho::new(1)).unwrap();
        assert_eq!(queue.len(), 1);
        
        queue.push(Ortho::new(2)).unwrap();
        assert_eq!(queue.len(), 2);
        
        queue.push(Ortho::new(3)).unwrap();
        assert_eq!(queue.len(), 3);
        
        queue.push(Ortho::new(4)).unwrap();
        assert_eq!(queue.len(), 4);
        
        queue.push(Ortho::new(5)).unwrap();
        assert_eq!(queue.len(), 5);
        
        queue.pop().unwrap();
        assert_eq!(queue.len(), 4);
        
        queue.pop().unwrap();
        assert_eq!(queue.len(), 3);
        
        queue.pop().unwrap();
        assert_eq!(queue.len(), 2);
        
        queue.push(Ortho::new(6)).unwrap();
        assert_eq!(queue.len(), 3);
        
        queue.pop().unwrap();
        assert_eq!(queue.len(), 2);
        
        queue.pop().unwrap();
        assert_eq!(queue.len(), 1);
        
        queue.pop().unwrap();
        assert_eq!(queue.len(), 0);
    }
    
    #[test]
    fn test_flush() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_path = temp_dir.path().join("queue");
        let mut queue = DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 10).unwrap();
        
        queue.push(Ortho::new(1)).unwrap();
        queue.push(Ortho::new(2)).unwrap();
        queue.push(Ortho::new(3)).unwrap();
        
        assert_eq!(queue.len(), 3);
        
        queue.flush().unwrap();
        
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.buffer.len(), 0);
        assert_eq!(queue.disk_count, 3);
        
        let result1 = queue.pop().unwrap().unwrap();
        assert_eq!(result1.version(), 1);
        
        let result2 = queue.pop().unwrap().unwrap();
        assert_eq!(result2.version(), 2);
        
        let result3 = queue.pop().unwrap().unwrap();
        assert_eq!(result3.version(), 3);
        
        assert!(queue.pop().unwrap().is_none());
    }
    
    #[test]
    fn test_flush_empty() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_path = temp_dir.path().join("queue");
        let mut queue = DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 10).unwrap();
        
        queue.flush().unwrap();
        
        assert_eq!(queue.len(), 0);
    }
    
    #[test]
    fn test_base_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let queue_path = temp_dir.path().join("queue");
        let queue = DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 10).unwrap();
        
        assert!(queue.base_path().contains("queue"));
    }
    
    #[test]
    fn test_new_from_path_with_existing_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_path = temp_dir.path().join("queue");
        
        // Create and populate initial queue
        {
            let mut queue = DiskBackedQueue::new_from_path(test_path.to_str().unwrap(), 5).unwrap();
            
            for v in 1..=8 {
                queue.push(Ortho::new(v)).unwrap();
            }
            
            queue.flush().unwrap();
        }
        
        // Load from same path
        {
            let mut queue = DiskBackedQueue::new_from_path(test_path.to_str().unwrap(), 5).unwrap();
            
            assert_eq!(queue.len(), 8);
            
            // Verify we can pop all items - order doesn't matter
            let mut results = Vec::new();
            for _ in 0..8 {
                results.push(queue.pop().unwrap().unwrap().version());
            }
            
            results.sort();
            assert_eq!(results, vec![1, 2, 3, 4, 5, 6, 7, 8]);
            assert!(queue.pop().unwrap().is_none());
        }
    }
}
