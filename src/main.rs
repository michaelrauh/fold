use fold::{disk_backed_queue::DiskBackedQueue, file_handler, interner::Interner, memory_config::MemoryConfig, ortho::Ortho, seen_tracker::SeenTracker, FoldError};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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
    
    // Create .txt.work folder in in_process directory
    let filename = Path::new(&file_path).file_stem().unwrap_or_default();
    let work_folder = format!("{}/{}.txt.work", in_process_dir, filename.to_str().unwrap_or("temp"));
    fs::create_dir_all(&work_folder).map_err(|e| FoldError::Io(e))?;
    
    // Move txt file to source.txt inside work folder
    let source_txt_path = format!("{}/source.txt", work_folder);
    fs::rename(&file_path, &source_txt_path).map_err(|e| FoldError::Io(e))?;
    println!("[fold] Moved to work folder: {}", work_folder);
    
    // Create heartbeat file inside work folder
    let heartbeat_path = file_handler::create_heartbeat(&work_folder)?;
    
    // Read text from source.txt
    let text = fs::read_to_string(&source_txt_path)
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
        filename.to_str().unwrap_or("temp"));
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
    
    // Create lineage tracking for this text file (just the filename)
    let lineage = format!("\"{}\"", filename.to_str().unwrap_or("unknown"));
    
    // Create timestamp-based archive name in INPUT directory
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .as_secs();
    let archive_path = format!("{}/archive_{}.bin", input_dir, timestamp);
    file_handler::save_archive(&archive_path, &interner, &mut results, &results_path, Some(&best_ortho), &lineage)?;
    
    println!("[fold] Archive saved to input: {}", archive_path);
    
    // Delete the work folder (including heartbeat and source.txt)
    fs::remove_dir_all(&work_folder).map_err(|e| FoldError::Io(e))?;
    println!("[fold] Work folder deleted");
    
    Ok(())
}

