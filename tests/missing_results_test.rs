use fold::{
    file_handler::{self, StateConfig},
    interner::Interner,
    disk_backed_queue::DiskBackedQueue,
    ortho::Ortho,
    FoldError,
};
use std::fs;
use std::path::PathBuf;

/// Test that archives without results directories are handled correctly
/// 
/// This test reproduces the bug where:
/// 1. An archive is saved with a results path that doesn't exist
/// 2. save_archive silently skips the results directory (line 255 check)
/// 3. When merging, the archive has no results directory
/// 4. Opening the results path creates an empty DiskBackedQueue
/// 5. The TUI shows 0 results for one side of the merge
#[test]
fn test_archive_without_results_causes_empty_merge_side() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    // Initialize
    file_handler::initialize_with_config(&config)?;
    
    // Create an interner
    let interner = Interner::from_text("hello world");
    
    // Create a results queue in a NON-EXISTENT directory
    // This simulates the bug where results_path doesn't exist
    let non_existent_results_path = temp_dir.path().join("non_existent_results");
    
    // Create an empty DiskBackedQueue but don't push anything
    // and then delete the directory to simulate the missing results case
    let mut results = DiskBackedQueue::new_from_path(
        non_existent_results_path.to_str().unwrap(), 
        10
    )?;
    
    // Add an ortho to verify it has content initially
    let ortho = Ortho::new();
    results.push(ortho.clone())?;
    
    // Flush and drop to close handles
    results.flush()?;
    drop(results);
    
    // NOW DELETE the results directory to simulate the bug condition
    if non_existent_results_path.exists() {
        fs::remove_dir_all(&non_existent_results_path)?;
    }
    
    // Try to save an archive with the non-existent results path
    let archive_path = config.input_dir().join("test_archive.bin");
    
    // This is what happens in file_handler::save_archive
    fs::create_dir_all(&archive_path).map_err(|e| FoldError::Io(e))?;
    
    let archive_results_path = archive_path.join("results");
    
    // This is the bug - if results_path doesn't exist, it's silently skipped!
    if non_existent_results_path.exists() {
        fs::rename(&non_existent_results_path, &archive_results_path).map_err(|e| FoldError::Io(e))?;
    }
    
    // Save the interner
    let interner_path = archive_path.join("interner.bin");
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?;
    fs::write(&interner_path, interner_bytes).map_err(|e| FoldError::Io(e))?;
    
    // Save lineage
    let lineage_path = archive_path.join("lineage.txt");
    fs::write(&lineage_path, "\"test\"").map_err(|e| FoldError::Io(e))?;
    
    // Now verify the bug: the archive has NO results directory
    assert!(!archive_results_path.exists(), 
        "Bug confirmed: Archive was saved without results directory!");
    
    // When we try to open this archive for merging, what happens?
    let loaded_results_path = archive_path.join("results");
    
    // This will create an EMPTY queue because the directory doesn't exist
    let loaded_results = DiskBackedQueue::new_from_path(
        loaded_results_path.to_str().unwrap(), 
        10
    )?;
    
    // This is the bug - we see 0 results even though the original had 1!
    assert_eq!(loaded_results.len(), 0, 
        "Bug: DiskBackedQueue created from non-existent directory has 0 length");
    
    println!("Bug confirmed: Archive without results directory shows 0 results in merge!");
    
    Ok(())
}

/// Test the correct behavior - archives should always have results
#[test]
fn test_archive_should_require_results_directory() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    
    // Create an interner
    let interner = Interner::from_text("hello world");
    
    // Create a results queue with actual content
    let results_path = temp_dir.path().join("results");
    let mut results = DiskBackedQueue::new_from_path(
        results_path.to_str().unwrap(), 
        10
    )?;
    
    // Add orthos
    results.push(Ortho::new())?;
    results.push(Ortho::new())?;
    
    results.flush()?;
    drop(results);
    
    // Verify results directory exists
    assert!(results_path.exists(), "Results directory should exist");
    
    // Save archive
    let archive_path = temp_dir.path().join("test_archive.bin");
    fs::create_dir_all(&archive_path).map_err(|e| FoldError::Io(e))?;
    
    let archive_results_path = archive_path.join("results");
    
    // This should succeed
    fs::rename(&results_path, &archive_results_path).map_err(|e| FoldError::Io(e))?;
    
    // Save the interner
    let interner_path = archive_path.join("interner.bin");
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?;
    fs::write(&interner_path, interner_bytes).map_err(|e| FoldError::Io(e))?;
    
    // Verify archive has results
    assert!(archive_results_path.exists(), "Archive should have results directory");
    
    // Load and verify
    let loaded_results = DiskBackedQueue::new_from_path(
        archive_results_path.to_str().unwrap(), 
        10
    )?;
    
    assert_eq!(loaded_results.len(), 2, "Should load all results");
    
    Ok(())
}
