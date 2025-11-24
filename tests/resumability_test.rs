use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, Duration};
use filetime;
use fold::file_handler::StateConfig;

/// Test that verifies the resumability/fault tolerance of the system.
/// 
/// Workflow:
/// 1. Start processing a file (moves to in_process with heartbeat)
/// 2. Simulate process death by killing it mid-processing
/// 3. Make the heartbeat stale (>10 minutes old)
/// 4. Start a new process - it should recover the abandoned work
/// 5. Verify the file gets processed successfully
/// 
/// This tests the heartbeat-based recovery mechanism that ensures
/// no work is lost if a process dies or hangs.
#[test]
fn test_resumability_after_stale_heartbeat() {
    // Setup: Create a temporary test environment
    let test_dir = tempfile::tempdir().unwrap();
    let test_path = test_dir.path();
    
    // Create test input file
    let input_file = test_path.join("test_input.txt");
    let test_content = "alpha beta gamma";
    fs::write(&input_file, test_content).unwrap();
    
    // Create state directory structure
    let state_dir = test_path.join("fold_state");
    let input_dir = state_dir.join("input");
    let in_process_dir = state_dir.join("in_process");
    fs::create_dir_all(&input_dir).unwrap();
    fs::create_dir_all(&in_process_dir).unwrap();
    
    println!("[test] Stage 1: Preparing file...");
    
    // Run stage.sh to split the file
    let stage_output = Command::new("bash")
        .arg("./stage.sh")
        .arg(&input_file)
        .arg("2")
        .arg(&state_dir)
        .output()
        .expect("Failed to execute stage.sh");
    
    assert!(stage_output.status.success(), "stage.sh failed");
    
    // Verify chunk was created
    let chunks: Vec<_> = fs::read_dir(&input_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "txt").unwrap_or(false))
        .collect();
    
    assert_eq!(chunks.len(), 1, "Expected 1 chunk");
    let chunk_path = chunks[0].path();
    let chunk_name = chunks[0].file_name();
    println!("[test] Created chunk: {:?}", chunk_name);
    
    println!("[test] Stage 2: Simulating abandoned work...");
    
    // Manually simulate what happens when a process starts but dies:
    // 1. Move file from input to in_process (as a .txt.work folder)
    let work_folder_name = format!("{}.work", chunk_name.to_string_lossy());
    let work_folder = in_process_dir.join(&work_folder_name);
    fs::create_dir_all(&work_folder).unwrap();
    
    // 2. Move the txt file to source.txt inside work folder
    let source_txt = work_folder.join("source.txt");
    fs::rename(&chunk_path, &source_txt).unwrap();
    println!("[test] Moved file to in_process: {:?}", work_folder);
    
    // 3. Create a heartbeat file
    let heartbeat_path = work_folder.join("heartbeat");
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(&heartbeat_path, now.to_string()).unwrap();
    println!("[test] Created heartbeat");
    
    // 4. Make the heartbeat stale by modifying its timestamp
    // Set modification time to 11 minutes ago (660 seconds)
    let stale_time = SystemTime::now() - Duration::from_secs(660);
    filetime::set_file_mtime(&heartbeat_path, filetime::FileTime::from_system_time(stale_time))
        .expect("Failed to set file time");
    println!("[test] Made heartbeat stale (11 minutes old)");
    
    // Verify file is NOT in input anymore
    let input_files: Vec<_> = fs::read_dir(&input_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "txt").unwrap_or(false))
        .collect();
    assert_eq!(input_files.len(), 0, "File should be in in_process, not input");
    
    // Verify work folder exists in in_process
    assert!(work_folder.exists(), "Work folder should exist");
    assert!(source_txt.exists(), "source.txt should exist");
    assert!(heartbeat_path.exists(), "Heartbeat should exist");
    
    println!("[test] Stage 3: Testing recovery...");
    
    // Use isolated test directory
    let test_state_dir = test_path.join("test_fold_state");
    let config = StateConfig::custom(test_state_dir.clone());
    copy_dir_all(&state_dir, &test_state_dir).unwrap();
    
    // Run fold - it should recover the abandoned file
    let fold_output = Command::new("./target/release/fold")
        .env("FOLD_STATE_DIR", test_state_dir.to_str().unwrap())
        .output()
        .expect("Failed to execute fold");
    
    println!("[test] Fold output:\n{}", String::from_utf8_lossy(&fold_output.stdout));
    
    if !fold_output.status.success() {
        println!("[test] Fold stderr:\n{}", String::from_utf8_lossy(&fold_output.stderr));
    }
    
    assert!(fold_output.status.success(), "Fold should recover and process successfully");
    
    println!("[test] Stage 4: Verifying recovery worked...");
    
    // Verify the file was recovered and processed
    let output = String::from_utf8_lossy(&fold_output.stdout);
    assert!(output.contains("Recovered"), "Should show recovery message");
    assert!(output.contains("Processing completed"), "Should complete processing");
    
    // Verify an archive was created
    let final_input: Vec<_> = fs::read_dir(config.input_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path().is_dir() && 
            e.path().extension().map(|ext| ext == "bin").unwrap_or(false)
        })
        .collect();
    
    assert_eq!(final_input.len(), 1, "Should have created one archive");
    println!("[test] Archive created successfully after recovery");
    
    // Test uses isolated directory, so no cross-test pollution
    println!("[test] Resumability test completed successfully!");
}

/// Helper function to recursively copy directories
fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}
