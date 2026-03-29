use fold::{
    FoldError,
    completion_pruning::bound_existing_ortho,
    disk_safety,
    file_handler::{self, ArchivePairPolicy, MemClaimGuard, StateConfig},
    generation_runner::{merge_threads_from_env, run_generation_loop, run_merge_generation_loop},
    generation_store::{Config, GenerationStore, Role},
    interner::Interner,
    memory_budget::MemoryBudget,
    memory_safety,
    metrics::Metrics,
    offload_config::OffloadConfig,
    offload_runtime::configure_offload_runtime,
    ortho::{Ortho, OrthoScore, payload_to_usize},
    tui::Tui,
};
use std::any::Any;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Instant;

// Helper to convert Role to string
fn role_as_str(role: Role) -> &'static str {
    match role {
        Role::Leader => "leader",
        Role::Follower => "follower",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MergePolicy {
    LargestSmallest,
    LargestLargest,
    SmallestSmallest,
}

impl MergePolicy {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "largest_smallest" => Some(Self::LargestSmallest),
            "largest_largest" => Some(Self::LargestLargest),
            "smallest_smallest" => Some(Self::SmallestSmallest),
            _ => None,
        }
    }

    fn default_for_role(role: Role) -> Self {
        match role {
            Role::Leader => Self::LargestLargest,
            Role::Follower => Self::SmallestSmallest,
        }
    }

    fn archive_pair_policy(self) -> ArchivePairPolicy {
        match self {
            Self::LargestSmallest => ArchivePairPolicy::LargestSmallest,
            Self::LargestLargest => ArchivePairPolicy::LargestLargest,
            Self::SmallestSmallest => ArchivePairPolicy::SmallestSmallest,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::LargestSmallest => "largest_smallest",
            Self::LargestLargest => "largest_largest",
            Self::SmallestSmallest => "smallest_smallest",
        }
    }
}

fn merge_policy_for_role(role: Role) -> MergePolicy {
    std::env::var("FOLD_MERGE_POLICY")
        .ok()
        .as_deref()
        .and_then(MergePolicy::parse)
        .unwrap_or_else(|| MergePolicy::default_for_role(role))
}

enum RunFailure {
    Error(FoldError),
    Panic {
        source: &'static str,
        message: String,
        payload: Box<dyn Any + Send>,
    },
}

fn main() -> Result<(), FoldError> {
    let program_start = Instant::now();
    // Check for test environment variable
    let config = if let Ok(test_dir) = std::env::var("FOLD_STATE_DIR") {
        StateConfig::custom(PathBuf::from(test_dir))
    } else {
        StateConfig::default()
    };

    // Initialize: setup directories and recover abandoned files
    file_handler::initialize_with_config(&config)?;

    // Initialize metrics and TUI
    let metrics = Metrics::new();
    let log_dir = config.logs_dir();
    fs::create_dir_all(&log_dir)?;
    metrics.add_log("Log initialized".to_string());
    let should_quit = Arc::new(AtomicBool::new(false));
    let tui_snapshot_path = log_dir.join("tui_state.log");

    let tui_enabled = std::env::var("FOLD_DISABLE_TUI").is_err() && std::io::stdout().is_terminal();

    // Spawn TUI thread with panic-forwarding so we don't leave the terminal in a broken state.
    let tui_handle = if tui_enabled {
        let metrics_clone = metrics.clone_handle();
        let should_quit_clone = Arc::clone(&should_quit);
        let snapshot_path = tui_snapshot_path.clone();
        Some(thread::spawn(move || {
            let result = std::panic::catch_unwind(move || {
                let mut tui = Tui::new(metrics_clone, should_quit_clone, Some(snapshot_path));
                tui.run()
            });
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => panic!("TUI error: {}", e),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }))
    } else {
        metrics.add_log("TUI disabled (no TTY or FOLD_DISABLE_TUI set)".to_string());
        None
    };

    // Count initial chunks
    let total_chunks = file_handler::count_all_chunks_with_config(&config)?;
    metrics.update_global(|g| {
        g.total_chunks = total_chunks;
        g.remaining_chunks = total_chunks;
    });

    // Initialize largest archive metric from existing archives
    if let Ok(Some(largest)) = file_handler::find_largest_archive_with_config(&config) {
        metrics.update_largest_archive(|la| {
            la.filename = largest.path.clone();
            la.ortho_count = largest.ortho_count;
            la.lineage = largest.lineage;
        });

        // Load and restore the optimal ortho from the largest archive
        let optimal_ortho = file_handler::load_optimal_ortho(&largest.path)?;
        let interner = file_handler::load_interner(&largest.path)?;

        let score = optimal_ortho.score();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        metrics.update_optimal_ortho(|opt| {
            opt.volume = score.volume;
            opt.variance_num = score.variance_num;
            opt.variance_den = score.variance_den;
            opt.dims = optimal_ortho.dims().clone();
            opt.fullness = score.fullness;
            opt.capacity = optimal_ortho.payload().len();
            opt.payload = optimal_ortho.payload().clone();
            opt.vocab = interner.vocabulary().to_vec();
            opt.last_update_time = now;
        });
        metrics.update_global(|g| {
            g.vocab_size = interner.vocabulary().len();
            g.interner_version = interner.version();
        });
        metrics.add_log(format!(
            "Restored optimal ortho from archive: volume={}",
            score.volume
        ));
    }

    // Main processing loop - two modes:
    // Mode 1: Merge archives (leaders pick the largest pair; followers merge the smallest pair only when no text is free)
    // Mode 2: Process txt into result
    let main_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), FoldError> {
            let mut last_role: Option<Role> = None;
            loop {
                // Check if user requested quit
                if should_quit.load(Ordering::Relaxed) {
                    metrics.add_log("User requested quit".to_string());
                    break;
                }

                // Check for stale heartbeats and recover abandoned work from crashed processes
                file_handler::check_and_recover_stale_work(&config)?;
                file_handler::cleanup_stale_mem_claims(&config)?;

                let role = determine_role(&config)?;
                if last_role != Some(role) {
                    metrics.add_log(format!("Role change: {:?}", role));
                    metrics.update_global(|g| g.role = role_as_str(role).to_string());
                    last_role = Some(role);
                } else {
                    metrics.update_global(|g| g.role = role_as_str(role).to_string());
                }

                // Update the count of distinct running jobs
                let jobs_count = file_handler::count_running_jobs_with_config(&config)?;
                let remaining_chunks = file_handler::count_all_chunks_with_config(&config)?;
                metrics.update_global(|g| {
                    g.distinct_jobs_count = jobs_count;
                    g.remaining_chunks = remaining_chunks;
                    if remaining_chunks > g.total_chunks {
                        g.total_chunks = remaining_chunks;
                    }
                });

                match role {
                    Role::Leader => {
                        let merge_policy = merge_policy_for_role(role);
                        let archive_pair = file_handler::get_archive_pair_with_config(
                            &config,
                            merge_policy.archive_pair_policy(),
                        )?;

                        if let Some((archive_a, archive_b)) = archive_pair {
                            // Mode 1: Merge archives
                            metrics.update_global(|g| g.mode = "Merging Archives".to_string());
                            metrics.clear_chart_history();
                            metrics.add_log("MODE 1: Merging archives".to_string());
                            metrics.add_log(format!(
                                "Merging (policy={}): {} + {}",
                                merge_policy.as_str(),
                                archive_a,
                                archive_b
                            ));

                            match merge_archives(&archive_a, &archive_b, &config, &metrics, role) {
                                Ok(()) => {}
                                Err(e) if is_concurrent_claim_error(&e) => {
                                    metrics.add_log(format!(
                                        "Lost race to claim archives ({}); retrying selection",
                                        e
                                    ));
                                    continue;
                                }
                                Err(e) => return Err(e),
                            }
                        } else {
                            // Mode 2: Process txt file
                            let txt_file = file_handler::find_txt_file_with_config(&config)?;

                            if txt_file.is_none() {
                                metrics.add_log("No more files to process".to_string());
                                metrics.add_log("Processing completed".to_string());
                                break;
                            }

                            metrics.update_global(|g| g.mode = "Processing Text".to_string());
                            metrics.clear_chart_history();
                            metrics.add_log("MODE 2: Processing text file".to_string());

                            let txt_file = txt_file.unwrap();
                            match process_txt_file(
                                txt_file.clone(),
                                &config,
                                &metrics,
                                role,
                                &should_quit,
                            ) {
                                Ok(()) => {}
                                Err(e) if is_concurrent_claim_error(&e) => {
                                    metrics.add_log(format!(
                                        "Lost race to claim {}; retrying selection",
                                        txt_file
                                    ));
                                    continue;
                                }
                                Err(e) => return Err(e),
                            }
                        }
                    }
                    Role::Follower => {
                        let merge_policy = merge_policy_for_role(role);
                        // Followers prioritize ingesting new text; merge smallest archives only when none available
                        if let Some(txt_file) = file_handler::find_txt_file_with_config(&config)? {
                            metrics.update_global(|g| g.mode = "Processing Text".to_string());
                            metrics.clear_chart_history();
                            metrics.add_log("MODE 2: Processing text file".to_string());

                            match process_txt_file(
                                txt_file.clone(),
                                &config,
                                &metrics,
                                role,
                                &should_quit,
                            ) {
                                Ok(()) => {}
                                Err(e) if is_concurrent_claim_error(&e) => {
                                    metrics.add_log(format!(
                                        "Lost race to claim {}; retrying selection",
                                        txt_file
                                    ));
                                    continue;
                                }
                                Err(e) => return Err(e),
                            }
                        } else if let Some((archive_a, archive_b)) =
                            file_handler::get_archive_pair_with_config(
                                &config,
                                merge_policy.archive_pair_policy(),
                            )?
                        {
                            // Mode 1 for followers: merge archives using the configured policy when no text is free
                            metrics.update_global(|g| g.mode = "Merging Archives".to_string());
                            metrics.clear_chart_history();
                            metrics.add_log("MODE 1: Merging archives".to_string());
                            metrics.add_log(format!(
                                "Merging (policy={}): {} + {}",
                                merge_policy.as_str(),
                                archive_a,
                                archive_b
                            ));

                            match merge_archives(&archive_a, &archive_b, &config, &metrics, role) {
                                Ok(()) => {}
                                Err(e) if is_concurrent_claim_error(&e) => {
                                    metrics.add_log(format!(
                                        "Lost race to claim archives ({}); retrying selection",
                                        e
                                    ));
                                    continue;
                                }
                                Err(e) => return Err(e),
                            }
                        } else {
                            metrics.add_log("No more files to process".to_string());
                            metrics.add_log("Processing completed".to_string());
                            break;
                        }
                    }
                }
            }
            Ok(())
        }));

    // Signal TUI to quit and wait for it
    should_quit.store(true, Ordering::Relaxed);
    let tui_result = if let Some(handle) = tui_handle {
        Some(handle.join())
    } else {
        None
    };

    cleanup_leader_lock(&config);

    let failure = match (main_result, tui_result) {
        (Ok(Ok(())), Some(Ok(()))) | (Ok(Ok(())), None) => None,
        (Ok(Ok(())), Some(Err(panic))) => Some(RunFailure::Panic {
            source: "tui",
            message: panic_payload_to_string(&*panic),
            payload: panic,
        }),
        (Ok(Err(e)), _) => Some(RunFailure::Error(e)),
        (Err(panic), _) => Some(RunFailure::Panic {
            source: "main",
            message: panic_payload_to_string(&*panic),
            payload: panic,
        }),
    };

    if let Some(ref run_failure) = failure {
        if let Err(write_err) = write_failure_artifact(&config, &metrics, run_failure) {
            eprintln!("!!! FOLD FAILURE artifact write failed: {}", write_err);
        }
    } else {
        let runtime = program_start.elapsed();
        write_fold_history(&config, &metrics, runtime)?;
    }

    match failure {
        None => Ok(()),
        Some(RunFailure::Error(err)) => Err(err),
        Some(RunFailure::Panic { payload, .. }) => std::panic::resume_unwind(payload),
    }
}

