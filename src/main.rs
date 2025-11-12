use fold::{checkpoint_manager::CheckpointManager, disk_backed_queue::DiskBackedQueue, interner::Interner, memory_config::MemoryConfig, ortho::Ortho, seen_tracker::SeenTracker, FoldError};
use std::fs;

/// Load checkpoint and gather metrics for memory configuration
fn load_checkpoint_with_metrics(manager: &CheckpointManager) -> Result<Option<(Interner, DiskBackedQueue, SeenTracker, usize, usize)>, FoldError> {
    // Check if checkpoint exists
    let checkpoint_dir = "./fold_state/checkpoint";
    let interner_path = format!("{}/interner.bin", checkpoint_dir);
    
    if !std::path::Path::new(&interner_path).exists() {
        return Ok(None);
    }
    
    // Read interner to get its size
    let interner_bytes_data = fs::read(&interner_path).map_err(|e| FoldError::Io(e))?;
    let interner_bytes = interner_bytes_data.len();
    let (_interner, _): (Interner, usize) = bincode::decode_from_slice(&interner_bytes_data, bincode::config::standard())?;
    
    // Count results to estimate bloom/shard needs
    let results_backup = format!("{}/results_backup", checkpoint_dir);
    let result_count = if std::path::Path::new(&results_backup).exists() {
        // Quick scan to count items
        let temp_queue = DiskBackedQueue::new_from_path(&results_backup, 1000)?;
        temp_queue.len()
    } else {
        0
    };
    
    println!("[fold] Checkpoint metrics - Interner: {} MB, Results: {}", 
             interner_bytes / 1_048_576, result_count);
    
    // Now calculate memory config and do full load
    let memory_config = MemoryConfig::calculate(interner_bytes, result_count);
    let loaded = manager.load(&memory_config)?;
    
    match loaded {
        Some((int, res, trk)) => Ok(Some((int, res, trk, interner_bytes, result_count))),
        None => Ok(None),
    }
}

fn calculate_score(ortho: &Ortho) -> (usize, usize) {
    let volume = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    (volume, fullness)
}

/// Check if an ortho has potential to generate any new (unseen) children.
/// Returns true if at least one completion would generate a child not in the tracker.
fn has_unseen_children(ortho: &Ortho, interner: &Interner, tracker: &mut SeenTracker) -> bool {
    let (forbidden, required) = ortho.get_requirements();
    let completions = interner.intersect(&required, &forbidden);
    let version = ortho.version();
    
    // Check if any completion would produce an unseen child
    for completion in completions {
        let children = ortho.add(completion, version);
        for child in children {
            let child_id = child.id();
            if !tracker.contains(&child_id) {
                return true;
            }
        }
    }
    
    false
}

