use fold::{disk_backed_queue::DiskBackedQueue, file_handler, interner::Interner, memory_config::MemoryConfig, ortho::Ortho, seen_tracker::SeenTracker, FoldError};
use std::fs;
use std::path::Path;

fn calculate_score(ortho: &Ortho) -> (usize, usize) {
    let volume = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    (volume, fullness)
}

fn main() -> Result<(), FoldError> {
    let input_dir = "./fold_state/input";
    let in_process_dir = "./fold_state/in_process";
    
    println!("[fold] Starting fold processing");
    println!("[fold] Input directory: {}", input_dir);
    
    // Create in_process directory if it doesn't exist
    fs::create_dir_all(in_process_dir).map_err(|e| FoldError::Io(e))?;
    
    // Recover any abandoned files from previous runs (includes heartbeat check)
    file_handler::recover_abandoned_files(input_dir, in_process_dir)?;
    
    // Process files one at a time until none remain
    loop {
        // Find next .txt file
        let txt_file = file_handler::find_next_txt_file(input_dir)?;
        
        if txt_file.is_none() {
            println!("[fold] No more .txt files to process");
            break;
        }
        
        let file_path = txt_file.unwrap();
        println!("\n[fold] ========================================");
        println!("[fold] Processing file: {}", file_path);
        println!("[fold] ========================================");
        
        // Move file to in-process location to prevent other processes from picking it up
        let filename = Path::new(&file_path).file_name().unwrap_or_default();
        let in_process_path = format!("{}/{}", in_process_dir, filename.to_str().unwrap_or("temp"));
        fs::rename(&file_path, &in_process_path).map_err(|e| FoldError::Io(e))?;
        println!("[fold] Moved to in-process: {}", in_process_path);
        
        // Create heartbeat file for this processing job
        let heartbeat_path = file_handler::create_heartbeat(&in_process_path)?;
        
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
                // Update heartbeat every 100k orthos
                file_handler::touch_heartbeat(&heartbeat_path)?;
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
        let archive_path = file_handler::get_archive_path(&in_process_path);
        file_handler::save_archive(&archive_path, &interner, &mut results, &results_path)?;
        
        println!("[fold] Archive saved: {}", archive_path);
        
        // Delete the heartbeat file
        if std::path::Path::new(&heartbeat_path).exists() {
            fs::remove_file(&heartbeat_path).map_err(|e| FoldError::Io(e))?;
        }
        
        // Delete the in-process .txt file
        fs::remove_file(&in_process_path).map_err(|e| FoldError::Io(e))?;
        println!("[fold] In-process file deleted");
    }
    
    println!("\n[fold] ========================================");
    println!("[fold] All files processed successfully");
    println!("[fold] ========================================");
    
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
    fn test_calculate_score() {
        let ortho = Ortho::new(1);
        let (volume, fullness) = calculate_score(&ortho);
        // Empty ortho with dims [2,2] has volume (2-1)*(2-1) = 1
        assert_eq!(volume, 1);
        // All 4 slots are None
        assert_eq!(fullness, 0);
    }
}
