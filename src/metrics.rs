use crate::ortho::{Dim, OrthoScore};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_LOGS: usize = 400;
const MAX_RATE_SAMPLES: usize = 64;
const RATE_WINDOW: Duration = Duration::from_secs(10);
const RATE_STALE_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub struct GlobalMetrics {
    pub input_path: String,
    pub phase: String,
    pub start_time: u64,
    pub nodes_expanded: u64,
    pub nodes_pruned: u64,
    pub completions_pruned: u64,
    pub current_depth: usize,
    pub max_depth: usize,
    pub current_bound: OrthoScore,
    pub incumbent_score: OrthoScore,
    pub incumbent_dims: Vec<Dim>,
    pub incumbent_capacity: usize,
    pub incumbent_display: String,
    pub checkpoint_time: u64,
    pub checkpoint_status: String,
    pub open_siblings_total: u64,
    pub open_siblings_by_depth: Vec<u64>,
    pub seen_by_depth: Vec<u64>,
    pub descended_by_depth: Vec<u64>,
    pub pruned_by_depth: Vec<u64>,
    pub path_progress_by_depth: Vec<(usize, usize)>,
    pub frontier_max_bound: Option<OrthoScore>,
    pub last_improvement_unix: u64,
    pub last_improvement_depth: usize,
    pub nodes_per_sec: f64,
    pub prunes_per_sec: f64,
    pub completion_prunes_per_sec: f64,
    pub effective_prune_score: OrthoScore,
    pub score_floor: OrthoScore,
    pub parallel: ParallelMetrics,
}