fn merge_archives(archive_a_path: &str, archive_b_path: &str, input_dir: &str, in_process_dir: &str) -> Result<(), FoldError> {
    // Move both archives from input to in_process for mutual exclusion
    let archive_a_name = Path::new(archive_a_path).file_name().unwrap().to_str().unwrap();
    let archive_b_name = Path::new(archive_b_path).file_name().unwrap().to_str().unwrap();
    let work_a_path = format!("{}/{}", in_process_dir, archive_a_name);
    let work_b_path = format!("{}/{}", in_process_dir, archive_b_name);
    
    println!("[fold] Moving archives to in_process for merging...");
    fs::rename(archive_a_path, &work_a_path).map_err(FoldError::Io)?;
    fs::rename(archive_b_path, &work_b_path).map_err(FoldError::Io)?;
    println!("[fold] Loading interners...");
    
    // Load both interners from work paths
    let interner_a = file_handler::load_interner(&work_a_path)?;
    let interner_b = file_handler::load_interner(&work_b_path)?;
    
    println!("[fold] Interner A: vocab size={}, version={}", 
             interner_a.vocabulary().len(), interner_a.version());
    println!("[fold] Interner B: vocab size={}, version={}", 
             interner_b.vocabulary().len(), interner_b.version());
    
    // Find differences from a to b and from b to a
    let impacted_a = interner_a.impacted_keys(&interner_b);
    let impacted_b = interner_b.impacted_keys(&interner_a);
    
    println!("[fold] Impacted keys in A: {}", impacted_a.len());
    println!("[fold] Impacted keys in B: {}", impacted_b.len());
    
    // Create merged interner using proper merge method
    let merged_interner = interner_a.merge(&interner_b);
    let new_version = merged_interner.version();
    
    println!("[fold] Merged interner: version={}, vocab size={}", 
             new_version, merged_interner.vocabulary().len());
    
    // Build vocabulary mapping for A and B
    let vocab_map_a = build_vocab_mapping(interner_a.vocabulary(), merged_interner.vocabulary());
    let vocab_map_b = build_vocab_mapping(interner_b.vocabulary(), merged_interner.vocabulary());
    
    println!("[fold] Vocab mappings created");
    
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
    
    // Create heartbeat for merge operation early
    let heartbeat_path = format!("{}/merge_{}.heartbeat", in_process_dir, std::process::id());
    file_handler::touch_heartbeat(&heartbeat_path)?;
    
    println!("[fold] Remapping and processing orthos from archives...");
    
    // Process archive A results: remap ALL orthos to results
    // Add impacted orthos to work queue for further processing
    let results_a_path = file_handler::get_results_path(&work_a_path);
    let mut results_a = DiskBackedQueue::new_from_path(&results_a_path, memory_config.queue_buffer_size)?;
    
    let mut total_from_a = 0;
    let mut impacted_from_a = 0;
    while let Some(ortho) = results_a.pop()? {
        // Remap the ortho to new vocabulary
        if let Some(remapped) = ortho.remap(&vocab_map_a, new_version) {
            let remapped_id = remapped.id();
            if !tracker.contains(&remapped_id) {
                tracker.insert(remapped_id);
                // Add ALL remapped orthos to merged results
                merged_results.push(remapped.clone())?;
                total_from_a += 1;
                
                // Log progress every 10k orthos and keep heartbeat fresh
                if total_from_a % 10000 == 0 {
                    println!("[fold] Remapping archive A: {} orthos processed", total_from_a);
                    file_handler::touch_heartbeat(&heartbeat_path)?;
                }
                
                // Check if this ortho is impacted - if so, add to work queue
                if is_ortho_impacted(&ortho, &impacted_a) {
                    work_queue.push(remapped)?;
                    impacted_from_a += 1;
                }
            }
        }
    }
    
    println!("[fold] Archive A: {} total remapped, {} impacted added to work queue", total_from_a, impacted_from_a);
    
    // Process archive B results: remap ALL orthos to results
    // Add impacted orthos to work queue for further processing
    let results_b_path = file_handler::get_results_path(archive_b_path);
    let mut results_b = DiskBackedQueue::new_from_path(&results_b_path, memory_config.queue_buffer_size)?;
    
    let mut total_from_b = 0;
    let mut impacted_from_b = 0;
    while let Some(ortho) = results_b.pop()? {
        // Remap the ortho to new vocabulary
        if let Some(remapped) = ortho.remap(&vocab_map_b, new_version) {
            let remapped_id = remapped.id();
            if !tracker.contains(&remapped_id) {
                tracker.insert(remapped_id);
                // Add ALL remapped orthos to merged results
                merged_results.push(remapped.clone())?;
                total_from_b += 1;
                
                // Log progress every 10k orthos and keep heartbeat fresh
                if total_from_b % 10000 == 0 {
                    println!("[fold] Remapping archive B: {} orthos processed", total_from_b);
                    file_handler::touch_heartbeat(&heartbeat_path)?;
                }
                
                // Check if this ortho is impacted - if so, add to work queue
                if is_ortho_impacted(&ortho, &impacted_b) {
                    work_queue.push(remapped)?;
                    impacted_from_b += 1;
                }
            }
        }
    }
    
    println!("[fold] Archive B: {} total remapped, {} impacted added to work queue", total_from_b, impacted_from_b);
    println!("[fold] Total work queue seeds: {} (empty ortho + {} impacted)", 1 + impacted_from_a + impacted_from_b, impacted_from_a + impacted_from_b);
    
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
    
    // Load lineages from both archives and create merged lineage
    let lineage_a = file_handler::load_lineage(archive_a_path)?;
    let lineage_b = file_handler::load_lineage(archive_b_path)?;
    let merged_lineage = format!("({} {})", lineage_a, lineage_b);
    
    println!("[fold] Merged lineage: {}", merged_lineage);
    
    // Create merged archive with timestamp-based name in INPUT directory
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let merged_archive_path = format!("{}/archive_{}.bin", input_dir, timestamp);
    
    file_handler::save_archive(&merged_archive_path, &merged_interner, &mut merged_results, &results_path, Some(&best_ortho), &merged_lineage)?;
    println!("[fold] Merged archive saved to input: {}", merged_archive_path);
    
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

// Build a mapping from old vocabulary indices to new vocabulary indices
fn build_vocab_mapping(old_vocab: &[String], new_vocab: &[String]) -> Vec<usize> {
    old_vocab.iter().map(|word| {
        new_vocab.iter().position(|w| w == word)
            .expect("Word from old vocab must exist in new vocab")
    }).collect()
}

// Check if an ortho uses any of the impacted keys
fn is_ortho_impacted(ortho: &Ortho, impacted_keys: &[Vec<usize>]) -> bool {
    if impacted_keys.is_empty() {
        return false;
    }
    
    // Get the requirement phrases from the ortho
    let requirement_phrases = ortho.get_requirement_phrases();
    
    // Check if any requirement phrase matches an impacted key
    for req_phrase in &requirement_phrases {
        for impacted_key in impacted_keys {
            if req_phrase == impacted_key {
                return true;
            }
        }
    }
    
    false
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
