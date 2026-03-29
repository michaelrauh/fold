use crate::{
    completion_pruning::{CompletionContext, bound_completion_ctx},
    disk_safety,
    error::FoldError,
    file_handler::StateConfig,
    generation_store::{Config, GenerationStore, ProgressCallback, Role},
    interner::Interner,
    memory_safety,
    metrics::{GenerationStat, Metrics, OperationDeltas},
    offload_config::OffloadConfig,
    ortho::{Ortho, OrthoScore, PayloadVal},
};
use fixedbitset::FixedBitSet;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use sysinfo::Disks;

pub const COMPLETION_CHUNK_SIZE: usize = 1_000;
pub const FANOUT_LOG_THRESHOLD: usize = COMPLETION_CHUNK_SIZE;
const EST_BYTES_PER_ORTHO: u64 = 200;

struct PressureWatchdog {
    landing_bytes_high_water: Option<u64>,
    enabled: bool,
}

impl PressureWatchdog {
    fn from_config(cfg: OffloadConfig) -> Self {
        Self {
            landing_bytes_high_water: cfg.landing_bytes_high_water,
            enabled: cfg.enabled,
        }
    }

    fn should_trigger(&self, landing_bytes: u64) -> bool {
        if !self.enabled {
            return false;
        }
        self.landing_bytes_high_water
            .map(|t| landing_bytes >= t)
            .unwrap_or(false)
    }

    fn maybe_handle(
        &self,
        store: &mut GenerationStore,
        cfg: &Config,
        metrics: &Metrics,
    ) -> Result<bool, FoldError> {
        let landing_est_bytes =
            (store.total_landing_size() as u64).saturating_mul(EST_BYTES_PER_ORTHO);
        if !self.should_trigger(landing_est_bytes) {
            return Ok(false);
        }
        metrics.add_log(format!(
            "Pressure watchdog: landing_est_bytes={}",
            landing_est_bytes
        ));
        metrics.record_pressure_trigger();
        let stats = store.pressure_spill_to_local_runs(cfg)?;
        metrics.add_log(format!(
            "Pressure spill (local): buckets_drained={}, spill_runs_created={}, spill_bytes_created={}",
            stats.buckets_drained, stats.spill_runs_created, stats.spill_bytes_created
        ));
        metrics.record_landing_buffer_count(store.total_landing_size());
        Ok(true)
    }
}

fn maybe_run_processing_reclaim_safe_point(
    store: &mut GenerationStore,
    metrics: &Metrics,
    reason: &str,
) -> Result<bool, FoldError> {
    let reservation = store.processing_reclaim_reservation_bytes();
    let Some(target_free) = disk_safety::reclaim_required(reservation)? else {
        return Ok(false);
    };
    metrics.add_log(format!(
        "Reclaim pending at safe point: reason={}, reservation_bytes={}, target_free={}",
        reason, reservation, target_free
    ));
    store.prepare_for_reclaim()?;
    disk_safety::run_reclaim_to_target(target_free, reason)?;
    metrics.add_log(format!(
        "Reclaim complete at safe point: reason={}, reservation_bytes={}, target_free={}",
        reason, reservation, target_free
    ));
    Ok(true)
}

fn flush_operation_deltas(metrics: &Metrics, deltas: &mut OperationDeltas) {
    metrics.apply_operation_deltas(*deltas);
    *deltas = OperationDeltas::default();
}

