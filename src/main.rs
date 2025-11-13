use fold::{disk_backed_queue::DiskBackedQueue, interner::Interner, memory_config::MemoryConfig, ortho::Ortho, seen_tracker::SeenTracker, FoldError};
use std::fs;
use std::path::Path;

fn calculate_score(ortho: &Ortho) -> (usize, usize) {
    let volume = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    (volume, fullness)
}

fn recover_abandoned_files(input_dir: &str, in_process_dir: &str) -> Result<(), FoldError> {
    // Scan in_process directory for any .txt files left from previous incomplete runs
    // and move them back to input directory for reprocessing
    let in_process_path = std::path::Path::new(in_process_dir);
    
    if !in_process_path.exists() {
        return Ok(());
    }
    
    let mut recovered_count = 0;
    for entry in fs::read_dir(in_process_path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();
        
        if entry_path.is_file() {
            if let Some(ext) = entry_path.extension() {
                if ext == "txt" {
                    // Found abandoned .txt file, move it back to input
                    let filename = entry_path.file_name().unwrap_or_default();
                    let target_path = format!("{}/{}", input_dir, filename.to_str().unwrap_or("recovered"));
                    fs::rename(&entry_path, &target_path).map_err(|e| FoldError::Io(e))?;
                    println!("[fold] Recovered abandoned file: {:?} -> {}", filename, target_path);
                    recovered_count += 1;
                }
            }
        }
    }
    
    if recovered_count > 0 {
        println!("[fold] Recovered {} abandoned file(s) from previous run", recovered_count);
    }
    
    Ok(())
}

