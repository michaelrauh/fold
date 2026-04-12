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
use rayon::{ThreadPool, ThreadPoolBuilder, prelude::*};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const COMPLETION_CHUNK_SIZE: usize = 1_000;
pub const FANOUT_LOG_THRESHOLD: usize = COMPLETION_CHUNK_SIZE;
const BUCKET_METRICS_UPDATE_INTERVAL: Duration = Duration::from_secs(15);
const PRESSURE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const PRESSURE_LANDING_GROWTH_BYTES: u64 = 64 * 1024 * 1024;
const JOB_COUNT_UPDATE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct PressureCheckState {
    last_full_check: Option<Instant>,
    last_landing_bytes: u64,
    force_next: bool,
}

impl PressureCheckState {
    pub fn new() -> Self {
        Self {
            last_full_check: None,
            last_landing_bytes: 0,
            force_next: false,
        }
    }

    fn should_run_now(&self, offload_cfg: &OffloadConfig, landing_bytes: u64) -> bool {
        if self.force_next {
            return true;
        }
        if landing_pressure_triggered(offload_cfg, landing_bytes) {
            return true;
        }
        if self
            .last_full_check
            .map(|instant| instant.elapsed() >= PRESSURE_CHECK_INTERVAL)
            .unwrap_or(true)
        {
            return true;
        }
        landing_bytes.abs_diff(self.last_landing_bytes) >= PRESSURE_LANDING_GROWTH_BYTES
    }

    fn note_full_check_complete(&mut self, landing_bytes: u64) {
        self.last_full_check = Some(Instant::now());
        self.last_landing_bytes = landing_bytes;
        self.force_next = false;
    }

    fn force_next_check(&mut self) {
        self.force_next = true;
    }
}

impl Default for PressureCheckState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct JobCountCache {
    cached_count: usize,
    last_refresh: Option<Instant>,
}

fn landing_pressure_triggered(offload_cfg: &OffloadConfig, landing_bytes: u64) -> bool {
    offload_cfg.enabled
        && offload_cfg
            .landing_bytes_high_water
            .map(|threshold| landing_bytes >= threshold)
            .unwrap_or(false)
}

fn record_landing_metrics(metrics: &Metrics, store: &GenerationStore) {
    metrics.record_landing_buffer_count(store.total_landing_size());
    metrics.record_landing_buffer_bytes(store.total_landing_bytes());
}

pub fn maybe_run_pressure_safe_point(
    pressure_state: &mut PressureCheckState,
    store: &mut GenerationStore,
    metrics: &Metrics,
    offload_cfg: &OffloadConfig,
    reason: &str,
) -> Result<bool, FoldError> {
    let landing_bytes = store.total_landing_bytes();
    if !pressure_state.should_run_now(offload_cfg, landing_bytes) {
        return Ok(false);
    }
    let reservation = store.processing_reclaim_reservation_bytes();
    let landing_pressure = landing_pressure_triggered(offload_cfg, landing_bytes);
    let ram_pressure = memory_safety::should_spill_to_disk();
    let reclaim_targets = disk_safety::reclaim_required(reservation)?;
    if !landing_pressure && !ram_pressure && reclaim_targets.is_none() {
        pressure_state.note_full_check_complete(landing_bytes);
        return Ok(false);
    }
    metrics.record_pressure_trigger();
    metrics.add_log(format!(
        "Pressure safe point: reason={}, landing_bytes={}, reservation_bytes={}, landing_pressure={}, ram_pressure={}, disk_pressure={}",
        reason,
        landing_bytes,
        reservation,
        landing_pressure,
        ram_pressure,
        reclaim_targets.is_some()
    ));
    let prepare = store.prepare_for_reclaim()?;
    metrics.add_log(format!(
        "Pressure local seal: reason={}, landing_bytes_before={}, landing_bytes_after={}, buckets_drained={}, spill_runs_created={}, spill_bytes_created={}, work_cache_spilled={}",
        reason,
        prepare.landing_bytes_before,
        prepare.landing_bytes_after_local_spill,
        prepare.buckets_drained,
        prepare.spill_runs_created,
        prepare.spill_bytes_created,
        prepare.work_cache_spilled
    ));
    if let Some(targets) = reclaim_targets {
        disk_safety::run_reclaim_to_target(targets, reason)?;
    }
    store.flush_store_metadata(reason)?;
    record_landing_metrics(metrics, store);
    update_bucket_metrics(metrics, store);
    let landing_after = store.total_landing_bytes();
    pressure_state.note_full_check_complete(landing_after);
    pressure_state.force_next_check();
    metrics.add_log(format!(
        "Pressure safe point complete: reason={}, landing_bytes={}, reservation_bytes={}",
        reason, landing_after, reservation
    ));
    Ok(true)
}