pub struct GenerationRunResult {
    pub best_ortho: Ortho,
    pub best_score: OrthoScore,
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
    let update_optimal_metrics = |ortho: &Ortho| {
        let score = ortho.score();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        metrics.update_optimal_ortho(|opt| {
            opt.volume = score.volume;
            opt.variance_num = score.variance_num;
            opt.variance_den = score.variance_den;
            opt.dims = ortho.dims().clone();
            opt.fullness = score.fullness;
            opt.capacity = ortho.payload().len();
            opt.payload = ortho.payload().clone();
            opt.vocab = interner.vocabulary().to_vec();
            opt.last_update_time = now;
        });
        metrics.record_optimal_volume(score.volume);
    };

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
    let mut disks = Disks::new_with_refreshed_list();
    let disk_base = state_config.map(|cfg| {
        cfg.base_dir
            .canonicalize()
            .unwrap_or_else(|_| cfg.base_dir.clone())
    });
    let mut generation = 0u64;
    let mut generation_stats: Vec<GenerationStat> = Vec::new();
    let offload_cfg = state_config
        .map(|cfg| OffloadConfig::from_env_with_base(&cfg.base_dir))
        .unwrap_or_else(OffloadConfig::from_env);
    let pressure_watchdog = PressureWatchdog::from_config(offload_cfg.clone());
    let mut prev_new_work: Option<u64> = None;
    let mut reclaim_pending = false;
    crate::generation_store::set_offload_metrics_handle(Some(metrics.clone_handle()));
    store.sync_runtime_metrics();

