use fold::{
    file_handler::{self, StateConfig},
    disk_backed_queue::DiskBackedQueue,
    ortho::Ortho,
    FoldError,
};
use std::fs;
use std::path::Path;

/// Test that the path used to create DiskBackedQueue matches the path passed to save_archive
#[test]
fn test_results_path_consistency() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    // Create a txt file
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
    
    // Get the results path TWICE like in the code
    let results_path_1 = ingestion.results_path();
    let results_path_2 = ingestion.results_path();
    
    println!("First call:  '{}'", results_path_1);
    println!("Second call: '{}'", results_path_2);
    println!("Match: {}", results_path_1 == results_path_2);
    
    assert_eq!(results_path_1, results_path_2, "results_path() should return same value");
    
    // Create the DiskBackedQueue with the first path
    let mut results = DiskBackedQueue::new_from_path(&results_path_1, 10)?;
    results.push(Ortho::new())?;
    results.push(Ortho::new())?;
    
    println!("\nDiskBackedQueue created at: {}", results_path_1);
    
    // Check files exist at that path
    if Path::new(&results_path_1).exists() {
        let entries: Vec<_> = fs::read_dir(&results_path_1)?.collect();
        println!("Directory exists before flush, {} entries", entries.len());
    }
    
    // Manually flush
    results.flush()?;
    
    // Check files exist after flush
    if Path::new(&results_path_1).exists() {
        let entries: Vec<_> = fs::read_dir(&results_path_1)?.collect();
        println!("Directory exists after flush, {} entries", entries.len());
        assert!(entries.len() > 0, "Should have files after flush");
    } else {
        panic!("Directory doesn't exist after flush!");
    }
    
    // Now simulate what save_archive does with the SECOND path
    // This is the path passed to save_archive
    println!("\nsave_archive will look for: {}", results_path_2);
    
    // Drop results to close handles (like save_archive does)
    drop(results);
    
    // Check if the directory exists at results_path_2
    if Path::new(&results_path_2).exists() {
        let entries: Vec<_> = fs::read_dir(&results_path_2)?.collect();
        println!("Directory found at save_archive path, {} entries", entries.len());
        
        if entries.len() == 0 {
            println!("BUG: Directory exists but is empty!");
            return Err(FoldError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Path mismatch - directory is empty"
            )));
        }
    } else {
        println!("BUG: Directory doesn't exist at save_archive path!");
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Path mismatch - directory not found"
        )));
    }
    
    Ok(())
}

/// Test with a filename that has special characters that might get encoded differently
#[test]
fn test_results_path_with_special_filename() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    // Create a txt file with a name that might cause encoding issues
    let txt_file_path = temp_dir.path().join("e_chunk_0022.txt");
    fs::write(&txt_file_path, "hello world test")?;
    
    let input_dir = config.input_dir();
    fs::create_dir_all(&input_dir)?;
    let input_file = input_dir.join("e_chunk_0022.txt");
    fs::rename(&txt_file_path, &input_file)?;
    
    let ingestion = file_handler::ingest_txt_file_with_config(
        input_file.to_str().unwrap(),
        &config
    )?;
    
    // Check the filename extracted
    println!("Ingestion filename: '{}'", ingestion.filename);
    
    // Get results paths
    let results_path_create = ingestion.results_path();
    println!("Path for creation: '{}'", results_path_create);
    
    // Create queue and add items
    let mut results = DiskBackedQueue::new_from_path(&results_path_create, 10)?;
    results.push(Ortho::new())?;
    results.flush()?;
    
    // Get path again for save
    let results_path_save = ingestion.results_path();
    println!("Path for save: '{}'", results_path_save);
    
    assert_eq!(results_path_create, results_path_save, "Paths must match");
    
    // Verify files exist
    let entries: Vec<_> = fs::read_dir(&results_path_save)?.collect();
    assert!(entries.len() > 0, "Should have files at save path");
    
    Ok(())
}

/// Test the actual flow where results_path is called at different times
#[test]
fn test_path_timing_issue() -> Result<(), FoldError> {
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
    
    // Store the path in a variable like main.rs does
    let results_path = ingestion.results_path();
    println!("Stored results_path: {}", results_path);
    
    // Create queue with stored path
    let mut results = DiskBackedQueue::new_from_path(&results_path, 10)?;
    results.push(Ortho::new())?;
    results.flush()?;
    drop(results);
    
    // Now in save_result, it calls results_path() AGAIN
    // Simulate this by getting a fresh path
    let fresh_results_path = ingestion.results_path();
    println!("Fresh results_path: {}", fresh_results_path);
    
    // Check if they match
    if results_path != fresh_results_path {
        println!("BUG FOUND: Paths don't match!");
        println!("  Original: {}", results_path);
        println!("  Fresh:    {}", fresh_results_path);
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Path mismatch between creation and save"
        )));
    }
    
    // Check if files exist at the fresh path (like save_archive does)
    if Path::new(&fresh_results_path).exists() {
        let entries: Vec<_> = fs::read_dir(&fresh_results_path)?.collect();
        println!("Fresh path has {} entries", entries.len());
        assert!(entries.len() > 0, "Should have files");
    } else {
        println!("BUG: Fresh path doesn't exist!");
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Fresh path not found"
        )));
    }
    
    Ok(())
}