fn panic_payload_to_string(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        "non-string panic payload".to_string()
    }
}

fn env_var_or_empty(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn env_var_or_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
}

fn write_failure_artifact(
    config: &StateConfig,
    metrics: &Metrics,
    failure: &RunFailure,
) -> Result<PathBuf, FoldError> {
    let snapshot = metrics.snapshot();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .as_secs();
    let artifact_path = match std::env::var("FOLD_FATAL_LOG_PATH") {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path),
        _ => config.logs_dir().join(format!(
            "fatal_{}_pid={}.log",
            timestamp,
            std::process::id()
        )),
    };

    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent).map_err(FoldError::Io)?;
    }

    let current_file = if snapshot.operation.current_file.is_empty() {
        env_var_or_empty("FOLD_RUN_STAGE_FILE")
    } else {
        snapshot.operation.current_file.clone()
    };
    let input_words = if snapshot.operation.word_count > 0 {
        snapshot.operation.word_count
    } else if snapshot.global.run_input_words > 0 {
        snapshot.global.run_input_words
    } else {
        env_var_or_usize("FOLD_RUN_INPUT_WORDS").unwrap_or(0)
    };
    let source_file = env_var_or_empty("FOLD_RUN_SOURCE_FILE");
    let run_id = env_var_or_empty("FOLD_RUN_ID");
    let bundle_dir = env_var_or_empty("FOLD_RUN_BUNDLE_DIR");
    let transcript_path = env_var_or_empty("FOLD_RUN_TRANSCRIPT_PATH");
    let offload_cfg = OffloadConfig::from_env();
    let (failure_kind, failure_source, failure_message) = match failure {
        RunFailure::Error(err) => ("error", "app", err.to_string()),
        RunFailure::Panic {
            source, message, ..
        } => ("panic", *source, message.clone()),
    };

    let mut artifact = String::new();
    artifact.push_str("=== FOLD FATAL START ===\n");
    artifact.push_str(&format!("timestamp: {}\n", timestamp));
    artifact.push_str(&format!("run_id: {}\n", run_id));
    artifact.push_str(&format!("kind: {}\n", failure_kind));
    artifact.push_str(&format!("source: {}\n", failure_source));
    artifact.push_str(&format!("message: {}\n", failure_message));
    artifact.push_str(&format!("current_file: {}\n", current_file));
    artifact.push_str(&format!("source_file: {}\n", source_file));
    artifact.push_str(&format!("input_words: {}\n", input_words));
    artifact.push_str(&format!("bundle_dir: {}\n", bundle_dir));
    artifact.push_str(&format!("transcript_path: {}\n", transcript_path));
    artifact.push_str(&format!("offload_prefix: {}\n", offload_cfg.spaces_prefix));
    artifact.push_str(&format!("mode: {}\n", snapshot.global.mode));
    artifact.push_str(&format!("role: {}\n", snapshot.global.role));
    artifact.push_str(&format!("generation: {}\n", snapshot.global.generation));
    artifact.push_str(&format!("phase: {}\n", snapshot.global.phase));
    artifact.push_str(&format!("status: {}\n", snapshot.operation.status));
    artifact.push_str(&format!(
        "progress: {}/{}\n",
        snapshot.operation.progress_current, snapshot.operation.progress_total
    ));
    artifact.push_str(&format!("work_len: {}\n", snapshot.global.work_len));
    artifact.push_str(&format!(
        "accepted: {}\n",
        snapshot.global.seen_len_accepted
    ));
    artifact.push_str(&format!(
        "memory: rss_bytes={} cap_bytes={}\n",
        snapshot.global.process_rss_bytes, snapshot.global.process_rss_cap_bytes
    ));
    artifact.push_str(&format!(
        "compaction: current_bytes={} cap_bytes={}\n",
        snapshot.global.compaction_arena_bytes, snapshot.global.compaction_arena_cap_bytes
    ));
    artifact.push_str(&format!(
        "work_cache: current_bytes={} cap_bytes={}\n",
        snapshot.global.work_cache_bytes, snapshot.global.work_cache_cap_bytes
    ));
    artifact.push_str(&format!(
        "segment_batch: current_bytes={} cap_bytes={}\n",
        snapshot.global.segment_batch_bytes, snapshot.global.segment_batch_cap_bytes
    ));
    artifact.push_str(&format!(
        "spill: created_files={} created_bytes={} pending_files={} pending_bytes={} consumed_files={} consumed_bytes={}\n",
        snapshot.global.spill_created_files,
        snapshot.global.spill_created_bytes,
        snapshot.global.spill_pending_files,
        snapshot.global.spill_pending_bytes,
        snapshot.global.spill_consumed_files,
        snapshot.global.spill_consumed_bytes
    ));
    artifact.push_str(&format!(
        "offload: files={} bytes={}\n",
        snapshot.global.offloaded_files, snapshot.global.offloaded_bytes
    ));
    artifact.push_str(&format!(
        "download: files={} bytes={}\n",
        snapshot.global.downloaded_files, snapshot.global.downloaded_bytes
    ));
    artifact.push_str(&format!(
        "cache: hits={} misses={}\n",
        snapshot.global.cache_hits, snapshot.global.cache_misses
    ));
    artifact.push_str(&format!(
        "pressure_triggers: {}\n",
        snapshot.global.pressure_triggers
    ));

    if !snapshot.logs.is_empty() {
        artifact.push_str("recent_logs:\n");
        let start = snapshot.logs.len().saturating_sub(20);
        for entry in snapshot.logs.iter().skip(start) {
            artifact.push_str(&format!("  - [{}] {}\n", entry.timestamp, entry.message));
        }
    }

    artifact.push_str("=== FOLD FATAL END ===\n");
    fs::write(&artifact_path, artifact).map_err(FoldError::Io)?;

    eprintln!(
        "!!! FOLD FAILURE kind={} current_file={} input_words={} artifact={}",
        failure_kind,
        if current_file.is_empty() {
            "(unknown)"
        } else {
            current_file.as_str()
        },
        input_words,
        artifact_path.display()
    );
    if !transcript_path.is_empty() {
        eprintln!("!!! Transcript: {}", transcript_path);
    }

    Ok(artifact_path)
}

