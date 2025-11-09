use std::fs;
use std::process::Command;

#[test]
fn test_fold_binary_processes_files() {
    // Setup test directory
    let test_dir = "/tmp/fold_test_integration";
    let input_dir = format!("{}/input", test_dir);
    
    // Clean up any previous test runs
    let _ = fs::remove_dir_all(test_dir);
    fs::create_dir_all(&input_dir).expect("Failed to create test input directory");
    
    // Create test input files
    fs::write(
        format!("{}/test1.txt", input_dir),
        "hello world hello there world there"
    ).expect("Failed to write test file 1");
    
    fs::write(
        format!("{}/test2.txt", input_dir),
        "foo bar foo baz bar baz"
    ).expect("Failed to write test file 2");
    
    // Run the fold binary
    let output = Command::new("cargo")
        .args(&["run", "--bin", "fold"])
        .env("FOLD_STATE_DIR", test_dir)
        .output()
        .expect("Failed to execute fold binary");
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    
    // Verify the binary ran successfully
    assert!(output.status.success(), "Binary failed to run. stderr: {}", stderr);
    
    // Verify expected output patterns
    assert!(stdout.contains("Processing file"), "Should process files");
    assert!(stdout.contains("OPTIMAL ORTHO"), "Should find optimal ortho");
    assert!(stdout.contains("Total orthos generated"), "Should report total orthos");
    assert!(stdout.contains("FINAL OPTIMAL ORTHO"), "Should show final optimal");
    
    // Clean up
    let _ = fs::remove_dir_all(test_dir);
}

#[test]
fn test_fold_binary_handles_empty_input_directory() {
    // Setup test directory
    let test_dir = "/tmp/fold_test_empty";
    let input_dir = format!("{}/input", test_dir);
    
    // Clean up any previous test runs
    let _ = fs::remove_dir_all(test_dir);
    fs::create_dir_all(&input_dir).expect("Failed to create test input directory");
    
    // Run the fold binary with empty input directory
    let output = Command::new("cargo")
        .args(&["run", "--bin", "fold"])
        .env("FOLD_STATE_DIR", test_dir)
        .output()
        .expect("Failed to execute fold binary");
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    // Verify the binary ran successfully
    assert!(output.status.success(), "Binary should handle empty input gracefully");
    
    // Verify it reports no files
    assert!(stdout.contains("No input files found"), "Should report no input files");
    
    // Clean up
    let _ = fs::remove_dir_all(test_dir);
}

#[test]
fn test_fold_binary_tracks_optimal_across_files() {
    // Setup test directory
    let test_dir = "/tmp/fold_test_optimal";
    let input_dir = format!("{}/input", test_dir);
    
    // Clean up any previous test runs
    let _ = fs::remove_dir_all(test_dir);
    fs::create_dir_all(&input_dir).expect("Failed to create test input directory");
    
    // Create test input files with specific content to test optimal tracking
    fs::write(
        format!("{}/a_test.txt", input_dir),
        "one two three four five six seven"
    ).expect("Failed to write test file a");
    
    fs::write(
        format!("{}/b_test.txt", input_dir),
        "eight nine ten"
    ).expect("Failed to write test file b");
    
    // Run the fold binary
    let output = Command::new("cargo")
        .args(&["run", "--bin", "fold"])
        .env("FOLD_STATE_DIR", test_dir)
        .output()
        .expect("Failed to execute fold binary");
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    
    // Verify the binary ran successfully
    assert!(output.status.success(), "Binary failed to run");
    
    // Count how many times "OPTIMAL ORTHO" appears (once per file + final)
    let optimal_count = stdout.matches("OPTIMAL ORTHO").count();
    assert!(optimal_count >= 3, "Should show optimal at least 3 times (2 files + final)");
    
    // Clean up
    let _ = fs::remove_dir_all(test_dir);
}