fn flush_operation_deltas(metrics: &Metrics, deltas: &mut OperationDeltas) {
    metrics.apply_operation_deltas(*deltas);
    *deltas = OperationDeltas::default();
}

fn update_bucket_metrics(metrics: &Metrics, store: &GenerationStore) {
    let bucket_metrics: Vec<_> = store
        .bucket_stats()
        .into_iter()
        .map(|bs| crate::metrics::BucketMetrics {
            bucket_id: bs.bucket_id,
            run_count: bs.run_count,
            landing_size: bs.landing_size,
            landing_bytes: bs.landing_bytes,
            history_size_estimate: bs.history_size_estimate,
            state: crate::metrics::BucketState::Pending,
            new_work: 0,
        })
        .collect();
    metrics.update_bucket_metrics(bucket_metrics);
}

fn update_bucket_metrics_if_due(
    metrics: &Metrics,
    store: &GenerationStore,
    last_bucket_metrics: &mut Instant,
    force: bool,
) {
    if force || last_bucket_metrics.elapsed() >= BUCKET_METRICS_UPDATE_INTERVAL {
        update_bucket_metrics(metrics, store);
        *last_bucket_metrics = Instant::now();
    }
}

fn update_optimal_metrics(metrics: &Metrics, interner: &Interner, ortho: &Ortho) {
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
}

fn cached_jobs_count(state_config: Option<&StateConfig>, cache: &mut JobCountCache) -> usize {
    let Some(cfg) = state_config else {
        cache.cached_count = 0;
        cache.last_refresh = None;
        return 0;
    };
    let needs_refresh = cache
        .last_refresh
        .map(|instant| instant.elapsed() >= JOB_COUNT_UPDATE_INTERVAL)
        .unwrap_or(true);
    if needs_refresh {
        cache.cached_count =
            crate::file_handler::count_running_jobs_with_config(cfg).unwrap_or(cache.cached_count);
        cache.last_refresh = Some(Instant::now());
    }
    cache.cached_count
}

fn update_global_resource_metrics(
    metrics: &Metrics,
    sys: &mut sysinfo::System,
    state_config: Option<&StateConfig>,
    jobs_cache: &mut JobCountCache,
) {
    sys.refresh_memory();
    let (used_bytes, total_bytes) = normalize_sysinfo_mem(sys.total_memory(), sys.used_memory());
    let proc_rss_bytes = memory_safety::current_process_rss_bytes();
    let percent = if total_bytes > 0 {
        ((used_bytes as f64 / total_bytes as f64) * 100.0).round() as usize
    } else {
        0
    };
    let jobs_count = cached_jobs_count(state_config, jobs_cache);
    metrics.update_global(|g| {
        g.ram_bytes = used_bytes;
        g.process_rss_bytes = proc_rss_bytes;
        g.system_memory_percent = percent;
        g.distinct_jobs_count = jobs_count;
    });

    if let Some(cfg) = state_config {
        if let Ok(snapshot) = disk_safety::disk_usage_snapshot_for_metrics(&cfg.base_dir) {
            metrics.set_disk_usage(snapshot.total_bytes, snapshot.available_bytes);
        }
    }
}

pub fn default_merge_threads() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get().saturating_sub(1).max(1))
        .unwrap_or(1)
}

