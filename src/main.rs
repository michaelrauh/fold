use fold::{disk_backed_queue::DiskBackedQueue, file_handler::{self, StateConfig}, interner::Interner, memory_config::MemoryConfig, ortho::Ortho, seen_tracker::SeenTracker, FoldError};
use std::path::PathBuf;

fn calculate_score(ortho: &Ortho) -> (usize, usize) {
    let volume = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    (volume, fullness)
}

fn main() -> Result<(), FoldError> {
    println!("[fold] Starting fold processing");
    
    // Check for test environment variable
    let config = if let Ok(test_dir) = std::env::var("FOLD_STATE_DIR") {
        StateConfig::custom(PathBuf::from(test_dir))
    } else {
        StateConfig::default()
    };
    
    // Initialize: setup directories and recover abandoned files
    file_handler::initialize_with_config(&config)?;
    
    // Main processing loop - two modes:
    // Mode 1: If there are 2+ result archives, merge smallest with largest
    // Mode 2: If there are not 2 results, process txt into result
    loop {
        // Check for existing archives
        let archive_pair = file_handler::get_smallest_and_largest_archives_with_config(&config)?;
        
        if let Some((smallest, largest)) = archive_pair {
            // Mode 1: Merge archives
            println!("\n[fold] ========================================");
            println!("[fold] MODE 1: Merging archives");
            println!("[fold] ========================================");
            
            println!("[fold] Merging smallest: {}", smallest);
            println!("[fold] With largest: {}", largest);
            
            merge_archives(&smallest, &largest, &config)?;
            
        } else {
            // Mode 2: Process txt file
            let txt_file = file_handler::find_txt_file_with_config(&config)?;
            
            if txt_file.is_none() {
                println!("[fold] No more .txt files to process");
                println!("[fold] All processing completed");
                break;
            }
            
            println!("\n[fold] ========================================");
            println!("[fold] MODE 2: Processing text file");
            println!("[fold] ========================================");
            
            process_txt_file(txt_file.unwrap(), &config)?;
        }
    }
    
    println!("\n[fold] ========================================");
    println!("[fold] Processing completed");
    println!("[fold] ========================================");
    
    Ok(())
}

