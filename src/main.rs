use fold::{
    FoldError,
    dfs_checkpoint::CheckpointManager,
    interner::Interner,
    metrics::Metrics,
    parallel_search::{ParallelCheckpointStore, ParallelSearchConfig, ParallelSearchResult, run_parallel_search},
    tui::Tui,
};
use rayon::ThreadPoolBuilder;
use serde_json::json;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

const DEFAULT_RAYON_NUM_THREADS: usize = 2;

fn main() -> Result<(), FoldError> {
    let rayon_threads = initialize_rayon()?;

    let state_dir = std::env::var("FOLD_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("fold_state"));
    fs::create_dir_all(&state_dir)?;
    fs::create_dir_all(state_dir.join("logs"))?;

    let checkpoint_mgr = CheckpointManager::new(state_dir.clone())?;
    let metrics = Metrics::new();
    let should_quit = Arc::new(AtomicBool::new(false));
    let parallel_cfg = ParallelSearchConfig::from_env();

    {
        let quit = Arc::clone(&should_quit);
        ctrlc::set_handler(move || {
            quit.store(true, Ordering::Relaxed);
        })
        .map_err(|e| FoldError::Other(format!("failed to install signal handler: {}", e)))?;
    }

    let tui_handle = spawn_tui_if_enabled(&metrics, &should_quit, &state_dir);
    metrics.add_log(format!("Rayon threads: {}", rayon_threads));
    run_parallel_main(
        parallel_cfg,
        checkpoint_mgr,
        metrics,
        should_quit,
        tui_handle,
    )
}

fn initialize_rayon() -> Result<usize, FoldError> {
    let threads = resolve_rayon_num_threads(std::env::var("RAYON_NUM_THREADS").ok().as_deref());
    ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .map_err(|e| FoldError::Other(format!("failed to initialize rayon thread pool: {}", e)))?;
    Ok(threads)
}

fn resolve_rayon_num_threads(env_value: Option<&str>) -> usize {
    env_value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&threads| threads > 0)
        .unwrap_or(DEFAULT_RAYON_NUM_THREADS)
}

fn run_parallel_main(
    parallel_cfg: ParallelSearchConfig,
    checkpoint_mgr: CheckpointManager,
    metrics: Metrics,
    should_quit: Arc<AtomicBool>,
    tui_handle: Option<thread::JoinHandle<()>>,
) -> Result<(), FoldError> {
    let input_path = resolve_input_path(checkpoint_mgr.root())?;
    let input_bytes = fs::read(&input_path)?;
    let input_fingerprint = fingerprint_bytes(&input_bytes);
    let text = String::from_utf8(input_bytes)
        .map_err(|e| FoldError::Other(format!("input is not valid UTF-8: {}", e)))?;
    let interner = Arc::new(Interner::from_text(&text));
    checkpoint_mgr.write_interner(&interner)?;
    let checkpoint_store =
        ParallelCheckpointStore::new(checkpoint_mgr.root(), checkpoint_mgr.output_dir())?;
    let resume_state = if parallel_cfg.resume_enabled {
        match checkpoint_store.load(input_fingerprint, &parallel_cfg) {
            Ok(state) => {
                if state.is_some() {
                    metrics.add_log("Resuming from parallel checkpoint".to_string());
                }
                state
            }
            Err(e) => {
                metrics.add_log(format!("Parallel checkpoint load failed, starting fresh: {}", e));
                None
            }
        }
    } else {
        None
    };
    metrics.add_log(format!(
        "Starting parallel search workers={} shard_depth={} hunt_nodes={}",
        parallel_cfg.workers, parallel_cfg.shard_depth, parallel_cfg.hunt_nodes
    ));

    let result = run_parallel_search(
        Arc::clone(&interner),
        metrics.clone(),
        Arc::clone(&should_quit),
        parallel_cfg.clone(),
        input_path.display().to_string(),
        input_fingerprint,
        checkpoint_store,
        resume_state,
    )?;
    let final_status = if result.finished {
        "parallel-complete"
    } else {
        "parallel-interrupted"
    };
    save_parallel_outputs(
        &checkpoint_mgr,
        &result,
        &interner,
        &input_path,
        input_fingerprint,
        &parallel_cfg,
        final_status,
    )?;
    metrics.add_log(format!(
        "Parallel run {}: expanded={} pruned={} best_volume={}",
        final_status,
        result.nodes_expanded,
        result.nodes_pruned,
        result.best.score().volume
    ));
    should_quit.store(true, Ordering::Relaxed);
    if let Some(handle) = tui_handle {
        let _ = handle.join();
    }
    println!(
        "Parallel DFS/BnB {}. Best volume={} dims={:?} outputs={}",
        final_status,
        result.best.score().volume,
        result.best.dims(),
        checkpoint_mgr.output_dir().display()
    );
    Ok(())
}