fn main() -> Result<(), FoldError> {
    let input_dir = "./fold_state/input";
    let in_process_dir = "./fold_state/in_process";
    
    println!("[fold] Starting fold processing");
    println!("[fold] Input directory: {}", input_dir);
    
    // Create in_process directory if it doesn't exist
    fs::create_dir_all(in_process_dir).map_err(|e| FoldError::Io(e))?;
    
    // Recover any abandoned files from previous runs
    recover_abandoned_files(input_dir, in_process_dir)?;
    
    // Process files one at a time until none remain
    loop {
        // Find next .txt file
        let txt_file = find_next_txt_file(input_dir)?;
        
        if txt_file.is_none() {
            println!("[fold] No more .txt files to process");
            break;
        }
        
        let file_path = txt_file.unwrap();
        println!("\n[fold] ========================================");
        println!("[fold] Processing file: {}", file_path);
        println!("[fold] ========================================");
        
        // Move file to in-process location to prevent other processes from picking it up
        // This provides mutual exclusion for distributed/parallel processing
        let filename = Path::new(&file_path).file_name().unwrap_or_default();
        let in_process_path = format!("{}/{}", in_process_dir, filename.to_str().unwrap_or("temp"));
        fs::rename(&file_path, &in_process_path).map_err(|e| FoldError::Io(e))?;
        println!("[fold] Moved to in-process: {}", in_process_path);
        
        // Recovery strategies for processes that exit after beginning but before finishing:
        // 1. Startup scan: On program start, check in_process directory for abandoned files
        //    and move them back to input directory for reprocessing
        // 2. Timestamp-based detection: Add .timestamp file alongside in-process file,
        //    update periodically. If timestamp is stale (e.g., >1 hour old), consider abandoned
        // 3. PID-based locking: Create .lock file with process ID, check if PID is still running
        // 4. Heartbeat mechanism: Periodically update a heartbeat file; other processes can
        //    detect stale heartbeats and reclaim abandoned work
        // 5. Archive validation: Before considering work complete, verify archive integrity
        //    and only then remove in-process file. If crash occurs, incomplete archives can be
        //    detected and source files recovered from in_process directory
        
        let text = fs::read_to_string(&in_process_path)
            .map_err(|e| FoldError::Io(e))?;
        
        // Build interner from this file only
        let interner = Interner::from_text(&text);
        let version = interner.version();
        
        println!("[fold] Interner version: {}", version);
        println!("[fold] Vocabulary size: {}", interner.vocabulary().len());
        
        // Calculate memory config for this file
        let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?.len();
        let memory_config = MemoryConfig::calculate(interner_bytes, 0);
        
        // Initialize work queue for this file
        let mut work_queue = DiskBackedQueue::new(memory_config.queue_buffer_size)?;
        
        // Initialize results queue for this file (disk-backed to avoid OOM)
        let results_path = format!("./fold_state/results_{}", 
            Path::new(&file_path).file_stem().unwrap_or_default().to_str().unwrap_or("temp"));
        let mut results = DiskBackedQueue::new_from_path(&results_path, memory_config.queue_buffer_size)?;
        
        // Initialize tracker for this file
        let mut tracker = SeenTracker::with_config(
            memory_config.bloom_capacity,
            memory_config.num_shards,
            memory_config.max_shards_in_memory,
        );
        
        // Seed with empty ortho
        let seed_ortho = Ortho::new(version);
        let seed_id = seed_ortho.id();
        println!("[fold] Seeding with empty ortho id={}, version={}", seed_id, version);
        
        tracker.insert(seed_id);
        let mut best_ortho = seed_ortho.clone();
        let mut best_score = calculate_score(&best_ortho);
        
        work_queue.push(seed_ortho)?;
        
        // Process work queue until empty
        let mut processed_count = 0;
        while let Some(ortho) = work_queue.pop()? {
            processed_count += 1;
            
            if processed_count % 1000 == 0 {
                println!("[fold] Processed {} orthos, queue size: {}, seen: {}", 
                         processed_count, work_queue.len(), tracker.len());
            }
            
            if processed_count % 100000 == 0 {
                print_optimal(&best_ortho, &interner);
            }
            
            // Get requirements from ortho
            let (forbidden, required) = ortho.get_requirements();
            
            // Get completions from interner
            let completions = interner.intersect(&required, &forbidden);
            
            // Generate child orthos
            for completion in completions {
                let children = ortho.add(completion, version);
                
                for child in children {
                    let child_id = child.id();
                    
                    // Use tracker for bloom-filtered deduplication check
                    if !tracker.contains(&child_id) {
                        tracker.insert(child_id);
                        
                        let candidate_score = calculate_score(&child);
                        if candidate_score > best_score {
                            best_ortho = child.clone();
                            best_score = candidate_score;
                        }
                        
                        results.push(child.clone())?;
                        work_queue.push(child)?;
                    }
                }
            }
        }
        
        println!("[fold] Finished processing file");
        println!("[fold] Total orthos generated: {}", results.len());
        
        print_optimal(&best_ortho, &interner);
        
        // Create archive for this file (use in_process_path to get the correct location)
        let archive_path = get_archive_path(&in_process_path);
        save_archive(&archive_path, &interner, &mut results, &results_path)?;
        
        println!("[fold] Archive saved: {}", archive_path);
        
        // Delete the in-process .txt file
        fs::remove_file(&in_process_path).map_err(|e| FoldError::Io(e))?;
        println!("[fold] In-process file deleted");
    }
    
    println!("\n[fold] ========================================");
    println!("[fold] All files processed successfully");
    println!("[fold] ========================================");
    
    Ok(())
}

fn find_next_txt_file(input_dir: &str) -> Result<Option<String>, FoldError> {
    let path = std::path::Path::new(input_dir);
    
    if !path.exists() {
        fs::create_dir_all(path).map_err(|e| FoldError::Io(e))?;
        return Ok(None);
    }
    
    for entry in fs::read_dir(path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();
        
        if entry_path.is_file() {
            if let Some(ext) = entry_path.extension() {
                if ext == "txt" {
                    if let Some(path_str) = entry_path.to_str() {
                        return Ok(Some(path_str.to_string()));
                    }
                }
            }
        }
    }
    
    Ok(None)
}

