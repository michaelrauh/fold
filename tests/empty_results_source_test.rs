use fold::{
    file_handler::{self, StateConfig},
    interner::Interner,
    disk_backed_queue::DiskBackedQueue,
    ortho::Ortho,
    seen_tracker::SeenTracker,
    memory_config::MemoryConfig,
    FoldError,
};
use std::fs;

/// This test demonstrates the real bug: when an interner produces no completions,
/// the results queue remains empty, and when saved, creates an archive with an 
/// empty results directory that may be skipped by save_archive's existence check.
#[test]
fn test_empty_text_produces_archive_with_no_results() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    // Create an interner from EMPTY or minimal text that produces no completions
    // An empty string produces an interner with no vocabulary
    let interner = Interner::from_text("");
    let version = interner.version();
    
    println!("Interner vocab size: {}", interner.vocabulary().len());
    
    // Simulate the processing loop from process_txt_file
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?.len();
    let memory_config = MemoryConfig::calculate(interner_bytes, 0);
    
    let work_queue_path = temp_dir.path().join("queue");
    let mut work_queue = DiskBackedQueue::new_from_path(work_queue_path.to_str().unwrap(), memory_config.queue_buffer_size)?;
    
    let results_path = temp_dir.path().join("results");
    let mut results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), memory_config.queue_buffer_size)?;
    
    let seen_shards_path = temp_dir.path().join("seen_shards");
    let mut tracker = SeenTracker::with_path(
        seen_shards_path.to_str().unwrap(),
        memory_config.bloom_capacity,
        memory_config.num_shards,
        memory_config.max_shards_in_memory,
    );
    
    // Seed with empty ortho
    let seed_ortho = Ortho::new();
    let seed_id = seed_ortho.id();
    tracker.insert(seed_id);
    
    work_queue.push(seed_ortho)?;
    
    // Process work queue - simulate the main loop
    let mut processed_count = 0;
    while let Some(ortho) = work_queue.pop()? {
        processed_count += 1;
        
        // Get requirements from ortho
        let (forbidden, required) = ortho.get_requirements();
        
        // Get completions from interner
        let completions = interner.intersect(&required, &forbidden);
        
        println!("Ortho {}: got {} completions", processed_count, completions.len());
        
        // Generate child orthos
        for completion in completions {
            let children = ortho.add(completion);
            
            for child in children {
                let child_id = child.id();
                
                if !tracker.contains(&child_id) {
                    tracker.insert(child_id);
                    
                    // This is where children would be pushed to results
                    results.push(child.clone())?;
                    work_queue.push(child)?;
                }
            }
        }
    }
    
    println!("Processing complete. Processed {} orthos, results count: {}", processed_count, results.len());
    
    // At this point, results should be EMPTY because empty interner produces no completions
    assert_eq!(results.len(), 0, "Empty interner should produce no results");
    
    // Now simulate saving the archive
    results.flush()?;
    
    // Check what's in the results directory
    println!("Results directory exists: {}", results_path.exists());
    if results_path.exists() {
        let entries: Vec<_> = fs::read_dir(&results_path)?.collect();
        println!("Results directory has {} entries", entries.len());
        
        // The directory exists but is EMPTY (no .bin files)
        assert_eq!(entries.len(), 0, "Results directory should be empty");
    }
    
    drop(results);
    
    // Now try to save this as an archive (simulating save_archive behavior)
    let archive_path = config.input_dir().join("test_archive.bin");
    fs::create_dir_all(&archive_path).map_err(|e| FoldError::Io(e))?;
    
    let archive_results_path = archive_path.join("results");
    
    // This is the critical check from save_archive
    if results_path.exists() {
        fs::rename(&results_path, &archive_results_path).map_err(|e| FoldError::Io(e))?;
    } else {
        println!("BUG: Results path doesn't exist, archive will have no results!");
    }
    
    // Save the interner
    let interner_path = archive_path.join("interner.bin");
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?;
    fs::write(&interner_path, interner_bytes).map_err(|e| FoldError::Io(e))?;
    
    // Save lineage
    let lineage_path = archive_path.join("lineage.txt");
    fs::write(&lineage_path, "\"empty_test\"").map_err(|e| FoldError::Io(e))?;
    
    // Archive has been saved - check if it has results
    if archive_results_path.exists() {
        println!("Archive has results directory");
        
        // But is it empty?
        let entries: Vec<_> = fs::read_dir(&archive_results_path)?.collect();
        println!("Archive results directory has {} files", entries.len());
        
        if entries.len() == 0 {
            println!("BUG CONFIRMED: Archive has empty results directory!");
        }
    } else {
        println!("BUG CONFIRMED: Archive has NO results directory!");
    }
    
    // Now simulate loading this archive for a merge
    let loaded_results = DiskBackedQueue::new_from_path(
        archive_results_path.to_str().unwrap(), 
        10
    )?;
    
    println!("Loaded results count: {}", loaded_results.len());
    assert_eq!(loaded_results.len(), 0, "Bug: Archive loaded with 0 results");
    
    Ok(())
}

/// Test with single-word text that might also produce minimal completions
#[test]
fn test_single_word_produces_minimal_results() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    
    // Single word should produce a small vocab
    let interner = Interner::from_text("a");
    let version = interner.version();
    
    println!("Single-word interner vocab size: {}", interner.vocabulary().len());
    
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?.len();
    let memory_config = MemoryConfig::calculate(interner_bytes, 0);
    
    let work_queue_path = temp_dir.path().join("queue");
    let mut work_queue = DiskBackedQueue::new_from_path(work_queue_path.to_str().unwrap(), memory_config.queue_buffer_size)?;
    
    let results_path = temp_dir.path().join("results");
    let mut results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), memory_config.queue_buffer_size)?;
    
    let seen_shards_path = temp_dir.path().join("seen_shards");
    let mut tracker = SeenTracker::with_path(
        seen_shards_path.to_str().unwrap(),
        memory_config.bloom_capacity,
        memory_config.num_shards,
        memory_config.max_shards_in_memory,
    );
    
    let seed_ortho = Ortho::new();
    let seed_id = seed_ortho.id();
    tracker.insert(seed_id);
    
    work_queue.push(seed_ortho)?;
    
    let mut processed_count = 0;
    while let Some(ortho) = work_queue.pop()? {
        processed_count += 1;
        
        if processed_count > 1000 {
            break; // Safety limit
        }
        
        let (forbidden, required) = ortho.get_requirements();
        let completions = interner.intersect(&required, &forbidden);
        
        for completion in completions {
            let children = ortho.add(completion);
            
            for child in children {
                let child_id = child.id();
                
                if !tracker.contains(&child_id) {
                    tracker.insert(child_id);
                    results.push(child.clone())?;
                    work_queue.push(child)?;
                }
            }
        }
    }
    
    println!("Single-word: Processed {} orthos, results count: {}", processed_count, results.len());
    
    // This one should produce SOME results
    // If it doesn't, that's also a bug scenario
    if results.len() == 0 {
        println!("WARNING: Single-word text also produced 0 results!");
    }
    
    Ok(())
}