fn process_txt_file(
    file_path: String,
    config: &StateConfig,
    metrics: &Metrics,
    role: Role,
    should_quit: &AtomicBool,
) -> Result<(), FoldError> {
    let run_start = Instant::now();
    let offload_cfg = OffloadConfig::from_env_with_base(&config.base_dir);
    let _offload_guard =
        configure_offload_runtime(&config.base_dir, &offload_cfg).map_err(FoldError::Io)?;
    disk_safety::set_metrics_handle(Some(metrics.clone_handle()));
    log_offload_policy(metrics, &offload_cfg);

    // Ingest the text file
    let ingestion = file_handler::ingest_txt_file_with_config(&file_path, config)
        .map_err(mark_claim_race_if_applicable)?;
    let remaining_chunks = file_handler::count_all_chunks_with_config(config)?;
    metrics.reset_new_orthos();

    metrics.update_operation(|op| {
        op.current_file = ingestion.filename.clone();
        op.text_preview = ingestion.text_preview.clone();
        op.word_count = ingestion.word_count;
    });
    metrics.set_operation_status("Building interner".to_string());
    metrics.update_global(|g| {
        g.remaining_chunks = remaining_chunks;
        if remaining_chunks > g.total_chunks {
            g.total_chunks = remaining_chunks;
        }
        g.current_lineage = format!("\"{}\"", ingestion.filename);
    });
    metrics.add_log(format!(
        "Ingested: {} ({} remaining)",
        ingestion.filename, remaining_chunks
    ));
    // Record run-level metadata up front so history write doesn't need to rewalk disk
    metrics.update_global(|g| {
        g.run_input_words = ingestion.word_count;
        g.run_disk_bytes = directory_size(&config.base_dir).unwrap_or(0);
    });

    let memory_budget = memory_budget_for_role(role)?;
    let mem_claim = acquire_memory_claim_simple(role, config, &memory_budget)?;
    let _memory_guard = memory_safety::ScopedProcessMemory::new(
        mem_claim.granted_bytes(),
        Some(metrics.clone_handle()),
    );
    let cfg = Config::from_memory_budget(&memory_budget);
    apply_memory_budget_metrics(metrics, &cfg, &memory_budget);

    // Build interner from the text
    let interner = Interner::from_text(&ingestion.text);

    metrics.update_global(|g| {
        g.interner_version = interner.version();
        g.vocab_size = interner.vocabulary().len();
    });
    metrics.add_log(format!(
        "Interner built: v{}, vocab={}",
        interner.version(),
        interner.vocabulary().len()
    ));

    // Initialize GenerationStore for this file (work folder becomes gen store base)
    let store_path = PathBuf::from(ingestion.work_queue_path())
        .parent()
        .unwrap()
        .to_path_buf();
    let mut store = GenerationStore::new_with_config(store_path.clone(), 8)?;
    store.configure(&cfg);

    metrics.add_log(format!(
        "[{} init] generation_store configured: claim={} MB, arena={} MB, work_cache={} MB, segment_max={} MB, fan_in={}, read_buf={} KB",
        role_as_str(role),
        memory_budget.process_claim_bytes / 1_048_576,
        cfg.run_budget_bytes / 1_048_576,
        cfg.work_queue_cache_bytes / 1_048_576,
        cfg.work_segment_max_bytes / 1_048_576,
        cfg.fan_in,
        cfg.read_buf_bytes / 1024
    ));

    let mut housekeeping = || -> Result<(), FoldError> {
        ingestion.touch_heartbeat()?;
        mem_claim.touch()?;
        touch_leader_lock_if_owner(config)?;
        Ok(())
    };

    let metrics_handle = metrics.clone_handle();
    let progress_factory = move |gen_for_closure: u64| {
        let metrics_clone = metrics_handle.clone_handle();
        Some(Box::new(move |msg: &str| {
            if msg.starts_with("TRANSITION_START:") {
                if let Some(bucket_count_str) = msg.strip_prefix("TRANSITION_START:") {
                    if let Ok(bucket_count) = bucket_count_str.parse::<usize>() {
                        let initial_buckets: Vec<_> = (0..bucket_count)
                            .map(|i| fold::metrics::BucketMetrics {
                                bucket_id: i,
                                run_count: 0,
                                landing_size: 0,
                                history_size_estimate: 0,
                                state: fold::metrics::BucketState::Pending,
                                new_work: 0,
                            })
                            .collect();
                        metrics_clone.update_bucket_metrics(initial_buckets);
                    }
                }
            } else if msg.starts_with("BUCKET_STATE:") {
                let parts: Vec<&str> = msg
                    .strip_prefix("BUCKET_STATE:")
                    .unwrap()
                    .split(':')
                    .collect();
                if parts.len() >= 2 {
                    if let Ok(bucket_id) = parts[0].parse::<usize>() {
                        let state_str = parts[1];
                        let new_work = if parts.len() >= 3 {
                            parts[2].parse::<usize>().unwrap_or(0)
                        } else {
                            0
                        };

                        let state = match state_str {
                            "draining" => fold::metrics::BucketState::Draining,
                            "sorting" => fold::metrics::BucketState::Sorting,
                            "merging" => fold::metrics::BucketState::Merging,
                            "antijoining" => fold::metrics::BucketState::AntiJoining,
                            "compacting" => fold::metrics::BucketState::Compacting,
                            "complete" => fold::metrics::BucketState::Complete,
                            "empty" => fold::metrics::BucketState::Empty,
                            _ => fold::metrics::BucketState::Pending,
                        };

                        let snapshot = metrics_clone.snapshot();
                        let mut updated_buckets = snapshot.bucket_metrics.clone();
                        if bucket_id < updated_buckets.len() {
                            updated_buckets[bucket_id].state = state;
                            updated_buckets[bucket_id].new_work = new_work;
                            metrics_clone.update_bucket_metrics(updated_buckets);
                        }
                    }
                }
            } else if msg == "TRANSITION_COMPLETE" {
                let snapshot = metrics_clone.snapshot();
                let reset_buckets: Vec<_> = snapshot
                    .bucket_metrics
                    .iter()
                    .map(|b| fold::metrics::BucketMetrics {
                        bucket_id: b.bucket_id,
                        run_count: b.run_count,
                        landing_size: b.landing_size,
                        history_size_estimate: b.history_size_estimate,
                        state: fold::metrics::BucketState::Pending,
                        new_work: 0,
                    })
                    .collect();
                metrics_clone.update_bucket_metrics(reset_buckets);
            }

            if !msg.starts_with("BUCKET_STATE:")
                && !msg.starts_with("TRANSITION_START:")
                && msg != "TRANSITION_COMPLETE"
            {
                metrics_clone.update_global(|g| {
                    g.phase = format!("Gen {} → {}: {}", gen_for_closure, gen_for_closure + 1, msg);
                });
                metrics_clone.add_log(format!("Gen {} transition: {}", gen_for_closure, msg));
            }
        }) as fold::generation_store::ProgressCallback)
    };

    let run_result = run_generation_loop(
        &interner,
        &mut store,
        &cfg,
        role,
        metrics,
        || should_quit.load(Ordering::Relaxed),
        &mut housekeeping,
        progress_factory,
        Some(config),
    )?;

    let generation_stats = run_result.generation_stats.clone();

    metrics.add_log(format!(
        "Completed {} generations, {} total orthos",
        generation_stats.len(),
        store.seen_len_accepted()
    ));

    if run_result.optimal_dirty {
        let score = run_result.best_score;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        metrics.update_optimal_ortho(|opt| {
            opt.volume = score.volume;
            opt.variance_num = score.variance_num;
            opt.variance_den = score.variance_den;
            opt.dims = run_result.best_ortho.dims().clone();
            opt.fullness = score.fullness;
            opt.capacity = run_result.best_ortho.payload().len();
            opt.payload = run_result.best_ortho.payload().clone();
            opt.vocab = interner.vocabulary().to_vec();
            opt.last_update_time = now;
        });

        // Prune history runs against the improved best before archiving.
        metrics.add_log("Best improved; pruning compaction pass before archive".to_string());
        let (kept, pruned) = store.prune_history_with_bound(
            &interner,
            run_result.best_score,
            None,
            cfg.read_buf_bytes,
        )?;
        metrics.add_log(format!(
            "Pruning compaction kept {} orthos, pruned {}",
            kept, pruned
        ));
        metrics.update_merge(|m| {
            m.compaction_kept = kept as usize;
            m.compaction_pruned = pruned as usize;
        });
    }

    let total_orthos = store.seen_len_accepted() as usize;
    metrics.add_log(format!("Archiving: {} orthos", total_orthos));
    metrics.increment_new_orthos(total_orthos);

    print_optimal(&run_result.best_ortho, &interner);

    store.flush_all()?;

    let archive_path = build_archive_path(config)?;
    let lineage = format!("\"{}\"", ingestion.filename);
    move_history_runs_to_archive(&store.history_run_paths(), &archive_path)?;
    write_archive_artifacts(
        &archive_path,
        &interner,
        Some(&run_result.best_ortho),
        &lineage,
        total_orthos,
        &ingestion.text_preview,
        ingestion.word_count,
    )?;

    metrics.add_log(format!("Archive saved: {}", archive_path.display()));

    // Update largest archive
    metrics.update_largest_archive(|la| {
        if total_orthos > la.ortho_count {
            la.filename = archive_path.to_string_lossy().to_string();
            la.ortho_count = total_orthos;
            la.lineage = lineage;
        }
    });

    metrics.update_global(|g| g.processed_chunks += 1);
    metrics.set_generation_stats(run_result.generation_stats);

    // Cleanup work folder
    ingestion.cleanup()?;

    // Capture final disk usage and write a per-ingest history entry
    metrics.update_global(|g| {
        g.run_disk_bytes = directory_size(&config.base_dir).unwrap_or(0);
    });
    write_fold_history(config, metrics, run_start.elapsed())?;

    Ok(())
}

