use fold::file_handler::{self, StateConfig};
use fold::interner::Interner;
use fold::disk_backed_queue::DiskBackedQueue;
use fold::ortho::Ortho;
use std::fs;

#[test]
fn test_archives_in_input_have_no_heartbeat() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    file_handler::initialize_with_config(&config).unwrap();
    
    // Create a txt file
    let txt_content = "test content for archive";
    let input_dir = config.input_dir();
    fs::create_dir_all(&input_dir).unwrap();
    let txt_file = input_dir.join("test.txt");
    fs::write(&txt_file, txt_content).unwrap();
    
    // Ingest it
    let ingestion = file_handler::ingest_txt_file_with_config(
        txt_file.to_str().unwrap(),
        &config
    ).unwrap();
    
    // Build interner and create a simple result
    let interner = Interner::from_text(txt_content);
    let results_path = ingestion.results_path();
    let mut results = DiskBackedQueue::new_from_path(&results_path, 1000).unwrap();
    let ortho = Ortho::new();
    results.push(ortho.clone()).unwrap();
    
    // Save the archive
    let (archive_path, _) = ingestion.save_result(&interner, results, Some(&ortho), 1).unwrap();
    
    // Verify archive is in input and has NO heartbeat
    let archive_heartbeat = format!("{}/heartbeat", archive_path);
    assert!(!std::path::Path::new(&archive_heartbeat).exists(), 
        "Archive in input/ should not have a heartbeat");
    
    // Cleanup the work folder
    ingestion.cleanup().unwrap();
}

#[test]
fn test_archives_in_process_get_heartbeat_when_moved() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    file_handler::initialize_with_config(&config).unwrap();
    
    // Create two archives in input (without heartbeats)
    let txt_content_a = "test content a";
    let txt_content_b = "test content b";
    
    // Ensure input directory exists
    let input_dir = config.input_dir();
    fs::create_dir_all(&input_dir).unwrap();
    
    // Create first archive
    let txt_file_a = input_dir.join("test_a.txt");
    fs::write(&txt_file_a, txt_content_a).unwrap();
    let ingestion_a = file_handler::ingest_txt_file_with_config(
        txt_file_a.to_str().unwrap(),
        &config
    ).unwrap();
    let interner_a = Interner::from_text(txt_content_a);
    let results_path_a = ingestion_a.results_path();
    let mut results_a = DiskBackedQueue::new_from_path(&results_path_a, 1000).unwrap();
    let ortho_a = Ortho::new();
    results_a.push(ortho_a.clone()).unwrap();
    let (archive_path_a, _) = ingestion_a.save_result(&interner_a, results_a, Some(&ortho_a), 1).unwrap();
    ingestion_a.cleanup().unwrap();
    
    // Create second archive
    let txt_file_b = input_dir.join("test_b.txt");
    fs::write(&txt_file_b, txt_content_b).unwrap();
    let ingestion_b = file_handler::ingest_txt_file_with_config(
        txt_file_b.to_str().unwrap(),
        &config
    ).unwrap();
    let interner_b = Interner::from_text(txt_content_b);
    let results_path_b = ingestion_b.results_path();
    let mut results_b = DiskBackedQueue::new_from_path(&results_path_b, 1000).unwrap();
    let ortho_b = Ortho::new();
    results_b.push(ortho_b.clone()).unwrap();
    let (archive_path_b, _) = ingestion_b.save_result(&interner_b, results_b, Some(&ortho_b), 1).unwrap();
    ingestion_b.cleanup().unwrap();
    
    // Verify archives in input have NO heartbeats
    let archive_a_heartbeat_before = format!("{}/heartbeat", archive_path_a);
    let archive_b_heartbeat_before = format!("{}/heartbeat", archive_path_b);
    assert!(!std::path::Path::new(&archive_a_heartbeat_before).exists(), 
        "Archive A in input/ should not have a heartbeat");
    assert!(!std::path::Path::new(&archive_b_heartbeat_before).exists(), 
        "Archive B in input/ should not have a heartbeat");
    
    // Now ingest them for merging (this moves them to in_process)
    let merge_ingestion = file_handler::ingest_archives_with_config(
        &archive_path_a,
        &archive_path_b,
        &config
    ).unwrap();
    
    // After moving to in_process, archives should have heartbeats
    // Check by looking for any .bin folders in in_process that have heartbeats
    let in_process_dir = config.in_process_dir();
    let mut found_archives_with_heartbeats = 0;
    
    for entry in fs::read_dir(&in_process_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() && path.extension().map(|e| e == "bin").unwrap_or(false) {
            let heartbeat = path.join("heartbeat");
            if heartbeat.exists() {
                found_archives_with_heartbeats += 1;
            }
        }
    }
    
    assert_eq!(found_archives_with_heartbeats, 2, 
        "Both archives in in_process/ should have heartbeats after being moved for merging");
    
    merge_ingestion.cleanup().unwrap();
}

