use fold::{checkpoint_manager::CheckpointManager, disk_backed_queue::DiskBackedQueue, interner::Interner, ortho::Ortho, seen_tracker::SeenTracker, FoldError};
use std::fs;

fn main() -> Result<(), FoldError> {
    let input_dir = "./fold_state/input";
    
    println!("[fold] Starting fold processing");
    println!("[fold] Input directory: {}", input_dir);
    
    let checkpoint_manager = CheckpointManager::new();
    
    // Try to load checkpoint
    let (mut interner, mut all_results, mut tracker) = if let Some((int, res, trk)) = checkpoint_manager.load()? {
        println!("[fold] Resuming from checkpoint");
        println!("[fold] Results queue size: {}", res.len());
        println!("[fold] Seen orthos: {}", trk.len());
        (Some(int), res, trk)
    } else {
        println!("[fold] Starting fresh");
        let fresh_tracker = SeenTracker::new(10_000_000);
        (None, DiskBackedQueue::new_from_path("./fold_state/results", 10000)?, fresh_tracker)
    };
    
    // Get all files from input directory, sorted
    let mut files = get_input_files(&input_dir)?;
    files.sort();
    
    if files.is_empty() {
        println!("[fold] No input files found in {}", input_dir);
        return Ok(());
    }
    
    println!("[fold] Found {} file(s) to process", files.len());
    
    let mut global_best: Ortho = Ortho::new(0);
    let mut global_best_score: usize = 0;
    
    for file_path in files {
        println!("\n[fold] Processing file: {}", file_path);
        
        let text = fs::read_to_string(&file_path)
            .map_err(|e| FoldError::Io(e))?;
        
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
        let mut work_queue = DiskBackedQueue::new(10000)?;
        
        // Seed with empty ortho
        let seed_ortho = Ortho::new(version);
        let seed_id = seed_ortho.id();
        println!("[fold] Seeding with ortho id={}, version={}", seed_id, version);
        
        tracker.insert(seed_id);
        if global_best_score == 0 {
            global_best = seed_ortho.clone();
            global_best_score = global_best.dims().iter().map(|x| x.saturating_sub(1)).product();
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
                        
                        let candidate_score = child.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
                        if candidate_score > global_best_score {
                            global_best = child.clone();
                            global_best_score = candidate_score;
                        }
                        
                        all_results.push(child.clone())?;
                        work_queue.push(child)?;
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
    println!("\n[fold] ===== OPTIMAL ORTHO =====");
    println!("[fold] Ortho ID: {}", ortho.id());
    println!("[fold] Version: {}", ortho.version());
    println!("[fold] Dimensions: {:?}", ortho.dims());
    println!("[fold] Score: {}", ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>());
    
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
        let mut results_queue = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), 10).unwrap();
        
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        let id1 = ortho1.id();
        let id2 = ortho2.id();
        
        results_queue.push(ortho1).unwrap();
        results_queue.push(ortho2).unwrap();
        
        // Save checkpoint (tracker reconstructed on load)
        manager.save(&interner, &mut results_queue).unwrap();
        
        // Load checkpoint
        let result = manager.load().unwrap();
        assert!(result.is_some());
        
        let (loaded_interner, loaded_results, mut loaded_tracker) = result.unwrap();
        
        assert_eq!(loaded_interner.version(), interner.version());
        assert_eq!(loaded_results.len(), 2, "Should have 2 results");
        assert_eq!(loaded_tracker.len(), 2, "Tracker should have 2 IDs");
        assert!(loaded_tracker.contains(&id1));
        assert!(loaded_tracker.contains(&id2));
    }
}