fn merge_archives(
    archive_a_path: &str,
    archive_b_path: &str,
    config: &StateConfig,
    metrics: &Metrics,
    role: Role,
) -> Result<(), FoldError> {
    let run_start = Instant::now();
    let offload_cfg = OffloadConfig::from_env_with_base(&config.base_dir);
    let _offload_guard =
        configure_offload_runtime(&config.base_dir, &offload_cfg).map_err(FoldError::Io)?;
    disk_safety::set_metrics_handle(Some(metrics.clone_handle()));
    log_offload_policy(metrics, &offload_cfg);

    // Get archive ortho counts for display BEFORE ingest moves them
    let orthos_a = file_handler::load_archive_metadata(archive_a_path).unwrap_or(0);
    let orthos_b = file_handler::load_archive_metadata(archive_b_path).unwrap_or(0);

    // Ingest archives for merging
    let ingestion =
        file_handler::ingest_archives_with_config(archive_a_path, archive_b_path, config)
            .map_err(mark_claim_race_if_applicable)?;

    metrics.set_operation_status("Loading interners".to_string());
    metrics.reset_prune_counts();

    // Load both interners
    let (interner_a, interner_b) = ingestion.load_interners()?;

    // Load lineages early to display provenance tree
    let (lineage_a_early, lineage_b_early) = ingestion.load_lineages()?;
    let merged_lineage_preview = format!("({} {})", lineage_a_early, lineage_b_early);
    metrics.update_global(|g| g.current_lineage = merged_lineage_preview);

    // Determine which interner is smaller to optimize remapping
    let a_is_smaller = interner_a.vocab_size() <= interner_b.vocab_size();

    let (larger_interner, smaller_interner) = if a_is_smaller {
        (interner_b, interner_a)
    } else {
        (interner_a, interner_b)
    };

    // Create merged interner first
    let merged_interner = larger_interner.merge(&smaller_interner);

    // Now calculate impacted keys by comparing merged against originals
    // Both impacted_larger and impacted_smaller are returned in MERGED vocabulary space
    let impacted_larger = merged_interner.impacted_keys(&larger_interner);
    let impacted_smaller = merged_interner.impacted_keys(&smaller_interner);

    // Build vocab mapping for remapping orthos (not keys) from smaller to merged
    let vocab_map_smaller =
        build_vocab_mapping(smaller_interner.vocabulary(), merged_interner.vocabulary());

    metrics.update_merge(|m| {
        m.current_merge = format!("merge_{}", std::process::id());
        m.archive_a_orthos = orthos_a;
        m.archive_b_orthos = orthos_b;
        m.impacted_a = if a_is_smaller {
            impacted_smaller.len()
        } else {
            impacted_larger.len()
        };
        m.impacted_b = if a_is_smaller {
            impacted_larger.len()
        } else {
            impacted_smaller.len()
        };
        m.seed_orthos_a = orthos_a;
        m.seed_orthos_b = orthos_b;
        m.text_preview_a = ingestion.text_preview_a.clone();
        m.text_preview_b = ingestion.text_preview_b.clone();
        m.word_count_a = ingestion.word_count_a;
        m.word_count_b = ingestion.word_count_b;
        m.compaction_kept = 0;
        m.compaction_pruned = 0;
        m.impacted_pruned_a = 0;
        m.impacted_pruned_b = 0;
    });
    metrics.reset_new_orthos();

    metrics.update_global(|g| {
        g.interner_version = merged_interner.version();
        g.vocab_size = merged_interner.vocabulary().len();
    });
    metrics.add_log(format!(
        "Merged interner: v{}, vocab={} (Archive {} is smaller)",
        merged_interner.version(),
        merged_interner.vocabulary().len(),
        if a_is_smaller { "A" } else { "B" }
    ));
    // Record run-level metadata up front so history write doesn't need to rewalk disk
    metrics.update_global(|g| {
        g.run_input_words = ingestion
            .word_count_a
            .saturating_add(ingestion.word_count_b);
        g.run_disk_bytes = directory_size(&config.base_dir).unwrap_or(0);
    });

    let memory_budget = memory_budget_for_role(role)?;
    let mem_claim = acquire_memory_claim_simple(role, config, &memory_budget)?;
    let _memory_guard = memory_safety::ScopedProcessMemory::new(
        mem_claim.granted_bytes(),
        Some(metrics.clone_handle()),
    );
    let cfg = Config::from_memory_budget(&memory_budget);
    apply_memory_budget_metrics(metrics, &cfg, &memory_budget);

    // Initialize GenerationStore for merge
    let store_path = PathBuf::from(ingestion.work_queue_path())
        .parent()
        .unwrap()
        .to_path_buf();
    let mut store = GenerationStore::new_with_config(store_path, 8)?;
    store.configure(&cfg);

    metrics.add_log(format!(
        "[{} merge init] generation_store configured: claim={} MB, arena={} MB, work_cache={} MB, segment_max={} MB, fan_in={}, read_buf={} KB",
        role_as_str(role),
        memory_budget.process_claim_bytes / 1_048_576,
        cfg.run_budget_bytes / 1_048_576,
        cfg.work_queue_cache_bytes / 1_048_576,
        cfg.work_segment_max_bytes / 1_048_576,
        cfg.fan_in,
        cfg.read_buf_bytes / 1024
    ));

    // Seed with empty ortho
    let seed_ortho = Ortho::new();
    let mut best_ortho = seed_ortho.clone();
    let mut best_score = best_ortho.score();

    store.push_segments(vec![seed_ortho])?;

    // Initialize metrics with initial merge state
    metrics.update_global(|g| {
        g.generation = 0;
        g.phase = "Idle".to_string();
        g.work_len = store.work_len();
        g.seen_len_accepted = store.seen_len_accepted();
        g.run_budget_bytes = cfg.run_budget_bytes;
        g.fan_in = cfg.fan_in;
    });
    metrics.record_work_len(store.work_len() as usize);
    metrics.record_landing_buffer_count(store.total_landing_size());

    // Get results paths
    let (results_a_path, results_b_path) = ingestion.get_results_paths();
    // Load per-archive optimal scores for symmetric pruning
    let load_opt_score = |path: &str| -> Option<OrthoScore> {
        file_handler::load_optimal_ortho(path)
            .ok()
            .map(|o| o.score())
    };
    let opt_score_a = load_opt_score(&results_a_path).unwrap_or(OrthoScore::zero());
    let opt_score_b = load_opt_score(&results_b_path).unwrap_or(OrthoScore::zero());
    let max_opt_score = std::cmp::max(opt_score_a, opt_score_b);

    // Set up paths and impacted keys for each archive
    // Note: larger archive orthos don't need remapping, their impacted keys are already in merged space
    // smaller archive orthos need remapping, and we use the remapped impacted keys
    let (larger_path, larger_impacted_ref, larger_name) = if a_is_smaller {
        (&results_b_path, &impacted_larger, "B")
    } else {
        (&results_a_path, &impacted_larger, "A")
    };

    let (smaller_path, smaller_impacted_ref, smaller_name) = if a_is_smaller {
        (&results_a_path, &impacted_smaller, "A")
    } else {
        (&results_b_path, &impacted_smaller, "B")
    };

    // Process larger archive (no remapping needed) - stream directly into GenerationStore
    metrics.set_operation_status(format!("Streaming Larger Archive {}", larger_name));

    // Create a temporary GenerationStore to read from the archive's results
    let larger_store = GenerationStore::from_existing(PathBuf::from(larger_path), 8)?;

    let mut total_from_larger = 0;
    let mut impacted_from_larger = 0;

    // Stream all orthos from larger archive's history into our merge store
    for bucket in 0..8 {
        for result in larger_store.history_iter_with_buffer(bucket, cfg.read_buf_bytes)? {
            let ortho_bytes = result?.bytes;
            let ortho = Ortho::from_bytes(ortho_bytes.as_ref())?;
            total_from_larger += 1;

            // Record to landing zone (will be deduped during generation end)
            store.record_result(&ortho)?;

            let candidate_score = ortho.score();
            if candidate_score > best_score {
                best_ortho = ortho.clone();
                best_score = candidate_score;
            }

            // If impacted, also seed to work queue
            if is_ortho_impacted_fast(&ortho, larger_impacted_ref) {
                let prune_score = std::cmp::max(best_score, max_opt_score);
                if !bound_existing_ortho(
                    &ortho,
                    &merged_interner,
                    prune_score,
                    Some(larger_impacted_ref),
                ) {
                    store.push_segments(vec![ortho])?;
                    impacted_from_larger += 1;
                } else {
                    metrics.update_merge(|m| {
                        if a_is_smaller {
                            m.impacted_pruned_b = m.impacted_pruned_b.saturating_add(1);
                        } else {
                            m.impacted_pruned_a = m.impacted_pruned_a.saturating_add(1);
                        }
                    });
                }
            }

            if total_from_larger % 10000 == 0 {
                ingestion.touch_heartbeat()?;
                mem_claim.touch()?;
                metrics.update_operation(|op| op.progress_current = total_from_larger);
            }
        }
    }

    metrics.add_log(format!(
        "Loaded {} orthos from larger archive {} ({} impacted)",
        total_from_larger, larger_name, impacted_from_larger
    ));

    // Process smaller archive (needs remapping) - stream and remap into GenerationStore
    metrics.set_operation_status(format!(
        "Streaming & Remapping Smaller Archive {}",
        smaller_name
    ));

    let smaller_store = GenerationStore::from_existing(PathBuf::from(smaller_path), 8)?;

    let mut total_from_smaller = 0;
    let mut impacted_from_smaller = 0;

    // Stream all orthos from smaller archive's history, remap, and store
    for bucket in 0..8 {
        for result in smaller_store.history_iter_with_buffer(bucket, cfg.read_buf_bytes)? {
            let ortho_bytes = result?.bytes;
            let ortho = Ortho::from_bytes(ortho_bytes.as_ref())?;
            total_from_smaller += 1;

            // Remap the ortho to merged vocabulary
            if let Some(remapped) = ortho.remap(&vocab_map_smaller) {
                // Record remapped ortho to landing zone
                store.record_result(&remapped)?;

                let candidate_score = remapped.score();
                if candidate_score > best_score {
                    best_ortho = remapped.clone();
                    best_score = candidate_score;
                }

                // If impacted, also seed to work queue
                if is_ortho_impacted_fast(&remapped, smaller_impacted_ref) {
                    let prune_score = std::cmp::max(best_score, max_opt_score);
                    if !bound_existing_ortho(
                        &remapped,
                        &merged_interner,
                        prune_score,
                        Some(smaller_impacted_ref),
                    ) {
                        store.push_segments(vec![remapped])?;
                        impacted_from_smaller += 1;
                    } else {
                        metrics.update_merge(|m| {
                            if a_is_smaller {
                                m.impacted_pruned_a = m.impacted_pruned_a.saturating_add(1);
                            } else {
                                m.impacted_pruned_b = m.impacted_pruned_b.saturating_add(1);
                            }
                        });
                    }
                }
            }

            if total_from_smaller % 10000 == 0 {
                ingestion.touch_heartbeat()?;
                mem_claim.touch()?;
                metrics.update_operation(|op| op.progress_current = total_from_smaller);
            }
        }
    }

    metrics.add_log(format!(
        "Loaded & remapped {} orthos from smaller archive {} ({} impacted)",
        total_from_smaller, smaller_name, impacted_from_smaller
    ));

    // Update metrics with impacted counts from both archives
    if a_is_smaller {
        metrics.update_merge(|m| {
            m.impacted_queued_a = impacted_from_smaller;
            m.impacted_queued_b = impacted_from_larger;
        });
    } else {
        metrics.update_merge(|m| {
            m.impacted_queued_a = impacted_from_larger;
            m.impacted_queued_b = impacted_from_smaller;
        });
    }

    metrics.add_log(format!(
        "Rehydration complete: {} work items ready (A:{} B:{})",
        store.work_len(),
        if a_is_smaller {
            impacted_from_smaller
        } else {
            impacted_from_larger
        },
        if a_is_smaller {
            impacted_from_larger
        } else {
            impacted_from_smaller
        }
    ));

    // Process generations in the shared runner. Merge-only threading stays behind the
    // coordinator so store mutation and pruning order remain deterministic.
    let merge_threads = merge_threads_from_env();
    metrics.add_log(format!(
        "Merge scheduling: threads={}, policy={}",
        merge_threads,
        merge_policy_for_role(role).as_str()
    ));

    let mut housekeeping = || -> Result<(), FoldError> {
        print_optimal(&best_ortho, &merged_interner);
        ingestion.touch_heartbeat()?;
        mem_claim.touch()?;
        touch_leader_lock_if_owner(config)?;
        Ok(())
    };

    let metrics_handle = metrics.clone_handle();
    let progress_factory = move |gen_for_closure: u64| {
        let metrics_clone = metrics_handle.clone_handle();
        Some(Box::new(move |msg: &str| {
            if msg.starts_with("TRANSITION_START:") {
                if let Some(bucket_count_str) = msg.strip_prefix("TRANSITION_START:") {
                    if let Ok(bucket_count) = bucket_count_str.parse::<usize>() {
                        let initial_buckets: Vec<_> = (0..bucket_count)
                            .map(|i| fold::metrics::BucketMetrics {
                                bucket_id: i,
                                run_count: 0,
                                landing_size: 0,
                                history_size_estimate: 0,
                                state: fold::metrics::BucketState::Pending,
                                new_work: 0,
                            })
                            .collect();
                        metrics_clone.update_bucket_metrics(initial_buckets);
                    }
                }
            } else if msg.starts_with("BUCKET_STATE:") {
                let parts: Vec<&str> = msg
                    .strip_prefix("BUCKET_STATE:")
                    .unwrap()
                    .split(':')
                    .collect();
                if parts.len() >= 2 {
                    if let Ok(bucket_id) = parts[0].parse::<usize>() {
                        let state_str = parts[1];
                        let new_work = if parts.len() >= 3 {
                            parts[2].parse::<usize>().unwrap_or(0)
                        } else {
                            0
                        };

                        let state = match state_str {
                            "draining" => fold::metrics::BucketState::Draining,
                            "sorting" => fold::metrics::BucketState::Sorting,
                            "merging" => fold::metrics::BucketState::Merging,
                            "antijoining" => fold::metrics::BucketState::AntiJoining,
                            "compacting" => fold::metrics::BucketState::Compacting,
                            "complete" => fold::metrics::BucketState::Complete,
                            "empty" => fold::metrics::BucketState::Empty,
                            _ => fold::metrics::BucketState::Pending,
                        };

                        let snapshot = metrics_clone.snapshot();
                        let mut updated_buckets = snapshot.bucket_metrics.clone();
                        if bucket_id < updated_buckets.len() {
                            updated_buckets[bucket_id].state = state;
                            updated_buckets[bucket_id].new_work = new_work;
                            metrics_clone.update_bucket_metrics(updated_buckets);
                        }
                    }
                }
            } else if msg == "TRANSITION_COMPLETE" {
                let snapshot = metrics_clone.snapshot();
                let reset_buckets: Vec<_> = snapshot
                    .bucket_metrics
                    .iter()
                    .map(|b| fold::metrics::BucketMetrics {
                        bucket_id: b.bucket_id,
                        run_count: b.run_count,
                        landing_size: b.landing_size,
                        history_size_estimate: b.history_size_estimate,
                        state: fold::metrics::BucketState::Pending,
                        new_work: 0,
                    })
                    .collect();
                metrics_clone.update_bucket_metrics(reset_buckets);
            }

            if !msg.starts_with("BUCKET_STATE:")
                && !msg.starts_with("TRANSITION_START:")
                && msg != "TRANSITION_COMPLETE"
            {
                metrics_clone.update_global(|g| {
                    g.phase = format!(
                        "Merge Gen {} → {}: {}",
                        gen_for_closure,
                        gen_for_closure + 1,
                        msg
                    );
                });
                metrics_clone.add_log(format!("Merge Gen {} transition: {}", gen_for_closure, msg));
            }
        }) as fold::generation_store::ProgressCallback)
    };

    let run_result = run_merge_generation_loop(
        &merged_interner,
        &mut store,
        &cfg,
        metrics,
        &mut housekeeping,
        progress_factory,
        merge_threads,
        Some(config),
    )?;

    best_ortho = run_result.best_ortho;
    best_score = run_result.best_score;
    let optimal_dirty = run_result.optimal_dirty;

    let mut compacted_counts: Option<(u64, u64)> = None;
    metrics.add_log(format!(
        "Merge completed {} generations, {} total orthos",
        run_result.generation_stats.len(),
        store.seen_len_accepted()
    ));

    if optimal_dirty {
        let score = best_score;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        metrics.update_optimal_ortho(|opt| {
            opt.volume = score.volume;
            opt.variance_num = score.variance_num;
            opt.variance_den = score.variance_den;
            opt.dims = best_ortho.dims().clone();
            opt.fullness = score.fullness;
            opt.capacity = best_ortho.payload().len();
            opt.payload = best_ortho.payload().clone();
            opt.vocab = merged_interner.vocabulary().to_vec();
            opt.last_update_time = now;
        });

        // Best changed during merge; run a pruning compaction pass over history before archiving.
        metrics.add_log("Best improved; pruning compaction pass before archive".to_string());
        let (kept, pruned) = store.prune_history_with_bound(
            &merged_interner,
            best_score,
            None,
            cfg.read_buf_bytes,
        )?;
        compacted_counts = Some((kept, pruned));
        metrics.update_merge(|m| {
            m.compaction_kept = kept as usize;
            m.compaction_pruned = pruned as usize;
        });
        metrics.add_log(format!(
            "Pruning compaction kept {} orthos, pruned {}",
            kept, pruned
        ));
    }

    let total_orthos = compacted_counts
        .map(|(kept, _)| kept as usize)
        .unwrap_or_else(|| store.seen_len_accepted() as usize);
    metrics.add_log(format!("Archiving merge: {} orthos", total_orthos));
    metrics.increment_new_orthos(total_orthos);

    print_optimal(&best_ortho, &merged_interner);

    store.flush_all()?;

    let archive_path = build_archive_path(config)?;
    let lineage = format!("({} {})", lineage_a_early, lineage_b_early);
    let text_preview = format!(
        "{} + {}",
        ingestion.text_preview_a, ingestion.text_preview_b
    );
    let word_count = ingestion.word_count_a + ingestion.word_count_b;

    move_history_runs_to_archive(&store.history_run_paths(), &archive_path)?;
    write_archive_artifacts(
        &archive_path,
        &merged_interner,
        Some(&best_ortho),
        &lineage,
        total_orthos,
        &text_preview,
        word_count,
    )?;

    metrics.add_log(format!("Merged archive saved: {}", archive_path.display()));

    // Update largest archive
    metrics.update_largest_archive(|la| {
        if total_orthos > la.ortho_count {
            la.filename = archive_path.to_string_lossy().to_string();
            la.ortho_count = total_orthos;
            la.lineage = lineage;
        }
    });

    metrics.update_global(|g| g.processed_chunks += 1);
    metrics.set_generation_stats(run_result.generation_stats);

    // Update merge metrics on completion
    metrics.update_merge(|m| {
        m.completed_merges += 1;
        m.new_orthos_from_merge = total_orthos;
    });

    // Cleanup
    ingestion.cleanup()?;

    // Capture final disk usage and write a per-merge history entry
    metrics.update_global(|g| {
        g.run_disk_bytes = directory_size(&config.base_dir).unwrap_or(0);
    });
    write_fold_history(config, metrics, run_start.elapsed())?;

    Ok(())
}

