use fold::{
    file_handler::{self, StateConfig},
    disk_backed_queue::DiskBackedQueue,
    ortho::Ortho,
    FoldError,
};
use std::fs;

/// Test what happens when save_archive is passed a DIFFERENT path than where
/// the DiskBackedQueue actually stored its files
#[test]
fn test_save_with_wrong_results_path() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    
    // Create a DiskBackedQueue at one path
    let actual_results_path = temp_dir.path().join("actual_results");
    let mut results = DiskBackedQueue::new_from_path(actual_results_path.to_str().unwrap(), 10)?;
    
    results.push(Ortho::new())?;
    results.push(Ortho::new())?;
    results.push(Ortho::new())?;
    
    println!("Created DiskBackedQueue at: {}", actual_results_path.display());
    
    // Flush to create files
    results.flush()?;
    
    // Verify files exist at actual path
    let files_at_actual: Vec<_> = fs::read_dir(&actual_results_path)?.collect();
    println!("Files at actual path: {}", files_at_actual.len());
    assert!(files_at_actual.len() > 0, "Should have files at actual path");
    
    drop(results);
    
    // Now simulate save_archive being called with a DIFFERENT path
    let wrong_results_path = temp_dir.path().join("wrong_results");
    println!("save_archive will look for: {}", wrong_results_path.display());
    
    // This is what save_archive does
    let archive_path = temp_dir.path().join("archive.bin");
    fs::create_dir_all(&archive_path)?;
    
    let archive_results_path = archive_path.join("results");
    
    // The critical check from save_archive
    if wrong_results_path.exists() {
        println!("Wrong path exists, renaming...");
        fs::rename(&wrong_results_path, &archive_results_path)?;
    } else {
        println!("BUG REPRODUCED: Wrong path doesn't exist, skipping rename!");
        println!("  Actual files are at: {}", actual_results_path.display());
        println!("  save_archive looked for: {}", wrong_results_path.display());
    }
    
    // Check archive results
    if archive_results_path.exists() {
        let files: Vec<_> = fs::read_dir(&archive_results_path)?.collect();
        println!("Archive has {} files", files.len());
        
        if files.len() == 0 {
            println!("Archive has empty results directory!");
        }
    } else {
        println!("Archive has NO results directory - it was skipped!");
    }
    
    // The archive would be saved without results!
    // Let's verify the actual files are still at the original location
    let files_still_there: Vec<_> = fs::read_dir(&actual_results_path)?.collect();
    println!("\nFiles still at actual path: {}", files_still_there.len());
    
    println!("\nBUG CONFIRMED: Path mismatch causes archives with no results!");
    
    Ok(())
}

/// Test if there's a way the stored path variable could differ from the ingestion method call
#[test]
fn test_stored_path_vs_method_call() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    let txt_file_path = temp_dir.path().join("test.txt");
    fs::write(&txt_file_path, "hello world test")?;
    
    let input_dir = config.input_dir();
    fs::create_dir_all(&input_dir)?;
    let input_file = input_dir.join("test.txt");
    fs::rename(&txt_file_path, &input_file)?;
    
    let ingestion = file_handler::ingest_txt_file_with_config(
        input_file.to_str().unwrap(),
        &config
    )?;
    
    // In main.rs, results_path is stored in a variable
    let stored_path = ingestion.results_path();
    
    // Create queue with stored path
    let mut results = DiskBackedQueue::new_from_path(&stored_path, 10)?;
    results.push(Ortho::new())?;
    results.flush()?;
    
    println!("Queue created with stored path: {}", stored_path);
    
    // Verify files exist
    let files: Vec<_> = fs::read_dir(&stored_path)?.collect();
    println!("Files at stored path: {}", files.len());
    
    // Now simulate calling save_result which internally calls results_path() again
    // But what if we DON'T pass the stored variable, and let it call the method?
    drop(results);
    
    // Get path from method again
    let method_path = ingestion.results_path();
    println!("Method path: {}", method_path);
    
    // Are they the same?
    if stored_path != method_path {
        println!("BUG: Stored path differs from method path!");
        println!("  Stored: {}", stored_path);
        println!("  Method: {}", method_path);
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Path mismatch"
        )));
    }
    
    println!("Paths match correctly");
    
    Ok(())
}
