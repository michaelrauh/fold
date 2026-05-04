use std::fs;
use std::process::Command;

#[test]
fn cli_runs_single_input_and_writes_dfs_outputs() {
    let temp_dir = tempfile::tempdir().unwrap();
    let state_dir = temp_dir.path().join("fold_state");
    let input_path = temp_dir.path().join("input.txt");
    fs::write(&input_path, "a b c. a d e.").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_fold"))
        .arg(&input_path)
        .env("FOLD_STATE_DIR", &state_dir)
        .env("FOLD_DISABLE_TUI", "1")
        .env("FOLD_CHECKPOINT_EVERY_NODES", "2")
        .output()
        .expect("failed to run fold");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(state_dir.join("checkpoints/current.manifest.json").exists());
    assert!(state_dir.join("output/optimal.bin").exists());
    assert!(state_dir.join("output/optimal.txt").exists());
    assert!(state_dir.join("output/summary.json").exists());
    assert!(!state_dir.join("in_process").exists());
    assert!(!state_dir.join("history").exists());
    assert!(!state_dir.join("work").exists());

    let summary: serde_json::Value =
        serde_json::from_slice(&fs::read(state_dir.join("output/summary.json")).unwrap()).unwrap();
    assert_eq!(
        summary["parallel_child_bounds_enabled"],
        serde_json::Value::Bool(cfg!(not(debug_assertions)))
    );
    assert_eq!(summary["rayon_num_threads"], serde_json::Value::from(2));
}
