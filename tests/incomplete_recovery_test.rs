use fold::{
    file_handler::{self, StateConfig},
    interner::Interner,
    disk_backed_queue::DiskBackedQueue,
    ortho::Ortho,
    FoldError,
};
use std::fs;

/// Test that reproduces the actual bug: merge is interrupted, leaving orphaned
/// results_merged_* directories and merge_*.work folders. When recovery happens,
/// these aren't cleaned up, and subsequent processing creates archives with no results.
#[test]
fn test_incomplete_merge_recovery_causes_empty_archives() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    println!("=== STEP 1: Create two archives with results ===");
    
    // Create archive A with results
    let archive_a_path = config.input_dir().join("archive_a.bin");
    fs::create_dir_all(&archive_a_path)?;
    
    let interner_a = Interner::from_text("hello world");
    let interner_bytes = bincode::encode_to_vec(&interner_a, bincode::config::standard())?;
    fs::write(archive_a_path.join("interner.bin"), interner_bytes)?;
    fs::write(archive_a_path.join("lineage.txt"), "\"test_a\"")?;
    fs::write(archive_a_path.join("heartbeat"), "1234567890")?;
    
    let archive_a_results = archive_a_path.join("results");
    let mut results_a = DiskBackedQueue::new_from_path(archive_a_results.to_str().unwrap(), 10)?;
    results_a.push(Ortho::new())?;
    results_a.flush()?;
    drop(results_a);
    
    let files_a: Vec<_> = fs::read_dir(&archive_a_results)?.collect();
    println!("Archive A has {} result files", files_a.len());
    assert!(files_a.len() > 0);
    
    // Create archive B with results
    let archive_b_path = config.input_dir().join("archive_b.bin");
    fs::create_dir_all(&archive_b_path)?;
    
    let interner_b = Interner::from_text("foo bar");
    let interner_bytes = bincode::encode_to_vec(&interner_b, bincode::config::standard())?;
    fs::write(archive_b_path.join("interner.bin"), interner_bytes)?;
    fs::write(archive_b_path.join("lineage.txt"), "\"test_b\"")?;
    fs::write(archive_b_path.join("heartbeat"), "1234567890")?;
    
    let archive_b_results = archive_b_path.join("results");
    let mut results_b = DiskBackedQueue::new_from_path(archive_b_results.to_str().unwrap(), 10)?;
    results_b.push(Ortho::new())?;
    results_b.flush()?;
    drop(results_b);
    
    let files_b: Vec<_> = fs::read_dir(&archive_b_results)?.collect();
    println!("Archive B has {} result files", files_b.len());
    assert!(files_b.len() > 0);
    
    println!("\n=== STEP 2: Simulate merge starting (moves archives to in_process) ===");
    
    // Ingest archives for merge
    let ingestion = file_handler::ingest_archives_with_config(
        archive_a_path.to_str().unwrap(),
        archive_b_path.to_str().unwrap(),
        &config
    )?;
    
    // Create a results_merged directory (simulating merge in progress)
    let fake_pid = 12345;
    let results_merged_path = config.base_dir.join(format!("results_merged_{}", fake_pid));
    fs::create_dir_all(&results_merged_path)?;
    
    // Add some files to it (partial results)
    let mut partial_results = DiskBackedQueue::new_from_path(results_merged_path.to_str().unwrap(), 10)?;
    partial_results.push(Ortho::new())?;
    partial_results.flush()?;
    drop(partial_results);
    
    println!("Created orphaned results_merged_{} directory", fake_pid);
    
    // The merge_*.work folder already exists from ingestion
    // Find it by listing in_process directory
    let in_process_entries: Vec<_> = fs::read_dir(&config.in_process_dir())?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    
    let merge_work_folder = in_process_entries.iter()
        .find(|name| name.starts_with("merge_") && name.ends_with(".work"))
        .expect("Should have merge work folder")
        .clone();
    
    println!("Merge work folder exists: {}", merge_work_folder);
    
    // Simulate crash - don't call cleanup, just drop ingestion
    drop(ingestion);
    
    println!("\n=== STEP 3: Simulate crash and recovery ===");
    
    // Mark heartbeats as stale by setting old timestamps
    use std::time::UNIX_EPOCH;
    let old_time = UNIX_EPOCH + std::time::Duration::from_secs(1000000000);
    let in_process_dir = config.in_process_dir();
    for entry in fs::read_dir(&in_process_dir)? {
        let entry = entry?;
        let heartbeat = entry.path().join("heartbeat");
        if heartbeat.exists() {
            // Write content and set old modification time
            fs::write(&heartbeat, "1000000000")?;
            filetime::set_file_mtime(&heartbeat, filetime::FileTime::from_system_time(old_time))
                .map_err(|e| FoldError::Io(e))?;
        }
    }
    
    // Now run recovery (this is what happens on restart)
    file_handler::initialize_with_config(&config)?;
    
    println!("\n=== STEP 4: Check what recovery left behind ===");
    
    // Check if archives were recovered to input
    let recovered_a = config.input_dir().join("archive_a.bin");
    let recovered_b = config.input_dir().join("archive_b.bin");
    
    if recovered_a.exists() {
        println!("✓ Archive A recovered to input");
        let results = recovered_a.join("results");
        if results.exists() {
            let files: Vec<_> = fs::read_dir(&results)?.collect();
            println!("  Archive A has {} result files", files.len());
        }
    }
    
    if recovered_b.exists() {
        println!("✓ Archive B recovered to input");
        let results = recovered_b.join("results");
        if results.exists() {
            let files: Vec<_> = fs::read_dir(&results)?.collect();
            println!("  Archive B has {} result files", files.len());
        }
    }
    
    // Check if merge work folder was cleaned up
    let merge_work_path = config.in_process_dir().join(&merge_work_folder);
    if merge_work_path.exists() {
        println!("✗ BUG: Merge work folder NOT cleaned up: {}", merge_work_folder);
    } else {
        println!("✓ Merge work folder cleaned up");
    }
    
    // Check if results_merged directory was cleaned up
    if results_merged_path.exists() {
        println!("✗ BUG: Orphaned results_merged_{} NOT cleaned up", fake_pid);
        let files: Vec<_> = fs::read_dir(&results_merged_path)?.collect();
        println!("  It has {} files that will never be used", files.len());
    } else {
        println!("✓ results_merged_{} cleaned up", fake_pid);
    }
    
    println!("\n=== STEP 5: Simulate processing recovered archives again ===");
    
    // This is what would happen when archives are picked up again
    // With the bug, the old results_merged_12345 exists but a new merge
    // will use a different PID, causing path mismatch
    
    let new_pid = 67890;
    let new_results_path = config.base_dir.join(format!("results_merged_{}", new_pid));
    
    println!("New merge would use: results_merged_{}", new_pid);
    println!("Old orphaned dir is: results_merged_{}", fake_pid);
    
    if results_merged_path.exists() && !new_results_path.exists() {
        println!("\n✗ BUG CONFIRMED:");
        println!("  - Old results_merged_{} has files but wrong PID", fake_pid);
        println!("  - New merge uses results_merged_{} (empty/non-existent)", new_pid);
        println!("  - save_archive looks for new path, finds nothing");
        println!("  - Archive saved with NO results directory!");
        
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Recovery incomplete: orphaned merge artifacts cause empty archives"
        )));
    }
    
    Ok(())
}

