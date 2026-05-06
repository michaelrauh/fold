use crate::{
    FoldError,
    dfs_checkpoint::{file_name_string, write_atomic},
    dfs_runner::{BranchOrdering, DfsRunner, SearchToggles, StepScratch},
    interner::Interner,
    metrics::{Metrics, WorkerMetrics},
    ortho::{Ortho, OrthoScore},
};
use bytecheck::CheckBytes;
use rkyv::{Archive, Deserialize, Serialize};
use rustc_hash::FxHashMap;
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WORKER_UPDATE_STEPS: usize = 2048;
const METRICS_UPDATE_MS: u64 = 500;
const FRONTIER_BUCKETS: usize = 48;
const PARALLEL_CHECKPOINT_VERSION: u32 = 2;

#[derive(Clone, Debug)]
pub struct ParallelSearchConfig {
    pub enabled: bool,
    pub workers: usize,
    pub hunt_nodes: u64,
    pub shard_depth: usize,
    pub checkpoint_every_secs: u64,
    pub resume_enabled: bool,
}

impl ParallelSearchConfig {
    pub fn default_runtime() -> Self {
        let default_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self {
            enabled: true,
            workers: default_workers,
            hunt_nodes: 5_000_000_000,
            shard_depth: 3,
            checkpoint_every_secs: 30,
            resume_enabled: true,
        }
    }

    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = rustc_hash::FxHasher::default();
        self.hunt_nodes.hash(&mut hasher);
        self.shard_depth.hash(&mut hasher);
        hasher.finish()
    }

    pub fn hunt_toggles(&self) -> SearchToggles {
        SearchToggles {
            branch_ordering: BranchOrdering::WorstFirst,
            completion_pruning: false,
            ..SearchToggles::default()
        }
    }

    pub fn proof_toggles(&self) -> SearchToggles {
        SearchToggles {
            completion_pruning: true,
            ..SearchToggles::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchPhase {
    Hunt,
    Proof,
}

impl SearchPhase {
    fn label(self) -> &'static str {
        match self {
            SearchPhase::Hunt => "Hunt",
            SearchPhase::Proof => "Proof",
        }
    }
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
struct ShardTask {
    id: usize,
    bucket: usize,
    ancestors: Vec<u64>,
    runner: DfsRunner,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
struct RunningShardCheckpoint {
    worker_id: usize,
    shard: ShardTask,
}

#[derive(Clone, Debug, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
pub struct ParallelCheckpointState {
    input_path: String,
    input_fingerprint: u64,
    config_fingerprint: u64,
    pending_shards: Vec<ShardTask>,
    running_shards: Vec<RunningShardCheckpoint>,
    completed_depths: CompletedDepths,
    best_incumbent: Ortho,
    last_improvement_unix: u64,
    last_improvement_depth: usize,
    nodes_expanded: u64,
    nodes_pruned: u64,
    completions_pruned: u64,
    hunt_expanded: u64,
    total_shards: usize,
    shards_done: usize,
    started_unix: u64,
    shallow_seen: Vec<u64>,
    level_completions: Vec<usize>,
}

#[derive(Clone, Debug, SerdeSerialize, SerdeDeserialize)]
pub struct ParallelCheckpointManifest {
    version: u32,
    input_path: String,
    input_fingerprint: u64,
    config_fingerprint: u64,
    state_path: String,
    checkpoint_unix: u64,
    checkpoint_status: String,
    nodes_expanded: u64,
    nodes_pruned: u64,
    completions_pruned: u64,
    hunt_expanded: u64,
    pending_shards: usize,
    running_shards: usize,
    shards_done: usize,
    total_shards: usize,
    incumbent_volume: usize,
    incumbent_fullness: usize,
}

#[derive(Clone, Debug)]
pub struct ParallelCheckpointStore {
    manifest_path: PathBuf,
    state_path: PathBuf,
    output_dir: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct WorkerState {
    active: bool,
    phase: String,
    shard_id: Option<usize>,
    bucket: Option<usize>,
    depth: usize,
    max_depth: usize,
    nodes_expanded: u64,
    nodes_pruned: u64,
    completions_pruned: u64,
    seen_by_depth: Vec<u64>,
    descended_by_depth: Vec<u64>,
    pruned_by_depth: Vec<u64>,
    current_bound: Option<OrthoScore>,
    rate: f64,
}

#[derive(Clone, Debug, Default, Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
struct CompletedDepths {
    seen_by_depth: Vec<u64>,
    descended_by_depth: Vec<u64>,
    pruned_by_depth: Vec<u64>,
}

#[derive(Debug)]
struct SharedBest {
    incumbent: Ortho,
    last_improvement_unix: u64,
    last_improvement_depth: usize,
}

#[derive(Debug)]
struct SharedState {
    queue: Mutex<VecDeque<ShardTask>>,
    workers: Mutex<Vec<WorkerState>>,
    completed_depths: Mutex<CompletedDepths>,
    best: Mutex<SharedBest>,
    checkpoint_snapshots: Mutex<Vec<RunningShardCheckpoint>>,
    checkpoint_status: Mutex<String>,
    total_expanded: AtomicU64,
    total_pruned: AtomicU64,
    total_completion_pruned: AtomicU64,
    hunt_expanded: AtomicU64,
    checkpoint_request: AtomicU64,
    checkpoint_release: AtomicU64,
    checkpoint_acks: AtomicUsize,
    checkpoint_time: AtomicU64,
    checkpoint_saving: AtomicBool,
    shards_running: AtomicUsize,
    shards_done: AtomicUsize,
    workers_active: AtomicUsize,
    total_shards: usize,
    started_unix: u64,
    finished: AtomicBool,
    shallow_seen: Vec<u64>,
    ancestor_total: Vec<FxHashMap<u64, usize>>,
    ancestor_done: Mutex<Vec<FxHashMap<u64, usize>>>,
    level_completions_base: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct ParallelSearchResult {
    pub best: Ortho,
    pub nodes_expanded: u64,
    pub nodes_pruned: u64,
    pub completions_pruned: u64,
    pub finished: bool,
    pub started_unix: u64,
    pub last_improvement_unix: u64,
    pub last_improvement_depth: usize,
    pub total_shards: usize,
    pub shards_done: usize,
}

impl ParallelCheckpointStore {
    pub fn new(root: &Path, output_dir: &Path) -> Result<Self, FoldError> {
        let dir = root.join("checkpoints").join("parallel");
        fs::create_dir_all(&dir)?;
        Ok(Self {
            manifest_path: dir.join("manifest.json"),
            state_path: dir.join("state.bin"),
            output_dir: output_dir.to_path_buf(),
        })
    }

    pub fn load(
        &self,
        input_fingerprint: u64,
        config: &ParallelSearchConfig,
    ) -> Result<Option<ParallelCheckpointState>, FoldError> {
        if !self.manifest_path.exists() || !self.state_path.exists() {
            return Ok(None);
        }
        let manifest_bytes = fs::read(&self.manifest_path)?;
        let manifest: ParallelCheckpointManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| FoldError::Deserialization(e.to_string()))?;
        if manifest.version != PARALLEL_CHECKPOINT_VERSION {
            return Err(FoldError::Other(format!(
                "unsupported parallel checkpoint version {}",
                manifest.version
            )));
        }
        if manifest.input_fingerprint != input_fingerprint {
            return Err(FoldError::Other(
                "parallel checkpoint input fingerprint differs from current input".to_string(),
            ));
        }
        if manifest.config_fingerprint != config.fingerprint() {
            return Err(FoldError::Other(
                "parallel checkpoint config differs from current hunt/shard config".to_string(),
            ));
        }
        let state_bytes = fs::read(&self.state_path)?;
        let state: ParallelCheckpointState = rkyv::from_bytes(&state_bytes)
            .map_err(|e| FoldError::Deserialization(e.to_string()))?;
        Ok(Some(state))
    }

    fn save(
        &self,
        state: &ParallelCheckpointState,
        status: &str,
    ) -> Result<ParallelCheckpointManifest, FoldError> {
        let now = now_unix();
        let state_bytes = rkyv::to_bytes::<_, 4096>(state)
            .map(|buf| buf.to_vec())
            .map_err(|e| FoldError::Serialization(e.to_string()))?;
        write_atomic(&self.state_path, &state_bytes)?;
        let manifest = ParallelCheckpointManifest {
            version: PARALLEL_CHECKPOINT_VERSION,
            input_path: state.input_path.clone(),
            input_fingerprint: state.input_fingerprint,
            config_fingerprint: state.config_fingerprint,
            state_path: file_name_string(&self.state_path),
            checkpoint_unix: now,
            checkpoint_status: status.to_string(),
            nodes_expanded: state.nodes_expanded,
            nodes_pruned: state.nodes_pruned,
            completions_pruned: state.completions_pruned,
            hunt_expanded: state.hunt_expanded,
            pending_shards: state.pending_shards.len(),
            running_shards: state.running_shards.len(),
            shards_done: state.shards_done,
            total_shards: state.total_shards,
            incumbent_volume: state.best_incumbent.score().volume,
            incumbent_fullness: state.best_incumbent.score().fullness,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| FoldError::Serialization(e.to_string()))?;
        write_atomic(&self.manifest_path, &manifest_bytes)?;
        Ok(manifest)
    }

    fn save_progress_output(
        &self,
        state: &ParallelCheckpointState,
        interner: &Interner,
        config: &ParallelSearchConfig,
        status: &str,
    ) -> Result<(), FoldError> {
        let display = format!("{}", state.best_incumbent.display(interner));
        let summary = serde_json::json!({
            "mode": "parallel",
            "status": status,
            "input_path": state.input_path,
            "input_fingerprint": state.input_fingerprint,
            "config_fingerprint": state.config_fingerprint,
            "started_unix": state.started_unix,
            "nodes_expanded": state.nodes_expanded,
            "nodes_pruned": state.nodes_pruned,
            "completions_pruned": state.completions_pruned,
            "hunt_expanded": state.hunt_expanded,
            "pending_shards": state.pending_shards.len(),
            "running_shards": state.running_shards.len(),
            "shards_done": state.shards_done,
            "total_shards": state.total_shards,
            "workers": config.workers,
            "hunt_nodes": config.hunt_nodes,
            "shard_depth": config.shard_depth,
            "checkpoint_every_secs": config.checkpoint_every_secs,
            "best_score": {
                "volume": state.best_incumbent.score().volume,
                "variance_num": state.best_incumbent.score().variance_num,
                "variance_den": state.best_incumbent.score().variance_den,
                "fullness": state.best_incumbent.score().fullness,
            },
            "best_dims": state.best_incumbent.dims(),
            "best_capacity": state.best_incumbent.payload().len(),
        });
        write_atomic(
            &self.output_dir.join("optimal.bin"),
            &state.best_incumbent.to_bytes()?,
        )?;
        write_atomic(&self.output_dir.join("optimal.txt"), display.as_bytes())?;
        let summary_bytes = serde_json::to_vec_pretty(&summary)
            .map_err(|e| FoldError::Serialization(e.to_string()))?;
        write_atomic(&self.output_dir.join("summary.json"), &summary_bytes)
    }
}

pub fn run_parallel_search(
    interner: Arc<Interner>,
    metrics: Metrics,
    should_quit: Arc<AtomicBool>,
    config: ParallelSearchConfig,
    input_path: String,
    input_fingerprint: u64,
    checkpoint_store: ParallelCheckpointStore,
    resume_state: Option<ParallelCheckpointState>,
) -> Result<ParallelSearchResult, FoldError> {
    let (
        queue,
        completed_depths,
        best,
        total_expanded,
        total_pruned,
        total_completion_pruned,
        hunt_expanded,
        shards_done,
        total_shards,
        started_unix,
        shallow_seen,
        ancestor_total,
        level_completions_base,
    ) = match resume_state {
        Some(state) => {
            let mut tasks: Vec<ShardTask> =
                Vec::with_capacity(state.pending_shards.len() + state.running_shards.len());
            for shard in state.pending_shards {
                tasks.push(shard);
            }
            for running in state.running_shards {
                tasks.push(running.shard);
            }
            tasks.sort_by_key(|t| t.runner.top_frame_bound());
            let total_for_bucket = tasks.len();
            for (sorted_idx, task) in tasks.iter_mut().enumerate() {
                task.bucket = bucket_for(sorted_idx, total_for_bucket);
            }
            let ancestor_total = build_ancestor_total(&tasks);
            let queue: VecDeque<ShardTask> = tasks.into();
            (
                queue,
                state.completed_depths,
                SharedBest {
                    incumbent: state.best_incumbent,
                    last_improvement_unix: state.last_improvement_unix,
                    last_improvement_depth: state.last_improvement_depth,
                },
                state.nodes_expanded,
                state.nodes_pruned,
                state.completions_pruned,
                state.hunt_expanded,
                state.shards_done,
                state.total_shards,
                state.started_unix,
                state.shallow_seen,
                ancestor_total,
                state.level_completions,
            )
        }
        None => {
            let mut shard_toggles = config.hunt_toggles();
            shard_toggles.node_pruning = false;
            let (shard_stacks, shard_ancestors, gen_seen) =
                DfsRunner::frontier_shards(&interner, config.shard_depth, &shard_toggles)?;
            if shard_stacks.is_empty() {
                return Err(FoldError::Other(
                    "parallel shard generation produced no work".to_string(),
                ));
            }
            let total_shards = shard_stacks.len();
            let mut tasks: Vec<ShardTask> = shard_stacks
                .into_iter()
                .zip(shard_ancestors)
                .enumerate()
                .map(|(id, (stack, ancestors))| ShardTask {
                    id,
                    bucket: 0,
                    ancestors,
                    runner: DfsRunner::from_stack(stack, Ortho::new()),
                })
                .collect();
            tasks.sort_by_key(|t| t.runner.top_frame_bound());
            let total_for_bucket = tasks.len();
            for (sorted_idx, task) in tasks.iter_mut().enumerate() {
                task.bucket = bucket_for(sorted_idx, total_for_bucket);
            }
            let ancestor_total = build_ancestor_total(&tasks);
            let queue: VecDeque<ShardTask> = tasks.into();
            let started_unix = now_unix();
            (
                queue,
                CompletedDepths::default(),
                SharedBest {
                    incumbent: Ortho::new(),
                    last_improvement_unix: started_unix,
                    last_improvement_depth: 1,
                },
                0,
                0,
                0,
                0,
                0,
                total_shards,
                started_unix,
                gen_seen,
                ancestor_total,
                Vec::new(),
            )
        }
    };
    let shared = Arc::new(SharedState {
        queue: Mutex::new(queue),
        workers: Mutex::new(vec![WorkerState::default(); config.workers]),
        completed_depths: Mutex::new(completed_depths),
        best: Mutex::new(best),
        checkpoint_snapshots: Mutex::new(Vec::new()),
        checkpoint_status: Mutex::new("Not yet checkpointed".to_string()),
        total_expanded: AtomicU64::new(total_expanded),
        total_pruned: AtomicU64::new(total_pruned),
        total_completion_pruned: AtomicU64::new(total_completion_pruned),
        hunt_expanded: AtomicU64::new(hunt_expanded),
        checkpoint_request: AtomicU64::new(0),
        checkpoint_release: AtomicU64::new(0),
        checkpoint_acks: AtomicUsize::new(0),
        checkpoint_time: AtomicU64::new(0),
        checkpoint_saving: AtomicBool::new(false),
        shards_running: AtomicUsize::new(0),
        shards_done: AtomicUsize::new(shards_done),
        workers_active: AtomicUsize::new(0),
        total_shards,
        started_unix,
        finished: AtomicBool::new(false),
        shallow_seen,
        ancestor_done: Mutex::new(vec![FxHashMap::default(); ancestor_total.len()]),
        level_completions_base,
        ancestor_total,
    });

    let monitor = spawn_monitor(
        Arc::clone(&shared),
        Arc::clone(&interner),
        metrics,
        Arc::clone(&should_quit),
        config.clone(),
        input_path,
        input_fingerprint,
        checkpoint_store,
    );

    thread::scope(|scope| {
        for worker_id in 0..config.workers {
            let shared = Arc::clone(&shared);
            let interner = Arc::clone(&interner);
            let should_quit = Arc::clone(&should_quit);
            let config = config.clone();
            scope.spawn(move || worker_loop(worker_id, shared, interner, should_quit, config));
        }
    });

    shared.finished.store(true, Ordering::Relaxed);
    let _ = monitor.join();

    let best = shared.best.lock().unwrap();
    Ok(ParallelSearchResult {
        best: best.incumbent.clone(),
        nodes_expanded: shared.total_expanded.load(Ordering::Relaxed),
        nodes_pruned: shared.total_pruned.load(Ordering::Relaxed),
        completions_pruned: shared.total_completion_pruned.load(Ordering::Relaxed),
        finished: !should_quit.load(Ordering::Relaxed),
        started_unix: shared.started_unix,
        last_improvement_unix: best.last_improvement_unix,
        last_improvement_depth: best.last_improvement_depth,
        total_shards: shared.total_shards,
        shards_done: shared.shards_done.load(Ordering::Relaxed),
    })
}

fn worker_loop(
    worker_id: usize,
    shared: Arc<SharedState>,
    interner: Arc<Interner>,
    should_quit: Arc<AtomicBool>,
    config: ParallelSearchConfig,
) {
    loop {
        if should_quit.load(Ordering::Relaxed) {
            break;
        }
        let Some(task) = pop_task(&shared) else {
            break;
        };
        shared.shards_running.fetch_add(1, Ordering::Relaxed);
        shared.workers_active.fetch_add(1, Ordering::Relaxed);
        let best = shared.best.lock().unwrap().incumbent.clone();
        let shard_id = task.id;
        let shard_bucket = task.bucket;
        let shard_ancestors = task.ancestors.clone();
        let mut runner = task.runner;
        runner.import_incumbent_if_better(&best);
        let mut last_expanded = 0;
        let mut last_pruned = 0;
        let mut last_cpruned = 0;
        let mut last_rate_at = Instant::now();
        let mut last_rate_nodes = 0;
        let mut steps_since_update = 0usize;
        let mut scratch = StepScratch::default();

        update_worker_state(
            &shared,
            worker_id,
            &runner,
            shard_id,
            shard_bucket,
            phase_for(&shared, &config),
            0.0,
        );

        while !runner.is_finished() && !should_quit.load(Ordering::Relaxed) {
            let phase = phase_for(&shared, &config);
            let toggles = match phase {
                SearchPhase::Hunt => config.hunt_toggles(),
                SearchPhase::Proof => config.proof_toggles(),
            };
            match runner.step_with_toggles_and_scratch(&interner, &toggles, &mut scratch) {
                Ok(event) => {
                    if event.incumbent_improved {
                        publish_best(&shared, runner.incumbent(), runner.current_depth());
                    }
                }
                Err(_) => {
                    should_quit.store(true, Ordering::Relaxed);
                    break;
                }
            }

            steps_since_update += 1;
            if steps_since_update >= WORKER_UPDATE_STEPS {
                flush_worker_progress(
                    &shared,
                    worker_id,
                    &runner,
                    shard_id,
                    shard_bucket,
                    phase,
                    &mut last_expanded,
                    &mut last_pruned,
                    &mut last_cpruned,
                    &mut last_rate_at,
                    &mut last_rate_nodes,
                );
                maybe_pause_for_checkpoint(
                    &shared,
                    worker_id,
                    shard_id,
                    shard_bucket,
                    &shard_ancestors,
                    &runner,
                    &mut last_expanded,
                    &mut last_pruned,
                    &mut last_cpruned,
                );
                import_best(&shared, &mut runner);
                steps_since_update = 0;
            }
        }

        flush_worker_progress(
            &shared,
            worker_id,
            &runner,
            shard_id,
            shard_bucket,
            phase_for(&shared, &config),
            &mut last_expanded,
            &mut last_pruned,
            &mut last_cpruned,
            &mut last_rate_at,
            &mut last_rate_nodes,
        );
        publish_best(&shared, runner.incumbent(), runner.current_depth());
        add_completed_depths(&shared, &runner);
        record_shard_ancestor_completion(&shared, &task.ancestors);
        clear_worker_state(&shared, worker_id);
        shared.shards_running.fetch_sub(1, Ordering::Relaxed);
        shared.workers_active.fetch_sub(1, Ordering::Relaxed);
        shared.shards_done.fetch_add(1, Ordering::Relaxed);
    }
}

fn flush_worker_progress(
    shared: &SharedState,
    worker_id: usize,
    runner: &DfsRunner,
    shard_id: usize,
    shard_bucket: usize,
    phase: SearchPhase,
    last_expanded: &mut u64,
    last_pruned: &mut u64,
    last_cpruned: &mut u64,
    last_rate_at: &mut Instant,
    last_rate_nodes: &mut u64,
) {
    let expanded = runner.nodes_expanded();
    let pruned = runner.nodes_pruned();
    let cpruned = runner.completions_pruned();
    let expanded_delta = expanded.saturating_sub(*last_expanded);
    let pruned_delta = pruned.saturating_sub(*last_pruned);
    let cpruned_delta = cpruned.saturating_sub(*last_cpruned);
    shared
        .total_expanded
        .fetch_add(expanded_delta, Ordering::Relaxed);
    shared
        .total_pruned
        .fetch_add(pruned_delta, Ordering::Relaxed);
    shared
        .total_completion_pruned
        .fetch_add(cpruned_delta, Ordering::Relaxed);
    if phase == SearchPhase::Hunt {
        shared
            .hunt_expanded
            .fetch_add(expanded_delta, Ordering::Relaxed);
    }
    *last_expanded = expanded;
    *last_pruned = pruned;
    *last_cpruned = cpruned;

    let elapsed = last_rate_at.elapsed().as_secs_f64();
    let rate_delta = expanded.saturating_sub(*last_rate_nodes);
    let rate = if elapsed > 0.0 {
        rate_delta as f64 / elapsed
    } else {
        0.0
    };
    *last_rate_nodes = expanded;
    *last_rate_at = Instant::now();
    update_worker_state(
        shared,
        worker_id,
        runner,
        shard_id,
        shard_bucket,
        phase,
        rate,
    );
}

fn maybe_pause_for_checkpoint(
    shared: &SharedState,
    worker_id: usize,
    shard_id: usize,
    shard_bucket: usize,
    shard_ancestors: &[u64],
    runner: &DfsRunner,
    last_expanded: &mut u64,
    last_pruned: &mut u64,
    last_cpruned: &mut u64,
) {
    let requested = shared.checkpoint_request.load(Ordering::Acquire);
    if requested == 0 || shared.checkpoint_release.load(Ordering::Acquire) >= requested {
        return;
    }

    let expanded = runner.nodes_expanded();
    let pruned = runner.nodes_pruned();
    let cpruned = runner.completions_pruned();
    shared
        .total_expanded
        .fetch_add(expanded.saturating_sub(*last_expanded), Ordering::Relaxed);
    shared
        .total_pruned
        .fetch_add(pruned.saturating_sub(*last_pruned), Ordering::Relaxed);
    shared
        .total_completion_pruned
        .fetch_add(cpruned.saturating_sub(*last_cpruned), Ordering::Relaxed);
    *last_expanded = expanded;
    *last_pruned = pruned;
    *last_cpruned = cpruned;

    shared
        .checkpoint_snapshots
        .lock()
        .unwrap()
        .push(RunningShardCheckpoint {
            worker_id,
            shard: ShardTask {
                id: shard_id,
                bucket: shard_bucket,
                ancestors: shard_ancestors.to_vec(),
                runner: runner.clone(),
            },
        });
    shared.checkpoint_acks.fetch_add(1, Ordering::Release);
    while shared.checkpoint_release.load(Ordering::Acquire) < requested {
        thread::sleep(Duration::from_millis(10));
    }
}

fn update_worker_state(
    shared: &SharedState,
    worker_id: usize,
    runner: &DfsRunner,
    shard_id: usize,
    shard_bucket: usize,
    phase: SearchPhase,
    rate: f64,
) {
    let snapshot = runner.search_snapshot();
    let mut workers = shared.workers.lock().unwrap();
    let worker = &mut workers[worker_id];
    worker.active = true;
    worker.phase = phase.label().to_string();
    worker.shard_id = Some(shard_id);
    worker.bucket = Some(shard_bucket);
    worker.depth = snapshot.current_depth;
    worker.max_depth = snapshot.max_depth;
    worker.nodes_expanded = runner.nodes_expanded();
    worker.nodes_pruned = runner.nodes_pruned();
    worker.completions_pruned = runner.completions_pruned();
    worker.seen_by_depth = snapshot.seen_by_depth;
    worker.descended_by_depth = snapshot.descended_by_depth;
    worker.pruned_by_depth = snapshot.pruned_by_depth;
    worker.current_bound = snapshot.current_bound;
    worker.rate = rate;
}

fn clear_worker_state(shared: &SharedState, worker_id: usize) {
    let mut workers = shared.workers.lock().unwrap();
    workers[worker_id] = WorkerState::default();
}

fn add_completed_depths(shared: &SharedState, runner: &DfsRunner) {
    let snapshot = runner.search_snapshot();
    let mut completed = shared.completed_depths.lock().unwrap();
    add_depth_vec(&mut completed.seen_by_depth, &snapshot.seen_by_depth);
    add_depth_vec(
        &mut completed.descended_by_depth,
        &snapshot.descended_by_depth,
    );
    add_depth_vec(&mut completed.pruned_by_depth, &snapshot.pruned_by_depth);
}

fn pop_task(shared: &SharedState) -> Option<ShardTask> {
    shared.queue.lock().unwrap().pop_front()
}

fn build_ancestor_total(tasks: &[ShardTask]) -> Vec<FxHashMap<u64, usize>> {
    let depth = tasks.iter().map(|t| t.ancestors.len()).max().unwrap_or(0);
    let mut result: Vec<FxHashMap<u64, usize>> = vec![FxHashMap::default(); depth];
    for task in tasks {
        for (level, &ancestor_id) in task.ancestors.iter().enumerate() {
            *result[level].entry(ancestor_id).or_insert(0) += 1;
        }
    }
    result
}

fn record_shard_ancestor_completion(shared: &SharedState, ancestors: &[u64]) {
    if shared.ancestor_total.is_empty() {
        return;
    }
    let mut done = shared.ancestor_done.lock().unwrap();
    for (level, &ancestor_id) in ancestors.iter().enumerate() {
        if level >= done.len() {
            break;
        }
        *done[level].entry(ancestor_id).or_insert(0) += 1;
    }
}

fn phase_for(shared: &SharedState, config: &ParallelSearchConfig) -> SearchPhase {
    if shared.hunt_expanded.load(Ordering::Relaxed) < config.hunt_nodes {
        SearchPhase::Hunt
    } else {
        SearchPhase::Proof
    }
}

fn import_best(shared: &SharedState, runner: &mut DfsRunner) {
    let incumbent = shared.best.lock().unwrap().incumbent.clone();
    runner.import_incumbent_if_better(&incumbent);
}

fn publish_best(shared: &SharedState, incumbent: &Ortho, depth: usize) {
    let mut best = shared.best.lock().unwrap();
    if incumbent.score() > best.incumbent.score() {
        best.incumbent = incumbent.clone();
        best.last_improvement_unix = now_unix();
        best.last_improvement_depth = depth;
    }
}

fn spawn_monitor(
    shared: Arc<SharedState>,
    interner: Arc<Interner>,
    metrics: Metrics,
    should_quit: Arc<AtomicBool>,
    config: ParallelSearchConfig,
    input_path: String,
    input_fingerprint: u64,
    checkpoint_store: ParallelCheckpointStore,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut last_checkpoint = Instant::now();
        while !shared.finished.load(Ordering::Relaxed) && !should_quit.load(Ordering::Relaxed) {
            update_parallel_metrics(&shared, &interner, &metrics, &config, &input_path);
            if last_checkpoint.elapsed() >= Duration::from_secs(config.checkpoint_every_secs) {
                let started = Instant::now();
                match save_parallel_checkpoint(
                    &shared,
                    &interner,
                    &checkpoint_store,
                    &config,
                    &input_path,
                    input_fingerprint,
                    "parallel-checkpoint",
                ) {
                    Ok(manifest) => {
                        *shared.checkpoint_status.lock().unwrap() =
                            format!("saved in {:.2}s", started.elapsed().as_secs_f64());
                        shared
                            .checkpoint_time
                            .store(manifest.checkpoint_unix, Ordering::Relaxed);
                    }
                    Err(err) => {
                        *shared.checkpoint_status.lock().unwrap() = format!("failed: {err}");
                    }
                }
                last_checkpoint = Instant::now();
            }
            thread::sleep(Duration::from_millis(METRICS_UPDATE_MS));
        }
        update_parallel_metrics(&shared, &interner, &metrics, &config, &input_path);
    })
}

fn save_parallel_checkpoint(
    shared: &SharedState,
    interner: &Interner,
    store: &ParallelCheckpointStore,
    config: &ParallelSearchConfig,
    input_path: &str,
    input_fingerprint: u64,
    status: &str,
) -> Result<ParallelCheckpointManifest, FoldError> {
    let generation = shared.checkpoint_request.fetch_add(1, Ordering::AcqRel) + 1;
    shared.checkpoint_saving.store(true, Ordering::Relaxed);
    *shared.checkpoint_status.lock().unwrap() = "saving".to_string();
    shared.checkpoint_acks.store(0, Ordering::Release);
    shared.checkpoint_snapshots.lock().unwrap().clear();

    let wait_start = Instant::now();
    loop {
        let active = shared.workers_active.load(Ordering::Acquire);
        let acks = shared.checkpoint_acks.load(Ordering::Acquire);
        if acks >= active {
            break;
        }
        if wait_start.elapsed() > Duration::from_secs(15) {
            shared
                .checkpoint_release
                .store(generation, Ordering::Release);
            shared.checkpoint_saving.store(false, Ordering::Relaxed);
            return Err(FoldError::Other(format!(
                "timed out waiting for checkpoint barrier acks={acks} active={active}"
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }

    let state = capture_parallel_checkpoint_state(shared, input_path, input_fingerprint, config);
    let result = (|| {
        let manifest = store.save(&state, status)?;
        store.save_progress_output(&state, interner, config, status)?;
        Ok::<ParallelCheckpointManifest, FoldError>(manifest)
    })();
    shared
        .checkpoint_release
        .store(generation, Ordering::Release);
    shared.checkpoint_saving.store(false, Ordering::Relaxed);
    let manifest = result?;
    shared
        .checkpoint_time
        .store(manifest.checkpoint_unix, Ordering::Relaxed);
    Ok(manifest)
}

fn capture_parallel_checkpoint_state(
    shared: &SharedState,
    input_path: &str,
    input_fingerprint: u64,
    config: &ParallelSearchConfig,
) -> ParallelCheckpointState {
    let pending_shards: Vec<ShardTask> = shared.queue.lock().unwrap().iter().cloned().collect();
    let running_shards = shared.checkpoint_snapshots.lock().unwrap().clone();
    let completed_depths = shared.completed_depths.lock().unwrap().clone();
    let best = shared.best.lock().unwrap();
    let level_completions = compute_level_completions(shared);
    ParallelCheckpointState {
        input_path: input_path.to_string(),
        input_fingerprint,
        config_fingerprint: config.fingerprint(),
        pending_shards,
        running_shards,
        completed_depths,
        best_incumbent: best.incumbent.clone(),
        last_improvement_unix: best.last_improvement_unix,
        last_improvement_depth: best.last_improvement_depth,
        nodes_expanded: shared.total_expanded.load(Ordering::Relaxed),
        nodes_pruned: shared.total_pruned.load(Ordering::Relaxed),
        completions_pruned: shared.total_completion_pruned.load(Ordering::Relaxed),
        hunt_expanded: shared.hunt_expanded.load(Ordering::Relaxed),
        total_shards: shared.total_shards,
        shards_done: shared.shards_done.load(Ordering::Relaxed),
        started_unix: shared.started_unix,
        shallow_seen: shared.shallow_seen.clone(),
        level_completions,
    }
}

fn compute_level_completions(shared: &SharedState) -> Vec<usize> {
    let done = shared.ancestor_done.lock().unwrap();
    compute_level_completions_from(
        &shared.ancestor_total,
        &done,
        &shared.level_completions_base,
    )
}

fn compute_level_completions_from(
    ancestor_total: &[FxHashMap<u64, usize>],
    ancestor_done: &[FxHashMap<u64, usize>],
    level_completions_base: &[usize],
) -> Vec<usize> {
    let levels = ancestor_total.len().max(level_completions_base.len());
    (0..levels)
        .map(|level| {
            let base = level_completions_base.get(level).copied().unwrap_or(0);
            let new_completions = ancestor_total
                .get(level)
                .map(|totals| completed_ancestor_count(totals, ancestor_done.get(level)) as usize)
                .unwrap_or(0);
            base + new_completions
        })
        .collect()
}

fn update_parallel_metrics(
    shared: &SharedState,
    interner: &Interner,
    metrics: &Metrics,
    config: &ParallelSearchConfig,
    input_path: &str,
) {
    let workers = shared.workers.lock().unwrap().clone();
    let completed = shared.completed_depths.lock().unwrap().clone();
    let best = shared.best.lock().unwrap();
    let pending = shared.queue.lock().unwrap().len();
    let phase = phase_for(shared, config);

    let mut seen_by_depth = shared.shallow_seen.clone();
    let mut descended_by_depth = vec![0; seen_by_depth.len()];
    let mut pruned_by_depth = vec![0; seen_by_depth.len()];
    let shard_local_start = config.shard_depth.saturating_sub(1);
    add_depth_vec_from(
        &mut seen_by_depth,
        &completed.seen_by_depth,
        shard_local_start,
    );
    add_depth_vec_from(
        &mut descended_by_depth,
        &completed.descended_by_depth,
        shard_local_start,
    );
    add_depth_vec_from(
        &mut pruned_by_depth,
        &completed.pruned_by_depth,
        shard_local_start,
    );
    let mut max_depth = 0usize;
    let mut current_depth = 0usize;
    let mut current_bound = None;
    let mut worker_summaries = Vec::new();
    let mut worker_rates = Vec::new();
    let mut active_buckets = Vec::new();

    for (id, worker) in workers.iter().enumerate() {
        if !worker.active {
            continue;
        }
        add_depth_vec_from(&mut seen_by_depth, &worker.seen_by_depth, shard_local_start);
        add_depth_vec_from(
            &mut descended_by_depth,
            &worker.descended_by_depth,
            shard_local_start,
        );
        add_depth_vec_from(
            &mut pruned_by_depth,
            &worker.pruned_by_depth,
            shard_local_start,
        );
        max_depth = max_depth.max(worker.max_depth);
        current_depth = current_depth.max(worker.depth);
        if let Some(bound) = worker.current_bound {
            current_bound = Some(match current_bound {
                Some(existing) if existing >= bound => existing,
                _ => bound,
            });
        }
        if let Some(bucket) = worker.bucket {
            active_buckets.push(bucket);
        }
        worker_rates.push((id, worker.rate));
        worker_summaries.push(WorkerMetrics {
            id,
            mode: worker.phase.clone(),
            shard_id: worker.shard_id,
            depth: worker.depth,
            rate: worker.rate,
            current_bound: worker.current_bound,
        });
    }

    let (worker_rate_min, worker_rate_avg, worker_rate_max, slowest_worker) =
        worker_rate_stats(&worker_rates);
    let (frontier_buckets, _) = frontier_buckets(shared);
    let effective_score = best
        .incumbent
        .score()
        .max(OrthoScore::optimistic_bound(8, 27));

    apply_ancestor_progress(
        &mut seen_by_depth,
        &mut descended_by_depth,
        &mut pruned_by_depth,
        &shared.ancestor_total,
        &shared.ancestor_done.lock().unwrap(),
        &shared.level_completions_base,
    );
    metrics.update_global(|g| {
        g.input_path = input_path.to_string();
        g.phase = if shared.checkpoint_saving.load(Ordering::Relaxed) {
            "Parallel Checkpointing".to_string()
        } else {
            format!("Parallel {}", phase.label())
        };
        g.start_time = shared.started_unix;
        g.nodes_expanded = shared.total_expanded.load(Ordering::Relaxed);
        g.nodes_pruned = shared.total_pruned.load(Ordering::Relaxed);
        g.completions_pruned = shared.total_completion_pruned.load(Ordering::Relaxed);
        g.current_depth = current_depth;
        g.max_depth = max_depth;
        g.current_bound = current_bound.unwrap_or_else(|| best.incumbent.score());
        g.open_siblings_total =
            pending as u64 + shared.shards_running.load(Ordering::Relaxed) as u64;
        g.open_siblings_by_depth = vec![g.open_siblings_total; max_depth.max(1)];
        g.seen_by_depth = seen_by_depth;
        g.descended_by_depth = descended_by_depth;
        g.pruned_by_depth = pruned_by_depth;
        g.path_progress_by_depth = Vec::new();
        g.frontier_max_bound = current_bound;
        g.incumbent_score = best.incumbent.score();
        g.effective_prune_score = effective_score;
        g.score_floor = OrthoScore::optimistic_bound(8, 27);
        g.incumbent_dims = best.incumbent.dims().clone();
        g.incumbent_capacity = best.incumbent.payload().len();
        g.incumbent_display = format!("{}", best.incumbent.display(interner));
        g.last_improvement_unix = best.last_improvement_unix;
        g.last_improvement_depth = best.last_improvement_depth;
        g.checkpoint_status = shared.checkpoint_status.lock().unwrap().clone();
        g.checkpoint_time = shared.checkpoint_time.load(Ordering::Relaxed);
        g.parallel.enabled = true;
        g.parallel.mode = phase.label().to_string();
        g.parallel.workers_total = config.workers;
        g.parallel.workers_active = shared.workers_active.load(Ordering::Relaxed);
        g.parallel.shards_pending = pending;
        g.parallel.shards_running = shared.shards_running.load(Ordering::Relaxed);
        g.parallel.shards_done = shared.shards_done.load(Ordering::Relaxed);
        g.parallel.hunt_nodes = shared.hunt_expanded.load(Ordering::Relaxed);
        g.parallel.hunt_target_nodes = config.hunt_nodes;
        g.parallel.frontier_buckets = frontier_buckets;
        g.parallel.active_buckets = active_buckets;
        g.parallel.worker_summaries = worker_summaries;
        g.parallel.worker_rate_min = worker_rate_min;
        g.parallel.worker_rate_avg = worker_rate_avg;
        g.parallel.worker_rate_max = worker_rate_max;
        g.parallel.slowest_worker = slowest_worker;
    });
}

fn frontier_buckets(shared: &SharedState) -> (Vec<u64>, Vec<usize>) {
    let mut buckets = vec![0u64; FRONTIER_BUCKETS];
    for task in shared.queue.lock().unwrap().iter() {
        if let Some(bucket) = buckets.get_mut(task.bucket) {
            *bucket = bucket.saturating_add(1);
        }
    }
    (buckets, Vec::new())
}

fn worker_rate_stats(worker_rates: &[(usize, f64)]) -> (f64, f64, f64, Option<usize>) {
    if worker_rates.is_empty() {
        return (0.0, 0.0, 0.0, None);
    }
    let mut min_rate = f64::MAX;
    let mut max_rate = 0.0;
    let mut sum = 0.0;
    let mut slowest = None;
    for &(id, rate) in worker_rates {
        if rate < min_rate {
            min_rate = rate;
            slowest = Some(id);
        }
        if rate > max_rate {
            max_rate = rate;
        }
        sum += rate;
    }
    (min_rate, sum / worker_rates.len() as f64, max_rate, slowest)
}

fn add_depth_vec(target: &mut Vec<u64>, source: &[u64]) {
    if target.len() < source.len() {
        target.resize(source.len(), 0);
    }
    for (idx, value) in source.iter().copied().enumerate() {
        target[idx] = target[idx].saturating_add(value);
    }
}

fn add_depth_vec_from(target: &mut Vec<u64>, source: &[u64], start_idx: usize) {
    if target.len() < source.len() {
        target.resize(source.len(), 0);
    }
    for (idx, value) in source.iter().copied().enumerate().skip(start_idx) {
        target[idx] = target[idx].saturating_add(value);
    }
}

fn apply_ancestor_progress(
    seen_by_depth: &mut Vec<u64>,
    descended_by_depth: &mut Vec<u64>,
    pruned_by_depth: &mut Vec<u64>,
    ancestor_total: &[FxHashMap<u64, usize>],
    ancestor_done: &[FxHashMap<u64, usize>],
    level_completions_base: &[usize],
) {
    for (ancestor_level, ancestor_map) in ancestor_total.iter().enumerate().skip(1) {
        let depth_idx = ancestor_level - 1;
        let seen = ancestor_map.len() as u64;
        let new_done = completed_ancestor_count(ancestor_map, ancestor_done.get(ancestor_level));
        let base_done = level_completions_base
            .get(ancestor_level)
            .copied()
            .unwrap_or(0) as u64;
        let touched = base_done.saturating_add(new_done);

        while seen_by_depth.len() <= depth_idx {
            seen_by_depth.push(0);
        }
        while descended_by_depth.len() <= depth_idx {
            descended_by_depth.push(0);
        }
        while pruned_by_depth.len() <= depth_idx {
            pruned_by_depth.push(0);
        }
        let existing_seen = seen_by_depth[depth_idx];
        let existing_touched =
            descended_by_depth[depth_idx].saturating_add(pruned_by_depth[depth_idx]);
        let merged_seen = existing_seen.max(seen);
        let merged_touched = existing_touched.max(touched).min(merged_seen);
        pruned_by_depth[depth_idx] = 0;
        seen_by_depth[depth_idx] = merged_seen;
        descended_by_depth[depth_idx] = merged_touched;
    }
}

fn completed_ancestor_count(
    ancestor_map: &FxHashMap<u64, usize>,
    done_map: Option<&FxHashMap<u64, usize>>,
) -> u64 {
    done_map.map_or(0, |done_map| {
        done_map
            .iter()
            .filter(|(id, done_n)| {
                ancestor_map
                    .get(*id)
                    .is_some_and(|&total| **done_n >= total)
            })
            .count() as u64
    })
}

fn bucket_for(idx: usize, total: usize) -> usize {
    if total == 0 {
        0
    } else {
        (idx * FRONTIER_BUCKETS / total).min(FRONTIER_BUCKETS - 1)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dfs_runner::{BranchOrdering, SearchToggles};
    use std::collections::HashSet;

    #[test]
    fn parallel_phase_toggles_use_completion_pruning_only_for_proof() {
        let config = ParallelSearchConfig {
            enabled: true,
            workers: 4,
            hunt_nodes: 1_000,
            shard_depth: 3,
            checkpoint_every_secs: 30,
            resume_enabled: true,
        };

        let hunt = config.hunt_toggles();
        assert_eq!(hunt.branch_ordering, BranchOrdering::WorstFirst);
        assert!(!hunt.completion_pruning);
        assert!(hunt.compute_bounds);
        assert!(hunt.node_pruning);

        let proof = config.proof_toggles();
        assert!(proof.completion_pruning);
        assert!(proof.compute_bounds);
        assert!(proof.node_pruning);
    }

    #[test]
    fn frontier_shards_are_disjoint_at_depth() {
        let interner = Interner::from_text("a b c. a d e.");
        let toggles = SearchToggles {
            node_pruning: false,
            completion_pruning: false,
            compute_bounds: false,
            ..SearchToggles::default()
        };
        let (shards, _, _) = DfsRunner::frontier_shards(&interner, 2, &toggles).unwrap();
        assert!(!shards.is_empty());

        let mut ids = HashSet::new();
        for shard in shards {
            assert_eq!(shard.len(), 2);
            let id = shard.last().unwrap().ortho.id();
            assert!(ids.insert(id), "duplicate shard leaf id {id}");
        }
    }

    #[test]
    fn ancestor_progress_does_not_display_root_level() {
        let mut seen_by_depth = vec![2_580];
        let mut descended_by_depth = vec![0];
        let mut pruned_by_depth = vec![0];
        let mut root = FxHashMap::default();
        root.insert(1, 4);
        let ancestor_total = vec![root];
        let ancestor_done = vec![FxHashMap::default()];

        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[],
        );

        assert_eq!(seen_by_depth, vec![2_580]);
        assert_eq!(descended_by_depth, vec![0]);
    }

    #[test]
    fn pre_shard_levels_start_open_when_frontier_exists() {
        let mut seen_by_depth = vec![2, 3];
        let mut descended_by_depth = vec![0; seen_by_depth.len()];
        let mut pruned_by_depth = vec![0; seen_by_depth.len()];
        let ancestor_total = vec![
            ancestor_totals(&[(1, 3)]),
            ancestor_totals(&[(10, 2), (20, 1)]),
            ancestor_totals(&[(100, 1), (101, 1), (200, 1)]),
        ];
        let ancestor_done = vec![
            FxHashMap::default(),
            FxHashMap::default(),
            FxHashMap::default(),
        ];

        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[],
        );

        assert_eq!(seen_by_depth, vec![2, 3]);
        assert_eq!(descended_by_depth, vec![0, 0]);
    }

    #[test]
    fn shard_depth_three_progress_ticks_shard_before_parent() {
        let mut seen_by_depth = vec![2, 3];
        let mut descended_by_depth = vec![0; seen_by_depth.len()];
        let mut pruned_by_depth = vec![0; seen_by_depth.len()];
        let ancestor_total = vec![
            ancestor_totals(&[(1, 3)]),
            ancestor_totals(&[(10, 2), (20, 1)]),
            ancestor_totals(&[(100, 1), (101, 1), (200, 1)]),
        ];
        let ancestor_done = vec![
            FxHashMap::default(),
            ancestor_totals(&[(10, 1)]),
            ancestor_totals(&[(100, 1)]),
        ];

        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[],
        );

        assert_eq!(seen_by_depth, vec![2, 3]);
        assert_eq!(descended_by_depth, vec![0, 1]);

        let ancestor_done = vec![
            FxHashMap::default(),
            ancestor_totals(&[(10, 2)]),
            ancestor_totals(&[(100, 1), (101, 1)]),
        ];
        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[],
        );

        assert_eq!(descended_by_depth, vec![1, 2]);
    }

    #[test]
    fn resumed_ancestor_progress_does_not_shrink_existing_totals() {
        let mut seen_by_depth = vec![2_580, 182];
        let mut descended_by_depth = vec![0, 100];
        let mut pruned_by_depth = vec![0, 20];
        let ancestor_total = vec![
            ancestor_totals(&[(1, 1)]),
            ancestor_totals(&[(10, 1), (20, 1)]),
            ancestor_totals(&[(100, 1), (200, 1), (300, 1)]),
        ];
        let ancestor_done = vec![
            FxHashMap::default(),
            ancestor_totals(&[(10, 1)]),
            ancestor_totals(&[(100, 1)]),
        ];

        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[0, 1, 1],
        );

        assert_eq!(seen_by_depth, vec![2_580, 182]);
        assert_eq!(descended_by_depth, vec![2, 120]);
        assert_eq!(pruned_by_depth, vec![0, 0]);
    }

    #[test]
    fn resumed_ancestor_progress_includes_saved_base() {
        let mut seen_by_depth = Vec::new();
        let mut descended_by_depth = Vec::new();
        let mut pruned_by_depth = Vec::new();
        let ancestor_total = vec![
            ancestor_totals(&[(1, 1)]),
            ancestor_totals(&[(10, 1)]),
            ancestor_totals(&[(100, 1), (200, 1)]),
        ];
        let ancestor_done = vec![
            FxHashMap::default(),
            FxHashMap::default(),
            FxHashMap::default(),
        ];

        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[0, 0, 1],
        );

        assert_eq!(seen_by_depth, vec![1, 2]);
        assert_eq!(descended_by_depth, vec![0, 1]);
    }

    #[test]
    fn resumed_ancestor_progress_adds_new_completions() {
        let mut seen_by_depth = Vec::new();
        let mut descended_by_depth = Vec::new();
        let mut pruned_by_depth = Vec::new();
        let ancestor_total = vec![
            ancestor_totals(&[(1, 1)]),
            ancestor_totals(&[(10, 2), (20, 1), (30, 1)]),
            ancestor_totals(&[(100, 1), (101, 1), (200, 1), (300, 1)]),
        ];
        let ancestor_done = vec![
            FxHashMap::default(),
            ancestor_totals(&[(10, 2), (20, 1), (30, 0)]),
            ancestor_totals(&[(100, 1), (101, 1), (200, 1)]),
        ];

        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[0, 1],
        );

        assert_eq!(seen_by_depth, vec![3, 4]);
        assert_eq!(descended_by_depth, vec![3, 3]);
    }

    #[test]
    fn resumed_ancestor_progress_clamps_to_seen() {
        let mut seen_by_depth = Vec::new();
        let mut descended_by_depth = Vec::new();
        let mut pruned_by_depth = Vec::new();
        let ancestor_total = vec![
            ancestor_totals(&[(1, 1)]),
            ancestor_totals(&[(10, 1)]),
            ancestor_totals(&[(100, 1), (200, 1)]),
        ];
        let ancestor_done = vec![
            FxHashMap::default(),
            ancestor_totals(&[(10, 1)]),
            ancestor_totals(&[(100, 1), (200, 1)]),
        ];

        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[0, 5, 5],
        );

        assert_eq!(seen_by_depth, vec![1, 2]);
        assert_eq!(descended_by_depth, vec![1, 2]);
    }

    #[test]
    fn resumed_ancestor_progress_keeps_running_shards_open() {
        let mut seen_by_depth = vec![2, 3];
        let mut descended_by_depth = vec![0; seen_by_depth.len()];
        let mut pruned_by_depth = vec![0; seen_by_depth.len()];
        let ancestor_total = vec![
            ancestor_totals(&[(1, 1)]),
            ancestor_totals(&[(10, 1)]),
            ancestor_totals(&[(102, 1)]),
        ];
        let ancestor_done = vec![
            FxHashMap::default(),
            FxHashMap::default(),
            FxHashMap::default(),
        ];

        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[0, 1, 2],
        );

        assert_eq!(seen_by_depth, vec![2, 3]);
        assert_eq!(descended_by_depth, vec![1, 2]);

        let ancestor_done = vec![
            FxHashMap::default(),
            ancestor_totals(&[(10, 1)]),
            ancestor_totals(&[(102, 1)]),
        ];
        apply_ancestor_progress(
            &mut seen_by_depth,
            &mut descended_by_depth,
            &mut pruned_by_depth,
            &ancestor_total,
            &ancestor_done,
            &[0, 1, 2],
        );

        assert_eq!(descended_by_depth, vec![2, 3]);
    }

    #[test]
    fn checkpoint_level_completions_include_fresh_run_levels() {
        let ancestor_total = vec![
            ancestor_totals(&[(1, 2)]),
            ancestor_totals(&[(10, 1), (20, 2)]),
        ];
        let ancestor_done = vec![
            ancestor_totals(&[(1, 2)]),
            ancestor_totals(&[(10, 1), (20, 1)]),
        ];

        let completions = compute_level_completions_from(&ancestor_total, &ancestor_done, &[]);

        assert_eq!(completions, vec![1, 1]);
    }

    #[test]
    fn checkpoint_level_completions_add_saved_base() {
        let ancestor_total = vec![
            ancestor_totals(&[(1, 2)]),
            ancestor_totals(&[(10, 1), (20, 2)]),
        ];
        let ancestor_done = vec![
            ancestor_totals(&[(1, 2)]),
            ancestor_totals(&[(10, 1), (20, 2)]),
        ];

        let completions = compute_level_completions_from(&ancestor_total, &ancestor_done, &[3, 5]);

        assert_eq!(completions, vec![4, 7]);
    }

    #[test]
    fn one_worker_parallel_matches_single_best_on_small_corpus() {
        let interner = Arc::new(Interner::from_text("a b c. a d e."));
        let mut single = DfsRunner::new();
        while !single.is_finished() {
            single.step(&interner).unwrap();
        }

        let metrics = Metrics::new();
        let should_quit = Arc::new(AtomicBool::new(false));
        let config = ParallelSearchConfig {
            enabled: true,
            workers: 1,
            hunt_nodes: 1,
            shard_depth: 2,
            checkpoint_every_secs: 3600,
            resume_enabled: false,
        };
        let tmp = tempfile::tempdir().unwrap();
        let checkpoint_store = ParallelCheckpointStore::new(tmp.path(), tmp.path()).unwrap();
        let result = run_parallel_search(
            interner,
            metrics,
            should_quit,
            config,
            "test.txt".to_string(),
            0,
            checkpoint_store,
            None,
        )
        .unwrap();

        assert!(result.finished);
        assert_eq!(result.best.score(), single.incumbent().score());
    }

    fn ancestor_totals(entries: &[(u64, usize)]) -> FxHashMap<u64, usize> {
        entries.iter().copied().collect()
    }
}
