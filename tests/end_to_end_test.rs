use fold::file_handler::StateConfig;
use std::fs;
use std::process::Command;

/// End-to-end test that simulates the full workflow for a single file:
///
/// 1. User puts a text file into working directory
/// 2. User calls stage.sh to split and move it
/// 3. User runs cargo run --release
/// 4. System processes chunk and produces archive
///
/// This test verifies:
/// - stage.sh correctly splits text into chunks
/// - fold processes the chunk and creates an archive
/// - Archive contains: interner.bin, results/, optimal.txt, lineage.txt
/// - Optimal ortho has expected structure and content
/// - Lineage tracks the source filename
/// - Input txt files are fully consumed (moved into archive)
///
/// The test uses a pattern that produces an interesting ortho structure.
/// Input contains words forming a pattern:
/// "red green blue yellow orange purple black white gray red yellow black green orange white blue purple gray"
///
/// This creates an ortho where the same colors appear in a structured pattern,
/// demonstrating the system's ability to find multi-dimensional word relationships.
///
/// With sorted canonical dims, the expected output is:
/// - Dimensions: [2, 7]
/// - Score: (volume=6, fullness=13)
/// - Geometry: color words visible in the structure
#[test]
fn test_end_to_end_single_file_workflow() {
    // Setup: Create a temporary test environment
    let test_dir = tempfile::tempdir().unwrap();
    let test_path = test_dir.path();

    // Create test input file with 3x3 pattern
    // This creates a more interesting ortho structure
    let input_file = test_path.join("test_input.txt");
    let test_content = "red green blue yellow orange purple black white gray red yellow black green orange white blue purple gray";
    fs::write(&input_file, test_content).unwrap();

    // Create state directory structure
    let state_dir = test_path.join("fold_state");
    let input_dir = state_dir.join("input");
    fs::create_dir_all(&input_dir).unwrap();

    // Step 1: Run stage.sh to split the file
    println!("[test] Running stage.sh...");
    let stage_output = Command::new("bash")
        .arg("./stage.sh")
        .arg(&input_file)
        .arg("2") // min_length = 2 words
        .arg(&state_dir)
        .output()
        .expect("Failed to execute stage.sh");

    assert!(
        stage_output.status.success(),
        "stage.sh failed: {}",
        String::from_utf8_lossy(&stage_output.stderr)
    );

    println!(
        "[test] stage.sh output: {}",
        String::from_utf8_lossy(&stage_output.stdout)
    );

    // Verify chunk was created (all sentences in one file)
    let chunks: Vec<_> = fs::read_dir(&input_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "txt")
                .unwrap_or(false)
        })
        .collect();

    assert_eq!(chunks.len(), 1, "Expected exactly 1 chunk file");
    println!("[test] Created {} chunk", chunks.len());

    // Read and verify chunk content
    let chunk = &chunks[0];
    let content = fs::read_to_string(chunk.path()).unwrap();
    println!(
        "[test] Chunk {}: '{}'",
        chunk.file_name().to_string_lossy(),
        content.trim()
    );
    assert_eq!(
        content.trim(),
        test_content,
        "Chunk content should match input"
    );

    // Step 2: Run the fold processor
    println!("[test] Running fold processor...");

    // Build the project first
    let build_output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .output()
        .expect("Failed to build project");

    assert!(
        build_output.status.success(),
        "Build failed: {}",
        String::from_utf8_lossy(&build_output.stderr)
    );

    // Use isolated test directory (not production fold_state)
    let test_state_dir = test_path.join("test_fold_state");
    let config = StateConfig::custom(test_state_dir.clone());

    fs::create_dir_all(config.input_dir()).unwrap();
    fs::create_dir_all(config.in_process_dir()).unwrap();

    // Copy chunks to test input directory
    for chunk in &chunks {
        let chunk_name = chunk.file_name();
        let dest = config.input_dir().join(chunk_name);
        fs::copy(chunk.path(), dest).unwrap();
    }

    // Run fold using the test directory via FOLD_STATE_DIR env var
    // (We'll need to update main.rs to support this)
    let fold_output = Command::new("./target/release/fold")
        .env("FOLD_STATE_DIR", test_state_dir.to_str().unwrap())
        .output()
        .expect("Failed to execute fold");

    println!(
        "[test] Fold output:\n{}",
        String::from_utf8_lossy(&fold_output.stdout)
    );

    if !fold_output.status.success() {
        println!(
            "[test] Fold stderr:\n{}",
            String::from_utf8_lossy(&fold_output.stderr)
        );
    }

    assert!(
        fold_output.status.success(),
        "Fold processing failed: {}",
        String::from_utf8_lossy(&fold_output.stderr)
    );

    // Step 3: Verify outputs
    println!("[test] Verifying outputs...");

    // Check that input directory now has only archive (txt file should be consumed)
    let remaining_files: Vec<_> = fs::read_dir(config.input_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();

    println!("[test] Remaining items in input: {}", remaining_files.len());
    for file in &remaining_files {
        println!("[test]   - {}", file.file_name().to_string_lossy());
    }

    // Should have exactly 1 archive now
    let archives: Vec<_> = remaining_files
        .iter()
        .filter(|e| {
            e.path().is_dir()
                && e.path()
                    .extension()
                    .map(|ext| ext == "bin")
                    .unwrap_or(false)
        })
        .collect();

    assert_eq!(archives.len(), 1, "Expected exactly 1 archive");
    println!("[test] Created 1 archive as expected");

    // Verify archive structure
    let archive = archives[0];
    let archive_path = archive.path();
    println!(
        "[test] Checking archive: {}",
        archive.file_name().to_string_lossy()
    );

    // Check for required files
    let interner_path = archive_path.join("interner.bin");
    let results_path = archive_path.join("results");
    let optimal_path = archive_path.join("optimal.txt");
    let lineage_path = archive_path.join("lineage.txt");

    assert!(interner_path.exists(), "interner.bin not found in archive");
    assert!(
        results_path.exists(),
        "results directory not found in archive"
    );
    assert!(optimal_path.exists(), "optimal.txt not found in archive");
    assert!(lineage_path.exists(), "lineage.txt not found in archive");

    // Read and display optimal ortho
    let optimal_content = fs::read_to_string(&optimal_path).unwrap();
    println!("[test] Optimal ortho content:\n{}", optimal_content);

    // Verify optimal ortho contains expected structure
    // With sorted canonical dims, the shape [2,7] is now the optimal found
    assert!(
        optimal_content.contains("OPTIMAL ORTHO"),
        "Optimal file missing header"
    );
    assert!(
        optimal_content.contains("Ortho ID:"),
        "Optimal file missing ortho ID"
    );
    assert!(
        optimal_content.contains("Dimensions: [2, 7]"),
        "Expected dimensions [2, 7]"
    );
    assert!(
        optimal_content.contains("Score: (volume=6, fullness=13)"),
        "Expected score (volume=6, fullness=13)"
    );

    // Verify geometry contains color words (the grid pattern is determined by the new sorted dims layout)
    assert!(
        optimal_content.contains("red"),
        "Expected 'red' in geometry"
    );
    assert!(
        optimal_content.contains("green"),
        "Expected 'green' in geometry"
    );
    assert!(
        optimal_content.contains("blue"),
        "Expected 'blue' in geometry"
    );
    assert!(
        optimal_content.contains("yellow"),
        "Expected 'yellow' in geometry"
    );
    assert!(
        optimal_content.contains("orange"),
        "Expected 'orange' in geometry"
    );
    assert!(
        optimal_content.contains("purple"),
        "Expected 'purple' in geometry"
    );
    assert!(
        optimal_content.contains("black"),
        "Expected 'black' in geometry"
    );
    assert!(
        optimal_content.contains("white"),
        "Expected 'white' in geometry"
    );
    assert!(
        optimal_content.contains("gray"),
        "Expected 'gray' in geometry"
    );

    // Read and verify lineage (should be a single filename in quotes)
    let lineage_content = fs::read_to_string(&lineage_path).unwrap();
    println!("[test] Lineage: {}", lineage_content);
    assert_eq!(
        lineage_content, "\"test_input_chunk_0001\"",
        "Expected exact lineage match"
    );

    // Check results directory has content
    let results_files: Vec<_> = fs::read_dir(&results_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!results_files.is_empty(), "Results directory is empty");
    println!(
        "[test] Results directory has {} file(s)",
        results_files.len()
    );

    // Verify no txt files remain (all consumed)
    let txt_files: Vec<_> = remaining_files
        .iter()
        .filter(|e| {
            e.path().is_file()
                && e.path()
                    .extension()
                    .map(|ext| ext == "txt")
                    .unwrap_or(false)
        })
        .collect();
    assert_eq!(txt_files.len(), 0, "All txt files should be consumed");

    // No cleanup - leave fold_state as-is to expose multi-file processing bugs
    println!("[test] End-to-end test completed successfully!");
    println!("[test] Note: fold_state remains for subsequent test runs to expose state bugs");
}