// Helper function to build vocabulary mapping
fn build_vocab_mapping(old_vocab: &[String], new_vocab: &[String]) -> Vec<usize> {
    let mut mapping = vec![0; old_vocab.len()];
    for (old_idx, word) in old_vocab.iter().enumerate() {
        if let Some(new_idx) = new_vocab.iter().position(|w| w == word) {
            mapping[old_idx] = new_idx;
        }
    }
    mapping
}

// Helper function to check if ortho is impacted by checking if any requirement matches impacted prefixes
fn is_ortho_impacted_fast(ortho: &Ortho, impacted_prefixes: &[Vec<usize>]) -> bool {
    // Get the ortho's requirement prefixes (not the entire payload)
    let requirements = ortho.get_requirement_phrases();
    let requirements_usize: Vec<Vec<usize>> = requirements
        .iter()
        .map(|req| req.iter().map(|v| payload_to_usize(*v)).collect())
        .collect();

    // Check if any requirement prefix matches any impacted prefix
    requirements_usize
        .iter()
        .any(|req| impacted_prefixes.contains(req))
}

#[allow(dead_code)]
fn archive_generation_config() -> Config {
    let run_budget_bytes = 256 * 1024 * 1024; // 256MB for archive materialization
    let read_buf_bytes = 256 * 1024;
    let fan_in = 64;

    Config {
        run_budget_bytes,
        fan_in,
        read_buf_bytes,
        allow_compaction: false,
        work_queue_cache_bytes: 64 * 1024 * 1024,
        bufwriter_capacity: 256 * 1024,
        work_segment_max_bytes: 16 * 1024 * 1024,
        history_cache_bytes: 8 * 1024 * 1024,
        landing_flush_threshold: 4 * 1024 * 1024,
    }
}