#[test]
fn test_stale_archives_in_process_are_recovered() {
    use filetime::{FileTime, set_file_mtime};
    
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    file_handler::initialize_with_config(&config).unwrap();
    
    // Create an archive directly in in_process with an old heartbeat
    let in_process_archive = config.in_process_dir().join("test_archive.bin");
    fs::create_dir_all(&in_process_archive).unwrap();
    
    // Create a results directory inside
    let results_dir = in_process_archive.join("results");
    fs::create_dir_all(&results_dir).unwrap();
    
    // Create metadata
    fs::write(in_process_archive.join("metadata.txt"), "10").unwrap();
    fs::write(in_process_archive.join("lineage.txt"), "test").unwrap();
    
    // Create a heartbeat file
    let heartbeat_path = in_process_archive.join("heartbeat");
    fs::write(&heartbeat_path, "test").unwrap();
    
    // Set the heartbeat file's modification time to 11 minutes ago
    let eleven_minutes_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() - 660; // 11 minutes = 660 seconds
    let file_time = FileTime::from_unix_time(eleven_minutes_ago as i64, 0);
    set_file_mtime(&heartbeat_path, file_time).unwrap();
    
    // Ensure input directory exists before recovery
    fs::create_dir_all(config.input_dir()).unwrap();
    
    // Run recovery
    file_handler::initialize_with_config(&config).unwrap();
    
    // Archive should have been moved to input
    let recovered_archive = config.input_dir().join("test_archive.bin");
    assert!(recovered_archive.exists(), 
        "Stale archive should have been recovered to input/");
    assert!(!in_process_archive.exists(), 
        "Archive should no longer be in in_process/");
}

#[test]
fn test_fresh_archives_in_process_are_not_recovered() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    file_handler::initialize_with_config(&config).unwrap();
    
    // Create an archive directly in in_process with a FRESH heartbeat
    let in_process_archive = config.in_process_dir().join("test_archive.bin");
    fs::create_dir_all(&in_process_archive).unwrap();
    
    // Create a results directory inside
    let results_dir = in_process_archive.join("results");
    fs::create_dir_all(&results_dir).unwrap();
    
    // Create metadata
    fs::write(in_process_archive.join("metadata.txt"), "10").unwrap();
    fs::write(in_process_archive.join("lineage.txt"), "test").unwrap();
    
    // Create a heartbeat file with a recent timestamp (1 minute ago)
    let heartbeat_path = in_process_archive.join("heartbeat");
    let one_minute_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() - 60; // 1 minute = 60 seconds
    fs::write(&heartbeat_path, one_minute_ago.to_string()).unwrap();
    
    // Run recovery
    file_handler::initialize_with_config(&config).unwrap();
    
    // Archive should still be in in_process
    assert!(in_process_archive.exists(), 
        "Fresh archive should remain in in_process/");
    
    // Archive should NOT be in input
    let would_be_recovered = config.input_dir().join("test_archive.bin");
    assert!(!would_be_recovered.exists(), 
        "Fresh archive should not be recovered to input/");
}