    // One-time snapshot: prefix_stats for single-token prefixes (to detect underestimation)
    {
        let single_prefix_lens: Vec<usize> = interner
            .prefix_entries()
            .filter_map(|(p, _)| {
                if p.len() == 1 {
                    interner.prefix_stats(p.as_slice())
                } else {
                    None
                }
            })
            .collect();
        if !single_prefix_lens.is_empty() {
            let mut sorted = single_prefix_lens.clone();
            sorted.sort_unstable();
            let len = sorted.len();
            let median = sorted[len / 2];
            let p90 = sorted[((len as f64 * 0.9).floor() as usize).min(len - 1)];
            metrics.add_log(format!(
                "prefix_stats single-token lens: min={}, median={}, p90={}, max={}, count={}",
                sorted[0],
                median,
                p90,
                sorted[len - 1],
                len
            ));
        }
    }

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
        // Extra logging to debug early queue exhaustion
        metrics.add_log(format!(
            "Gen {} start: work_len={}, landing={}, best_score=(v={}, var={}/{}, f={}), prev_new_work={}",
            generation,
            work_len,
            store.total_landing_size(),
            best_score.volume,
            best_score.variance_num,
            best_score.variance_den,
            best_score.fullness,
            prev_new_work
                .map(|v| v.to_string())
                .unwrap_or_else(|| "none".to_string())
        ));
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
                metrics.record_optimal_volume(cache_score.volume);
                if cache_score > best_score {
                    best_ortho = cache_ortho;
                    best_score = cache_score;
                    optimal_dirty = true;
                    update_optimal_metrics(&best_ortho);
                }
            }
        }

        metrics.add_log(format!(
            "Generation {}: processing {} work items",
            generation, work_len
        ));

        metrics.reset_prune_counts();
        let mut gen_processed = 0u64;
        let mut pop_work_calls = 0u64;
        let mut hot_deltas = OperationDeltas::default();
        let mut completion_ctx = CompletionContext::default();
        let mut completion_bits = FixedBitSet::with_capacity(interner.vocab_size());
        completion_bits.grow(interner.vocab_size());
        let mut completion_chunk = Vec::with_capacity(COMPLETION_CHUNK_SIZE);

        let gen_start = Instant::now();
        let accepted_before = store.seen_len_accepted();
        last_housekeeping = Instant::now();

        // Process all work in this generation
        while let Some(ortho) = store.pop_work()? {
            pop_work_calls += 1;
            if should_quit() {
                break;
            }
            gen_processed += 1;
            total_processed += 1;

            // Periodic updates on a time cadence
            if last_housekeeping.elapsed().as_millis() >= 1000 {
                flush_operation_deltas(metrics, &mut hot_deltas);
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
                let proc_rss_bytes = memory_safety::current_process_rss_bytes();
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

                // Disk usage for base dir, if known
                if let Some(base_dir) = &disk_base {
                    disks.refresh_list();
                    disks.refresh();
                    let mut best: Option<(u64, u64, usize)> = None;
                    for disk in disks.iter() {
                        let mount = disk.mount_point();
                        if base_dir.starts_with(mount) {
                            let score = mount.as_os_str().to_string_lossy().len();
                            if best.map_or(true, |(_, _, best_len)| score > best_len) {
                                best = Some((disk.total_space(), disk.available_space(), score));
                            }
                        }
                    }
                    if let Some((total, available, _)) = best {
                        metrics.set_disk_usage(total, available);
                    }
                }

                // Compression stats
                let comp = store.compression_stats();
                metrics.set_compression_bytes(comp.uncompressed_bytes, comp.compressed_bytes);

                // Housekeeping hook (heartbeats, mem claim, leader lock)
                housekeeping()?;

                // Pressure watchdog: drain/compact/offload under landing/disk pressure.
                let _ = pressure_watchdog.maybe_handle(store, cfg, metrics)?;
                reclaim_pending = disk_safety::reclaim_required(
                    store.processing_reclaim_reservation_bytes(),
                )?
                .is_some();
                if reclaim_pending
                    && maybe_run_processing_reclaim_safe_point(
                        store,
                        metrics,
                        "processing housekeeping",
                    )?
                {
                    reclaim_pending = false;
                }
            }

            completion_ctx.reset(&ortho);

            // Get completions from interner
            interner.intersect_into(
                completion_ctx.required_usize(),
                completion_ctx.forbidden_usize(),
                &mut completion_bits,
            );
            let total_completions = completion_bits.count_ones(..);
            if total_completions > FANOUT_LOG_THRESHOLD {
                let chunks =
                    (total_completions + COMPLETION_CHUNK_SIZE - 1) / COMPLETION_CHUNK_SIZE;
                metrics.add_log(format!(
                    "Fanout: {} completions; processing in {} chunks of {}",
                    total_completions, chunks, COMPLETION_CHUNK_SIZE
                ));
            }

            // Generate child orthos and record results
            completion_chunk.clear();
            for completion in completion_bits.ones() {
                completion_chunk.push(completion);
                if completion_chunk.len() < COMPLETION_CHUNK_SIZE {
                    continue;
                }
                reclaim_pending = reclaim_pending
                    || disk_safety::reclaim_required(store.processing_reclaim_reservation_bytes())?
                        .is_some();
                if reclaim_pending
                    && maybe_run_processing_reclaim_safe_point(
                        store,
                        metrics,
                        "processing completion chunk",
                    )?
                {
                    reclaim_pending = false;
                }

                for &completion in &completion_chunk {
                    if bound_completion_ctx(&mut completion_ctx, completion, interner, best_score) {
                        hot_deltas.pruned_completions =
                            hot_deltas.pruned_completions.saturating_add(1);
                        if completion_ctx.is_root() {
                            // Root span prune
                            hot_deltas.pruned_root_span =
                                hot_deltas.pruned_root_span.saturating_add(1);
                        } else {
                            hot_deltas.pruned_bound =
                                hot_deltas.pruned_bound.saturating_add(1);
                        }
                        continue;
                    }
                    hot_deltas.expanded_completions =
                        hot_deltas.expanded_completions.saturating_add(1);
                    let completion_val =
                        PayloadVal::try_from(completion).expect("completion overflowed u32");
                    let children = ortho.add(completion_val);
                    for child in children {
                        let candidate_score = child.score();
                        if candidate_score > best_score {
                            best_ortho = child.clone();
                            best_score = candidate_score;
                            update_optimal_metrics(&best_ortho);
                        }
                        if candidate_score > global_score {
                            global_score = candidate_score;
                            optimal_dirty = true;
                        }

                        // Record result to landing zone
                        store.record_result_with_threshold(&child, cfg.landing_flush_threshold)?;

                        // Increment new orthos counter for each generated ortho
                        hot_deltas.new_orthos = hot_deltas.new_orthos.saturating_add(1);
                    }
                }
                flush_operation_deltas(metrics, &mut hot_deltas);
                completion_chunk.clear();
            }

            if !completion_chunk.is_empty() {
                reclaim_pending = reclaim_pending
                    || disk_safety::reclaim_required(store.processing_reclaim_reservation_bytes())?
                        .is_some();
                if reclaim_pending
                    && maybe_run_processing_reclaim_safe_point(
                        store,
                        metrics,
                        "processing completion chunk",
                    )?
                {
                    reclaim_pending = false;
                }

                for &completion in &completion_chunk {
                    if bound_completion_ctx(&mut completion_ctx, completion, interner, best_score) {
                        hot_deltas.pruned_completions =
                            hot_deltas.pruned_completions.saturating_add(1);
                        if completion_ctx.is_root() {
                            hot_deltas.pruned_root_span =
                                hot_deltas.pruned_root_span.saturating_add(1);
                        } else {
                            hot_deltas.pruned_bound =
                                hot_deltas.pruned_bound.saturating_add(1);
                        }
                        continue;
                    }
                    hot_deltas.expanded_completions =
                        hot_deltas.expanded_completions.saturating_add(1);
                    let completion_val =
                        PayloadVal::try_from(completion).expect("completion overflowed u32");
                    let children = ortho.add(completion_val);
                    for child in children {
                        let candidate_score = child.score();
                        if candidate_score > best_score {
                            best_ortho = child.clone();
                            best_score = candidate_score;
                            update_optimal_metrics(&best_ortho);
                        }
                        if candidate_score > global_score {
                            global_score = candidate_score;
                            optimal_dirty = true;
                        }

                        store.record_result_with_threshold(&child, cfg.landing_flush_threshold)?;
                        hot_deltas.new_orthos = hot_deltas.new_orthos.saturating_add(1);
                    }
                }
                flush_operation_deltas(metrics, &mut hot_deltas);
            }
        }

        flush_operation_deltas(metrics, &mut hot_deltas);

        metrics.add_log(format!(
            "Generation {}: processed={}, pop_work_calls={}",
            generation, gen_processed, pop_work_calls
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
            "Generation {} complete: processed={}, pop_work_calls={}, accepted_delta={}, new_work={}, work_len_after={}, total_seen={}",
            generation,
            gen_processed,
            pop_work_calls,
            accepted_delta,
            new_work,
            store.work_len(),
            store.seen_len_accepted()
        ));
        prev_new_work = Some(new_work);
        let (pruned, expanded, pruned_root_span, pruned_bound) = metrics.take_prune_counts();
        metrics.record_prune_sample(generation, pruned, expanded, pruned_root_span, pruned_bound);

        generation += 1;
        if new_work == 0 {
            metrics.add_log("No new work after transition; stopping generations".to_string());
            break;
        }
    }

    crate::generation_store::set_offload_metrics_handle(None);

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