fn process_txt_file(file_path: String, config: &StateConfig) -> Result<(), FoldError> {
    println!("[fold] Processing file: {}", file_path);
    
    // Ingest the text file (now includes reading the text)
    let ingestion = file_handler::ingest_txt_file_with_config(&file_path, config)?;
    let remaining = file_handler::count_txt_files_remaining_with_config(config)?;
    println!("[fold] Ingested file: {} ({} chunks remaining in input)", ingestion.filename, remaining);
    
    // Build interner from the text
    let interner = Interner::from_text(&ingestion.text);
    let version = interner.version();
    
    println!("[fold] Interner version: {}", version);
    println!("[fold] Vocabulary size: {}", interner.vocabulary().len());
    
    // Calculate memory config for this file
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?.len();
    let memory_config = MemoryConfig::calculate(interner_bytes, 0);
    
    // Initialize work queue for this file (isolated to work folder)
    let work_queue_path = ingestion.work_queue_path();
    let mut work_queue = DiskBackedQueue::new_from_path(&work_queue_path, memory_config.queue_buffer_size)?;
    
    // Initialize results queue for this file (disk-backed to avoid OOM)
    let results_path = ingestion.results_path();
    let mut results = DiskBackedQueue::new_from_path(&results_path, memory_config.queue_buffer_size)?;
    
    // Initialize tracker for this file (isolated to work folder)
    let seen_shards_path = ingestion.seen_shards_path();
    let mut tracker = SeenTracker::with_path(
        &seen_shards_path,
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
        
        if processed_count % 50000 == 0 {
            let remaining = file_handler::count_txt_files_remaining_with_config(config)?;
            println!("[fold] Reminder: {} chunks remaining in input", remaining);
        }
        
        if processed_count % 100000 == 0 {
            print_optimal(&best_ortho, &interner);
            // Update heartbeat every 100k orthos (zero-arity)
            ingestion.touch_heartbeat()?;
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
    
    // Save the result (using method on ingestion)
    let archive_path = ingestion.save_result(&interner, results, Some(&best_ortho))?;
    
    println!("[fold] Archive saved: {}", archive_path);
    
    // Delete the work folder (using consuming cleanup method)
    ingestion.cleanup()?;
    println!("[fold] Work folder deleted");
    
    Ok(())
}

fn merge_archives(archive_a_path: &str, archive_b_path: &str, config: &StateConfig) -> Result<(), FoldError> {
    // Ingest archives for merging
    println!("[fold] Moving archives to in_process for merging...");
    let ingestion = file_handler::ingest_archives_with_config(archive_a_path, archive_b_path, config)?;
    println!("[fold] Loading interners...");
    
    // Load both interners using method
    let (interner_a, interner_b) = ingestion.load_interners()?;
    
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
    
    // Initialize work queue and results for merge (isolated to merge work folder)
    let work_queue_path = ingestion.work_queue_path();
    let mut work_queue = DiskBackedQueue::new_from_path(&work_queue_path, memory_config.queue_buffer_size)?;
    let results_path = config.results_dir(&format!("merged_{}", std::process::id()));
    let mut merged_results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), memory_config.queue_buffer_size)?;
    
    // Initialize tracker (isolated to merge work folder)
    let seen_shards_path = ingestion.seen_shards_path();
    let mut tracker = SeenTracker::with_path(
        &seen_shards_path,
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
    
    println!("[fold] Remapping and processing orthos from archives...");
    
    // Get results paths using method
    let (results_a_path, results_b_path) = ingestion.get_results_paths();
    
    // Process archive A results: remap ALL orthos to results
    // Add impacted orthos to work queue for further processing
    let mut results_a = DiskBackedQueue::new_from_path(&results_a_path, memory_config.queue_buffer_size)?;
    
    let total_a_count = results_a.len();
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
                
                // Log progress every 10k orthos and keep heartbeat fresh (zero-arity)
                if total_from_a % 10000 == 0 {
                    let percent = (total_from_a as f64 / total_a_count as f64 * 100.0) as u32;
                    println!("[fold] Remapping archive A: {} / {} orthos processed ({}%)", total_from_a, total_a_count, percent);
                    ingestion.touch_heartbeat()?;
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
    let mut results_b = DiskBackedQueue::new_from_path(&results_b_path, memory_config.queue_buffer_size)?;
    
    let total_b_count = results_b.len();
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
                
                // Log progress every 10k orthos and keep heartbeat fresh (zero-arity)
                if total_from_b % 10000 == 0 {
                    let percent = (total_from_b as f64 / total_b_count as f64 * 100.0) as u32;
                    println!("[fold] Remapping archive B: {} / {} orthos processed ({}%)", total_from_b, total_b_count, percent);
                    ingestion.touch_heartbeat()?;
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
    
    // Process work queue
    let mut processed_count = 0;
    while let Some(ortho) = work_queue.pop()? {
        processed_count += 1;
        
        if processed_count % 1000 == 0 {
            println!("[fold] Processed {} orthos, queue size: {}, seen: {}", 
                     processed_count, work_queue.len(), tracker.len());
        }
        
        if processed_count % 50000 == 0 {
            let remaining = file_handler::count_txt_files_remaining_with_config(config)?;
            println!("[fold] Reminder: {} chunks remaining in input", remaining);
        }
        
        if processed_count % 100000 == 0 {
            print_optimal(&best_ortho, &merged_interner);
            // Touch heartbeat (zero-arity)
            ingestion.touch_heartbeat()?;
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
    
    // Load lineages using method
    let (lineage_a, lineage_b) = ingestion.load_lineages()?;
    
    println!("[fold] Merged lineage: ({} {})", lineage_a, lineage_b);
    
    // Save the merged result using method
    let merged_archive_path = ingestion.save_result(
        &merged_interner, 
        merged_results, 
        results_path.to_str().unwrap(), 
        Some(&best_ortho), 
        &lineage_a, 
        &lineage_b
    )?;
    
    println!("[fold] Merged archive saved: {}", merged_archive_path);
    
    // Delete the original archives using consuming cleanup method
    ingestion.cleanup()?;
    println!("[fold] Deleted original archives");
    
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