// Internal function to save archive from Vec<Ortho>
#[allow(dead_code)]
fn save_archive_vec_internal(
    interner: &Interner,
    orthos: Vec<Ortho>,
    best_ortho: Option<&Ortho>,
    lineage: &str,
    _ortho_count: usize,
    text_preview: &str,
    word_count: usize,
    config: &StateConfig,
) -> Result<(String, String), FoldError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    // Create unique archive path
    let archive_name = format!("archive_{}_{}", now.as_secs(), now.subsec_nanos());
    let archive_path = config.input_dir().join(format!("{}=.bin", archive_name));

    // Create archive directory
    fs::create_dir_all(&archive_path).map_err(FoldError::Io)?;

    // Write orthos using GenerationStore format (history runs)
    let results_dir = archive_path.join("results");
    fs::create_dir_all(&results_dir).map_err(FoldError::Io)?;

    // Create a temporary GenerationStore to write orthos and produce history runs
    let mut temp_store = GenerationStore::new_with_config(results_dir.clone(), 8)?;
    let cfg = archive_generation_config();
    temp_store.configure(&cfg);
    for ortho in orthos {
        temp_store.record_result_with_threshold(&ortho, cfg.landing_flush_threshold)?;
    }
    temp_store.on_generation_end(&cfg, None)?;
    let ortho_count = temp_store.seen_len_accepted() as usize;
    drop(temp_store);

    // Clean up intermediate landing/work/runs for a lean archive
    let _ = fs::remove_dir_all(results_dir.join("landing"));
    let _ = fs::remove_dir_all(results_dir.join("work"));
    let _ = fs::remove_dir_all(results_dir.join("runs"));

    // Write the interner
    let interner_path = archive_path.join("interner.bin");
    let interner_bytes = interner.to_bytes()?;
    fs::write(interner_path, interner_bytes).map_err(FoldError::Io)?;

    // Write optimal ortho if provided
    if let Some(ortho) = best_ortho {
        let optimal_bin_path = archive_path.join("optimal.bin");
        let optimal_bytes = ortho.to_bytes()?;
        fs::write(optimal_bin_path, optimal_bytes).map_err(FoldError::Io)?;
    }

    // Write lineage
    let lineage_path = archive_path.join("lineage.txt");
    fs::write(lineage_path, lineage).map_err(FoldError::Io)?;

    // Write metadata
    let metadata_path = archive_path.join("metadata.txt");
    fs::write(metadata_path, ortho_count.to_string()).map_err(FoldError::Io)?;

    // Write text metadata (format: word_count on line 1, preview on line 2)
    let text_meta_path = archive_path.join("text_meta.txt");
    let text_metadata = format!("{}\n{}", word_count, text_preview);
    fs::write(text_meta_path, text_metadata).map_err(FoldError::Io)?;

    Ok((
        archive_path.to_string_lossy().to_string(),
        lineage.to_string(),
    ))
}

fn build_archive_path(config: &StateConfig) -> Result<PathBuf, FoldError> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    let archive_name = format!("archive_{}_{}", now.as_secs(), now.subsec_nanos());
    let archive_path = config.input_dir().join(format!("{}=.bin", archive_name));

    fs::create_dir_all(&archive_path).map_err(FoldError::Io)?;
    fs::create_dir_all(archive_path.join("results")).map_err(FoldError::Io)?;

    Ok(archive_path)
}

fn move_history_runs_to_archive(
    history_runs: &[(usize, Vec<PathBuf>)],
    archive_path: &PathBuf,
) -> Result<(), FoldError> {
    let history_dir = archive_path.join("results").join("history");
    for (bucket, runs) in history_runs {
        let bucket_dir = history_dir.join(format!("b={:02}", bucket));
        fs::create_dir_all(&bucket_dir).map_err(FoldError::Io)?;
        for run_path in runs {
            let filename = run_path.file_name().ok_or_else(|| {
                FoldError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Missing run filename",
                ))
            })?;
            let dest_path = bucket_dir.join(filename);
            fs::rename(run_path, &dest_path).map_err(FoldError::Io)?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn offload_archive_dir(path: &Path, metrics: &Metrics) -> Result<Option<u64>, FoldError> {
    let mut uploaded_bytes = 0u64;

    fn walk_and_offload(path: &Path, uploaded: &mut u64) -> Result<bool, FoldError> {
        let mut offloaded_any = false;
        if path.is_dir() {
            for entry in fs::read_dir(path).map_err(FoldError::Io)? {
                let entry = entry.map_err(FoldError::Io)?;
                let p = entry.path();
                offloaded_any |= walk_and_offload(&p, uploaded)?;
            }
        } else if path.is_file() {
            let size = fs::metadata(path).map_err(FoldError::Io)?.len();
            if fold::generation_store::offload_path_if_configured(path).map_err(FoldError::Io)? {
                offloaded_any = true;
                *uploaded = uploaded.saturating_add(size);
            }
        }
        Ok(offloaded_any)
    }

    if walk_and_offload(path, &mut uploaded_bytes)? {
        fs::remove_dir_all(path).map_err(FoldError::Io)?;
        metrics.record_landing_buffer_count(0); // reuse metric for quick visibility
        return Ok(Some(uploaded_bytes));
    }
    Ok(None)
}

fn write_archive_artifacts(
    archive_path: &PathBuf,
    interner: &Interner,
    best_ortho: Option<&Ortho>,
    lineage: &str,
    ortho_count: usize,
    text_preview: &str,
    word_count: usize,
) -> Result<(), FoldError> {
    // Write the interner
    let interner_path = archive_path.join("interner.bin");
    let interner_bytes = interner.to_bytes()?;
    fs::write(interner_path, interner_bytes).map_err(FoldError::Io)?;

    // Write optimal ortho if provided
    if let Some(ortho) = best_ortho {
        let optimal_bin_path = archive_path.join("optimal.bin");
        let optimal_bytes = ortho.to_bytes()?;
        fs::write(optimal_bin_path, optimal_bytes).map_err(FoldError::Io)?;
    }

    // Write lineage
    let lineage_path = archive_path.join("lineage.txt");
    fs::write(lineage_path, lineage).map_err(FoldError::Io)?;

    // Write metadata
    let metadata_path = archive_path.join("metadata.txt");
    fs::write(metadata_path, ortho_count.to_string()).map_err(FoldError::Io)?;

    // Write text metadata (format: word_count on line 1, preview on line 2)
    let text_meta_path = archive_path.join("text_meta.txt");
    let text_metadata = format!("{}\n{}", word_count, text_preview);
    fs::write(text_meta_path, text_metadata).map_err(FoldError::Io)?;

    Ok(())
}

// Old helper functions removed (build_vocab_mapping, is_ortho_impacted_fast) - were only used by merge_archives

fn print_optimal(_ortho: &Ortho, _interner: &Interner) {
    // Optimal ortho info is now displayed in TUI metrics
}

fn memory_budget_for_role(role: Role) -> Result<MemoryBudget, FoldError> {
    let total_ram_bytes = memory_safety::total_system_ram_bytes();
    MemoryBudget::for_role(role, total_ram_bytes)
}

fn log_offload_policy(metrics: &Metrics, cfg: &OffloadConfig) {
    for message in cfg.startup_messages() {
        metrics.add_log(message);
    }
}

fn apply_memory_budget_metrics(metrics: &Metrics, cfg: &Config, budget: &MemoryBudget) {
    metrics.update_global(|g| {
        g.process_rss_cap_bytes = budget.process_claim_bytes;
        g.run_budget_bytes = cfg.run_budget_bytes;
        g.compaction_arena_cap_bytes = cfg.run_budget_bytes;
        g.work_cache_cap_bytes = cfg.work_queue_cache_bytes;
        g.segment_batch_cap_bytes = cfg.work_segment_max_bytes;
        g.fan_in = cfg.fan_in;
    });
}

fn acquire_memory_claim_simple(
    role: Role,
    config: &StateConfig,
    budget: &MemoryBudget,
) -> Result<MemClaimGuard, FoldError> {
    let total_ram_bytes = memory_safety::total_system_ram_bytes();
    let claim_pool_bytes = total_ram_bytes.saturating_sub(budget.system_reserve_bytes);
    let active_claims = file_handler::load_active_mem_claims(config)?;
    let active_granted_bytes = active_claims
        .iter()
        .map(|claim| claim.granted_bytes)
        .sum::<usize>();

    if active_granted_bytes.saturating_add(budget.process_claim_bytes) > claim_pool_bytes {
        return Err(FoldError::MemoryBudgetExceeded(format!(
            "insufficient shared memory budget: active_claims={} requested={} pool={}",
            active_granted_bytes, budget.process_claim_bytes, claim_pool_bytes
        )));
    }

    file_handler::create_mem_claim(
        config,
        role_as_str(role),
        budget.process_claim_bytes,
        budget.process_claim_bytes,
    )
}

// Old acquire_memory_claim kept for any remaining merge_archives code
fn determine_role(config: &StateConfig) -> Result<Role, FoldError> {
    if let Ok(force) = std::env::var("FOLD_FORCE_ROLE") {
        let force_lower = force.to_lowercase();
        if force_lower == "follower" {
            return Ok(Role::Follower);
        } else if force_lower == "leader" {
            return ensure_leader_lock(config);
        }
    }
    ensure_leader_lock(config)
}

fn ensure_leader_lock(config: &StateConfig) -> Result<Role, FoldError> {
    let lock_path = config.in_process_dir().join("leader.lock");
    fs::create_dir_all(config.in_process_dir()).map_err(FoldError::Io)?;

    let claim_leader = || -> Result<bool, FoldError> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
            .as_secs();
        use std::fs::OpenOptions;
        use std::io::Write;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let content = format!("{}:{}", timestamp, std::process::id());
                file.write_all(content.as_bytes()).map_err(FoldError::Io)?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(FoldError::Io(e)),
        }
    };

    if lock_path.exists() {
        if file_handler::is_heartbeat_file_stale(&lock_path)? {
            let _ = fs::remove_file(&lock_path);
        } else {
            let owner_pid = fs::read_to_string(&lock_path).ok().and_then(|contents| {
                contents
                    .split(':')
                    .nth(1)
                    .and_then(|p| p.split_whitespace().next())
                    .and_then(|p| p.parse::<u32>().ok())
            });
            if owner_pid == Some(std::process::id()) {
                file_handler::touch_heartbeat_file(lock_path.to_str().unwrap())?;
                return Ok(Role::Leader);
            } else {
                return Ok(Role::Follower);
            }
        }
    }

    if claim_leader()? {
        Ok(Role::Leader)
    } else {
        Ok(Role::Follower)
    }
}