/// Test that recovery SHOULD clean up all merge artifacts
#[test]
fn test_recovery_should_clean_merge_artifacts() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    // Create some orphaned merge artifacts that SHOULD be cleaned up
    
    // 1. Orphaned merge_*.work folders
    let merge_work_1 = config.in_process_dir().join("merge_12345.work");
    fs::create_dir_all(&merge_work_1)?;
    let heartbeat_1 = merge_work_1.join("heartbeat");
    fs::write(&heartbeat_1, "1000000000")?;
    // Set file modification time to be very old (simulating stale heartbeat)
    use std::time::UNIX_EPOCH;
    let old_time = UNIX_EPOCH + std::time::Duration::from_secs(1000000000);
    filetime::set_file_mtime(&heartbeat_1, filetime::FileTime::from_system_time(old_time))
        .map_err(|e| FoldError::Io(e))?;
    fs::create_dir_all(merge_work_1.join("queue"))?;
    fs::create_dir_all(merge_work_1.join("seen_shards"))?;
    
    let merge_work_2 = config.in_process_dir().join("merge_67890.work");
    fs::create_dir_all(&merge_work_2)?;
    let heartbeat_2 = merge_work_2.join("heartbeat");
    fs::write(&heartbeat_2, "1000000000")?;
    filetime::set_file_mtime(&heartbeat_2, filetime::FileTime::from_system_time(old_time))
        .map_err(|e| FoldError::Io(e))?;
    
    // 2. Orphaned results_merged_* directories
    let results_merged_1 = config.base_dir.join("results_merged_11111");
    fs::create_dir_all(&results_merged_1)?;
    let mut queue1 = DiskBackedQueue::new_from_path(results_merged_1.to_str().unwrap(), 10)?;
    queue1.push(Ortho::new())?;
    queue1.flush()?;
    drop(queue1);
    
    let results_merged_2 = config.base_dir.join("results_merged_22222");
    fs::create_dir_all(&results_merged_2)?;
    
    println!("Created orphaned artifacts:");
    println!("  - merge_12345.work");
    println!("  - merge_67890.work");
    println!("  - results_merged_11111");
    println!("  - results_merged_22222");
    
    // Run recovery
    file_handler::initialize_with_config(&config)?;
    
    println!("\nAfter recovery:");
    
    // Check what remains
    let mut issues = Vec::new();
    
    if merge_work_1.exists() {
        issues.push("merge_12345.work still exists");
    }
    if merge_work_2.exists() {
        issues.push("merge_67890.work still exists");
    }
    if results_merged_1.exists() {
        issues.push("results_merged_11111 still exists");
    }
    if results_merged_2.exists() {
        issues.push("results_merged_22222 still exists");
    }
    
    if !issues.is_empty() {
        println!("✗ Issues found:");
        for issue in &issues {
            println!("  - {}", issue);
        }
        println!("\nRecovery should clean up ALL merge artifacts!");
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Recovery doesn't clean up merge artifacts"
        )));
    }
    
    println!("✓ All merge artifacts cleaned up correctly");
    
    Ok(())
}

