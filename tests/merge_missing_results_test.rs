use fold::{
    FoldError,
    disk_backed_queue::DiskBackedQueue,
    file_handler::{self, StateConfig},
    interner::Interner,
    ortho::Ortho,
};
use std::fs;

/// Test that simulates the actual merge scenario to find where results go missing
#[test]
fn test_merge_scenario_with_actual_archives() -> Result<(), FoldError> {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());

    file_handler::initialize_with_config(&config)?;

    // Create two archives with actual results (simulating txt processing)

    // Archive A
    let interner_a = Interner::from_text("hello world");
    let archive_a_path = config.input_dir().join("archive_a_test.bin");
    fs::create_dir_all(&archive_a_path)?;

    let results_a_path = temp_dir.path().join("results_a");
    let mut results_a = DiskBackedQueue::new_from_path(results_a_path.to_str().unwrap(), 10)?;
    results_a.push(Ortho::new())?;
    results_a.push(Ortho::new())?;
    results_a.push(Ortho::new())?;
    results_a.flush()?;
    drop(results_a);

    // Check that results_a has files
    let results_a_files: Vec<_> = fs::read_dir(&results_a_path)?.collect();
    println!(
        "Archive A results directory has {} files",
        results_a_files.len()
    );
    assert!(
        results_a_files.len() > 0,
        "Archive A should have result files"
    );

    // Move results to archive
    let archive_a_results_path = archive_a_path.join("results");
    fs::rename(&results_a_path, &archive_a_results_path)?;

    // Save interner and lineage
    let interner_bytes = bincode::encode_to_vec(&interner_a, bincode::config::standard())?;
    fs::write(archive_a_path.join("interner.bin"), interner_bytes)?;
    fs::write(archive_a_path.join("lineage.txt"), "\"test_a\"")?;
    fs::write(archive_a_path.join("heartbeat"), "12345")?;

    // Archive B
    let interner_b = Interner::from_text("foo bar");
    let archive_b_path = config.input_dir().join("archive_b_test.bin");
    fs::create_dir_all(&archive_b_path)?;

    let results_b_path = temp_dir.path().join("results_b");
    let mut results_b = DiskBackedQueue::new_from_path(results_b_path.to_str().unwrap(), 10)?;
    results_b.push(Ortho::new())?;
    results_b.push(Ortho::new())?;
    results_b.flush()?;
    drop(results_b);

    // Check that results_b has files
    let results_b_files: Vec<_> = fs::read_dir(&results_b_path)?.collect();
    println!(
        "Archive B results directory has {} files",
        results_b_files.len()
    );
    assert!(
        results_b_files.len() > 0,
        "Archive B should have result files"
    );

    // Move results to archive
    let archive_b_results_path = archive_b_path.join("results");
    fs::rename(&results_b_path, &archive_b_results_path)?;

    // Save interner and lineage
    let interner_bytes = bincode::encode_to_vec(&interner_b, bincode::config::standard())?;
    fs::write(archive_b_path.join("interner.bin"), interner_bytes)?;
    fs::write(archive_b_path.join("lineage.txt"), "\"test_b\"")?;
    fs::write(archive_b_path.join("heartbeat"), "12345")?;

    println!("\n=== Archives created successfully ===");
    println!("Archive A: {:?}", archive_a_path);
    println!("Archive B: {:?}", archive_b_path);

    // Now simulate the merge ingestion
    let ingestion = file_handler::ingest_archives_with_config(
        archive_a_path.to_str().unwrap(),
        archive_b_path.to_str().unwrap(),
        &config,
    )?;

    // Get the results paths that the merge will use
    let (work_a_results, work_b_results) = ingestion.get_results_paths();

    println!("\n=== After ingestion ===");
    println!("Work A results path: {}", work_a_results);
    println!("Work B results path: {}", work_b_results);

    // Check if these paths exist and have files
    if fs::metadata(&work_a_results).is_ok() {
        let files: Vec<_> = fs::read_dir(&work_a_results)?.collect();
        println!("Work A results has {} files", files.len());

        if files.len() == 0 {
            println!("BUG: Archive A moved to work but results directory is empty!");
        }
    } else {
        println!("BUG: Archive A results path doesn't exist!");
    }

    if fs::metadata(&work_b_results).is_ok() {
        let files: Vec<_> = fs::read_dir(&work_b_results)?.collect();
        println!("Work B results has {} files", files.len());

        if files.len() == 0 {
            println!("BUG: Archive B moved to work but results directory is empty!");
        }
    } else {
        println!("BUG: Archive B results path doesn't exist!");
    }

    // Now try to open these as DiskBackedQueues (like the merge code does)
    // Note: len() is not reliable for reloaded queues, so we verify by popping
    let mut loaded_a = DiskBackedQueue::new_from_path(&work_a_results, 10)?;
    let mut loaded_b = DiskBackedQueue::new_from_path(&work_b_results, 10)?;

    println!("\n=== Loaded as DiskBackedQueues ===");

    // Count items by popping (len() is not reliable for reloaded queues)
    let mut count_a = 0;
    while loaded_a.pop()?.is_some() {
        count_a += 1;
    }
    let mut count_b = 0;
    while loaded_b.pop()?.is_some() {
        count_b += 1;
    }

    println!("Loaded A actual count: {}", count_a);
    println!("Loaded B actual count: {}", count_b);

    assert!(count_a > 0, "Archive A should have results");
    assert!(count_b > 0, "Archive B should have results");

    Ok(())
}