#[derive(Clone, Debug, Default)]
pub struct ParallelMetrics {
    pub enabled: bool,
    pub mode: String,
    pub workers_total: usize,
    pub workers_active: usize,
    pub shards_pending: usize,
    pub shards_running: usize,
    pub shards_done: usize,
    pub hunt_nodes: u64,
    pub hunt_target_nodes: u64,
    pub frontier_buckets: Vec<u64>,
    pub active_buckets: Vec<usize>,
    pub worker_summaries: Vec<WorkerMetrics>,
    pub worker_rate_min: f64,
    pub worker_rate_avg: f64,
    pub worker_rate_max: f64,
    pub slowest_worker: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkerMetrics {
    pub id: usize,
    pub mode: String,
    pub shard_id: Option<usize>,
    pub depth: usize,
    pub rate: f64,
    pub current_bound: Option<OrthoScore>,
}

impl Default for GlobalMetrics {
    fn default() -> Self {
        Self {
            input_path: String::new(),
            phase: "Starting".to_string(),
            start_time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            nodes_expanded: 0,
            nodes_pruned: 0,
            completions_pruned: 0,
            current_depth: 0,
            max_depth: 0,
            current_bound: OrthoScore::zero(),
            incumbent_score: OrthoScore::zero(),
            incumbent_dims: Vec::new(),
            incumbent_capacity: 0,
            incumbent_display: String::new(),
            checkpoint_time: 0,
            checkpoint_status: "Not yet checkpointed".to_string(),
            open_siblings_total: 0,
            open_siblings_by_depth: Vec::new(),
            seen_by_depth: Vec::new(),
            descended_by_depth: Vec::new(),
            pruned_by_depth: Vec::new(),
            path_progress_by_depth: Vec::new(),
            frontier_max_bound: None,
            last_improvement_unix: 0,
            last_improvement_depth: 0,
            nodes_per_sec: 0.0,
            prunes_per_sec: 0.0,
            completion_prunes_per_sec: 0.0,
            effective_prune_score: OrthoScore::zero(),
            score_floor: OrthoScore::zero(),
            parallel: ParallelMetrics::default(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MetricsSnapshot {
    pub global: GlobalMetrics,
    pub logs: Vec<String>,
}

#[derive(Clone)]
pub struct Metrics(Arc<Mutex<MetricsState>>);

#[derive(Debug, Default)]
struct MetricsState {
    global: GlobalMetrics,
    logs: VecDeque<String>,
    rate_samples: VecDeque<RateSample>,
}

#[derive(Clone, Debug)]
struct RateSample {
    at: Instant,
    nodes_expanded: u64,
    nodes_pruned: u64,
    completions_pruned: u64,
}

impl Metrics {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(MetricsState::default())))
    }

    pub fn update_global<F>(&self, update: F)
    where
        F: FnOnce(&mut GlobalMetrics),
    {
        let mut state = self.0.lock().unwrap();
        update(&mut state.global);
        record_rate_sample(&mut state);
    }

    pub fn add_log(&self, message: impl Into<String>) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut state = self.0.lock().unwrap();
        if state.logs.len() >= MAX_LOGS {
            state.logs.pop_front();
        }
        state.logs.push_back(format!("{}  {}", now, message.into()));
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let state = self.0.lock().unwrap();
        let mut global = state.global.clone();
        let (nodes_per_sec, prunes_per_sec, completion_prunes_per_sec) = compute_rates(&state);
        global.nodes_per_sec = nodes_per_sec;
        global.prunes_per_sec = prunes_per_sec;
        global.completion_prunes_per_sec = completion_prunes_per_sec;
        MetricsSnapshot {
            global,
            logs: state.logs.iter().cloned().collect(),
        }
    }
}

fn record_rate_sample(state: &mut MetricsState) {
    let now = Instant::now();
    state.rate_samples.push_back(RateSample {
        at: now,
        nodes_expanded: state.global.nodes_expanded,
        nodes_pruned: state.global.nodes_pruned,
        completions_pruned: state.global.completions_pruned,
    });
    while state.rate_samples.len() > MAX_RATE_SAMPLES {
        state.rate_samples.pop_front();
    }
    while let Some(sample) = state.rate_samples.front() {
        if now.duration_since(sample.at) <= RATE_WINDOW {
            break;
        }
        state.rate_samples.pop_front();
    }
}

fn compute_rates(state: &MetricsState) -> (f64, f64, f64) {
    let Some(newest) = state.rate_samples.back() else {
        return (0.0, 0.0, 0.0);
    };
    let now = Instant::now();
    if now.duration_since(newest.at) > RATE_STALE_AFTER {
        return (0.0, 0.0, 0.0);
    }

    let oldest = state
        .rate_samples
        .iter()
        .find(|sample| newest.at.duration_since(sample.at) <= RATE_WINDOW)
        .unwrap_or(newest);
    let elapsed = newest.at.duration_since(oldest.at).as_secs_f64();
    if elapsed <= 0.0 {
        return (0.0, 0.0, 0.0);
    }

    (
        newest.nodes_expanded.saturating_sub(oldest.nodes_expanded) as f64 / elapsed,
        newest.nodes_pruned.saturating_sub(oldest.nodes_pruned) as f64 / elapsed,
        newest
            .completions_pruned
            .saturating_sub(oldest.completions_pruned) as f64
            / elapsed,
    )
}

#[cfg(test)]
mod tests {
    use super::Metrics;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn rates_become_nonzero_and_decay_when_stale() {
        let metrics = Metrics::new();
        metrics.update_global(|g| {
            g.nodes_expanded = 0;
            g.nodes_pruned = 0;
            g.completions_pruned = 0;
        });
        thread::sleep(Duration::from_millis(50));
        metrics.update_global(|g| {
            g.nodes_expanded = 200;
            g.nodes_pruned = 40;
            g.completions_pruned = 10;
        });

        let snapshot = metrics.snapshot();
        assert!(snapshot.global.nodes_per_sec > 0.0);
        assert!(snapshot.global.prunes_per_sec > 0.0);
        assert!(snapshot.global.completion_prunes_per_sec > 0.0);

        thread::sleep(Duration::from_millis(1100));
        let stale = metrics.snapshot();
        assert_eq!(stale.global.nodes_per_sec, 0.0);
        assert_eq!(stale.global.prunes_per_sec, 0.0);
        assert_eq!(stale.global.completion_prunes_per_sec, 0.0);
    }
}
