use fold::{disk_backed_queue::DiskBackedQueue, interner::Interner, memory_config::MemoryConfig, ortho::Ortho, seen_tracker::SeenTracker, FoldError};
use std::fs;
use std::io::Write;
use std::path::Path;

fn calculate_score(ortho: &Ortho) -> (usize, usize) {
    let volume = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    (volume, fullness)
}

fn main() -> Result<(), FoldError> {
    let input_dir = "./fold_state/input";
    let output_dir = "./fold_state/output";
    
    println!("[fold] Starting fold processing");
    println!("[fold] Input directory: {}", input_dir);
    println!("[fold] Output directory: {}", output_dir);
    
    // Create output directory if it doesn't exist
    fs::create_dir_all(output_dir).map_err(|e| FoldError::Io(e))?;
    
    // Get all files from input directory, sorted
    let mut files = get_input_files(&input_dir)?;
    files.sort();
    
    if files.is_empty() {
        println!("[fold] No input files found in {}", input_dir);
        return Ok(());
    }
    
    println!("[fold] Found {} file(s) to process", files.len());
    
    // Process each file independently
    for file_path in files {
        println!("\n[fold] ========================================");
        println!("[fold] Processing file: {}", file_path);
        println!("[fold] ========================================");
        
        let text = fs::read_to_string(&file_path)
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
        
        // Create archive for this file
        let archive_path = get_archive_path(&file_path, output_dir);
        save_archive(&archive_path, &interner, &mut results)?;
        
        println!("[fold] Archive saved: {}", archive_path);
        
        // Clean up temporary results directory
        if Path::new(&results_path).exists() {
            fs::remove_dir_all(&results_path).map_err(|e| FoldError::Io(e))?;
        }
        
        // Delete the processed file
        fs::remove_file(&file_path).map_err(|e| FoldError::Io(e))?;
        println!("[fold] Input file deleted");
    }
    
    println!("\n[fold] ========================================");
    println!("[fold] All files processed successfully");
    println!("[fold] ========================================");
    
    Ok(())
}

fn get_input_files(input_dir: &str) -> Result<Vec<String>, FoldError> {
    let path = std::path::Path::new(input_dir);
    
    if !path.exists() {
        return Ok(Vec::new());
    }
    
    let mut files = Vec::new();
    
    for entry in fs::read_dir(path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let path = entry.path();
        
        if path.is_file() {
            if let Some(path_str) = path.to_str() {
                files.push(path_str.to_string());
            }
        }
    }
    
    Ok(files)
}

fn get_archive_path(input_file_path: &str, output_dir: &str) -> String {
    let path = Path::new(input_file_path);
    let filename = path.file_stem().unwrap_or_default().to_str().unwrap_or("output");
    format!("{}/{}.archive", output_dir, filename)
}

fn save_archive(archive_path: &str, interner: &Interner, results: &mut DiskBackedQueue) -> Result<(), FoldError> {
    // Flush results to ensure all are on disk
    results.flush()?;
    
    // Create archive file
    let archive_file = fs::File::create(archive_path).map_err(|e| FoldError::Io(e))?;
    let mut writer = std::io::BufWriter::new(archive_file);
    
    // Encode interner first
    let interner_bytes = bincode::encode_to_vec(interner, bincode::config::standard())?;
    let interner_len = interner_bytes.len() as u64;
    
    // Write interner length (8 bytes) then interner data
    writer.write_all(&interner_len.to_le_bytes()).map_err(|e| FoldError::Io(e))?;
    writer.write_all(&interner_bytes).map_err(|e| FoldError::Io(e))?;
    
    // Write results count (8 bytes)
    let results_count = results.len() as u64;
    writer.write_all(&results_count.to_le_bytes()).map_err(|e| FoldError::Io(e))?;
    
    // Stream results from disk-backed queue to archive
    let config = bincode::config::standard();
    results.scan(|ortho| {
        // Encode each ortho and write to archive
        if let Ok(ortho_bytes) = bincode::encode_to_vec(ortho, config) {
            let ortho_len = ortho_bytes.len() as u32;
            let _ = writer.write_all(&ortho_len.to_le_bytes());
            let _ = writer.write_all(&ortho_bytes);
        }
    })?;
    
    writer.flush().map_err(|e| FoldError::Io(e))?;
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
        let output_dir = "./fold_state/output";
        let archive_path = get_archive_path(input, output_dir);
        assert_eq!(archive_path, "./fold_state/output/test_chunk_0001.archive");
    }
    
    #[test]
    fn test_save_and_load_archive() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.archive");
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
        save_archive(archive_path.to_str().unwrap(), &interner, &mut results).unwrap();
        
        // Verify archive exists
        assert!(archive_path.exists());
        
        // Load and verify the archive format
        use std::io::Read;
        let mut archive_file = fs::File::open(&archive_path).unwrap();
        let mut reader = std::io::BufReader::new(archive_file);
        
        // Read interner length
        let mut len_bytes = [0u8; 8];
        reader.read_exact(&mut len_bytes).unwrap();
        let interner_len = u64::from_le_bytes(len_bytes) as usize;
        
        // Read interner data
        let mut interner_bytes = vec![0u8; interner_len];
        reader.read_exact(&mut interner_bytes).unwrap();
        let (loaded_interner, _): (Interner, usize) = 
            bincode::decode_from_slice(&interner_bytes, bincode::config::standard()).unwrap();
        
        assert_eq!(loaded_interner.version(), interner.version());
        assert_eq!(loaded_interner.vocabulary().len(), interner.vocabulary().len());
        
        // Read results count
        reader.read_exact(&mut len_bytes).unwrap();
        let results_count = u64::from_le_bytes(len_bytes);
        assert_eq!(results_count, 2);
        
        // Read first ortho
        let mut ortho_len_bytes = [0u8; 4];
        reader.read_exact(&mut ortho_len_bytes).unwrap();
        let ortho1_len = u32::from_le_bytes(ortho_len_bytes) as usize;
        let mut ortho1_bytes = vec![0u8; ortho1_len];
        reader.read_exact(&mut ortho1_bytes).unwrap();
        let (loaded_ortho1, _): (Ortho, usize) = 
            bincode::decode_from_slice(&ortho1_bytes, bincode::config::standard()).unwrap();
        assert_eq!(loaded_ortho1.id(), id1);
        
        // Read second ortho
        reader.read_exact(&mut ortho_len_bytes).unwrap();
        let ortho2_len = u32::from_le_bytes(ortho_len_bytes) as usize;
        let mut ortho2_bytes = vec![0u8; ortho2_len];
        reader.read_exact(&mut ortho2_bytes).unwrap();
        let (loaded_ortho2, _): (Ortho, usize) = 
            bincode::decode_from_slice(&ortho2_bytes, bincode::config::standard()).unwrap();
        assert_eq!(loaded_ortho2.id(), id2);
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
