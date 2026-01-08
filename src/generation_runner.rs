use crate::{
    completion_pruning::bound_completion,
    error::FoldError,
    file_handler::StateConfig,
    generation_store::{Config, GenerationStore, ProgressCallback, Role},
    interner::Interner,
    metrics::{GenerationStat, Metrics},
    ortho::{Ortho, PayloadVal, payload_to_usize},
};
use std::time::Instant;
use sysinfo::{ProcessesToUpdate, System, get_current_pid};

pub const COMPLETION_CHUNK_SIZE: usize = 1_000;
pub const FANOUT_LOG_THRESHOLD: usize = COMPLETION_CHUNK_SIZE;

pub struct GenerationRunResult {
    pub best_ortho: Ortho,
    pub best_score: (usize, usize),
    pub generation_stats: Vec<GenerationStat>,
    pub total_processed: u64,
    pub optimal_dirty: bool,
}

/// Run the core generation loop over the provided store/interner.
///
/// This is extracted from `main.rs` so it can be reused (e.g., by export_data) without
/// duplicating logic. Callers supply lightweight closures for quit/housekeeping and an
/// optional progress callback for store transitions.
pub fn run_generation_loop<FQuit, FHousekeeping>(
    interner: &Interner,
    store: &mut GenerationStore,
    cfg: &Config,
    role: Role,
    metrics: &Metrics,
    mut should_quit: FQuit,
    mut housekeeping: FHousekeeping,
    mut progress_cb_factory: impl FnMut(u64) -> Option<ProgressCallback>,
    state_config: Option<&StateConfig>,
) -> Result<GenerationRunResult, FoldError>
where
    FQuit: FnMut() -> bool,
    FHousekeeping: FnMut() -> Result<(), FoldError>,
{
    let _ = role;
    // Seed with empty ortho
    let seed_ortho = Ortho::new();
    let mut best_ortho = seed_ortho.clone();
    let mut best_score = best_ortho.score();
    let mut global_score = metrics.optimal_score();
    let mut optimal_dirty = false;
    let mut total_processed = 0u64;

    // Push seed to work queue
    store.push_segments(vec![seed_ortho])?;

    // Initialize metrics with initial state
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

    // Check cache for initial optimal ortho
    if let Some(cache_ortho) = store.peek_best_ortho_in_cache() {
        metrics.record_optimal_volume(cache_ortho.volume());
    }

    metrics.set_operation_status("Processing orthos".to_string());
    metrics.reset_prune_counts();

    let mut sys = sysinfo::System::new();
    let mut generation = 0u64;
    let mut generation_stats: Vec<GenerationStat> = Vec::new();

    // Tracking for throughput calculation
    let mut last_report_time = Instant::now();
    let mut last_report_count = 0u64;
    let mut last_housekeeping: Instant;

    // Generational processing loop
    loop {
        let work_len = store.work_len();
        if work_len == 0 || should_quit() {
            break;
        }

        // Update global metrics at start of generation
        metrics.update_global(|g| {
            g.generation = generation;
            g.phase = format!("Gen {} Processing", generation);
            g.work_len = work_len;
            g.seen_len_accepted = store.seen_len_accepted();
            g.run_budget_bytes = cfg.run_budget_bytes;
            g.fan_in = cfg.fan_in;
        });
        // Set progress tracking for this generation
        metrics.update_operation(|op| {
            op.progress_total = work_len as usize;
            op.progress_current = 0;
        });
        metrics.set_operation_status(format!("Processing Gen {}", generation));
        // Record samples for charts
        metrics.record_work_len(work_len as usize);
        metrics.record_seen_len_accepted(store.seen_len_accepted() as usize);

        // Update bucket metrics
        let bucket_stats = store.bucket_stats();
        let bucket_metrics: Vec<_> = bucket_stats
            .into_iter()
            .map(|bs| crate::metrics::BucketMetrics {
                bucket_id: bs.bucket_id,
                run_count: bs.run_count,
                landing_size: bs.landing_size,
                history_size_estimate: bs.history_size_estimate,
                state: crate::metrics::BucketState::Pending,
                new_work: 0,
            })
            .collect();
        metrics.update_bucket_metrics(bucket_metrics);

        // Low-frequency cache check (once per generation)
        if let Some(cache_ortho) = store.peek_best_ortho_in_cache() {
            if cache_ortho.volume() > best_ortho.volume() {
                let cache_score = cache_ortho.score();
                metrics.record_optimal_volume(cache_score.0);
                if cache_score > best_score {
                    best_ortho = cache_ortho;
                    best_score = cache_score;
                    optimal_dirty = true;
                }
            }
        }

        metrics.add_log(format!(
            "Generation {}: processing {} work items",
            generation, work_len
        ));

        let mut gen_processed = 0u64;

        let gen_start = Instant::now();
        let accepted_before = store.seen_len_accepted();
        last_housekeeping = Instant::now();

        // Process all work in this generation
        while let Some(ortho) = store.pop_work()? {
            if should_quit() {
                break;
            }
            gen_processed += 1;
            total_processed += 1;

            // Periodic updates on a time cadence
            if last_housekeeping.elapsed().as_millis() >= 1000 {
                // Update progress for current generation
                metrics.update_operation(|op| {
                    op.progress_current = gen_processed as usize;
                });

                let now = Instant::now();
                let elapsed = now.duration_since(last_report_time).as_secs_f64();

                // Calculate throughput
                let processed_since_last = total_processed - last_report_count;
                let throughput = if elapsed > 0.0 {
                    (processed_since_last as f64 / elapsed) as usize
                } else {
                    0
                };

                // Update every second for visibility
                if elapsed >= 1.0 {
                    metrics.update_global(|g| {
                        g.phase = format!("Gen {} Processing ({}/s)", generation, throughput);
                    });
                    last_report_time = now;
                    last_report_count = total_processed;
                }
                last_housekeeping = now;

                metrics.record_optimal_volume(best_ortho.volume());

                // Update work queue metrics
                metrics.update_global(|g| {
                    g.work_len = store.work_len();
                    g.seen_len_accepted = store.seen_len_accepted();
                });
                metrics.record_work_len(store.work_len() as usize);
                metrics.record_landing_buffer_count(store.total_landing_size());

                // Update bucket metrics for TUI visualization
                let bucket_stats = store.bucket_stats();
                let bucket_metrics: Vec<_> = bucket_stats
                    .into_iter()
                    .map(|bs| crate::metrics::BucketMetrics {
                        bucket_id: bs.bucket_id,
                        run_count: bs.run_count,
                        landing_size: bs.landing_size,
                        history_size_estimate: bs.history_size_estimate,
                        state: crate::metrics::BucketState::Pending,
                        new_work: 0,
                    })
                    .collect();
                metrics.update_bucket_metrics(bucket_metrics);

                // System metrics (RAM)
                sys.refresh_memory();
                let (used_bytes, total_bytes) =
                    normalize_sysinfo_mem(sys.total_memory(), sys.used_memory());
                let proc_rss_bytes = current_process_rss_bytes(&mut sys);
                let percent = if total_bytes > 0 {
                    ((used_bytes as f64 / total_bytes as f64) * 100.0).round() as usize
                } else {
                    0
                };
                let jobs_count = if let Some(cfg) = state_config {
                    crate::file_handler::count_running_jobs_with_config(cfg).unwrap_or(0)
                } else {
                    0
                };
                metrics.update_global(|g| {
                    g.ram_bytes = used_bytes;
                    g.process_rss_bytes = proc_rss_bytes;
                    g.system_memory_percent = percent;
                    g.distinct_jobs_count = jobs_count;
                });

                // Housekeeping hook (heartbeats, mem claim, leader lock)
                housekeeping()?;
            }

            // Get requirements from ortho
            let (forbidden, required) = ortho.get_requirements();
            let forbidden_usize: Vec<usize> =
                forbidden.iter().map(|v| payload_to_usize(*v)).collect();
            let required_usize: Vec<Vec<usize>> = required
                .iter()
                .map(|r| r.iter().map(|v| payload_to_usize(*v)).collect())
                .collect();

            // Get completions from interner
            let completions = interner.intersect(&required_usize, &forbidden_usize);
            let total_completions = completions.len();
            if total_completions > FANOUT_LOG_THRESHOLD {
                let chunks =
                    (total_completions + COMPLETION_CHUNK_SIZE - 1) / COMPLETION_CHUNK_SIZE;
                metrics.add_log(format!(
                    "Fanout: {} completions; processing in {} chunks of {}",
                    total_completions, chunks, COMPLETION_CHUNK_SIZE
                ));
            }

            // Generate child orthos and record results
            for completion in completions {
                if bound_completion(&ortho, completion, interner, best_score) {
                    metrics.increment_pruned_completions(1);
                    continue;
                }
                metrics.increment_expanded_completions(1);
                let completion_val =
                    PayloadVal::try_from(completion).expect("completion overflowed u32");
                let children = ortho.add(completion_val);
                for child in children {
                    let candidate_score = child.score();
                    if candidate_score > best_score {
                        best_ortho = child.clone();
                        best_score = candidate_score;
                    }
                    if candidate_score > global_score {
                        global_score = candidate_score;
                        optimal_dirty = true;
                    }

                    // Record result to landing zone
                    store.record_result_with_threshold(&child, cfg.landing_flush_threshold)?;

                    // Increment new orthos counter for each generated ortho
                    metrics.increment_new_orthos(1);
                }
            }
        }

        metrics.add_log(format!(
            "Generation {}: processed {} orthos",
            generation, gen_processed
        ));

        // End of generation: drain, compact, anti-join, push new work
        metrics.update_global(|g| {
            g.phase = format!(
                "Gen {} → {} transition starting",
                generation,
                generation + 1
            );
        });
        metrics.set_operation_status(format!(
            "Gen {} → {} transition",
            generation,
            generation + 1
        ));

        let processing_secs = gen_start.elapsed().as_secs_f64();
        let transition_start = Instant::now();
        let progress_callback = progress_cb_factory(generation);
        let new_work = store.on_generation_end(cfg, progress_callback.as_ref())?;
        let transition_secs = transition_start.elapsed().as_secs_f64();
        let accepted_delta = store.seen_len_accepted().saturating_sub(accepted_before);
        generation_stats.push(GenerationStat {
            generation,
            processing_secs,
            transition_secs,
            accepted: accepted_delta,
            new_work,
        });

        // Update metrics after generation transition
        metrics.update_global(|g| {
            g.work_len = store.work_len();
            g.seen_len_accepted = store.seen_len_accepted();
            g.phase = format!("Gen {} complete", generation);
        });
        metrics.record_work_len(store.work_len() as usize);
        metrics.record_landing_buffer_count(store.total_landing_size());

        metrics.add_log(format!(
            "Generation {} complete: {} new work items, {} total seen",
            generation,
            new_work,
            store.seen_len_accepted()
        ));

        generation += 1;
        if new_work == 0 {
            metrics.add_log("No new work after transition; stopping generations".to_string());
            break;
        }
    }

    Ok(GenerationRunResult {
        best_ortho,
        best_score,
        generation_stats,
        total_processed,
        optimal_dirty,
    })
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
                fn within_10_pct(a: f64, b: f64) -> bool {
                    (a - b).abs() / a.max(b) <= 0.1
                }
                if within_10_pct(total_raw as f64, mem_total_kib_f) {
                    let factor = 1024usize;
                    return (
                        (used_raw as usize).saturating_mul(factor),
                        (total_raw as usize).saturating_mul(factor),
                    );
                }
                let mem_total_bytes_f = mem_total_kib_f * 1024.0;
                if within_10_pct(total_raw as f64, mem_total_bytes_f) {
                    return (used_raw as usize, total_raw as usize);
                }
            }
        }
    }
    let factor = 1024usize;
    (
        (used_raw as usize).saturating_mul(factor),
        (total_raw as usize).saturating_mul(factor),
    )
}

fn current_process_rss_bytes(sys: &mut System) -> usize {
    if let Ok(pid) = get_current_pid() {
        let _ = sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
        if let Some(proc_) = sys.process(pid) {
            return proc_.memory() as usize;
        }
    }
    0
}
