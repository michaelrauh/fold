use fold::{
    dfs_checkpoint::CheckpointManager,
    dfs_runner::{DfsConfig, DfsRunner},
    interner::Interner,
};
use std::fs;

#[test]
fn checkpoint_resume_matches_uninterrupted_run() {
    let temp_dir = tempfile::tempdir().unwrap();
    let state_dir = temp_dir.path().join("fold_state");
    let checkpoint_mgr = CheckpointManager::new(state_dir).unwrap();
    let interner = Interner::from_text("a b c. a d e.");
    let cfg = DfsConfig {
        checkpoint_every_nodes: 1,
        checkpoint_every_secs: 1,
        metrics_every_nodes: 1,
        max_frame_branch_cache: None,
    };

    checkpoint_mgr.write_interner(&interner).unwrap();

    let mut uninterrupted = DfsRunner::new();
    while !uninterrupted.is_finished() {
        uninterrupted.step(&interner).unwrap();
    }

    let mut partial = DfsRunner::new();
    for _ in 0..3 {
        if partial.is_finished() {
            break;
        }
        partial.step(&interner).unwrap();
    }

    checkpoint_mgr
        .save(
            &partial,
            checkpoint_mgr.interner_path(),
            12345,
            &cfg,
            "test",
        )
        .unwrap();

    let loaded = checkpoint_mgr.load().unwrap().unwrap();
    let mut resumed = loaded.runner;
    while !resumed.is_finished() {
        resumed.step(&loaded.interner).unwrap();
    }

    assert_eq!(loaded.manifest.version, 7);
    assert_eq!(resumed.incumbent_score(), uninterrupted.incumbent_score());
}

#[test]
fn version_one_checkpoint_is_rejected_with_clean_break_message() {
    let temp_dir = tempfile::tempdir().unwrap();
    let state_dir = temp_dir.path().join("fold_state");
    let checkpoint_mgr = CheckpointManager::new(state_dir).unwrap();

    fs::write(checkpoint_mgr.interner_path(), b"stale").unwrap();
    fs::write(
        checkpoint_mgr.root().join("checkpoints/current.state.bin"),
        b"stale",
    )
    .unwrap();
    fs::write(
        checkpoint_mgr
            .root()
            .join("checkpoints/current.manifest.json"),
        serde_json::json!({
            "version": 1,
            "input_path": "e.txt",
            "input_fingerprint": 1,
            "config_fingerprint": 1,
            "interner_path": "interner.bin",
            "state_path": "current.state.bin",
            "started_unix": 1,
            "checkpoint_unix": 1,
            "checkpoint_status": "stale",
            "nodes_expanded": 0,
            "nodes_pruned": 0,
            "completions_pruned": 0,
            "current_depth": 1,
            "max_depth": 1,
            "open_siblings_total": 0,
            "frontier_max_bound_volume": null,
            "frontier_max_bound_variance_num": null,
            "frontier_max_bound_variance_den": null,
            "frontier_max_bound_fullness": null,
            "last_improvement_unix": 1,
            "last_improvement_depth": 1,
            "incumbent_volume": 0,
            "incumbent_variance_num": 0,
            "incumbent_variance_den": 1,
            "incumbent_fullness": 0,
            "incumbent_dims": [2, 2]
        })
        .to_string(),
    )
    .unwrap();

    let err = checkpoint_mgr.load().unwrap_err();
    assert!(err.to_string().contains("checkpoint format changed"));
}