#[cfg(test)]
mod pressure_watchdog_tests {
    use super::*;
    use crate::generation_store::{RunOffloader, set_run_offloader};
    use std::io;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    struct RecordingOffloader {
        uploads: Arc<Mutex<usize>>,
    }

    impl RunOffloader for RecordingOffloader {
        fn offload(&self, _path: &Path) -> io::Result<bool> {
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
    fn pressure_watchdog_drains_and_offloads() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let uploads: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let offloader = Arc::new(RecordingOffloader {
            uploads: uploads.clone(),
        });
        let _guard = OffloaderGuard;
        set_run_offloader(Some(offloader));

        let mut store = GenerationStore::new_with_config(base_path.clone(), 2).unwrap();
        let cfg = Config::test_config(256 * 1024, 8);
        store.configure(&cfg);

        // Add a couple of orthos to create landing data
        let ortho = Ortho::new();
        store.record_result(&ortho).unwrap();
        store.record_result(&ortho).unwrap();
        store.flush_all().unwrap();

        let watchdog = PressureWatchdog::from_config(OffloadConfig {
            enabled: true,
            landing_bytes_high_water: Some(1),
            disk_free_low_water: None,
            ..OffloadConfig::with_base_dir(base_path.clone())
        });
        let metrics = Metrics::new();
        let triggered = watchdog.maybe_handle(&mut store, &cfg, &metrics).unwrap();
        assert!(triggered);
        assert_eq!(store.total_landing_size(), 0);
    }
}