fn touch_leader_lock_if_owner(config: &StateConfig) -> Result<(), FoldError> {
    let lock_path = config.in_process_dir().join("leader.lock");

    if let Ok(contents) = fs::read_to_string(&lock_path) {
        let owner_pid = contents
            .split(':')
            .nth(1)
            .and_then(|p| p.split_whitespace().next())
            .and_then(|p| p.parse::<u32>().ok());

        if owner_pid == Some(std::process::id()) {
            file_handler::touch_heartbeat_file(lock_path.to_str().unwrap())?;
        }
    }

    Ok(())
}

fn cleanup_leader_lock(config: &StateConfig) {
    let lock_path = config.in_process_dir().join("leader.lock");
    if let Ok(contents) = fs::read_to_string(&lock_path) {
        let owner_pid = contents
            .split(':')
            .nth(1)
            .and_then(|p| p.split_whitespace().next())
            .and_then(|p| p.parse::<u32>().ok());
        if owner_pid == Some(std::process::id()) {
            let _ = fs::remove_file(lock_path);
        }
    }
}

fn is_claim_race_io(io_err: &std::io::Error) -> bool {
    matches!(
        io_err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::AlreadyExists
    ) || matches!(io_err.raw_os_error(), Some(39) | Some(66))
}

fn mark_claim_race_if_applicable(err: FoldError) -> FoldError {
    match err {
        FoldError::Io(io_err) if is_claim_race_io(&io_err) => {
            FoldError::ConcurrentClaim(io_err.to_string())
        }
        other => other,
    }
}

// Only explicit claim-race errors are retryable.
fn is_concurrent_claim_error(err: &FoldError) -> bool {
    matches!(err, FoldError::ConcurrentClaim(_))
}

