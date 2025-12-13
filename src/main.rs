use fold::{
    FoldError,
    disk_backed_queue::DiskBackedQueue,
    file_handler::{self, MemClaimGuard, StateConfig},
    interner::Interner,
    memory_config::MemoryConfig,
    metrics::Metrics,
    ortho::Ortho,
    seen_tracker::{BatchResult, SeenTracker},
    tui::Tui,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use sysinfo::ProcessesToUpdate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerRole {
    Leader,
    Follower,
}

impl WorkerRole {
    fn as_str(&self) -> &'static str {
        match self {
            WorkerRole::Leader => "leader",
            WorkerRole::Follower => "follower",
        }
    }
}

#[derive(Clone)]
enum PendingSource {
    Search,
    LargerArchive,
    SmallerArchive,
}

#[derive(Clone)]
struct PendingEntry {
    ortho: Ortho,
    source: PendingSource,
}

fn main() -> Result<(), FoldError> {
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
    let should_quit = Arc::new(AtomicBool::new(false));

    let tui_enabled = std::env::var("FOLD_DISABLE_TUI").is_err() && std::io::stdout().is_terminal();

    // Spawn TUI thread with panic-forwarding so we don't leave the terminal in a broken state.
    let tui_handle = if tui_enabled {
        let metrics_clone = metrics.clone_handle();
        let should_quit_clone = Arc::clone(&should_quit);
        Some(thread::spawn(move || {
            let result = std::panic::catch_unwind(move || {
                let mut tui = Tui::new(metrics_clone, should_quit_clone);
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

        let (volume, fullness) = optimal_ortho.score();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        metrics.update_optimal_ortho(|opt| {
            opt.volume = volume;
            opt.dims = optimal_ortho.dims().clone();
            opt.fullness = fullness;
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
            volume
        ));
    }

    // Main processing loop - two modes:
    // Mode 1: Merge archives (leaders pick the largest pair; followers merge the smallest pair only when no text is free)
    // Mode 2: Process txt into result
    let main_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), FoldError> {
            let mut last_role: Option<WorkerRole> = None;
            loop {
                // Check for stale heartbeats and recover abandoned work from crashed processes
                file_handler::check_and_recover_stale_work(&config)?;
                file_handler::cleanup_stale_mem_claims(&config)?;

                let role = determine_role(&config)?;
                if last_role != Some(role) {
                    metrics.add_log(format!("Role change: {:?}", role));
                    metrics.update_global(|g| g.role = role.as_str().to_string());
                    last_role = Some(role);
                } else {
                    metrics.update_global(|g| g.role = role.as_str().to_string());
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
                    WorkerRole::Leader => {
                        let archive_pair =
                            file_handler::get_two_largest_archives_with_config(&config)?;

                        if let Some((second_largest, largest)) = archive_pair {
                            // Mode 1: Merge archives
                            metrics.update_global(|g| g.mode = "Merging Archives".to_string());
                            metrics.clear_chart_history();
                            metrics.add_log("MODE 1: Merging archives".to_string());
                            metrics.add_log(format!("Merging: {} + {}", second_largest, largest));

                            match merge_archives(&second_largest, &largest, &config, &metrics, role)
                            {
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
                            match process_txt_file(txt_file.clone(), &config, &metrics, role) {
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
                    WorkerRole::Follower => {
                        // Followers prioritize ingesting new text; merge smallest archives only when none are available.
                        if let Some(txt_file) = file_handler::find_txt_file_with_config(&config)? {
                            metrics.update_global(|g| g.mode = "Processing Text".to_string());
                            metrics.clear_chart_history();
                            metrics.add_log("MODE 2: Processing text file".to_string());

                            match process_txt_file(txt_file.clone(), &config, &metrics, role) {
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
                        } else if let Some((smallest, second_smallest)) =
                            file_handler::get_two_smallest_archives_with_config(&config)?
                        {
                            // Mode 1 for followers: merge the two smallest archives when no text is free
                            metrics.update_global(|g| g.mode = "Merging Archives".to_string());
                            metrics.clear_chart_history();
                            metrics.add_log("MODE 1: Merging archives".to_string());
                            metrics.add_log(format!("Merging: {} + {}", smallest, second_smallest));

                            match merge_archives(
                                &smallest,
                                &second_smallest,
                                &config,
                                &metrics,
                                role,
                            ) {
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

    match (main_result, tui_result) {
        (Ok(Ok(())), Some(Ok(()))) | (Ok(Ok(())), None) => Ok(()),
        (Ok(Ok(())), Some(Err(panic))) => std::panic::resume_unwind(panic),
        (Ok(Err(e)), _) => Err(e),
        (Err(panic), _) => std::panic::resume_unwind(panic),
    }
}

fn process_txt_file(
    file_path: String,
    config: &StateConfig,
    metrics: &Metrics,
    role: WorkerRole,
) -> Result<(), FoldError> {
    // Ingest the text file (now includes reading the text)
    let ingestion = file_handler::ingest_txt_file_with_config(&file_path, config)?;
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

    // Calculate memory config for this file
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?.len();
    let mut memory_config = MemoryConfig::calculate(interner_bytes, 0);
    let mem_claim =
        acquire_memory_claim(role, config, metrics, &mut memory_config, interner_bytes)?;
    let mem_budget_bytes = memory_budget_bytes();

    metrics.update_global(|g| {
        g.queue_buffer_size = memory_config.queue_buffer_size;
        g.bloom_capacity = memory_config.bloom_capacity;
        g.num_shards = memory_config.num_shards;
        g.max_shards_in_memory = memory_config.max_shards_in_memory;
    });

    // Initialize work queue for this file (isolated to work folder)
    let work_queue_path = ingestion.work_queue_path();
    let mut work_queue =
        DiskBackedQueue::new_from_path(&work_queue_path, memory_config.queue_buffer_size)?;

    // Initialize results queue for this file (disk-backed to avoid OOM)
    let results_path = ingestion.results_path();
    let mut results =
        DiskBackedQueue::new_from_path(&results_path, memory_config.queue_buffer_size)?;

    // Initialize tracker for this file (isolated to work folder)
    let seen_shards_path = ingestion.seen_shards_path();
    let mut tracker = SeenTracker::with_path(
        &seen_shards_path,
        memory_config.bloom_capacity,
        memory_config.num_shards,
        memory_config.max_shards_in_memory,
    );
    let mut sys = sysinfo::System::new();
    metrics.update_global(|g| g.bloom_fp_rate = tracker.estimated_false_positive_rate());
    metrics.update_global(|g| g.bloom_fp_rate = tracker.estimated_false_positive_rate());

    // Seed with empty ortho
    let seed_ortho = Ortho::new();
    let seed_id = seed_ortho.id();

    tracker.insert(seed_id);
    let mut best_ortho = seed_ortho.clone();
    let mut best_score = best_ortho.score();
    let mut global_score = metrics.optimal_score();
    let mut optimal_dirty = false;
    let mut pending_new_orthos = 0usize;
    let mut pending_children: HashMap<usize, PendingEntry> = HashMap::new();

    work_queue.push(seed_ortho)?;

    metrics.set_operation_status("Processing orthos".to_string());

    // Process work queue until empty
    let mut processed_count = 0;
    loop {
        match work_queue.pop()? {
            Some(ortho) => {
                processed_count += 1;

                if processed_count % 1000 == 0 {
                    // Update metrics
                    metrics.record_queue_depth(work_queue.len());
                    metrics.record_seen_size(tracker.len());
                    metrics.record_optimal_volume(best_ortho.volume());
                    metrics.update_operation(|op| {
                        op.progress_current = processed_count;
                    });
                    if pending_new_orthos > 0 {
                        metrics.increment_new_orthos(pending_new_orthos);
                        pending_new_orthos = 0;
                    }
                    if optimal_dirty {
                        let (volume, fullness) = best_score;
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        metrics.update_optimal_ortho(|opt| {
                            opt.volume = volume;
                            opt.dims = best_ortho.dims().clone();
                            opt.fullness = fullness;
                            opt.capacity = best_ortho.payload().len();
                            opt.payload = best_ortho.payload().clone();
                            opt.vocab = interner.vocabulary().to_vec();
                            opt.last_update_time = now;
                        });
                        optimal_dirty = false;
                    }

                    // Update RAM usage and jobs count
                    sys.refresh_memory();
                    let (used_bytes, total_bytes) =
                        normalize_sysinfo_mem(sys.total_memory(), sys.used_memory());
                    let proc_rss_bytes = current_process_rss_bytes(&mut sys);
                    let percent = if total_bytes > 0 {
                        ((used_bytes as f64 / total_bytes as f64) * 100.0).round() as usize
                    } else {
                        0
                    };
                    let jobs_count =
                        file_handler::count_running_jobs_with_config(config).unwrap_or(0);
                    metrics.update_global(|g| {
                        g.ram_bytes = used_bytes as usize;
                        g.process_rss_bytes = proc_rss_bytes;
                        g.system_memory_percent = percent;
                        g.distinct_jobs_count = jobs_count;
                    });
                    apply_shard_pressure_valve(
                        &mut tracker,
                        metrics,
                        mem_budget_bytes,
                        used_bytes as usize,
                        role,
                    )?;
                    maybe_expand_bloom(&mut tracker, metrics, mem_budget_bytes, &mut sys, role)?;
                }

                if processed_count % 50000 == 0 {
                    metrics.add_log(format!("Progress: {} orthos processed", processed_count));
                }

                if processed_count % 100000 == 0 {
                    print_optimal(&best_ortho, &interner);
                    // Update heartbeat every 100k orthos (zero-arity)
                    ingestion.touch_heartbeat()?;
                    mem_claim.touch()?;
                    touch_leader_lock_if_owner(config)?;
                }

                // Get requirements from ortho
                let (forbidden, required) = ortho.get_requirements();

                // Get completions from interner
                let completions = interner.intersect(&required, &forbidden);

                // Generate child orthos
                let mut batch_ids = Vec::new();
                for completion in completions {
                    let children = ortho.add(completion);

                    for child in children {
                        let child_id = child.id();
                        pending_children.entry(child_id).or_insert(PendingEntry {
                            ortho: child,
                            source: PendingSource::Search,
                        });
                        batch_ids.push(child_id);
                    }
                }

                if !batch_ids.is_empty() {
                    let batch_result = tracker.check_batch(&batch_ids, false)?;
                    let mut on_new = |entry: PendingEntry| -> Result<(), FoldError> {
                        let child = entry.ortho;
                        let candidate_score = child.score();
                        if candidate_score > best_score {
                            best_ortho = child.clone();
                            best_score = candidate_score;
                        }
                        if candidate_score > global_score {
                            global_score = candidate_score;
                            optimal_dirty = true;
                        }
                        results.push(child.clone())?;
                        pending_new_orthos = pending_new_orthos.saturating_add(1);
                        work_queue.push(child)?;
                        Ok(())
                    };
                    handle_batch_result(batch_result, &mut pending_children, &mut on_new)?;
                }
            }
            None => {
                if tracker.buffered_total() == 0 {
                    break;
                }
                let batch_result = tracker.flush_pending()?;
                if batch_result.new.is_empty() && batch_result.seen.is_empty() {
                    if tracker.buffered_total() == 0 {
                        break;
                    }
                }
                let mut on_new = |entry: PendingEntry| -> Result<(), FoldError> {
                    let child = entry.ortho;
                    let candidate_score = child.score();
                    if candidate_score > best_score {
                        best_ortho = child.clone();
                        best_score = candidate_score;
                    }
                    if candidate_score > global_score {
                        global_score = candidate_score;
                        optimal_dirty = true;
                    }
                    results.push(child.clone())?;
                    pending_new_orthos = pending_new_orthos.saturating_add(1);
                    work_queue.push(child)?;
                    Ok(())
                };
                handle_batch_result(batch_result, &mut pending_children, &mut on_new)?;
            }
        }
    }

    if pending_new_orthos > 0 {
        metrics.increment_new_orthos(pending_new_orthos);
    }
    if optimal_dirty {
        let (volume, fullness) = best_score;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        metrics.update_optimal_ortho(|opt| {
            opt.volume = volume;
            opt.dims = best_ortho.dims().clone();
            opt.fullness = fullness;
            opt.capacity = best_ortho.payload().len();
            opt.payload = best_ortho.payload().clone();
            opt.vocab = interner.vocabulary().to_vec();
            opt.last_update_time = now;
        });
    }

    let total_orthos = results.len();
    metrics.add_log(format!("Completed: {} orthos generated", total_orthos));

    print_optimal(&best_ortho, &interner);

    // Save the result (using method on ingestion)
    let (archive_path, lineage) =
        ingestion.save_result(&interner, results, Some(&best_ortho), total_orthos)?;

    metrics.add_log(format!("Archive saved: {}", archive_path));

    // Update largest archive if this is bigger
    metrics.update_largest_archive(|la| {
        if total_orthos > la.ortho_count {
            la.filename = archive_path.clone();
            la.ortho_count = total_orthos;
            la.lineage = lineage;
        }
    });

    metrics.update_global(|g| g.processed_chunks += 1);

    // Delete the work folder (using consuming cleanup method)
    ingestion.cleanup()?;

    Ok(())
}

fn merge_archives(
    archive_a_path: &str,
    archive_b_path: &str,
    config: &StateConfig,
    metrics: &Metrics,
    role: WorkerRole,
) -> Result<(), FoldError> {
    // Get archive ortho counts for display BEFORE ingest moves them
    let orthos_a = file_handler::load_archive_metadata(archive_a_path).unwrap_or(0);
    let orthos_b = file_handler::load_archive_metadata(archive_b_path).unwrap_or(0);

    // Ingest archives for merging
    let ingestion =
        file_handler::ingest_archives_with_config(archive_a_path, archive_b_path, config)?;

    metrics.set_operation_status("Loading interners".to_string());

    // Load both interners using method
    let (interner_a, interner_b) = ingestion.load_interners()?;

    // Load lineages early to display provenance tree during merge
    let (lineage_a_early, lineage_b_early) = ingestion.load_lineages()?;
    let merged_lineage_preview = format!("({} {})", lineage_a_early, lineage_b_early);
    metrics.update_global(|g| g.current_lineage = merged_lineage_preview);

    // Determine which interner is smaller to optimize remapping
    // Only the smaller side needs remapping; larger side vocabulary becomes the base
    let a_is_smaller = interner_a.vocab_size() <= interner_b.vocab_size();

    let (larger_interner, smaller_interner, impacted_larger, impacted_smaller) = if a_is_smaller {
        let impacted_a = interner_a.impacted_keys(&interner_b);
        let impacted_b = interner_b.impacted_keys(&interner_a);
        (interner_b, interner_a, impacted_b, impacted_a)
    } else {
        let impacted_a = interner_a.impacted_keys(&interner_b);
        let impacted_b = interner_b.impacted_keys(&interner_a);
        (interner_a, interner_b, impacted_a, impacted_b)
    };

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
    });
    metrics.reset_new_orthos();
    metrics.reset_seen_history(); // start fresh for rehydrate stage

    // Create merged interner: larger absorbs smaller, so larger's vocab is the base
    let merged_interner = larger_interner.merge(&smaller_interner);

    metrics.update_global(|g| {
        g.interner_version = merged_interner.version();
        g.vocab_size = merged_interner.vocabulary().len();
    });
    metrics.add_log(format!(
        "Merged interner: v{}, vocab={} (Archive {} is smaller, only remapping that side)",
        merged_interner.version(),
        merged_interner.vocabulary().len(),
        if a_is_smaller { "A" } else { "B" }
    ));

    // Build vocabulary mapping for the smaller side (full remapping needed)
    let vocab_map_smaller =
        build_vocab_mapping(smaller_interner.vocabulary(), merged_interner.vocabulary());

    // Calculate memory config
    let interner_bytes =
        bincode::encode_to_vec(&merged_interner, bincode::config::standard())?.len();
    let mut memory_config = MemoryConfig::calculate(interner_bytes, 0);
    let mem_claim =
        acquire_memory_claim(role, config, metrics, &mut memory_config, interner_bytes)?;
    let mem_budget_bytes = memory_budget_bytes();

    metrics.update_global(|g| {
        g.queue_buffer_size = memory_config.queue_buffer_size;
        g.bloom_capacity = memory_config.bloom_capacity;
        g.num_shards = memory_config.num_shards;
        g.max_shards_in_memory = memory_config.max_shards_in_memory;
    });

    // Initialize work queue and results for merge (isolated to merge work folder)
    let work_queue_path = ingestion.work_queue_path();
    let mut work_queue =
        DiskBackedQueue::new_from_path(&work_queue_path, memory_config.queue_buffer_size)?;
    let results_path = config.results_dir(&format!("merged_{}", std::process::id()));
    let mut merged_results = DiskBackedQueue::new_from_path(
        results_path.to_str().unwrap(),
        memory_config.queue_buffer_size,
    )?;

    // Initialize tracker (isolated to merge work folder)
    let seen_shards_path = ingestion.seen_shards_path();
    let mut tracker = SeenTracker::with_path(
        &seen_shards_path,
        memory_config.bloom_capacity,
        memory_config.num_shards,
        memory_config.max_shards_in_memory,
    );
    let mut sys = sysinfo::System::new();

    // Seed with empty ortho
    let seed_ortho = Ortho::new();
    let seed_id = seed_ortho.id();
    tracker.insert(seed_id);
    metrics.reset_seen_size(tracker.len());
    let mut best_ortho = seed_ortho.clone();
    let mut best_score = best_ortho.score();
    work_queue.push(seed_ortho)?;
    let mut pending_ingest: HashMap<usize, PendingEntry> = HashMap::new();

    // Get results paths using method
    let (results_a_path, results_b_path) = ingestion.get_results_paths();

    // Process larger archive (no remapping needed - just version update)
    let (larger_path, larger_impacted, larger_name) = if a_is_smaller {
        (&results_b_path, &impacted_larger, "B")
    } else {
        (&results_a_path, &impacted_larger, "A")
    };

    // Pre-build HashSet for O(1) impact lookups instead of O(n×m) nested loops
    let larger_impacted_set: HashSet<&Vec<usize>> = larger_impacted.iter().collect();

    metrics.set_operation_status(format!("Processing Larger Archive {}", larger_name));
    metrics.update_operation(|op| {
        op.progress_current = 0;
        op.progress_total = 0;
    });

    let mut results_larger =
        DiskBackedQueue::new_from_path(larger_path, memory_config.queue_buffer_size)?;
    let mut total_from_larger = 0;
    let mut impacted_from_larger = 0;

    while let Some(ortho) = results_larger.pop()? {
        // Larger archive orthos don't need remapping - their vocabulary is already the base
        let ortho_id = ortho.id();
        pending_ingest.entry(ortho_id).or_insert(PendingEntry {
            ortho: ortho.clone(),
            source: PendingSource::LargerArchive,
        });
        let batch_result = tracker.check_batch(&[ortho_id], false)?;
        let mut on_new = |entry: PendingEntry| -> Result<(), FoldError> {
            let PendingEntry {
                ortho: child,
                source,
            } = entry;
            merged_results.push(child.clone())?;
            if matches!(source, PendingSource::LargerArchive) {
                total_from_larger += 1;
            }

            if total_from_larger % 10000 == 0 {
                ingestion.touch_heartbeat()?;
                mem_claim.touch()?;
                touch_leader_lock_if_owner(config)?;
                metrics.update_operation(|op| op.progress_current = total_from_larger);
                metrics.record_seen_size(tracker.len());
                maybe_expand_bloom(&mut tracker, metrics, mem_budget_bytes, &mut sys, role)?;
            }

            if is_ortho_impacted_fast(&child, &larger_impacted_set) {
                work_queue.push(child)?;
                if matches!(source, PendingSource::LargerArchive) {
                    impacted_from_larger += 1;
                }
            }
            Ok(())
        };
        handle_batch_result(batch_result, &mut pending_ingest, &mut on_new)?;
    }

    // Process smaller archive (needs remapping)
    let (smaller_path, smaller_impacted, smaller_name) = if a_is_smaller {
        (&results_a_path, &impacted_smaller, "A")
    } else {
        (&results_b_path, &impacted_smaller, "B")
    };

    // Pre-build HashSet for O(1) impact lookups
    let smaller_impacted_set: HashSet<&Vec<usize>> = smaller_impacted.iter().collect();

    metrics.set_operation_status(format!("Remapping Smaller Archive {}", smaller_name));
    metrics.update_operation(|op| {
        op.progress_current = 0;
        op.progress_total = 0;
    });

    let mut results_smaller =
        DiskBackedQueue::new_from_path(smaller_path, memory_config.queue_buffer_size)?;
    let mut total_from_smaller = 0;
    let mut impacted_from_smaller = 0;

    while let Some(ortho) = results_smaller.pop()? {
        // Smaller archive orthos need remapping to merged vocabulary
        if let Some(remapped) = ortho.remap(&vocab_map_smaller) {
            let remapped_id = remapped.id();
            pending_ingest.entry(remapped_id).or_insert(PendingEntry {
                ortho: remapped.clone(),
                source: PendingSource::SmallerArchive,
            });
            let batch_result = tracker.check_batch(&[remapped_id], false)?;
            let mut on_new = |entry: PendingEntry| -> Result<(), FoldError> {
                let PendingEntry {
                    ortho: child,
                    source,
                } = entry;
                merged_results.push(child.clone())?;
                if matches!(source, PendingSource::SmallerArchive) {
                    total_from_smaller += 1;
                }

                if total_from_smaller % 10000 == 0 {
                    ingestion.touch_heartbeat()?;
                    mem_claim.touch()?;
                    touch_leader_lock_if_owner(config)?;
                    metrics.update_operation(|op| op.progress_current = total_from_smaller);
                    metrics.record_seen_size(tracker.len());
                    maybe_expand_bloom(&mut tracker, metrics, mem_budget_bytes, &mut sys, role)?;
                }

                if is_ortho_impacted_fast(&ortho, &smaller_impacted_set) {
                    work_queue.push(child)?;
                    if matches!(source, PendingSource::SmallerArchive) {
                        impacted_from_smaller += 1;
                    }
                }
                Ok(())
            };
            handle_batch_result(batch_result, &mut pending_ingest, &mut on_new)?;
        }
    }

    while tracker.buffered_total() > 0 {
        let batch_result = tracker.flush_pending()?;
        if batch_result.new.is_empty() && batch_result.seen.is_empty() {
            break;
        }
        let mut on_new = |entry: PendingEntry| -> Result<(), FoldError> {
            let PendingEntry {
                ortho: child,
                source,
            } = entry;
            merged_results.push(child.clone())?;
            match source {
                PendingSource::LargerArchive => total_from_larger += 1,
                PendingSource::SmallerArchive => total_from_smaller += 1,
                PendingSource::Search => {}
            }

            if is_ortho_impacted_fast(&child, &smaller_impacted_set)
                || is_ortho_impacted_fast(&child, &larger_impacted_set)
            {
                work_queue.push(child.clone())?;
            }
            Ok(())
        };
        handle_batch_result(batch_result, &mut pending_ingest, &mut on_new)?;
    }
    pending_ingest.clear();

    // Update metrics based on which archive was which
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

    let (total_from_a, total_from_b) = if a_is_smaller {
        (total_from_smaller, total_from_larger)
    } else {
        (total_from_larger, total_from_smaller)
    };

    metrics.add_log(format!(
        "Remapping complete: A={}, B={}",
        total_from_a, total_from_b
    ));

    // Reset seen history for the search phase so the chart reflects the new stage only.
    metrics.reset_seen_history();
    metrics.reset_seen_size(tracker.len());

    metrics.set_operation_status("Processing merged space".to_string());
    metrics.update_operation(|op| {
        op.progress_current = 0;
        op.progress_total = work_queue.len();
    });

    let mut global_score = metrics.optimal_score();
    let mut optimal_dirty = false;
    let mut pending_new_orthos = 0usize;
    let mut pending_children: HashMap<usize, PendingEntry> = HashMap::new();

    // Process work queue
    let mut processed_count = 0;
    loop {
        match work_queue.pop()? {
            Some(ortho) => {
                processed_count += 1;

                if processed_count % 1000 == 0 {
                    let queue_depth = work_queue.len();
                    metrics.record_queue_depth(queue_depth);
                    metrics.record_seen_size(tracker.len());
                    metrics.record_optimal_volume(best_ortho.volume());
                    metrics.update_operation(|op| {
                        op.progress_current = processed_count;
                        op.progress_total = processed_count + queue_depth;
                    });
                    if pending_new_orthos > 0 {
                        metrics.increment_new_orthos(pending_new_orthos);
                        pending_new_orthos = 0;
                    }
                    if optimal_dirty {
                        let (volume, fullness) = best_score;
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        metrics.update_optimal_ortho(|opt| {
                            opt.volume = volume;
                            opt.dims = best_ortho.dims().clone();
                            opt.fullness = fullness;
                            opt.capacity = best_ortho.payload().len();
                            opt.payload = best_ortho.payload().clone();
                            opt.vocab = merged_interner.vocabulary().to_vec();
                            opt.last_update_time = now;
                        });
                        optimal_dirty = false;
                    }

                    // Update RAM usage and jobs count
                    sys.refresh_memory();
                    let (used_bytes, total_bytes) =
                        normalize_sysinfo_mem(sys.total_memory(), sys.used_memory());
                    let proc_rss_bytes = current_process_rss_bytes(&mut sys);
                    let percent = if total_bytes > 0 {
                        ((used_bytes as f64 / total_bytes as f64) * 100.0).round() as usize
                    } else {
                        0
                    };
                    let jobs_count =
                        file_handler::count_running_jobs_with_config(config).unwrap_or(0);
                    metrics.update_global(|g| {
                        g.ram_bytes = used_bytes as usize;
                        g.process_rss_bytes = proc_rss_bytes;
                        g.system_memory_percent = percent;
                        g.distinct_jobs_count = jobs_count;
                    });
                    apply_shard_pressure_valve(
                        &mut tracker,
                        metrics,
                        mem_budget_bytes,
                        used_bytes as usize,
                        role,
                    )?;
                    maybe_expand_bloom(&mut tracker, metrics, mem_budget_bytes, &mut sys, role)?;
                }

                if processed_count % 50000 == 0 {
                    metrics.add_log(format!("Merge progress: {} orthos", processed_count));
                }

                if processed_count % 100000 == 0 {
                    print_optimal(&best_ortho, &merged_interner);
                    // Touch heartbeat (zero-arity)
                    ingestion.touch_heartbeat()?;
                    mem_claim.touch()?;
                    touch_leader_lock_if_owner(config)?;
                }

                let (forbidden, required) = ortho.get_requirements();
                let completions = merged_interner.intersect(&required, &forbidden);

                let mut batch_ids = Vec::new();
                for completion in completions {
                    let children = ortho.add(completion);

                    for child in children {
                        let child_id = child.id();
                        pending_children.entry(child_id).or_insert(PendingEntry {
                            ortho: child,
                            source: PendingSource::Search,
                        });
                        batch_ids.push(child_id);
                    }
                }

                if !batch_ids.is_empty() {
                    let batch_result = tracker.check_batch(&batch_ids, false)?;
                    let mut on_new = |entry: PendingEntry| -> Result<(), FoldError> {
                        let child = entry.ortho;
                        let candidate_score = child.score();

                        // Update local best for this operation
                        if candidate_score > best_score {
                            best_ortho = child.clone();
                            best_score = candidate_score;
                        }
                        if candidate_score > global_score {
                            global_score = candidate_score;
                            optimal_dirty = true;
                        }

                        merged_results.push(child.clone())?;
                        pending_new_orthos = pending_new_orthos.saturating_add(1);
                        work_queue.push(child)?;
                        Ok(())
                    };
                    handle_batch_result(batch_result, &mut pending_children, &mut on_new)?;
                }
            }
            None => {
                if tracker.buffered_total() == 0 {
                    break;
                }
                let batch_result = tracker.flush_pending()?;
                if batch_result.new.is_empty() && batch_result.seen.is_empty() {
                    if tracker.buffered_total() == 0 {
                        break;
                    }
                }
                let mut on_new = |entry: PendingEntry| -> Result<(), FoldError> {
                    let child = entry.ortho;
                    let candidate_score = child.score();

                    if candidate_score > best_score {
                        best_ortho = child.clone();
                        best_score = candidate_score;
                    }
                    if candidate_score > global_score {
                        global_score = candidate_score;
                        optimal_dirty = true;
                    }

                    merged_results.push(child.clone())?;
                    pending_new_orthos = pending_new_orthos.saturating_add(1);
                    work_queue.push(child)?;
                    Ok(())
                };
                handle_batch_result(batch_result, &mut pending_children, &mut on_new)?;
            }
        }
    }

    if pending_new_orthos > 0 {
        metrics.increment_new_orthos(pending_new_orthos);
    }
    if optimal_dirty {
        let (volume, fullness) = best_score;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        metrics.update_optimal_ortho(|opt| {
            opt.volume = volume;
            opt.dims = best_ortho.dims().clone();
            opt.fullness = fullness;
            opt.capacity = best_ortho.payload().len();
            opt.payload = best_ortho.payload().clone();
            opt.vocab = merged_interner.vocabulary().to_vec();
            opt.last_update_time = now;
        });
    }

    let total_merged = merged_results.len();
    print_optimal(&best_ortho, &merged_interner);

    let new_orthos_from_merge = total_merged.saturating_sub(total_from_a + total_from_b);

    metrics.add_log(format!(
        "Merge complete: {} orthos ({} new)",
        total_merged, new_orthos_from_merge
    ));
    metrics.update_merge(|m| {
        m.completed_merges += 1;
        m.new_orthos_from_merge = new_orthos_from_merge;
    });
    metrics.update_global(|g| g.processed_chunks += 1);

    // Save the merged result using method (lineages already loaded earlier)
    let (merged_archive_path, merged_lineage) = ingestion.save_result(
        &merged_interner,
        merged_results,
        results_path.to_str().unwrap(),
        Some(&best_ortho),
        &lineage_a_early,
        &lineage_b_early,
        total_merged,
    )?;

    metrics.add_log(format!("Archive saved: {}", merged_archive_path));

    // Update largest archive metrics
    metrics.update_largest_archive(|la| {
        if total_merged > la.ortho_count {
            la.filename = merged_archive_path.clone();
            la.ortho_count = total_merged;
            la.lineage = merged_lineage;
        }
    });

    // Delete the original archives using consuming cleanup method
    ingestion.cleanup()?;

    Ok(())
}

// Build a mapping from old vocabulary indices to new vocabulary indices
fn build_vocab_mapping(old_vocab: &[String], new_vocab: &[String]) -> Vec<usize> {
    old_vocab
        .iter()
        .map(|word| {
            new_vocab
                .iter()
                .position(|w| w == word)
                .expect("Word from old vocab must exist in new vocab")
        })
        .collect()
}

// Check if an ortho uses any of the impacted keys (optimized with HashSet)
fn is_ortho_impacted_fast(ortho: &Ortho, impacted_set: &HashSet<&Vec<usize>>) -> bool {
    if impacted_set.is_empty() {
        return false;
    }

    let requirement_phrases = ortho.get_requirement_phrases();
    requirement_phrases
        .iter()
        .any(|phrase| impacted_set.contains(phrase))
}

fn print_optimal(_ortho: &Ortho, _interner: &Interner) {
    // Optimal ortho info is now displayed in TUI metrics
}

fn acquire_memory_claim(
    role: WorkerRole,
    config: &StateConfig,
    metrics: &Metrics,
    memory_config: &mut MemoryConfig,
    interner_bytes: usize,
) -> Result<MemClaimGuard, FoldError> {
    let budget = memory_budget_bytes();
    let active_claims = file_handler::load_active_mem_claims(config)?;
    let used: usize = active_claims.iter().map(|c| c.granted_bytes).sum();
    let available = budget.saturating_sub(used);
    let requested_bytes = memory_config.estimate_bytes(interner_bytes);
    let mut granted_bytes = requested_bytes;

    if requested_bytes > available {
        if let Some(scaled) = memory_config.scale_to_budget(available, interner_bytes) {
            *memory_config = scaled;
            granted_bytes = memory_config.estimate_bytes(interner_bytes);
            metrics.add_log(format!(
                "Scaled memory request for {}: requested={} granted={} available={}",
                role.as_str(),
                requested_bytes,
                granted_bytes,
                available
            ));
        } else {
            if role == WorkerRole::Leader {
                metrics.add_log(format!(
                    "Leader proceeding without budget: requested {} bytes, available {} bytes. Claiming anyway.",
                    requested_bytes, available
                ));
                granted_bytes = requested_bytes;
            } else {
                metrics.add_log(format!(
                    "Exiting: insufficient memory budget (requested {} bytes, available {}).",
                    requested_bytes, available
                ));
                return Err(FoldError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Insufficient memory budget for job",
                )));
            }
        }
    }

    file_handler::create_mem_claim(config, role.as_str(), requested_bytes, granted_bytes)
}

fn memory_budget_bytes() -> usize {
    if let Ok(budget_env) = std::env::var("FOLD_RAM_BUDGET_BYTES") {
        if let Ok(parsed) = budget_env.parse::<usize>() {
            return parsed;
        }
    }

    let mut sys = sysinfo::System::new_all();
    sys.refresh_memory();
    let (_, total_bytes) = normalize_sysinfo_mem(sys.total_memory(), sys.used_memory());
    (total_bytes * 75) / 100
}

fn normalize_sysinfo_mem(total_raw: u64, used_raw: u64) -> (usize, usize) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            if let Some(mem_total_kib) = meminfo
                .lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
            {
                let mem_total_kib_f = mem_total_kib as f64;
                // If sysinfo matches /proc/meminfo in KiB, convert to bytes.
                if within_10_pct(total_raw as f64, mem_total_kib_f) {
                    let factor = 1024usize;
                    return (
                        (used_raw as usize).saturating_mul(factor),
                        (total_raw as usize).saturating_mul(factor),
                    );
                }
                // If sysinfo already reports bytes (matches /proc/meminfo bytes), keep as-is.
                let mem_total_bytes_f = mem_total_kib_f * 1024.0;
                if within_10_pct(total_raw as f64, mem_total_bytes_f) {
                    return (used_raw as usize, total_raw as usize);
                }
            }
        }
    }
    // Fallback: assume values are in KiB, convert to bytes.
    let factor = 1024usize;
    (
        (used_raw as usize).saturating_mul(factor),
        (total_raw as usize).saturating_mul(factor),
    )
}

fn within_10_pct(v: f64, target: f64) -> bool {
    if !v.is_finite() || !target.is_finite() || target == 0.0 {
        return false;
    }
    let diff = (v - target).abs();
    diff <= target * 0.10
}

fn current_process_rss_bytes(sys: &mut sysinfo::System) -> usize {
    if let Ok(pid) = sysinfo::get_current_pid() {
        let _ = sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
        if let Some(proc) = sys.process(pid) {
            return proc.memory() as usize;
        }
    }
    0
}

fn apply_shard_pressure_valve(
    tracker: &mut SeenTracker,
    metrics: &Metrics,
    mem_budget_bytes: usize,
    used_memory_bytes: usize,
    role: WorkerRole,
) -> Result<(), FoldError> {
    if mem_budget_bytes == 0 {
        return Ok(());
    }

    let (high_water_pct, low_water_pct) = match role {
        WorkerRole::Leader => (0.90, 0.80),
        WorkerRole::Follower => (0.80, 0.70),
    };
    let high_water = (mem_budget_bytes as f64 * high_water_pct).round() as usize;
    let low_water = (mem_budget_bytes as f64 * low_water_pct).round() as usize;

    let current_cap = tracker.max_shards_in_memory();

    if used_memory_bytes >= high_water {
        if current_cap <= 1 {
            if role == WorkerRole::Follower && used_memory_bytes >= high_water {
                metrics.add_log(format!(
                    "Memory pressure ({}): used {} MB (>= {:.0}% of budget {} MB) at min shards; follower exiting",
                    role.as_str(),
                    used_memory_bytes / 1_048_576,
                    high_water_pct * 100.0,
                    mem_budget_bytes / 1_048_576,
                ));
                return Err(FoldError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Follower exiting: memory pressure",
                )));
            }
            return Ok(());
        }

        let new_cap = current_cap.saturating_sub(1).max(1);

        let evicted = tracker.shrink_shards_in_memory(new_cap);
        if current_cap != tracker.max_shards_in_memory() || evicted > 0 {
            metrics.add_log(format!(
                "Memory pressure ({}): used {} MB (>= {:.0}% of budget {} MB); shard cap {} -> {} (evicted {} shards)",
                role.as_str(),
                used_memory_bytes / 1_048_576,
                high_water_pct * 100.0,
                mem_budget_bytes / 1_048_576,
                current_cap,
                tracker.max_shards_in_memory(),
                evicted
            ));
            metrics.update_global(|g| g.max_shards_in_memory = tracker.max_shards_in_memory());
        }
    } else if used_memory_bytes <= low_water {
        let grow_target = current_cap
            .saturating_add(1)
            .min(tracker.max_shards_ceiling());
        let grown = tracker.grow_shards_in_memory(grow_target);
        if grown > 0 {
            metrics.add_log(format!(
                "Memory recovered ({}): used {} MB (<= {:.0}% of budget {} MB); shard cap {} -> {} (grew {})",
                role.as_str(),
                used_memory_bytes / 1_048_576,
                low_water_pct * 100.0,
                mem_budget_bytes / 1_048_576,
                current_cap,
                tracker.max_shards_in_memory(),
                grown
            ));
            metrics.update_global(|g| g.max_shards_in_memory = tracker.max_shards_in_memory());
        }
    }
    Ok(())
}

fn maybe_expand_bloom(
    tracker: &mut SeenTracker,
    metrics: &Metrics,
    mem_budget_bytes: usize,
    sys: &mut sysinfo::System,
    role: WorkerRole,
) -> Result<(), FoldError> {
    if mem_budget_bytes == 0 {
        return Ok(());
    }

    let current_fp = tracker.estimated_false_positive_rate();
    metrics.update_global(|g| g.bloom_fp_rate = current_fp);

    const FP_THRESHOLD: f64 = 0.02; // trigger expansion when above 2%
    const FP_TARGET: f64 = 0.01; // aim back toward 1%

    if current_fp <= FP_THRESHOLD {
        return Ok(());
    }

    let mut target_capacity = tracker.bloom_capacity().saturating_mul(2);
    let mut projected_fp = tracker.estimated_false_positive_rate_for_capacity(target_capacity);
    while projected_fp > FP_TARGET {
        target_capacity = target_capacity.saturating_mul(2);
        projected_fp = tracker.estimated_false_positive_rate_for_capacity(target_capacity);
        if target_capacity >= tracker.bloom_capacity().saturating_mul(64) {
            break;
        }
    }

    // Estimate memory for the rebuild (conservative: old bloom + new bloom while rebuilding).
    const BLOOM_BYTES_PER_ITEM_EST: usize = 2;
    let new_bloom_bytes = target_capacity.saturating_mul(BLOOM_BYTES_PER_ITEM_EST);

    sys.refresh_memory();
    let (mut used_bytes, _) = normalize_sysinfo_mem(sys.total_memory(), sys.used_memory());
    let shrink_threshold_bytes = (mem_budget_bytes as f64 * 0.90).round() as usize;

    // If we can't fit the rebuild, shrink shards by one until we can or we hit minimum.
    while used_bytes.saturating_add(new_bloom_bytes) >= shrink_threshold_bytes
        && tracker.max_shards_in_memory() > 1
    {
        let current_cap = tracker.max_shards_in_memory();
        let new_cap = current_cap.saturating_sub(1).max(1);
        let evicted = tracker.shrink_shards_in_memory(new_cap);
        if evicted > 0 || current_cap != tracker.max_shards_in_memory() {
            metrics.add_log(format!(
                "Bloom headroom: shrinking shards {} -> {} (evicted {}) to make room",
                current_cap,
                tracker.max_shards_in_memory(),
                evicted
            ));
            metrics.update_global(|g| g.max_shards_in_memory = tracker.max_shards_in_memory());
        } else {
            break;
        }
        sys.refresh_memory();
        used_bytes = normalize_sysinfo_mem(sys.total_memory(), sys.used_memory()).0;
    }

    // After shrinking, check if we have enough headroom.
    if used_bytes.saturating_add(new_bloom_bytes) >= shrink_threshold_bytes {
        match role {
            WorkerRole::Follower => {
                metrics.add_log(format!(
                    "Bloom rebuild aborted: not enough headroom even after shard shrink (used {} MB, need {} MB, budget {:.0}%); follower exiting",
                    used_bytes / 1_048_576,
                    new_bloom_bytes / 1_048_576,
                    (shrink_threshold_bytes as f64 / mem_budget_bytes as f64) * 100.0
                ));
                return Err(FoldError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Follower exiting: insufficient headroom for bloom rebuild",
                )));
            }
            WorkerRole::Leader => {
                metrics.add_log(format!(
                    "Bloom rebuild deferred: not enough headroom even after shard shrink (used {} MB, need {} MB, budget {:.0}%)",
                    used_bytes / 1_048_576,
                    new_bloom_bytes / 1_048_576,
                    (shrink_threshold_bytes as f64 / mem_budget_bytes as f64) * 100.0
                ));
                return Ok(());
            }
        }
    }

    metrics.add_log(format!(
        "Bloom FP degraded to {:.2}% (cap {}, seen {}); rebuilding bloom to {} (projected {:.2}%)",
        current_fp * 100.0,
        tracker.bloom_capacity(),
        tracker.len(),
        target_capacity,
        projected_fp * 100.0
    ));

    let new_fp = tracker.rebuild_bloom(target_capacity)?;
    metrics.update_global(|g| {
        g.bloom_capacity = target_capacity;
        g.bloom_fp_rate = new_fp;
    });

    sys.refresh_memory();
    let used_bytes = normalize_sysinfo_mem(sys.total_memory(), sys.used_memory()).0;
    apply_shard_pressure_valve(tracker, metrics, mem_budget_bytes, used_bytes, role)?;

    Ok(())
}

fn determine_role(config: &StateConfig) -> Result<WorkerRole, FoldError> {
    if let Ok(force) = std::env::var("FOLD_FORCE_ROLE") {
        let force_lower = force.to_lowercase();
        if force_lower == "follower" {
            return Ok(WorkerRole::Follower);
        } else if force_lower == "leader" {
            return ensure_leader_lock(config);
        }
    }
    ensure_leader_lock(config)
}

fn ensure_leader_lock(config: &StateConfig) -> Result<WorkerRole, FoldError> {
    let lock_path = config.in_process_dir().join("leader.lock");
    fs::create_dir_all(config.in_process_dir()).map_err(FoldError::Io)?;

    let claim_leader = || -> Result<bool, FoldError> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
            .as_secs();
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
                return Ok(WorkerRole::Leader);
            } else {
                return Ok(WorkerRole::Follower);
            }
        }
    }

    if claim_leader()? {
        Ok(WorkerRole::Leader)
    } else {
        Ok(WorkerRole::Follower)
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

fn handle_batch_result(
    batch_result: BatchResult,
    pending_children: &mut HashMap<usize, PendingEntry>,
    on_new: &mut impl FnMut(PendingEntry) -> Result<(), FoldError>,
) -> Result<(), FoldError> {
    for id in batch_result.seen {
        pending_children.remove(&id);
    }
    for id in batch_result.new {
        if let Some(entry) = pending_children.remove(&id) {
            on_new(entry)?;
        }
    }
    Ok(())
}

// Treat common IO races (files already moved by another process) as recoverable.
fn is_concurrent_claim_error(err: &FoldError) -> bool {
    match err {
        FoldError::Io(io_err) => {
            matches!(
                io_err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::AlreadyExists
            ) || matches!(io_err.raw_os_error(), Some(39) | Some(66))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn test_score() {
        let ortho = Ortho::new();
        let (volume, fullness) = ortho.score();
        // Empty ortho with dims [2,2] has volume (2-1)*(2-1) = 1
        assert_eq!(volume, 1);
        // All 4 slots are None
        assert_eq!(fullness, 0);
    }

    #[test]
    fn concurrent_claim_errors_are_retryable() {
        let not_found = FoldError::Io(std::io::Error::new(ErrorKind::NotFound, "missing"));
        assert!(is_concurrent_claim_error(&not_found));

        let already_exists = FoldError::Io(std::io::Error::new(ErrorKind::AlreadyExists, "exists"));
        assert!(is_concurrent_claim_error(&already_exists));

        let dir_not_empty = FoldError::Io(std::io::Error::from_raw_os_error(39));
        assert!(is_concurrent_claim_error(&dir_not_empty));

        let permission_denied =
            FoldError::Io(std::io::Error::new(ErrorKind::PermissionDenied, "denied"));
        assert!(!is_concurrent_claim_error(&permission_denied));
    }
}