fn get_archive_path(input_file_path: &str) -> String {
    let path = Path::new(input_file_path);
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path.file_stem().unwrap_or_default().to_str().unwrap_or("output");
    format!("{}/{}.bin", parent.display(), filename)
}

fn save_archive(archive_path: &str, interner: &Interner, results: &mut DiskBackedQueue, results_path: &str) -> Result<(), FoldError> {
    // Flush results to ensure all are on disk
    results.flush()?;
    
    // Create archive directory
    fs::create_dir_all(archive_path).map_err(|e| FoldError::Io(e))?;
    
    // Move the DiskBackedQueue directory to the archive
    let archive_results_path = format!("{}/results", archive_path);
    if Path::new(results_path).exists() {
        fs::rename(results_path, &archive_results_path).map_err(|e| FoldError::Io(e))?;
    }
    
    // Write the interner to the archive folder
    let interner_path = format!("{}/interner.bin", archive_path);
    let interner_bytes = bincode::encode_to_vec(interner, bincode::config::standard())?;
    fs::write(interner_path, interner_bytes).map_err(|e| FoldError::Io(e))?;
    
    Ok(())
}

fn print_optimal(ortho: &Ortho, interner: &Interner) {
    let (volume, fullness) = calculate_score(ortho);
    println!("\n[fold] ===== OPTIMAL ORTHO =====");
    println!("[fold] Ortho ID: {}", ortho.id());
    println!("[fold] Version: {}", ortho.version());
    println!("[fold] Dimensions: {:?}", ortho.dims());
    println!("[fold] Score: (volume={}, fullness={})", volume, fullness);
    
    println!("[fold] Geometry:");
    for line in format!("{}", ortho.display(interner)).lines() {
        println!("[fold]   {}", line);
    }
    
    println!("[fold] ========================\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_get_archive_path() {
        let input = "./fold_state/input/test_chunk_0001.txt";
        let archive_path = get_archive_path(input);
        assert_eq!(archive_path, "./fold_state/input/test_chunk_0001.bin");
    }
    
    #[test]
    fn test_save_and_load_archive() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.bin");
        let results_path = temp_dir.path().join("test_results");
        
        let interner = Interner::from_text("hello world test");
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        let id1 = ortho1.id();
        let id2 = ortho2.id();
        
        // Create a DiskBackedQueue and add orthos
        let mut results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), 10).unwrap();
        results.push(ortho1).unwrap();
        results.push(ortho2).unwrap();
        
        // Save archive
        save_archive(archive_path.to_str().unwrap(), &interner, &mut results, results_path.to_str().unwrap()).unwrap();
        
        // Verify archive directory exists
        assert!(archive_path.exists());
        assert!(archive_path.is_dir());
        
        // Verify interner.bin exists
        let interner_path = archive_path.join("interner.bin");
        assert!(interner_path.exists());
        
        // Load and verify interner
        let interner_bytes = fs::read(&interner_path).unwrap();
        let (loaded_interner, _): (Interner, usize) = 
            bincode::decode_from_slice(&interner_bytes, bincode::config::standard()).unwrap();
        
        assert_eq!(loaded_interner.version(), interner.version());
        assert_eq!(loaded_interner.vocabulary().len(), interner.vocabulary().len());
        
        // Verify results directory was moved
        let archive_results_path = archive_path.join("results");
        assert!(archive_results_path.exists());
        assert!(archive_results_path.is_dir());
        
        // Load results from the archive
        let loaded_results = DiskBackedQueue::new_from_path(archive_results_path.to_str().unwrap(), 10).unwrap();
        assert_eq!(loaded_results.len(), 2);
    }
    
    #[test]
    fn test_calculate_score() {
        let ortho = Ortho::new(1);
        let (volume, fullness) = calculate_score(&ortho);
        // Empty ortho with dims [2,2] has volume (2-1)*(2-1) = 1
        assert_eq!(volume, 1);
        // All 4 slots are None
        assert_eq!(fullness, 0);
    }
}
