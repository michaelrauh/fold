use fold::{file_handler, interner::Interner, ortho::Ortho, disk_backed_queue::DiskBackedQueue};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_optimal_ortho_saved_and_loaded() {
    // Create a temp directory for testing
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("test_archive");
    let results_path = temp_dir.path().join("results");
    
    // Create test interner
    let interner = Interner::from_text("the quick brown fox");
    let version = interner.version();
    
    // Create a test ortho with some structure
    let mut ortho = Ortho::new(version);
    ortho = ortho.add(0, version).into_iter().next().unwrap();
    ortho = ortho.add(1, version).into_iter().next().unwrap();
    
    // Create a DiskBackedQueue with results
    let mut results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), 100).unwrap();
    results.push(ortho.clone()).unwrap();
    
    // Calculate expected score
    let volume = ortho.dims().iter().map(|&d| d - 1).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    
    // Save archive with optimal ortho
    let lineage = "test".to_string();
    let ortho_count = 1;
    
    // Use the internal save function through public API
    fs::create_dir_all(&archive_path).unwrap();
    
    // Manually construct what save_archive does
    results.flush().unwrap();
    drop(results);
    
    let archive_results_path = archive_path.join("results");
    fs::rename(&results_path, &archive_results_path).unwrap();
    
    let interner_path = archive_path.join("interner.bin");
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard()).unwrap();
    fs::write(interner_path, interner_bytes).unwrap();
    
    // Save optimal ortho in binary format
    let optimal_bin_path = archive_path.join("optimal.bin");
    let optimal_bytes = bincode::encode_to_vec(&ortho, bincode::config::standard()).unwrap();
    fs::write(optimal_bin_path, optimal_bytes).unwrap();
    
    // Write other required files
    let lineage_path = archive_path.join("lineage.txt");
    fs::write(lineage_path, lineage).unwrap();
    
    let metadata_path = archive_path.join("metadata.txt");
    fs::write(metadata_path, ortho_count.to_string()).unwrap();
    
    // Now test loading the optimal ortho back
    let loaded_ortho = file_handler::load_optimal_ortho(archive_path.to_str().unwrap())
        .expect("Should load optimal ortho");
    
    // Verify the loaded ortho matches the original
    assert_eq!(loaded_ortho.id(), ortho.id());
    assert_eq!(loaded_ortho.dims(), ortho.dims());
    assert_eq!(loaded_ortho.payload(), ortho.payload());
    assert_eq!(loaded_ortho.version(), ortho.version());
    
    // Verify score calculation still works
    let loaded_volume = loaded_ortho.dims().iter().map(|&d| d - 1).product::<usize>();
    let loaded_fullness = loaded_ortho.payload().iter().filter(|x| x.is_some()).count();
    assert_eq!(loaded_volume, volume);
    assert_eq!(loaded_fullness, fullness);
}

#[test]
fn test_load_optimal_ortho_missing_file() {
    // Create a temp directory for testing
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("empty_archive");
    fs::create_dir_all(&archive_path).unwrap();
    
    // Try to load from archive without optimal.bin - should error
    let result = file_handler::load_optimal_ortho(archive_path.to_str().unwrap());
    
    // Should return an error when file doesn't exist
    assert!(result.is_err());
}