pub fn merge_threads_from_env() -> usize {
    std::env::var("FOLD_MERGE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or_else(default_merge_threads)
}

fn build_merge_thread_pool(merge_threads: usize) -> Result<Option<ThreadPool>, FoldError> {
    if merge_threads <= 1 {
        return Ok(None);
    }
    ThreadPoolBuilder::new()
        .num_threads(merge_threads)
        .thread_name(|idx| format!("fold-merge-{}", idx))
        .build()
        .map(Some)
        .map_err(|err| FoldError::Other(format!("failed to build merge thread pool: {}", err)))
}

type ChildBatch = Vec<Vec<(Ortho, OrthoScore)>>;
type ExpandedBatches = Vec<ChildBatch>;

fn materialize_child_batches(
    ortho: &Ortho,
    completion_batches: &[Vec<usize>],
    thread_pool: Option<&ThreadPool>,
) -> ExpandedBatches {
    let expand_batch = |batch: &Vec<usize>| -> Vec<Vec<(Ortho, OrthoScore)>> {
        batch
            .iter()
            .map(|&completion| {
                let completion_val =
                    PayloadVal::try_from(completion).expect("completion overflowed u32");
                ortho
                    .add(completion_val)
                    .into_iter()
                    .map(|child| {
                        let score = child.score();
                        (child, score)
                    })
                    .collect()
            })
            .collect()
    };

    if let Some(pool) = thread_pool.filter(|_| completion_batches.len() > 1) {
        pool.install(|| completion_batches.par_iter().map(expand_batch).collect())
    } else {
        completion_batches.iter().map(expand_batch).collect()
    }
}

pub struct GenerationRunResult {
    pub best_ortho: Ortho,
    pub best_score: OrthoScore,
    pub generation_stats: Vec<GenerationStat>,
    pub total_processed: u64,
    pub optimal_dirty: bool,
}

pub struct MergeGenerationStepResult {
    pub best_ortho: Ortho,
    pub best_score: OrthoScore,
    pub generation_stat: GenerationStat,
    pub processed: u64,
    pub optimal_dirty: bool,
    pub quiesced: bool,
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
    record_landing_metrics(metrics, store);

    // Check cache for initial optimal ortho
    if let Some(cache_ortho) = store.peek_best_ortho_in_cache() {
        metrics.record_optimal_volume(cache_ortho.volume());
    }

    metrics.set_operation_status("Processing orthos".to_string());
    metrics.reset_prune_counts();

    let mut sys = sysinfo::System::new();
    let mut jobs_cache = JobCountCache::default();
    let mut generation = 0u64;
    let mut generation_stats: Vec<GenerationStat> = Vec::new();
    let offload_cfg = state_config
        .map(|cfg| OffloadConfig::from_env_with_base(&cfg.base_dir))
        .unwrap_or_else(OffloadConfig::from_env);
    let mut prev_new_work: Option<u64> = None;
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
        record_landing_metrics(metrics, store);

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
        let mut last_bucket_metrics = Instant::now();
        let mut pressure_state = PressureCheckState::new();
        update_bucket_metrics_if_due(metrics, store, &mut last_bucket_metrics, true);

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
                record_landing_metrics(metrics, store);

                update_bucket_metrics_if_due(metrics, store, &mut last_bucket_metrics, false);

                // System metrics (RAM)
                update_global_resource_metrics(metrics, &mut sys, state_config, &mut jobs_cache);

                // Compression stats
                let comp = store.compression_stats();
                metrics.set_compression_bytes(comp.uncompressed_bytes, comp.compressed_bytes);

                // Housekeeping hook (heartbeats, mem claim, leader lock)
                housekeeping()?;

                let _ = maybe_run_pressure_safe_point(
                    &mut pressure_state,
                    store,
                    metrics,
                    &offload_cfg,
                    "processing housekeeping",
                )?;
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
                let _ = maybe_run_pressure_safe_point(
                    &mut pressure_state,
                    store,
                    metrics,
                    &offload_cfg,
                    "processing completion chunk",
                )?;

                for &completion in &completion_chunk {
                    if bound_completion_ctx(&mut completion_ctx, completion, interner, best_score) {
                        hot_deltas.pruned_completions =
                            hot_deltas.pruned_completions.saturating_add(1);
                        if completion_ctx.is_root() {
                            // Root span prune
                            hot_deltas.pruned_root_span =
                                hot_deltas.pruned_root_span.saturating_add(1);
                        } else {
                            hot_deltas.pruned_bound = hot_deltas.pruned_bound.saturating_add(1);
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
                let _ = maybe_run_pressure_safe_point(
                    &mut pressure_state,
                    store,
                    metrics,
                    &offload_cfg,
                    "processing completion chunk",
                )?;

                for &completion in &completion_chunk {
                    if bound_completion_ctx(&mut completion_ctx, completion, interner, best_score) {
                        hot_deltas.pruned_completions =
                            hot_deltas.pruned_completions.saturating_add(1);
                        if completion_ctx.is_root() {
                            hot_deltas.pruned_root_span =
                                hot_deltas.pruned_root_span.saturating_add(1);
                        } else {
                            hot_deltas.pruned_bound = hot_deltas.pruned_bound.saturating_add(1);
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
        record_landing_metrics(metrics, store);

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

pub fn run_merge_generation_loop<FHousekeeping>(
    interner: &Interner,
    store: &mut GenerationStore,
    cfg: &Config,
    metrics: &Metrics,
    mut housekeeping: FHousekeeping,
    mut progress_cb_factory: impl FnMut(u64) -> Option<ProgressCallback>,
    merge_threads: usize,
    state_config: Option<&StateConfig>,
) -> Result<GenerationRunResult, FoldError>
where
    FHousekeeping: FnMut() -> Result<(), FoldError>,
{
    let mut best_ortho = Ortho::new();
    let mut best_score = best_ortho.score();
    let mut generation_stats: Vec<GenerationStat> = Vec::new();
    let mut total_processed = 0u64;
    let mut optimal_dirty = false;

    let mut generation = 0u64;
    while store.work_len() > 0 {
        let step = run_single_merge_generation(
            interner,
            store,
            cfg,
            metrics,
            &mut housekeeping,
            progress_cb_factory(generation),
            merge_threads,
            state_config,
            generation,
            best_ortho,
            best_score,
        )?;
        best_ortho = step.best_ortho;
        best_score = step.best_score;
        total_processed = total_processed.saturating_add(step.processed);
        optimal_dirty |= step.optimal_dirty;
        generation_stats.push(step.generation_stat);
        generation = generation.saturating_add(1);
        if step.quiesced {
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

pub fn run_single_merge_generation<FHousekeeping>(
    interner: &Interner,
    store: &mut GenerationStore,
    cfg: &Config,
    metrics: &Metrics,
    housekeeping: &mut FHousekeeping,
    progress_callback: Option<ProgressCallback>,
    merge_threads: usize,
    state_config: Option<&StateConfig>,
    generation: u64,
    mut best_ortho: Ortho,
    mut best_score: OrthoScore,
) -> Result<MergeGenerationStepResult, FoldError>
where
    FHousekeeping: FnMut() -> Result<(), FoldError>,
{
    let mut optimal_dirty = false;
    let mut total_processed = 0u64;

    let thread_pool = build_merge_thread_pool(merge_threads)?;
    let merge_thread_count = thread_pool.as_ref().map(|_| merge_threads).unwrap_or(1);
    metrics.add_log(format!(
        "Merge threading configured: requested={}, active={}",
        merge_threads, merge_thread_count
    ));

    let mut sys = sysinfo::System::new();
    let mut jobs_cache = JobCountCache::default();
    let offload_cfg = state_config
        .map(|cfg| OffloadConfig::from_env_with_base(&cfg.base_dir))
        .unwrap_or_else(OffloadConfig::from_env);
    let mut pressure_state = PressureCheckState::new();

    metrics.set_operation_status("Processing merge generations".to_string());
    metrics.record_work_len(store.work_len() as usize);
    record_landing_metrics(metrics, store);

    let mut last_report_time = Instant::now();
    let mut last_report_count = 0u64;
    let mut last_housekeeping = Instant::now();
    let mut last_bucket_metrics = Instant::now();
    let work_len = store.work_len();
    if work_len == 0 {
        return Ok(MergeGenerationStepResult {
            best_ortho,
            best_score,
            generation_stat: GenerationStat {
                generation,
                processing_secs: 0.0,
                transition_secs: 0.0,
                accepted: 0,
                new_work: 0,
            },
            processed: 0,
            optimal_dirty: false,
            quiesced: true,
        });
    }

    metrics.update_global(|g| {
        g.generation = generation;
        g.phase = format!("Merge Gen {} Processing", generation);
        g.work_len = work_len;
        g.seen_len_accepted = store.seen_len_accepted();
        g.run_budget_bytes = cfg.run_budget_bytes;
        g.fan_in = cfg.fan_in;
    });
    metrics.update_operation(|op| {
        op.progress_total = work_len as usize;
        op.progress_current = 0;
    });
    metrics.set_operation_status(format!("Processing Merge Gen {}", generation));
    metrics.record_work_len(work_len as usize);
    record_landing_metrics(metrics, store);
    update_bucket_metrics_if_due(metrics, store, &mut last_bucket_metrics, true);

    if let Some(cache_ortho) = store.peek_best_ortho_in_cache() {
        let cache_score = cache_ortho.score();
        if cache_score > best_score {
            best_ortho = cache_ortho;
            best_score = cache_score;
            optimal_dirty = true;
            update_optimal_metrics(metrics, interner, &best_ortho);
        }
    }

    metrics.add_log(format!(
        "Merge Generation {}: processing {} work items",
        generation, work_len
    ));

    metrics.reset_prune_counts();
    let mut gen_processed = 0u64;
    let mut hot_deltas = OperationDeltas::default();
    let mut completion_ctx = CompletionContext::default();
    let mut completion_bits = FixedBitSet::with_capacity(interner.vocab_size());
    completion_bits.grow(interner.vocab_size());
    let mut completion_batches = Vec::new();
    let mut completion_chunk = Vec::with_capacity(COMPLETION_CHUNK_SIZE);

    let gen_start = Instant::now();
    let accepted_before = store.seen_len_accepted();

    while let Some(ortho) = store.pop_work()? {
        gen_processed += 1;
        total_processed += 1;

        if last_housekeeping.elapsed().as_millis() >= 1000 {
            flush_operation_deltas(metrics, &mut hot_deltas);
            metrics.update_operation(|op| {
                op.progress_current = gen_processed as usize;
            });

            let now = Instant::now();
            let elapsed = now.duration_since(last_report_time).as_secs_f64();
            let processed_since_last = total_processed - last_report_count;
            let throughput = if elapsed > 0.0 {
                (processed_since_last as f64 / elapsed) as usize
            } else {
                0
            };
            if elapsed >= 1.0 {
                metrics.update_global(|g| {
                    g.phase = format!("Merge Gen {} Processing ({}/s)", generation, throughput);
                });
                last_report_time = now;
                last_report_count = total_processed;
            }
            last_housekeeping = now;

            metrics.record_optimal_volume(best_ortho.volume());
            metrics.update_global(|g| {
                g.work_len = store.work_len();
                g.seen_len_accepted = store.seen_len_accepted();
            });
            metrics.record_work_len(store.work_len() as usize);
            record_landing_metrics(metrics, store);
            update_bucket_metrics_if_due(metrics, store, &mut last_bucket_metrics, false);

            if optimal_dirty {
                update_optimal_metrics(metrics, interner, &best_ortho);
                optimal_dirty = false;
            }

            update_global_resource_metrics(metrics, &mut sys, state_config, &mut jobs_cache);

            let comp = store.compression_stats();
            metrics.set_compression_bytes(comp.uncompressed_bytes, comp.compressed_bytes);

            housekeeping()?;

            let _ = maybe_run_pressure_safe_point(
                &mut pressure_state,
                store,
                metrics,
                &offload_cfg,
                "merge processing housekeeping",
            )?;
        }

        if total_processed % 50_000 == 0 {
            metrics.add_log(format!(
                "Merge progress: {} orthos processed",
                total_processed
            ));
        }

        if total_processed % 100_000 == 0 {
            housekeeping()?;
        }

        completion_ctx.reset(&ortho);
        interner.intersect_into(
            completion_ctx.required_usize(),
            completion_ctx.forbidden_usize(),
            &mut completion_bits,
        );

        let total_completions = completion_bits.count_ones(..);
        if total_completions > FANOUT_LOG_THRESHOLD {
            let chunks = (total_completions + COMPLETION_CHUNK_SIZE - 1) / COMPLETION_CHUNK_SIZE;
            metrics.add_log(format!(
                "Fanout: {} completions; {} chunks",
                total_completions, chunks
            ));
        }

        completion_batches.clear();
        completion_chunk.clear();
        for completion in completion_bits.ones() {
            completion_chunk.push(completion);
            if completion_chunk.len() == COMPLETION_CHUNK_SIZE {
                completion_batches.push(std::mem::take(&mut completion_chunk));
                completion_chunk = Vec::with_capacity(COMPLETION_CHUNK_SIZE);
            }
        }
        if !completion_chunk.is_empty() {
            completion_batches.push(std::mem::take(&mut completion_chunk));
            completion_chunk = Vec::with_capacity(COMPLETION_CHUNK_SIZE);
        }

        let child_batches =
            materialize_child_batches(&ortho, &completion_batches, thread_pool.as_ref());

        for (batch, expanded_children) in completion_batches.iter().zip(child_batches.into_iter()) {
            let _ = maybe_run_pressure_safe_point(
                &mut pressure_state,
                store,
                metrics,
                &offload_cfg,
                "merge completion batch",
            )?;
            for (&completion, children) in batch.iter().zip(expanded_children.into_iter()) {
                if bound_completion_ctx(&mut completion_ctx, completion, interner, best_score) {
                    hot_deltas.pruned_completions = hot_deltas.pruned_completions.saturating_add(1);
                    if completion_ctx.is_root() {
                        hot_deltas.pruned_root_span = hot_deltas.pruned_root_span.saturating_add(1);
                    } else {
                        hot_deltas.pruned_bound = hot_deltas.pruned_bound.saturating_add(1);
                    }
                    continue;
                }

                hot_deltas.expanded_completions = hot_deltas.expanded_completions.saturating_add(1);
                for (child, candidate_score) in children {
                    if candidate_score > best_score {
                        best_ortho = child.clone();
                        best_score = candidate_score;
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
        "Merge Generation {}: processed {} orthos",
        generation, gen_processed
    ));

    metrics.update_global(|g| {
        g.phase = format!(
            "Merge Gen {} → {} transition starting",
            generation,
            generation + 1
        );
    });
    metrics.set_operation_status(format!(
        "Merge Gen {} → {} transition",
        generation,
        generation + 1
    ));

    let processing_secs = gen_start.elapsed().as_secs_f64();
    let transition_start = Instant::now();
    let new_work = store.on_generation_end(cfg, progress_callback.as_ref())?;
    let transition_secs = transition_start.elapsed().as_secs_f64();
    let accepted_delta = store.seen_len_accepted().saturating_sub(accepted_before);
    let generation_stat = GenerationStat {
        generation,
        processing_secs,
        transition_secs,
        accepted: accepted_delta,
        new_work,
    };

    metrics.update_global(|g| {
        g.work_len = store.work_len();
        g.seen_len_accepted = store.seen_len_accepted();
        g.phase = format!("Merge Gen {} complete", generation);
    });
    metrics.record_work_len(store.work_len() as usize);
    record_landing_metrics(metrics, store);

    metrics.add_log(format!(
        "Merge Generation {} complete: {} new work, {} total seen",
        generation,
        new_work,
        store.seen_len_accepted()
    ));
    let (pruned, expanded, pruned_root_span, pruned_bound) = metrics.take_prune_counts();
    metrics.record_prune_sample(generation, pruned, expanded, pruned_root_span, pruned_bound);

    if new_work == 0 {
        metrics.add_log("No new work after transition; stopping merge generations".to_string());
    }

    if optimal_dirty {
        update_optimal_metrics(metrics, interner, &best_ortho);
    }

    Ok(MergeGenerationStepResult {
        best_ortho,
        best_score,
        generation_stat,
        processed: total_processed,
        optimal_dirty,
        quiesced: new_work == 0,
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
    use crate::disk_safety::debug_exact_disk_probe_count;
    use crate::generation_store::{RunOffloader, set_run_offloader};
    use crate::offload_runtime::configure_offload_runtime;
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

        let offload_cfg = OffloadConfig {
            enabled: true,
            landing_bytes_high_water: Some(1),
            disk_free_low_water: None,
            ..OffloadConfig::with_base_dir(base_path.clone())
        };
        let metrics = Metrics::new();
        let mut pressure_state = PressureCheckState::new();
        let triggered = maybe_run_pressure_safe_point(
            &mut pressure_state,
            &mut store,
            &metrics,
            &offload_cfg,
            "test watchdog",
        )
        .unwrap();
        assert!(triggered);
        assert_eq!(store.total_landing_size(), 0);
    }

    #[test]
    fn no_pressure_safe_point_path_skips_repeated_exact_disk_probes() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let free_now = std::fs::create_dir_all(&base_path)
            .and_then(|_| crate::disk_safety::disk_usage_snapshot_for_metrics(&base_path))
            .unwrap()
            .available_bytes;
        let mut offload_cfg = OffloadConfig::with_base_dir(base_path.clone());
        offload_cfg.enabled = true;
        offload_cfg.in_memory_store = true;
        offload_cfg.cache_dir = base_path.join("offload_cache");
        offload_cfg.disk_hysteresis_margin_bytes = 0;
        offload_cfg.disk_free_low_water = Some(free_now.saturating_sub(512 * 1024 * 1024));

        let _guard = configure_offload_runtime(&base_path, &offload_cfg)
            .unwrap()
            .unwrap();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 2).unwrap();
        store.configure(&Config::test_config(256 * 1024, 8));
        let metrics = Metrics::new();
        let mut pressure_state = PressureCheckState::new();

        assert!(
            !maybe_run_pressure_safe_point(
                &mut pressure_state,
                &mut store,
                &metrics,
                &offload_cfg,
                "test no pressure",
            )
            .unwrap()
        );
        let probes_after_first = debug_exact_disk_probe_count();
        assert!(
            !maybe_run_pressure_safe_point(
                &mut pressure_state,
                &mut store,
                &metrics,
                &offload_cfg,
                "test no pressure",
            )
            .unwrap()
        );
        assert_eq!(debug_exact_disk_probe_count(), probes_after_first);
    }
}

#[cfg(test)]
mod pressure_state_tests {
    use super::*;

    #[test]
    fn pressure_state_skips_repeated_small_growth_checks() {
        let mut state = PressureCheckState::new();
        let offload_cfg = OffloadConfig {
            enabled: true,
            landing_bytes_high_water: Some(PRESSURE_LANDING_GROWTH_BYTES.saturating_mul(4)),
            ..OffloadConfig::default()
        };
        state.note_full_check_complete(1024);
        state.last_full_check = Some(Instant::now());

        assert!(!state.should_run_now(&offload_cfg, 2048));
    }

    #[test]
    fn pressure_state_triggers_on_growth_or_high_water() {
        let mut state = PressureCheckState::new();
        state.note_full_check_complete(0);
        state.last_full_check = Some(Instant::now());

        let growth_cfg = OffloadConfig {
            enabled: true,
            landing_bytes_high_water: Some(PRESSURE_LANDING_GROWTH_BYTES.saturating_mul(4)),
            ..OffloadConfig::default()
        };
        assert!(state.should_run_now(&growth_cfg, PRESSURE_LANDING_GROWTH_BYTES));

        let high_water_cfg = OffloadConfig {
            enabled: true,
            landing_bytes_high_water: Some(1),
            ..OffloadConfig::default()
        };
        state.note_full_check_complete(0);
        state.last_full_check = Some(Instant::now());
        assert!(state.should_run_now(&high_water_cfg, 1));
    }
}

#[cfg(test)]
mod merge_thread_tests {
    use super::*;

    #[test]
    fn merge_threads_default_is_at_least_one() {
        assert!(default_merge_threads() >= 1);
    }

    #[test]
    fn merge_threads_env_override_is_respected() {
        let previous = std::env::var("FOLD_MERGE_THREADS").ok();
        unsafe {
            std::env::set_var("FOLD_MERGE_THREADS", "7");
        }
        assert_eq!(merge_threads_from_env(), 7);
        unsafe {
            match previous {
                Some(value) => std::env::set_var("FOLD_MERGE_THREADS", value),
                None => std::env::remove_var("FOLD_MERGE_THREADS"),
            }
        }
    }

    #[test]
    fn merge_threads_invalid_override_falls_back() {
        let previous = std::env::var("FOLD_MERGE_THREADS").ok();
        unsafe {
            std::env::set_var("FOLD_MERGE_THREADS", "0");
        }
        assert_eq!(merge_threads_from_env(), default_merge_threads());
        unsafe {
            std::env::set_var("FOLD_MERGE_THREADS", "invalid");
        }
        assert_eq!(merge_threads_from_env(), default_merge_threads());
        unsafe {
            match previous {
                Some(value) => std::env::set_var("FOLD_MERGE_THREADS", value),
                None => std::env::remove_var("FOLD_MERGE_THREADS"),
            }
        }
    }
}
