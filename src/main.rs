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
    
    // Main processing loop - two modes:
    // Mode 1: If there are 2+ result archives, merge smallest with largest
    // Mode 2: If there are not 2 results, process txt into result
    loop {
        // Check for existing archives
        let mut archives = file_handler::find_archives(in_process_dir)?;
        
        if archives.len() >= 2 {
            // Mode 1: Merge archives
            println!("\n[fold] ========================================");
            println!("[fold] MODE 1: Merging archives");
            println!("[fold] Found {} archives", archives.len());
            println!("[fold] ========================================");
            
            // Sort by size to find smallest and largest
            archives.sort_by_key(|(_, size)| *size);
            let smallest = archives[0].0.clone();
            let largest = archives[archives.len() - 1].0.clone();
            
            println!("[fold] Merging smallest: {} (size: {})", smallest, archives[0].1);
            println!("[fold] With largest: {} (size: {})", largest, archives[archives.len() - 1].1);
            
            merge_archives(&smallest, &largest, in_process_dir)?;
            
        } else {
            // Mode 2: Process txt file
            let txt_file = file_handler::find_next_txt_file(input_dir)?;
            
            if txt_file.is_none() {
                println!("[fold] No more .txt files to process");
                if archives.is_empty() {
                    println!("[fold] No archives remaining");
                } else {
                    println!("[fold] {} archive(s) remaining", archives.len());
                }
                break;
            }
            
            println!("\n[fold] ========================================");
            println!("[fold] MODE 2: Processing text file");
            println!("[fold] ========================================");
            
            process_txt_file(txt_file.unwrap(), input_dir, in_process_dir)?;
        }
    }
    
    println!("\n[fold] ========================================");
    println!("[fold] All processing completed");
    println!("[fold] ========================================");
    
    Ok(())
}

fn process_txt_file(file_path: String, _input_dir: &str, in_process_dir: &str) -> Result<(), FoldError> {
    println!("[fold] Processing file: {}", file_path);
    
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
        Path::new(&in_process_path).file_stem().unwrap_or_default().to_str().unwrap_or("temp"));
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
    
    Ok(())
}

fn merge_archives(archive_a_path: &str, archive_b_path: &str, in_process_dir: &str) -> Result<(), FoldError> {
    println!("[fold] Loading interners...");
    
    // Load both interners
    let interner_a = file_handler::load_interner(archive_a_path)?;
    let interner_b = file_handler::load_interner(archive_b_path)?;
    
    println!("[fold] Interner A: vocab size={}", interner_a.vocabulary().len());
    println!("[fold] Interner B: vocab size={}", interner_b.vocabulary().len());
    
    // Find differences from a to b and from b to a
    let impacted_a = interner_a.impacted_keys(&interner_b);
    let impacted_b = interner_b.impacted_keys(&interner_a);
    
    println!("[fold] Impacted keys in A: {}", impacted_a.len());
    println!("[fold] Impacted keys in B: {}", impacted_b.len());
    
    // Create merged interner
    let merged_interner = interner_a.add_text(&format!("{}", interner_b.vocabulary().join(" ")));
    let new_version = merged_interner.version();
    
    println!("[fold] Merged interner: version={}, vocab size={}", 
             new_version, merged_interner.vocabulary().len());
    
    // Calculate memory config
    let interner_bytes = bincode::encode_to_vec(&merged_interner, bincode::config::standard())?.len();
    let memory_config = MemoryConfig::calculate(interner_bytes, 0);
    
    // Initialize work queue and results for merge
    let mut work_queue = DiskBackedQueue::new(memory_config.queue_buffer_size)?;
    let results_path = format!("./fold_state/results_merged_{}", std::process::id());
    let mut merged_results = DiskBackedQueue::new_from_path(&results_path, memory_config.queue_buffer_size)?;
    
    // Initialize tracker
    let mut tracker = SeenTracker::with_config(
        memory_config.bloom_capacity,
        memory_config.num_shards,
        memory_config.max_shards_in_memory,
    );
    
    // Seed with empty ortho
    let seed_ortho = Ortho::new(new_version);
    let seed_id = seed_ortho.id();
    tracker.insert(seed_id);
    let mut best_ortho = seed_ortho.clone();
    let mut best_score = calculate_score(&best_ortho);
    work_queue.push(seed_ortho)?;
    
    println!("[fold] Remapping and finding impacted orthos from archives...");
    
    // TODO: Stream through results from archive A, remap, check if impacted, add to queue
    // TODO: Stream through results from archive B, remap, check if impacted, add to queue
    // For now, this is a simplified version that just seeds with empty
    
    println!("[fold] Processing merged ortho space...");
    
    // Create heartbeat for merge operation
    let heartbeat_path = format!("{}/merge_{}.heartbeat", in_process_dir, std::process::id());
    file_handler::touch_heartbeat(&heartbeat_path)?;
    
    // Process work queue
    let mut processed_count = 0;
    while let Some(ortho) = work_queue.pop()? {
        processed_count += 1;
        
        if processed_count % 1000 == 0 {
            println!("[fold] Processed {} orthos, queue size: {}, seen: {}", 
                     processed_count, work_queue.len(), tracker.len());
        }
        
        if processed_count % 100000 == 0 {
            print_optimal(&best_ortho, &merged_interner);
            file_handler::touch_heartbeat(&heartbeat_path)?;
        }
        
        let (forbidden, required) = ortho.get_requirements();
        let completions = merged_interner.intersect(&required, &forbidden);
        
        for completion in completions {
            let children = ortho.add(completion, new_version);
            
            for child in children {
                let child_id = child.id();
                
                if !tracker.contains(&child_id) {
                    tracker.insert(child_id);
                    
                    let candidate_score = calculate_score(&child);
                    if candidate_score > best_score {
                        best_ortho = child.clone();
                        best_score = candidate_score;
                    }
                    
                    merged_results.push(child.clone())?;
                    work_queue.push(child)?;
                }
            }
        }
    }
    
    println!("[fold] Merge complete. Total orthos: {}", merged_results.len());
    print_optimal(&best_ortho, &merged_interner);
    
    // Create merged archive
    let archive_name_a = Path::new(archive_a_path).file_stem().unwrap_or_default().to_str().unwrap_or("a");
    let archive_name_b = Path::new(archive_b_path).file_stem().unwrap_or_default().to_str().unwrap_or("b");
    let merged_archive_path = format!("{}/merged_{}_{}.bin", in_process_dir, archive_name_a, archive_name_b);
    
    file_handler::save_archive(&merged_archive_path, &merged_interner, &mut merged_results, &results_path)?;
    println!("[fold] Merged archive saved: {}", merged_archive_path);
    
    // Clean up heartbeat
    if Path::new(&heartbeat_path).exists() {
        fs::remove_file(&heartbeat_path).map_err(|e| FoldError::Io(e))?;
    }
    
    // Delete the original archives
    if Path::new(archive_a_path).exists() {
        fs::remove_dir_all(archive_a_path).map_err(|e| FoldError::Io(e))?;
        println!("[fold] Deleted archive: {}", archive_a_path);
    }
    if Path::new(archive_b_path).exists() {
        fs::remove_dir_all(archive_b_path).map_err(|e| FoldError::Io(e))?;
        println!("[fold] Deleted archive: {}", archive_b_path);
    }
    
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