fn save_parallel_outputs(
    checkpoint_mgr: &CheckpointManager,
    result: &ParallelSearchResult,
    interner: &Interner,
    input_path: &Path,
    input_fingerprint: u64,
    cfg: &ParallelSearchConfig,
    status: &str,
) -> Result<(), FoldError> {
    let display = format!("{}", result.best.display(interner));
    let summary = json!({
        "mode": "parallel",
        "input_path": input_path.display().to_string(),
        "input_fingerprint": input_fingerprint,
        "status": status,
        "started_unix": result.started_unix,
        "finished": result.finished,
        "nodes_expanded": result.nodes_expanded,
        "nodes_pruned": result.nodes_pruned,
        "completions_pruned": result.completions_pruned,
        "last_improvement_unix": result.last_improvement_unix,
        "last_improvement_depth": result.last_improvement_depth,
        "total_shards": result.total_shards,
        "shards_done": result.shards_done,
        "workers": cfg.workers,
        "hunt_nodes": cfg.hunt_nodes,
        "shard_depth": cfg.shard_depth,
        "completion_pruning_default": false,
        "best_score": {
            "volume": result.best.score().volume,
            "variance_num": result.best.score().variance_num,
            "variance_den": result.best.score().variance_den,
            "fullness": result.best.score().fullness,
        },
        "best_dims": result.best.dims(),
        "best_capacity": result.best.payload().len(),
    });
    checkpoint_mgr.save_optimal(&result.best, &display, &summary)
}

fn resolve_input_path(state_dir: &Path) -> Result<PathBuf, FoldError> {
    if let Some(arg) = std::env::args_os().nth(1) {
        return Ok(PathBuf::from(arg));
    }
    if let Ok(env_path) = std::env::var("FOLD_INPUT_FILE") {
        return Ok(PathBuf::from(env_path));
    }
    let input_dir = state_dir.join("input");
    let mut txt_files = Vec::new();
    if input_dir.exists() {
        for entry in fs::read_dir(&input_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("txt") {
                txt_files.push(path);
            }
        }
    }
    txt_files.sort();
    match txt_files.len() {
        1 => Ok(txt_files.remove(0)),
        0 => Err(FoldError::Other(format!(
            "no input file found; pass a file path, set FOLD_INPUT_FILE, or place one .txt in {}",
            input_dir.display()
        ))),
        _ => Err(FoldError::Other(format!(
            "multiple input files found in {}; pass the exact file path",
            input_dir.display()
        ))),
    }
}

fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn spawn_tui_if_enabled(
    metrics: &Metrics,
    should_quit: &Arc<AtomicBool>,
    state_dir: &Path,
) -> Option<thread::JoinHandle<()>> {
    let tui_enabled = std::env::var("FOLD_DISABLE_TUI").is_err() && std::io::stdout().is_terminal();
    if !tui_enabled {
        return None;
    }
    let metrics = metrics.clone();
    let should_quit = Arc::clone(should_quit);
    let snapshot_path = Some(state_dir.join("logs").join("tui_state.log"));
    Some(thread::spawn(move || {
        let mut tui = Tui::new(metrics, should_quit, snapshot_path);
        if let Err(e) = tui.run() {
            eprintln!("TUI error: {}", e);
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::resolve_rayon_num_threads;

    #[test]
    fn rayon_threads_default_to_two_when_unset() {
        assert_eq!(resolve_rayon_num_threads(None), 2);
    }

    #[test]
    fn rayon_threads_honor_positive_env_override() {
        assert_eq!(resolve_rayon_num_threads(Some("4")), 4);
    }

    #[test]
    fn rayon_threads_ignore_invalid_or_zero_env_values() {
        assert_eq!(resolve_rayon_num_threads(Some("0")), 2);
        assert_eq!(resolve_rayon_num_threads(Some("nope")), 2);
    }
}
