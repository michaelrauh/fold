use filetime::{FileTime, set_file_mtime};
use fold::file_handler::{self, StateConfig};
use std::fs;

#[test]
fn test_orphaned_archive_has_heartbeat_removed() {
    // This test simulates an orphaned archive (e.g., after a failed merge recovery)
    // that gets caught by the fallback orphaned archive recovery
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());
    file_handler::initialize_with_config(&config).unwrap();

    // Create an orphaned archive in in_process with a stale heartbeat
    let in_process_archive = config.in_process_dir().join("orphaned_archive.bin");
    fs::create_dir_all(&in_process_archive).unwrap();

    // Create minimal archive structure
    let results_dir = in_process_archive.join("results");
    fs::create_dir_all(&results_dir).unwrap();
    fs::write(in_process_archive.join("metadata.txt"), "10").unwrap();
    fs::write(in_process_archive.join("lineage.txt"), "test").unwrap();

    // Create heartbeat and make it stale
    let heartbeat_path = in_process_archive.join("heartbeat");
    fs::write(&heartbeat_path, "test").unwrap();
    let eleven_minutes_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 660;
    let file_time = FileTime::from_unix_time(eleven_minutes_ago as i64, 0);
    set_file_mtime(&heartbeat_path, file_time).unwrap();

    // Verify heartbeat exists before recovery
    assert!(
        heartbeat_path.exists(),
        "Heartbeat should exist in in_process before recovery"
    );

    // Ensure input directory exists for recovery
    fs::create_dir_all(config.input_dir()).unwrap();

    // Run recovery
    file_handler::initialize_with_config(&config).unwrap();

    // Orphaned archive should be recovered to input
    let recovered_archive = config.input_dir().join("orphaned_archive.bin");
    assert!(
        recovered_archive.exists(),
        "Orphaned archive should be recovered to input/"
    );

    // Heartbeat should NOT exist in recovered archive
    let recovered_heartbeat = recovered_archive.join("heartbeat");
    assert!(
        !recovered_heartbeat.exists(),
        "Recovered archive in input/ should NOT have a heartbeat"
    );

    // Archive should no longer be in in_process
    assert!(
        !in_process_archive.exists(),
        "Archive should no longer be in in_process/"
    );
}
