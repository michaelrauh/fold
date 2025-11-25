use fold::{
    file_handler::{self, StateConfig},
    interner::Interner,
    disk_backed_queue::DiskBackedQueue,
    ortho::Ortho,
    FoldError,
};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, Duration};

/// Test what happens when a txt file is processed, results are created,
/// then archive is saved but results_path points to wrong location
#[test]
fn test_txt_processing_results_path_mismatch() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    // Simulate txt ingestion
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
    
    println!("Ingestion filename: {}", ingestion.filename);
    
    // Get the results path that SHOULD be used
    let correct_results_path = ingestion.results_path();
    println!("Correct results path: {}", correct_results_path);
    
    // But simulate creating the queue at a DIFFERENT path (like if paths are computed differently)
    let wrong_results_path = config.results_dir("test_wrong");
    let mut results = DiskBackedQueue::new_from_path(wrong_results_path.to_str().unwrap(), 10)?;
    results.push(Ortho::new())?;
    results.flush()?;
    
    println!("Actually created queue at: {}", wrong_results_path.display());
    
    // Now save_result will look for files at correct_results_path
    let interner = Interner::from_text(&ingestion.text);
    
    // Manually call save_archive like save_result does
    let archive_path = input_dir.join("archive_test.bin");
    fs::create_dir_all(&archive_path)?;
    
    // Flush and drop
    drop(results);
    
    // Try to rename from correct_results_path (which is empty/missing)
    let archive_results_path = archive_path.join("results");
    if Path::new(&correct_results_path).exists() {
        println!("Correct path exists, renaming");
        fs::rename(&correct_results_path, &archive_results_path)?;
    } else {
        println!("BUG: Correct path doesn't exist, archive will have no results!");
    }
    
    // Check archive
    if archive_results_path.exists() {
        let files: Vec<_> = fs::read_dir(&archive_results_path)?.collect();
        println!("Archive has {} files", files.len());
        if files.len() == 0 {
            println!("Archive has empty results!");
        }
    } else {
        println!("Archive has NO results directory!");
    }
    
    // Verify files are still at wrong path
    if Path::new(&wrong_results_path).exists() {
        let files: Vec<_> = fs::read_dir(&wrong_results_path)?.collect();
        println!("Files still at wrong path: {}", files.len());
    }
    
    Ok(())
}

/// Test archive recovery scenario: archive is processed but crashes,
/// gets recovered, then processed again with different process ID
#[test]
fn test_archive_recovery_with_process_id_change() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    // Create an archive with results
    let archive_path = config.input_dir().join("test_archive.bin");
    fs::create_dir_all(&archive_path)?;
    
    let interner = Interner::from_text("hello world");
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?;
    fs::write(archive_path.join("interner.bin"), &interner_bytes)?;
    fs::write(archive_path.join("lineage.txt"), "\"test\"")?;
    
    // Create results directory with files
    let archive_results = archive_path.join("results");
    let mut results = DiskBackedQueue::new_from_path(archive_results.to_str().unwrap(), 10)?;
    let ortho1 = Ortho::new();
    let ortho2 = ortho1.add(0).into_iter().next().unwrap();
    results.push(ortho1.clone())?;
    results.push(ortho2.clone())?;
    results.flush()?;
    drop(results);
    
    let files: Vec<_> = fs::read_dir(&archive_results)?.collect();
    println!("Archive initially has {} result files", files.len());
    assert!(files.len() > 0, "Archive should have results");
    
    // Create a second archive
    let archive_path2 = config.input_dir().join("test_archive2.bin");
    fs::create_dir_all(&archive_path2)?;
    fs::write(archive_path2.join("interner.bin"), &interner_bytes)?;
    fs::write(archive_path2.join("lineage.txt"), "\"test2\"")?;
    let archive_results2 = archive_path2.join("results");
    let mut results2 = DiskBackedQueue::new_from_path(archive_results2.to_str().unwrap(), 10)?;
    results2.push(ortho1)?;
    results2.push(ortho2)?;
    results2.flush()?;
    drop(results2);
    
    // Simulate ingestion for merge (moves both to in_process)
    let ingestion = file_handler::ingest_archives_with_config(
        archive_path.to_str().unwrap(),
        archive_path2.to_str().unwrap(),
        &config
    )?;
    
    // Check that results moved with the archive
    let (results_a, _) = ingestion.get_results_paths();
    println!("After ingestion, results path: {}", results_a);
    
    if Path::new(&results_a).exists() {
        let files: Vec<_> = fs::read_dir(&results_a)?.collect();
        println!("Results in in_process: {} files", files.len());
        assert!(files.len() > 0, "Results should have moved with archive");
    } else {
        panic!("Results directory should exist in in_process after ingestion!");
    }
    
    Ok(())
}

/// Test the specific scenario: process creates results_merged_{pid} directory,
/// crashes, then on restart with new PID, save_archive looks for wrong path
#[test]
fn test_merge_results_path_with_pid_change() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    
    file_handler::initialize_with_config(&config)?;
    
    // Simulate first run with PID 12345
    let first_pid = 12345;
    let first_results_path = config.results_dir(&format!("merged_{}", first_pid));
    
    println!("First run - PID {}: results at {}", first_pid, first_results_path.display());
    
    // Create results at first path
    let mut results = DiskBackedQueue::new_from_path(first_results_path.to_str().unwrap(), 10)?;
    results.push(Ortho::new())?;
    results.flush()?;
    
    let files: Vec<_> = fs::read_dir(&first_results_path)?.collect();
    println!("Created {} files at first path", files.len());
    
    drop(results);
    
    // Simulate crash - results_path variable is lost, directory remains
    
    // Simulate second run with different PID 67890
    let second_pid = 67890;
    let second_results_path = config.results_dir(&format!("merged_{}", second_pid));
    
    println!("\nSecond run - PID {}: results at {}", second_pid, second_results_path.display());
    println!("This is a DIFFERENT path than first run!");
    
    // Now when save_archive is called, it looks for second_results_path
    // but the files are at first_results_path
    
    let archive_path = config.input_dir().join("merged_archive.bin");
    fs::create_dir_all(&archive_path)?;
    
    let archive_results = archive_path.join("results");
    
    // save_archive checks if second_results_path exists
    if second_results_path.exists() {
        println!("Second path exists, renaming");
        fs::rename(&second_results_path, &archive_results)?;
    } else {
        println!("BUG REPRODUCED: Second path doesn't exist!");
        println!("  Files are at: {}", first_results_path.display());
        println!("  Looking for:  {}", second_results_path.display());
        println!("  Archive will have no results!");
    }
    
    // Verify archive has no results
    if archive_results.exists() {
        let files: Vec<_> = fs::read_dir(&archive_results)?.collect();
        println!("\nArchive has {} files", files.len());
    } else {
        println!("\nArchive has NO results directory!");
    }
    
    // Verify files still at first path
    if first_results_path.exists() {
        let files: Vec<_> = fs::read_dir(&first_results_path)?.collect();
        println!("Orphaned files at first path: {}", files.len());
    }
    
    println!("\nBUG CONFIRMED: Process ID change causes path mismatch!");
    
    Ok(())
}
