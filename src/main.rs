use fold::{
    FoldError,
    dfs_checkpoint::{CheckpointManager, LoadedCheckpoint},
    dfs_runner::{
        DfsConfig, DfsRunner, bound_reuse_enabled, bound_reuse_shadow_verify_enabled,
        parallel_child_bounds_enabled, parallel_child_bounds_min_branches,
    },
    interner::Interner,
    metrics::Metrics,
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
use std::time::{Duration, Instant};

const DEFAULT_RAYON_NUM_THREADS: usize = 2;

fn main() -> Result<(), FoldError> {
    let rayon_threads = initialize_rayon()?;

    let state_dir = std::env::var("FOLD_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("fold_state"));
    fs::create_dir_all(&state_dir)?;
    fs::create_dir_all(state_dir.join("logs"))?;

    let cfg = DfsConfig::from_env();
    let checkpoint_mgr = CheckpointManager::new(state_dir.clone())?;
    let metrics = Metrics::new();
    let should_quit = Arc::new(AtomicBool::new(false));

    {
        let quit = Arc::clone(&should_quit);
        ctrlc::set_handler(move || {
            quit.store(true, Ordering::Relaxed);
        })
        .map_err(|e| FoldError::Other(format!("failed to install signal handler: {}", e)))?;
    }

    let tui_handle = spawn_tui_if_enabled(&metrics, &should_quit, &state_dir);

    let loaded = checkpoint_mgr.load()?;
    let (input_path, input_fingerprint, interner, mut runner, resumed) =
        initialize_run(&checkpoint_mgr, &cfg, loaded, &metrics)?;

    let mut incumbent_display = format!("{}", runner.incumbent().display(&interner));
    let mut last_metrics_nodes = 0u64;
    let mut last_checkpoint_nodes = 0u64;
    let mut last_checkpoint_at = Instant::now();
    let mut last_checkpoint_score = runner.incumbent_score();

    update_metrics(
        &metrics,
        &runner,
        &input_path,
        &incumbent_display,
        if resumed { "Resumed" } else { "Running" },
    );
    let reuse_enabled = bound_reuse_enabled();
    let reuse_shadow_verify = bound_reuse_shadow_verify_enabled();
    metrics.add_log(format!(
        "Bound reuse: enabled={} shadow_verify={}",
        reuse_enabled, reuse_shadow_verify
    ));
    let parallel_child_bounds = parallel_child_bounds_enabled();
    let parallel_child_bounds_min = parallel_child_bounds_min_branches();
    metrics.add_log(format!(
        "Parallel child bounds: enabled={} min_branches={}",
        parallel_child_bounds, parallel_child_bounds_min
    ));
    metrics.add_log(format!("Rayon threads: {}", rayon_threads));
    metrics.add_log(format!(
        "{} search for {}",
        if resumed { "Resumed" } else { "Starting" },
        input_path.display()
    ));

    if !resumed {
        let manifest =
            checkpoint_mgr.save(&runner, &input_path, input_fingerprint, &cfg, "initial")?;
        save_outputs(
            &checkpoint_mgr,
            &runner,
            &incumbent_display,
            &input_path,
            input_fingerprint,
            &cfg,
            "initial",
            rayon_threads,
        )?;
        metrics.update_global(|g| {
            g.checkpoint_status = manifest.checkpoint_status.clone();
            g.checkpoint_time = manifest.checkpoint_unix;
        });
    }

    while !runner.is_finished() && !should_quit.load(Ordering::Relaxed) {
        let event = runner.step(&interner)?;

        if event.incumbent_improved {
            incumbent_display = format!("{}", runner.incumbent().display(&interner));
            metrics.add_log(format!(
                "Incumbent improved: vol={} dims={:?}",
                runner.incumbent_score().volume,
                runner.incumbent().dims()
            ));
        }

        let should_refresh_metrics = runner.nodes_expanded().saturating_sub(last_metrics_nodes)
            >= cfg.metrics_every_nodes
            || event.incumbent_improved
            || event.finished;
        if should_refresh_metrics {
            update_metrics(
                &metrics,
                &runner,
                &input_path,
                &incumbent_display,
                "Running",
            );
            last_metrics_nodes = runner.nodes_expanded();
        }

        let should_checkpoint = event.incumbent_improved
            || runner
                .nodes_expanded()
                .saturating_sub(last_checkpoint_nodes)
                >= cfg.checkpoint_every_nodes
            || last_checkpoint_at.elapsed() >= Duration::from_secs(cfg.checkpoint_every_secs);

        if should_checkpoint {
            let checkpoint_status = if event.incumbent_improved {
                "best-improved"
            } else {
                "periodic"
            };
            let manifest = checkpoint_mgr.save(
                &runner,
                &input_path,
                input_fingerprint,
                &cfg,
                checkpoint_status,
            )?;
            save_outputs(
                &checkpoint_mgr,
                &runner,
                &incumbent_display,
                &input_path,
                input_fingerprint,
                &cfg,
                checkpoint_status,
                rayon_threads,
            )?;
            metrics.update_global(|g| {
                g.checkpoint_status = manifest.checkpoint_status.clone();
                g.checkpoint_time = manifest.checkpoint_unix;
            });
            metrics.add_log(format!(
                "Checkpoint saved: status={} nodes={} depth={}",
                checkpoint_status,
                runner.nodes_expanded(),
                runner.current_depth()
            ));
            last_checkpoint_nodes = runner.nodes_expanded();
            last_checkpoint_at = Instant::now();
            last_checkpoint_score = runner.incumbent_score();
        } else if runner.incumbent_score() > last_checkpoint_score {
            last_checkpoint_score = runner.incumbent_score();
        }
    }

    let final_status = if runner.is_finished() {
        "complete"
    } else {
        "interrupted"
    };
    let manifest =
        checkpoint_mgr.save(&runner, &input_path, input_fingerprint, &cfg, final_status)?;
    save_outputs(
        &checkpoint_mgr,
        &runner,
        &incumbent_display,
        &input_path,
        input_fingerprint,
        &cfg,
        final_status,
        rayon_threads,
    )?;
    update_metrics(
        &metrics,
        &runner,
        &input_path,
        &incumbent_display,
        if runner.is_finished() {
            "Complete"
        } else {
            "Interrupted"
        },
    );
    metrics.update_global(|g| {
        g.checkpoint_status = manifest.checkpoint_status.clone();
        g.checkpoint_time = manifest.checkpoint_unix;
    });
    metrics.add_log(format!(
        "Run {}: expanded={} pruned={} best_volume={}",
        final_status,
        runner.nodes_expanded(),
        runner.nodes_pruned(),
        runner.incumbent_score().volume
    ));

    should_quit.store(true, Ordering::Relaxed);
    if let Some(handle) = tui_handle {
        let _ = handle.join();
    }

    println!(
        "DFS/BnB {}. Best volume={} dims={:?} outputs={}",
        final_status,
        runner.incumbent_score().volume,
        runner.incumbent().dims(),
        checkpoint_mgr.output_dir().display()
    );

    Ok(())
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

fn initialize_run(
    checkpoint_mgr: &CheckpointManager,
    cfg: &DfsConfig,
    loaded: Option<LoadedCheckpoint>,
    metrics: &Metrics,
) -> Result<(PathBuf, u64, Interner, DfsRunner, bool), FoldError> {
    if let Some(loaded) = loaded {
        if loaded.manifest.config_fingerprint != cfg.fingerprint() {
            metrics.add_log(format!(
                "Checkpoint config fingerprint differs (checkpoint={} current={}); resuming anyway",
                loaded.manifest.config_fingerprint,
                cfg.fingerprint()
            ));
        }
        let input_path = PathBuf::from(&loaded.manifest.input_path);
        return Ok((
            input_path,
            loaded.manifest.input_fingerprint,
            loaded.interner,
            loaded.runner,
            true,
        ));
    }

    let input_path = resolve_input_path(checkpoint_mgr.root())?;
    let input_bytes = fs::read(&input_path)?;
    let input_fingerprint = fingerprint_bytes(&input_bytes);
    let text = String::from_utf8(input_bytes)
        .map_err(|e| FoldError::Other(format!("input is not valid UTF-8: {}", e)))?;
    let interner = Interner::from_text(&text);
    checkpoint_mgr.write_interner(&interner)?;
    Ok((
        input_path,
        input_fingerprint,
        interner,
        DfsRunner::new(),
        false,
    ))
}

fn update_metrics(
    metrics: &Metrics,
    runner: &DfsRunner,
    input_path: &Path,
    incumbent_display: &str,
    phase: &str,
) {
    let snapshot = runner.snapshot();
    metrics.update_global(|g| {
        g.input_path = input_path.display().to_string();
        g.phase = phase.to_string();
        g.start_time = snapshot.started_unix;
        g.nodes_expanded = snapshot.nodes_expanded;
        g.nodes_pruned = snapshot.nodes_pruned;
        g.completions_pruned = snapshot.completions_pruned;
        g.current_depth = snapshot.current_depth;
        g.max_depth = snapshot.max_depth;
        g.current_bound = snapshot
            .current_bound
            .unwrap_or_else(|| snapshot.incumbent.score());
        g.open_siblings_total = snapshot.open_siblings_total;
        g.open_siblings_by_depth = snapshot.open_siblings_by_depth.clone();
        g.seen_by_depth = snapshot.seen_by_depth.clone();
        g.descended_by_depth = snapshot.descended_by_depth.clone();
        g.pruned_by_depth = snapshot.pruned_by_depth.clone();
        g.path_progress_by_depth = snapshot.path_progress_by_depth.clone();
        g.frontier_max_bound = snapshot.frontier_max_bound;
        g.incumbent_score = snapshot.incumbent.score();
        g.incumbent_dims = snapshot.incumbent.dims().clone();
        g.incumbent_capacity = snapshot.incumbent.payload().len();
        g.incumbent_display = incumbent_display.to_string();
        g.last_improvement_unix = snapshot.last_improvement_unix;
        g.last_improvement_depth = snapshot.last_improvement_depth;
        g.dedup_lookups = snapshot.dedup_lookups;
        g.dedup_hits = snapshot.dedup_hits;
    });
}

fn save_outputs(
    checkpoint_mgr: &CheckpointManager,
    runner: &DfsRunner,
    incumbent_display: &str,
    input_path: &Path,
    input_fingerprint: u64,
    cfg: &DfsConfig,
    status: &str,
    rayon_threads: usize,
) -> Result<(), FoldError> {
    let snapshot = runner.snapshot();
    let summary = json!({
        "input_path": input_path.display().to_string(),
        "input_fingerprint": input_fingerprint,
        "config_fingerprint": cfg.fingerprint(),
        "status": status,
        "started_unix": snapshot.started_unix,
        "finished": snapshot.finished,
        "nodes_expanded": snapshot.nodes_expanded,
        "nodes_pruned": snapshot.nodes_pruned,
        "completions_pruned": snapshot.completions_pruned,
        "current_depth": snapshot.current_depth,
        "max_depth": snapshot.max_depth,
        "open_siblings_total": snapshot.open_siblings_total,
        "seen_by_depth": snapshot.seen_by_depth,
        "last_improvement_unix": snapshot.last_improvement_unix,
        "last_improvement_depth": snapshot.last_improvement_depth,
        "frontier_max_bound": snapshot.frontier_max_bound.map(|bound| json!({
            "volume": bound.volume,
            "variance_num": bound.variance_num,
            "variance_den": bound.variance_den,
            "fullness": bound.fullness,
        })),
        "best_score": {
            "volume": snapshot.incumbent.score().volume,
            "variance_num": snapshot.incumbent.score().variance_num,
            "variance_den": snapshot.incumbent.score().variance_den,
            "fullness": snapshot.incumbent.score().fullness,
        },
        "best_dims": snapshot.incumbent.dims(),
        "best_capacity": snapshot.incumbent.payload().len(),
        "bound_reuse_enabled": bound_reuse_enabled(),
        "bound_reuse_shadow_verify": bound_reuse_shadow_verify_enabled(),
        "parallel_child_bounds_enabled": parallel_child_bounds_enabled(),
        "parallel_child_bounds_min_branches": parallel_child_bounds_min_branches(),
        "rayon_num_threads": rayon_threads,
    });
    checkpoint_mgr.save_optimal(runner.incumbent(), incumbent_display, &summary)
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
    let snapshot_path = state_dir.join("logs").join("tui_state.log");
    Some(thread::spawn(move || {
        let mut tui = Tui::new(metrics, should_quit, Some(snapshot_path));
        let _ = tui.run();
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