fn main() -> Result<(), FoldError> {
    let input_dir = "./fold_state/input";
    
    println!("[fold] Starting fold processing");
    println!("[fold] Input directory: {}", input_dir);
    
    let checkpoint_manager = CheckpointManager::new();
    
    // Try to load checkpoint and calculate memory configuration
    let (mut interner, mut all_results, mut tracker, memory_config, mut global_best, mut global_best_score) = if let Some((int, res, trk, interner_bytes, result_count)) = load_checkpoint_with_metrics(&checkpoint_manager)? {
        println!("[fold] Resuming from checkpoint");
        println!("[fold] Results queue size: {}", res.len());
        println!("[fold] Seen orthos: {}", trk.len());
        
        // Calculate memory config based on checkpoint state
        let config = MemoryConfig::calculate(interner_bytes, result_count);
        
        (Some(int), res, trk, config, Ortho::new(0), (0, 0))
    } else {
        println!("[fold] Starting fresh");
        
        // Calculate memory config with defaults for fresh start
        let config = MemoryConfig::calculate(0, 0);
        
        let fresh_tracker = SeenTracker::with_config(config.bloom_capacity, config.num_shards, config.max_shards_in_memory);
        let fresh_results = DiskBackedQueue::new_from_path("./fold_state/results", config.queue_buffer_size)?;
        (None, fresh_results, fresh_tracker, config, Ortho::new(0), (0, 0))
    };
    
    // Get all files from input directory, sorted
    let mut files = get_input_files(&input_dir)?;
    files.sort();
    
    if files.is_empty() {
        println!("[fold] No input files found in {}", input_dir);
        return Ok(());
    }
    
    println!("[fold] Found {} file(s) to process", files.len());
    
    for file_path in files {
        println!("\n[fold] Processing file: {}", file_path);
        
        let text = fs::read_to_string(&file_path)
            .map_err(|e| FoldError::Io(e))?;
        
        // Keep old interner to detect changes
        let old_interner = interner.clone();
        
        // Build or extend interner
        interner = Some(if let Some(prev) = interner {
            prev.add_text(&text)
        } else {
            Interner::from_text(&text)
        });
        
        let current_interner = interner.as_ref().unwrap();
        let version = current_interner.version();
        
        println!("[fold] Interner version: {}", version);
        println!("[fold] Vocabulary size: {}", current_interner.vocabulary().len());
        
        // Initialize work queue - use global seen orthos set
        let mut work_queue = DiskBackedQueue::new(memory_config.queue_buffer_size)?;
        
        // If interner changed, find impacted orthos from results and re-queue them
        if let Some(old_int) = old_interner {
            let impacted_keys = old_int.impacted_keys(current_interner);
            
            if !impacted_keys.is_empty() {
            println!("[fold] Interner changed: {} impacted keys detected", impacted_keys.len());
            println!("[fold] Scanning {} results for impacted orthos...", all_results.len());
            
            let mut requeued_count = 0;
            let mut scanned_count = 0;
            let total_results = all_results.len();
            
            // Create a temporary queue to consume all results and rebuild
            let mut temp_results = DiskBackedQueue::new(memory_config.queue_buffer_size)?;
            
            println!("[fold] Starting scan to identify impacted orthos...");
            
            while let Some(ortho) = all_results.pop()? {
                scanned_count += 1;
                
                if scanned_count % 1000 == 0 {
                println!("[fold] Scanning... {}/{} results ({:.1}%), requeued {} impacted orthos so far", 
                     scanned_count, total_results, 
                     (scanned_count as f64 / total_results as f64) * 100.0,
                     requeued_count);
                }
                
                // Check for optimal while streaming through results
                let score = calculate_score(&ortho);
                if score > global_best_score {
                    global_best = ortho.clone();
                    global_best_score = score;
                }
                
                let requirement_phrases = ortho.get_requirement_phrases();
                
                // Check if any requirement phrase overlaps with impacted keys
                let is_impacted = requirement_phrases.iter()
                .any(|phrase| impacted_keys.contains(phrase));
                
                if is_impacted {
                work_queue.push(ortho.clone())?;
                requeued_count += 1;
                }
                
                temp_results.push(ortho)?;
            }
            
            // Swap temp results back to all_results
            all_results = temp_results;
            
            println!("[fold] Re-queued {} impacted orthos for reprocessing", requeued_count);
            }
        }
        
        // Always seed with empty ortho
        let seed_ortho = Ortho::new(version);
        let seed_id = seed_ortho.id();
        println!("[fold] Seeding with empty ortho id={}, version={}", seed_id, version);
        
        tracker.insert(seed_id);
        if global_best_score == (0, 0) {
            global_best = seed_ortho.clone();
            global_best_score = calculate_score(&global_best);
        }
        
        work_queue.push(seed_ortho)?;
        
        // Process work queue until empty
        let mut processed_count = 0;
        while let Some(ortho) = work_queue.pop()? {
            processed_count += 1;
            
            if processed_count % 1000 == 0 {
                println!("[fold] Processed {} orthos, queue size: {}, seen: {}", 
                         processed_count, work_queue.len(), tracker.len());
            }
            
            // Get requirements from ortho
            let (forbidden, required) = ortho.get_requirements();
            
            // Get completions from interner
            let completions = current_interner.intersect(&required, &forbidden);
            
            // Generate child orthos
            for completion in completions {
                let children = ortho.add(completion, version);
                
                for child in children {
                    let child_id = child.id();
                    
                    // Use tracker for bloom-filtered deduplication check
                    if !tracker.contains(&child_id) {
                        tracker.insert(child_id);
                        
                        let candidate_score = calculate_score(&child);
                        if candidate_score > global_best_score {
                            global_best = child.clone();
                            global_best_score = candidate_score;
                        }
                        
                        all_results.push(child.clone())?;
                        
                        // Only queue if the child has potential to generate new descendants.
                        // This prevents 100% duplication periods where the queue fills with
                        // orthos whose children are all already seen. By checking ahead of time,
                        // we avoid wasting CPU cycles processing orthos that produce only duplicates.
                        if has_unseen_children(&child, current_interner, &mut tracker) {
                            work_queue.push(child)?;
                        }
                    }
                }
            }
        }
        
        println!("[fold] Finished processing file");
        println!("[fold] Total orthos seen globally: {}", tracker.len());
        
        print_optimal(&global_best, current_interner);
        
        // Save checkpoint after successful file processing
        checkpoint_manager.save(current_interner, &mut all_results)?;
        
        // Delete the processed file
        fs::remove_file(&file_path).map_err(|e| FoldError::Io(e))?;
        println!("[fold] Checkpoint saved, file deleted");
    }
    
    println!("\n[fold] All files processed");
    println!("[fold] Total results: {}", all_results.len());
    
    if let Some(final_interner) = interner {
        println!("\n[fold] ===== FINAL OPTIMAL ORTHO =====");
        print_optimal(&global_best, &final_interner);
    }
    
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
    fn test_results_queue_persistence() {
        let temp_dir = tempfile::tempdir().unwrap();
        let persist_path = temp_dir.path().join("persist");
        
        {
            let mut queue = DiskBackedQueue::new_from_path(persist_path.to_str().unwrap(), 5).unwrap();
            
            for v in 1..=10 {
                queue.push(Ortho::new(v)).unwrap();
            }
            
            queue.flush().unwrap();
            assert_eq!(queue.len(), 10);
        }
        
        // Reload from same path
        {
            let mut queue = DiskBackedQueue::new_from_path(persist_path.to_str().unwrap(), 5).unwrap();
            assert_eq!(queue.len(), 10);
            
            let first = queue.pop().unwrap().unwrap();
            assert_eq!(first.version(), 1);
        }
    }

    #[test]
    fn test_checkpoint_manager_integration() {
        let temp_dir = tempfile::tempdir().unwrap();
        let fold_state = temp_dir.path().join("fold_state");
        fs::create_dir_all(&fold_state).unwrap();
        
        let manager = CheckpointManager::with_base_dir(&fold_state);
        let interner = Interner::from_text("hello world");
        let results_path = fold_state.join("results");
        let memory_config = MemoryConfig::default_config();
        let mut results_queue = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), memory_config.queue_buffer_size).unwrap();
        
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        let id1 = ortho1.id();
        let id2 = ortho2.id();
        
        results_queue.push(ortho1).unwrap();
        results_queue.push(ortho2).unwrap();
        
        // Save checkpoint (tracker reconstructed on load)
        manager.save(&interner, &mut results_queue).unwrap();
        
        // Load checkpoint
        let result = manager.load(&memory_config).unwrap();
        assert!(result.is_some());
        
        let (loaded_interner, loaded_results, mut loaded_tracker) = result.unwrap();
        
        assert_eq!(loaded_interner.version(), interner.version());
        assert_eq!(loaded_results.len(), 2, "Should have 2 results");
        assert_eq!(loaded_tracker.len(), 2, "Tracker should have 2 IDs");
        assert!(loaded_tracker.contains(&id1));
        assert!(loaded_tracker.contains(&id2));
    }
    
    #[test]
    fn test_has_unseen_children_with_all_seen() {
        let interner = Interner::from_text("a b c");
        let mut tracker = SeenTracker::new(100);
        
        // Create an ortho with one token
        let ortho = Ortho::new(1);
        let ortho = ortho.add(0, 1).into_iter().next().unwrap(); // add token 'a'
        
        // Mark all potential children as seen
        let (forbidden, required) = ortho.get_requirements();
        let completions = interner.intersect(&required, &forbidden);
        for completion in completions {
            let children = ortho.add(completion, 1);
            for child in children {
                tracker.insert(child.id());
            }
        }
        
        // Now check that has_unseen_children returns false
        assert!(!has_unseen_children(&ortho, &interner, &mut tracker));
    }
    
    #[test]
    fn test_has_unseen_children_with_some_unseen() {
        let interner = Interner::from_text("a b c");
        let mut tracker = SeenTracker::new(100);
        
        // Create an ortho with one token
        let ortho = Ortho::new(1);
        let ortho = ortho.add(0, 1).into_iter().next().unwrap(); // add token 'a'
        
        // Don't mark any children as seen
        // has_unseen_children should return true
        assert!(has_unseen_children(&ortho, &interner, &mut tracker));
    }
}