fn write_fold_history(
    config: &StateConfig,
    metrics: &Metrics,
    runtime: std::time::Duration,
) -> Result<(), FoldError> {
    let history_dir = PathBuf::from("fold_history");
    fs::create_dir_all(&history_dir).map_err(FoldError::Io)?;

    let snapshot = metrics.snapshot();
    let role_label = if snapshot.global.role.is_empty() {
        "unknown".to_string()
    } else {
        snapshot.global.role.clone()
    };
    let pid = std::process::id();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .as_secs();
    let summary_path = history_dir.join(format!(
        "run_{}_role={}_pid={}.txt",
        timestamp, role_label, pid
    ));

    let input_words = if snapshot.global.run_input_words > 0 {
        snapshot.global.run_input_words
    } else {
        collect_archive_totals(config)?.1
    };
    let ortho_count = snapshot.global.seen_len_accepted as usize;
    let disk_space_bytes = if snapshot.global.run_disk_bytes > 0 {
        snapshot.global.run_disk_bytes
    } else {
        directory_size(&config.base_dir)?
    };
    let mut summary = String::new();
    summary.push_str(&format!("=== RUN START input_words={} ===\n", input_words));
    summary.push_str(&format!(
        "ortho_count: {}\ndisk_space_bytes: {}\nruntime_secs: {:.3}\ninput_words: {}\n",
        ortho_count,
        disk_space_bytes,
        runtime.as_secs_f64(),
        input_words
    ));

    if !snapshot.generation_stats.is_empty() {
        summary.push_str("generation_stats:\n");
        for stat in snapshot.generation_stats.iter() {
            summary.push_str(&format!(
                "  gen {}: processing_secs={:.3} transition_secs={:.3} accepted={} new_work={}\n",
                stat.generation,
                stat.processing_secs,
                stat.transition_secs,
                stat.accepted,
                stat.new_work
            ));
        }
    }

    if !snapshot.prune_history.is_empty() {
        summary.push_str("pruning:\n");
        for sample in snapshot.prune_history.iter() {
            let total = sample.pruned + sample.expanded;
            let ratio = if total == 0 {
                0.0
            } else {
                (sample.pruned as f64) / (total as f64)
            };
            summary.push_str(&format!(
                "  gen {}: pruned={} expanded={} prune_pct={:.1}% root_span={} bound={}\n",
                sample.generation,
                sample.pruned,
                sample.expanded,
                ratio * 100.0,
                sample.pruned_root_span,
                sample.pruned_bound
            ));
        }
        if snapshot.merge.compaction_kept + snapshot.merge.compaction_pruned > 0 {
            let total = snapshot.merge.compaction_kept + snapshot.merge.compaction_pruned;
            let ratio = if total == 0 {
                0.0
            } else {
                snapshot.merge.compaction_pruned as f64 / total as f64
            };
            summary.push_str(&format!(
                "  compaction: kept={} pruned={} prune_pct={:.1}%\n",
                snapshot.merge.compaction_kept,
                snapshot.merge.compaction_pruned,
                ratio * 100.0
            ));
        }
        if snapshot.merge.impacted_pruned_a + snapshot.merge.impacted_pruned_b > 0 {
            summary.push_str(&format!(
                "  impacted_pruned: A={} B={}\n",
                snapshot.merge.impacted_pruned_a, snapshot.merge.impacted_pruned_b
            ));
        }
    }

    let opt = snapshot.optimal_ortho;
    if !opt.dims.is_empty() || !opt.payload.is_empty() {
        summary.push_str("optimal_ortho:\n");
        if !opt.dims.is_empty() {
            let dims_str = opt
                .dims
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(",");
            summary.push_str(&format!("  dims=[{}]\n", dims_str));
        }
        summary.push_str(&format!("  volume={}\n", opt.volume));
        summary.push_str(&format!("  fullness={}\n", opt.fullness));
        summary.push_str(&format!("  capacity={}\n", opt.capacity));
        if !opt.payload.is_empty() {
            let vocab = opt.vocab;
            let payload_str = opt
                .payload
                .iter()
                .map(|p| {
                    if let Some(v) = p {
                        let idx = payload_to_usize(*v);
                        vocab.get(idx).cloned().unwrap_or_else(|| idx.to_string())
                    } else {
                        "None".to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(",");
            summary.push_str(&format!("  payload=[{}]\n", payload_str));
        }
    }
    summary.push_str("=== RUN END ===\n");

    fs::write(&summary_path, summary).map_err(FoldError::Io)?;
    metrics.add_log(format!(
        "Run summary written to {}",
        summary_path.to_string_lossy()
    ));
    Ok(())
}

fn collect_archive_totals(config: &StateConfig) -> Result<(usize, usize), FoldError> {
    let input_dir = config.input_dir();
    if !input_dir.exists() {
        return Ok((0, 0));
    }

    let mut ortho_count = 0usize;
    let mut word_count = 0usize;

    for entry in fs::read_dir(&input_dir).map_err(FoldError::Io)? {
        let entry = entry.map_err(FoldError::Io)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("bin") {
            continue;
        }

        let archive_path = path.to_string_lossy().to_string();
        ortho_count =
            ortho_count.saturating_add(file_handler::load_archive_metadata(&archive_path)?);

        let text_meta_path = path.join("text_meta.txt");
        let content = fs::read_to_string(&text_meta_path).map_err(FoldError::Io)?;
        let mut lines = content.lines();
        let first_line = lines.next().ok_or_else(|| {
            FoldError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "text_meta.txt missing word count",
            ))
        })?;
        let words = first_line
            .trim()
            .parse::<usize>()
            .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        word_count = word_count.saturating_add(words);
    }

    Ok((ortho_count, word_count))
}

fn directory_size(path: &Path) -> Result<u64, FoldError> {
    fn walk(path: &Path) -> std::io::Result<u64> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            let mut total = 0u64;
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                total += walk(&entry.path())?;
            }
            Ok(total)
        } else if metadata.is_file() {
            Ok(metadata.len())
        } else {
            Ok(0)
        }
    }

    walk(path).map_err(FoldError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fold::generation_store::{RunOffloader, set_run_offloader};
    use fold::ortho::PayloadVal;
    use std::io::ErrorKind;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    fn with_test_memory_overrides<T>(f: impl FnOnce() -> T) -> T {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();

        let total_ram = memory_safety::total_system_ram_bytes();
        let leader = total_ram.to_string();
        let follower = (total_ram / 2).to_string();
        let reserve = "0".to_string();
        let prev_leader = std::env::var("FOLD_MEMORY_LEADER_MAX_BYTES").ok();
        let prev_follower = std::env::var("FOLD_MEMORY_FOLLOWER_MAX_BYTES").ok();
        let prev_reserve = std::env::var("FOLD_MEMORY_SYSTEM_RESERVE_BYTES").ok();

        unsafe {
            std::env::set_var("FOLD_MEMORY_LEADER_MAX_BYTES", &leader);
            std::env::set_var("FOLD_MEMORY_FOLLOWER_MAX_BYTES", &follower);
            std::env::set_var("FOLD_MEMORY_SYSTEM_RESERVE_BYTES", &reserve);
        }

        let result = f();

        unsafe {
            match prev_leader {
                Some(value) => std::env::set_var("FOLD_MEMORY_LEADER_MAX_BYTES", value),
                None => std::env::remove_var("FOLD_MEMORY_LEADER_MAX_BYTES"),
            }
            match prev_follower {
                Some(value) => std::env::set_var("FOLD_MEMORY_FOLLOWER_MAX_BYTES", value),
                None => std::env::remove_var("FOLD_MEMORY_FOLLOWER_MAX_BYTES"),
            }
            match prev_reserve {
                Some(value) => std::env::set_var("FOLD_MEMORY_SYSTEM_RESERVE_BYTES", value),
                None => std::env::remove_var("FOLD_MEMORY_SYSTEM_RESERVE_BYTES"),
            }
        }

        result
    }

    fn with_env_override<T>(name: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let previous = std::env::var(name).ok();
        unsafe {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
        let result = f();
        unsafe {
            match previous {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
        result
    }

    fn build_test_merge_archives(config: &StateConfig) -> (String, String) {
        let interner_a = Interner::from_text("foo bar");
        let foo_idx_a = interner_a
            .vocabulary()
            .iter()
            .position(|w| w == "foo")
            .unwrap();
        let bar_idx_a = interner_a
            .vocabulary()
            .iter()
            .position(|w| w == "bar")
            .unwrap();
        let foo_val_a = PayloadVal::try_from(foo_idx_a).unwrap();
        let bar_val_a = PayloadVal::try_from(bar_idx_a).unwrap();

        let impacted_a = {
            let first = Ortho::new().add(foo_val_a)[0].clone();
            first.add(bar_val_a)[0].clone()
        };
        let non_impacted_a = {
            let first = Ortho::new().add(bar_val_a)[0].clone();
            first.add(foo_val_a)[0].clone()
        };

        let (archive_a_path, _) = save_archive_vec_internal(
            &interner_a,
            vec![impacted_a.clone(), non_impacted_a],
            Some(&impacted_a),
            "\"A\"",
            2,
            "foo bar",
            2,
            config,
        )
        .unwrap();

        let interner_b = Interner::from_text("foo baz");
        let foo_idx_b = interner_b
            .vocabulary()
            .iter()
            .position(|w| w == "foo")
            .unwrap();
        let baz_idx_b = interner_b
            .vocabulary()
            .iter()
            .position(|w| w == "baz")
            .unwrap();
        let foo_val_b = PayloadVal::try_from(foo_idx_b).unwrap();
        let baz_val_b = PayloadVal::try_from(baz_idx_b).unwrap();

        let impacted_b = {
            let first = Ortho::new().add(foo_val_b)[0].clone();
            first.add(baz_val_b)[0].clone()
        };
        let non_impacted_b = {
            let first = Ortho::new().add(baz_val_b)[0].clone();
            first.add(foo_val_b)[0].clone()
        };

        let (archive_b_path, _) = save_archive_vec_internal(
            &interner_b,
            vec![impacted_b.clone(), non_impacted_b],
            Some(&impacted_b),
            "\"B\"",
            2,
            "foo baz",
            2,
            config,
        )
        .unwrap();

        (archive_a_path, archive_b_path)
    }

    fn capture_archive_signature(config: &StateConfig) -> (Ortho, String, Vec<u64>) {
        let largest = file_handler::find_largest_archive_with_config(config)
            .unwrap()
            .expect("merged archive should exist");
        let optimal = file_handler::load_optimal_ortho(&largest.path).unwrap();
        let lineage = fs::read_to_string(Path::new(&largest.path).join("lineage.txt")).unwrap();
        let reader =
            GenerationStore::from_existing(Path::new(&largest.path).join("results"), 8).unwrap();
        let mut ids = Vec::new();
        for bucket in 0..8 {
            for result in reader.history_iter_with_buffer(bucket, 64 * 1024).unwrap() {
                let ortho = Ortho::from_bytes(result.unwrap().bytes.as_ref()).unwrap();
                ids.push(ortho.id());
            }
        }
        ids.sort_unstable();
        (optimal, lineage, ids)
    }

    #[test]
    fn test_score() {
        let ortho = Ortho::new();
        let score = ortho.score();
        // Empty ortho with dims [2,2] has volume (2-1)*(2-1) = 1
        assert_eq!(score.volume, 1);
        // All 4 slots are None
        assert_eq!(score.fullness, 0);
    }

    #[test]
    fn concurrent_claim_errors_are_retryable() {
        let not_found = mark_claim_race_if_applicable(FoldError::Io(std::io::Error::new(
            ErrorKind::NotFound,
            "missing",
        )));
        assert!(is_concurrent_claim_error(&not_found));

        let raw_not_found = FoldError::Io(std::io::Error::new(ErrorKind::NotFound, "missing"));
        assert!(!is_concurrent_claim_error(&raw_not_found));

        let already_exists = mark_claim_race_if_applicable(FoldError::Io(std::io::Error::new(
            ErrorKind::AlreadyExists,
            "exists",
        )));
        assert!(is_concurrent_claim_error(&already_exists));

        let dir_not_empty =
            mark_claim_race_if_applicable(FoldError::Io(std::io::Error::from_raw_os_error(39)));
        assert!(is_concurrent_claim_error(&dir_not_empty));

        let permission_denied = mark_claim_race_if_applicable(FoldError::Io(std::io::Error::new(
            ErrorKind::PermissionDenied,
            "denied",
        )));
        assert!(!is_concurrent_claim_error(&permission_denied));
    }

    #[test]
    fn merge_policy_defaults_and_env_override() {
        with_env_override("FOLD_MERGE_POLICY", None, || {
            assert_eq!(
                merge_policy_for_role(Role::Leader),
                MergePolicy::LargestLargest
            );
            assert_eq!(
                merge_policy_for_role(Role::Follower),
                MergePolicy::SmallestSmallest
            );
        });

        with_env_override("FOLD_MERGE_POLICY", Some("largest_smallest"), || {
            assert_eq!(
                merge_policy_for_role(Role::Leader),
                MergePolicy::LargestSmallest
            );
            assert_eq!(
                merge_policy_for_role(Role::Follower),
                MergePolicy::LargestSmallest
            );
        });
    }

    #[test]
    fn threaded_merge_matches_single_threaded_archive_output() {
        with_test_memory_overrides(|| {
            let run_merge = |merge_threads: &str| {
                let temp = TempDir::new().unwrap();
                let config = StateConfig::custom(temp.path().to_path_buf());
                file_handler::initialize_with_config(&config).unwrap();
                let (archive_a_path, archive_b_path) = build_test_merge_archives(&config);
                let metrics = Metrics::new();

                with_env_override("FOLD_MERGE_THREADS", Some(merge_threads), || {
                    merge_archives(
                        &archive_a_path,
                        &archive_b_path,
                        &config,
                        &metrics,
                        Role::Leader,
                    )
                    .unwrap();
                });

                capture_archive_signature(&config)
            };

            let single_thread = run_merge("1");
            let multi_thread = run_merge("3");

            assert_eq!(
                single_thread.0, multi_thread.0,
                "optimal ortho should match"
            );
            assert_eq!(single_thread.1, multi_thread.1, "lineage should match");
            assert_eq!(
                single_thread.2, multi_thread.2,
                "archive contents should match"
            );
        });
    }

    #[test]
    fn merge_seeds_impacted_work_queue() {
        with_test_memory_overrides(|| {
            // Temp state
            let temp = TempDir::new().unwrap();
            let config = StateConfig::custom(temp.path().to_path_buf());
            file_handler::initialize_with_config(&config).unwrap();
            let (archive_a_path, archive_b_path) = build_test_merge_archives(&config);

            // Merge and verify impacted queues are non-zero
            let metrics = Metrics::new();
            merge_archives(
                &archive_a_path,
                &archive_b_path,
                &config,
                &metrics,
                Role::Leader,
            )
            .unwrap();

            let snapshot = metrics.snapshot();
            assert!(
                snapshot.merge.impacted_queued_a > 0 && snapshot.merge.impacted_queued_b > 0,
                "impacted queues should be non-empty (got A:{} B:{})",
                snapshot.merge.impacted_queued_a,
                snapshot.merge.impacted_queued_b
            );
        });
    }

    struct ArchiveOffloader {
        dest: std::path::PathBuf,
        uploads: std::sync::Arc<std::sync::Mutex<usize>>,
    }

    impl RunOffloader for ArchiveOffloader {
        fn offload(&self, path: &std::path::Path) -> std::io::Result<bool> {
            fs::create_dir_all(&self.dest)?;
            let dest = self.dest.join(
                path.file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("artifact")),
            );
            fs::copy(path, &dest)?;
            *self.uploads.lock().unwrap() += 1;
            Ok(true)
        }
    }

    struct OffloaderGuard;
    impl Drop for OffloaderGuard {
        fn drop(&mut self) {
            set_run_offloader(None);
        }
    }

    #[test]
    fn archive_offload_removes_local_copy() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().to_path_buf();
        let archive_dir = base.join("archive_test.bin");
        fs::create_dir_all(&archive_dir).unwrap();
        let artifact = archive_dir.join("interner.bin");
        fs::write(&artifact, b"data").unwrap();

        let uploads = std::sync::Arc::new(std::sync::Mutex::new(0));
        let offloader = std::sync::Arc::new(ArchiveOffloader {
            dest: base.join("offloaded"),
            uploads: uploads.clone(),
        });
        let _guard = OffloaderGuard;
        set_run_offloader(Some(offloader));

        let metrics = Metrics::new();
        let result = offload_archive_dir(&archive_dir, &metrics).unwrap();
        assert!(result.is_some());
        assert!(!archive_dir.exists());
        assert!(*uploads.lock().unwrap() >= 1);
    }
}
