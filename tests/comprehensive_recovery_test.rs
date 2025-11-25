use fold::file_handler::{self, StateConfig};
use fold::interner::Interner;
use fold::disk_backed_queue::DiskBackedQueue;
use fold::ortho::Ortho;
use std::fs;
use filetime::{FileTime, set_file_mtime};

#[test]
fn test_txt_recovery_removes_all_partial_work() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    file_handler::initialize_with_config(&config).unwrap();
    
    // Set up directories
    fs::create_dir_all(config.input_dir()).unwrap();
    
    // Create a txt file
    let txt_file = config.input_dir().join("test.txt");
    fs::write(&txt_file, "test content").unwrap();
    
    // Ingest it (moves to in_process)
    let ingestion = file_handler::ingest_txt_file_with_config(
        txt_file.to_str().unwrap(),
        &config
    ).unwrap();
    
    // Simulate partial processing by creating work directories
    let work_queue_path = ingestion.work_queue_path();
    let _work_queue = DiskBackedQueue::new_from_path(&work_queue_path, 1000).unwrap();
    
    let seen_shards_path = ingestion.seen_shards_path();
    fs::create_dir_all(&seen_shards_path).unwrap();
    fs::write(format!("{}/shard_0.bin", seen_shards_path), "test").unwrap();
    
    let results_path = ingestion.results_path();
    let mut results = DiskBackedQueue::new_from_path(&results_path, 1000).unwrap();
    results.push(Ortho::new()).unwrap();
    drop(results);
    
    // Get paths before cleanup
    let work_folder = format!("{}/test.txt.work", config.in_process_dir().display());
    let heartbeat_path = format!("{}/heartbeat", work_folder);
    
    // Make heartbeat stale
    let eleven_minutes_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() - 660;
    let file_time = FileTime::from_unix_time(eleven_minutes_ago as i64, 0);
    set_file_mtime(&heartbeat_path, file_time).unwrap();
    
    // Verify partial work exists
    assert!(std::path::Path::new(&work_folder).exists());
    assert!(std::path::Path::new(&work_queue_path).exists());
    assert!(std::path::Path::new(&seen_shards_path).exists());
    assert!(std::path::Path::new(&results_path).exists());
    
    // Run recovery
    file_handler::initialize_with_config(&config).unwrap();
    
    // Txt file should be recovered
    let recovered_txt = config.input_dir().join("test.txt");
    assert!(recovered_txt.exists(), "Txt file should be recovered to input/");
    
    // All partial work should be deleted
    assert!(!std::path::Path::new(&work_folder).exists(), 
        "Work folder should be deleted");
    assert!(!std::path::Path::new(&results_path).exists(), 
        "Results directory should be deleted");
}

#[test]
fn test_merge_recovery_restores_archives_and_removes_partial_work() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    file_handler::initialize_with_config(&config).unwrap();
    
    fs::create_dir_all(config.input_dir()).unwrap();
    
    // Create two archives in input
    let archive_a = config.input_dir().join("archive_a.bin");
    fs::create_dir_all(&archive_a).unwrap();
    fs::create_dir_all(archive_a.join("results")).unwrap();
    fs::write(archive_a.join("metadata.txt"), "5").unwrap();
    fs::write(archive_a.join("lineage.txt"), "\"a\"").unwrap();
    let interner_a = Interner::from_text("test a");
    let interner_a_bytes = bincode::encode_to_vec(&interner_a, bincode::config::standard()).unwrap();
    fs::write(archive_a.join("interner.bin"), interner_a_bytes).unwrap();
    
    let archive_b = config.input_dir().join("archive_b.bin");
    fs::create_dir_all(&archive_b).unwrap();
    fs::create_dir_all(archive_b.join("results")).unwrap();
    fs::write(archive_b.join("metadata.txt"), "3").unwrap();
    fs::write(archive_b.join("lineage.txt"), "\"b\"").unwrap();
    let interner_b = Interner::from_text("test b");
    let interner_b_bytes = bincode::encode_to_vec(&interner_b, bincode::config::standard()).unwrap();
    fs::write(archive_b.join("interner.bin"), interner_b_bytes).unwrap();
    
    // Ingest for merge (moves to in_process, adds heartbeats)
    let ingestion = file_handler::ingest_archives_with_config(
        archive_a.to_str().unwrap(),
        archive_b.to_str().unwrap(),
        &config
    ).unwrap();
    
    // Simulate partial merge processing
    let work_queue_path = ingestion.work_queue_path();
    let _work_queue = DiskBackedQueue::new_from_path(&work_queue_path, 1000).unwrap();
    
    let seen_shards_path = ingestion.seen_shards_path();
    fs::create_dir_all(&seen_shards_path).unwrap();
    fs::write(format!("{}/shard_0.bin", seen_shards_path), "test").unwrap();
    
    let results_merged_path = config.base_dir.join(format!("results_merged_{}", std::process::id()));
    let mut results = DiskBackedQueue::new_from_path(results_merged_path.to_str().unwrap(), 1000).unwrap();
    results.push(Ortho::new()).unwrap();
    drop(results);
    
    // Get paths
    let merge_work_folder = format!("{}/merge_{}.work", config.in_process_dir().display(), std::process::id());
    let heartbeat_path = format!("{}/heartbeat", merge_work_folder);
    let archive_a_in_process = config.in_process_dir().join("archive_a.bin");
    let archive_b_in_process = config.in_process_dir().join("archive_b.bin");
    
    // Make heartbeat stale
    let eleven_minutes_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() - 660;
    let file_time = FileTime::from_unix_time(eleven_minutes_ago as i64, 0);
    set_file_mtime(&heartbeat_path, file_time).unwrap();
    
    // Verify partial work exists before recovery
    assert!(archive_a_in_process.exists(), "Archive A should be in in_process");
    assert!(archive_b_in_process.exists(), "Archive B should be in in_process");
    assert!(archive_a_in_process.join("heartbeat").exists(), "Archive A should have heartbeat");
    assert!(archive_b_in_process.join("heartbeat").exists(), "Archive B should have heartbeat");
    assert!(std::path::Path::new(&merge_work_folder).exists(), "Merge work folder should exist");
    assert!(std::path::Path::new(&work_queue_path).exists(), "Work queue should exist");
    assert!(std::path::Path::new(&seen_shards_path).exists(), "Seen shards should exist");
    assert!(results_merged_path.exists(), "Results merged should exist");
    
    // Run recovery
    file_handler::initialize_with_config(&config).unwrap();
    
    // Archives should be back in input WITHOUT heartbeats
    let recovered_a = config.input_dir().join("archive_a.bin");
    let recovered_b = config.input_dir().join("archive_b.bin");
    assert!(recovered_a.exists(), "Archive A should be recovered to input/");
    assert!(recovered_b.exists(), "Archive B should be recovered to input/");
    assert!(!recovered_a.join("heartbeat").exists(), "Archive A should not have heartbeat in input/");
    assert!(!recovered_b.join("heartbeat").exists(), "Archive B should not have heartbeat in input/");
    
    // All partial work should be deleted
    assert!(!archive_a_in_process.exists(), "Archive A should not be in in_process");
    assert!(!archive_b_in_process.exists(), "Archive B should not be in in_process");
    assert!(!std::path::Path::new(&merge_work_folder).exists(), 
        "Merge work folder should be deleted");
    assert!(!results_merged_path.exists(), 
        "Results merged directory should be deleted");
}