/// Test that recovery DOES NOT delete active merge artifacts with fresh heartbeats
#[test]
fn test_recovery_preserves_active_merge() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    let fake_pid = 99999;
    
    // Create an active merge_*.work folder with FRESH heartbeat
    let merge_work = config.in_process_dir().join(format!("merge_{}.work", fake_pid));
    fs::create_dir_all(&merge_work)?;
    let heartbeat = merge_work.join("heartbeat");
    fs::write(&heartbeat, "active")?;
    // Don't set old mtime - keep it fresh (just created)
    fs::create_dir_all(merge_work.join("queue"))?;
    
    // Create corresponding results_merged_* directory (active merge in progress)
    let results_merged = config.base_dir.join(format!("results_merged_{}", fake_pid));
    fs::create_dir_all(&results_merged)?;
    let mut queue = DiskBackedQueue::new_from_path(results_merged.to_str().unwrap(), 10)?;
    queue.push(Ortho::new())?;
    queue.flush()?;
    drop(queue);
    
    println!("Created active merge artifacts:");
    println!("  - merge_{}.work (fresh heartbeat)", fake_pid);
    println!("  - results_merged_{}", fake_pid);
    
    // Run recovery (should NOT touch active work)
    file_handler::initialize_with_config(&config)?;
    
    println!("\nAfter recovery:");
    
    // Both should still exist
    if !merge_work.exists() {
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Active merge work folder was incorrectly deleted!"
        )));
    }
    println!("✓ Active merge work folder preserved");
    
    if !results_merged.exists() {
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Active results_merged directory was incorrectly deleted!"
        )));
    }
    println!("✓ Active results_merged directory preserved");
    
    // Verify results are still there
    let files: Vec<_> = fs::read_dir(&results_merged)?.collect();
    if files.is_empty() {
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Active merge results were lost!"
        )));
    }
    println!("✓ Active merge results intact ({} files)", files.len());
    
    Ok(())
}

