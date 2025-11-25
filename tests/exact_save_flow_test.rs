use fold::{
    file_handler::{self, StateConfig},
    interner::Interner,
    disk_backed_queue::DiskBackedQueue,
    ortho::Ortho,
    FoldError,
};
use std::fs;
use std::path::Path;

/// Test that reproduces the exact save flow from process_txt_file
/// to understand why results directories end up empty
#[test]
fn test_exact_txt_processing_save_flow() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    // Simulate ingesting a txt file
    let txt_file_path = temp_dir.path().join("test.txt");
    fs::write(&txt_file_path, "hello world test")?;
    
    // Move it to input
    let input_dir = config.input_dir();
    fs::create_dir_all(&input_dir)?;
    let input_file = input_dir.join("test.txt");
    fs::rename(&txt_file_path, &input_file)?;
    
    println!("Input file: {}", input_file.display());
    
    let ingestion = file_handler::ingest_txt_file_with_config(
        input_file.to_str().unwrap(),
        &config
    ).map_err(|e| {
        eprintln!("Failed to ingest file: {:?}", e);
        e
    })?;
    
    let interner = Interner::from_text(&ingestion.text);
    
    // Create results queue using the ingestion's results_path
    let results_path = ingestion.results_path();
    println!("Results path: {}", results_path);
    
    let mut results = DiskBackedQueue::new_from_path(&results_path, 10).map_err(|e| {
        eprintln!("Failed to create DiskBackedQueue: {:?}", e);
        e
    })?;
    
    // Push some orthos
    results.push(Ortho::new())?;
    results.push(Ortho::new())?;
    results.push(Ortho::new())?;
    
    println!("Results queue length before save: {}", results.len());
    println!("Results queue disk_path: {:?}", temp_dir.path().join("results"));
    
    // Check that the results directory has files before save
    if Path::new(&results_path).exists() {
        let files: Vec<_> = fs::read_dir(&results_path)?.collect();
        println!("Results directory has {} files before save", files.len());
    } else {
        println!("Results directory doesn't exist before save!");
    }
    
    // Force flush before save to test
    results.flush()?;
    
    // Check again after manual flush
    if Path::new(&results_path).exists() {
        let files: Vec<_> = fs::read_dir(&results_path)?.collect();
        println!("Results directory has {} files after manual flush", files.len());
    }
    
    // Now call save_result - this is what happens in main.rs
    let (archive_path, _lineage) = ingestion.save_result(&interner, results, Some(&Ortho::new()), 1)?;
    
    println!("Archive saved to: {}", archive_path);
    
    // Check if the archive has a results directory
    let archive_path_obj = Path::new(&archive_path);
    let archive_results = archive_path_obj.join("results");
    
    if archive_results.exists() {
        let files: Vec<_> = fs::read_dir(&archive_results)?.collect();
        println!("Archive results directory has {} files", files.len());
        
        if files.len() == 0 {
            println!("BUG REPRODUCED: Archive has empty results directory!");
            
            // Check if the original results_path still exists
            if Path::new(&results_path).exists() {
                println!("Original results_path still exists - rename failed?");
            } else {
                println!("Original results_path was moved successfully");
            }
            
            return Err(FoldError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Archive has empty results directory"
            )));
        }
    } else {
        println!("BUG: Archive has NO results directory!");
        return Err(FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::Other,
            "Archive missing results directory"
        )));
    }
    
    Ok(())
}

/// Test the specific scenario where results directory might not exist when save_archive is called
#[test]
fn test_results_directory_deleted_before_save() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    
    let interner = Interner::from_text("hello world");
    
    let results_path = temp_dir.path().join("results");
    let mut results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), 10)?;
    
    results.push(Ortho::new())?;
    results.push(Ortho::new())?;
    
    // Flush to disk
    results.flush()?;
    
    // Check files exist
    let files_before: Vec<_> = fs::read_dir(&results_path)?.collect();
    println!("Files before drop: {}", files_before.len());
    
    // Now DROP the results - this closes file handles
    drop(results);
    
    // Check files still exist after drop
    let files_after: Vec<_> = fs::read_dir(&results_path)?.collect();
    println!("Files after drop: {}", files_after.len());
    
    // Now simulate what save_archive does
    let archive_path = temp_dir.path().join("archive.bin");
    fs::create_dir_all(&archive_path)?;
    
    let archive_results_path = archive_path.join("results");
    
    // This is the critical check from save_archive
    if results_path.exists() {
        println!("Results path exists, attempting rename...");
        fs::rename(&results_path, &archive_results_path)?;
        println!("Rename successful");
    } else {
        println!("BUG: Results path doesn't exist at time of save!");
    }
    
    // Verify archive has the files
    if archive_results_path.exists() {
        let final_files: Vec<_> = fs::read_dir(&archive_results_path)?.collect();
        println!("Archive has {} files", final_files.len());
        assert!(final_files.len() > 0, "Archive should have result files");
    }
    
    Ok(())
}
