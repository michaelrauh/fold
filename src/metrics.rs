use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SAMPLES: usize = 1000;

#[derive(Clone, Debug)]
pub struct MetricSample {
    pub timestamp: u64,
    pub value: usize,
}

#[derive(Clone, Debug)]
pub struct GlobalMetrics {
    pub mode: String,
    pub interner_version: usize,
    pub vocab_size: usize,
    pub total_chunks: usize,
    pub processed_chunks: usize,
    pub remaining_chunks: usize,
    pub system_memory_percent: usize,
    pub start_time: u64,
    pub ram_mb: usize,
    pub current_lineage: String,
    pub queue_buffer_size: usize,
    pub bloom_capacity: usize,
    pub num_shards: usize,
    pub max_shards_in_memory: usize,
}

impl Default for GlobalMetrics {
    fn default() -> Self {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self {
            mode: "Starting".to_string(),
            interner_version: 0,
            vocab_size: 0,
            total_chunks: 0,
            processed_chunks: 0,
            remaining_chunks: 0,
            system_memory_percent: 0,
            start_time,
            ram_mb: 0,
            current_lineage: String::new(),
            queue_buffer_size: 0,
            bloom_capacity: 0,
            num_shards: 0,
            max_shards_in_memory: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OperationStatus {
    pub current_file: String,
    pub status: String,
    pub progress_current: usize,
    pub progress_total: usize,
}

impl Default for OperationStatus {
    fn default() -> Self {
        Self {
            current_file: String::new(),
            status: "Idle".to_string(),
            progress_current: 0,
            progress_total: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MergeStatus {
    pub completed_merges: usize,
    pub current_merge: String,
    pub archive_a_orthos: usize,
    pub archive_b_orthos: usize,
    pub impacted_a: usize,
    pub impacted_b: usize,
    pub seed_orthos_a: usize,
    pub seed_orthos_b: usize,
    pub impacted_queued_a: usize,
    pub impacted_queued_b: usize,
}

impl Default for MergeStatus {
    fn default() -> Self {
        Self {
            completed_merges: 0,
            current_merge: String::new(),
            archive_a_orthos: 0,
            archive_b_orthos: 0,
            impacted_a: 0,
            impacted_b: 0,
            seed_orthos_a: 0,
            seed_orthos_b: 0,
            impacted_queued_a: 0,
            impacted_queued_b: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LargestArchive {
    pub filename: String,
    pub ortho_count: usize,
    pub lineage: String,
}

impl Default for LargestArchive {
    fn default() -> Self {
        Self {
            filename: String::new(),
            ortho_count: 0,
            lineage: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OptimalOrtho {
    pub volume: usize,
    pub dims: Vec<usize>,
    pub fullness: usize,
    pub capacity: usize,
    pub payload: Vec<Option<usize>>,
    pub vocab: Vec<String>,
}

impl Default for OptimalOrtho {
    fn default() -> Self {
        Self {
            volume: 0,
            dims: vec![],
            fullness: 0,
            capacity: 0,
            payload: vec![],
            vocab: vec![],
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub timestamp: u64,
    pub message: String,
}

pub struct Metrics {
    inner: Arc<Mutex<MetricsInner>>,
}

struct MetricsInner {
    global: GlobalMetrics,
    operation: OperationStatus,
    merge: MergeStatus,
    largest_archive: LargestArchive,
    optimal_ortho: OptimalOrtho,
    
    queue_depth_samples: VecDeque<MetricSample>,
    seen_size_samples: VecDeque<MetricSample>,
    results_count_samples: VecDeque<MetricSample>,
    optimal_volume_samples: VecDeque<MetricSample>,
    
    logs: VecDeque<LogEntry>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MetricsInner {
                global: GlobalMetrics::default(),
                operation: OperationStatus::default(),
                merge: MergeStatus::default(),
                largest_archive: LargestArchive::default(),
                optimal_ortho: OptimalOrtho::default(),
                queue_depth_samples: VecDeque::with_capacity(MAX_SAMPLES),
                seen_size_samples: VecDeque::with_capacity(MAX_SAMPLES),
                results_count_samples: VecDeque::with_capacity(MAX_SAMPLES),
                optimal_volume_samples: VecDeque::with_capacity(MAX_SAMPLES),
                logs: VecDeque::with_capacity(100),
            })),
        }
    }

    pub fn clone_handle(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }

    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    pub fn update_global(&self, update: impl FnOnce(&mut GlobalMetrics)) {
        let mut inner = self.inner.lock().unwrap();
        update(&mut inner.global);
    }

    pub fn update_operation(&self, update: impl FnOnce(&mut OperationStatus)) {
        let mut inner = self.inner.lock().unwrap();
        update(&mut inner.operation);
    }

    pub fn update_merge(&self, update: impl FnOnce(&mut MergeStatus)) {
        let mut inner = self.inner.lock().unwrap();
        update(&mut inner.merge);
    }

    pub fn update_largest_archive(&self, update: impl FnOnce(&mut LargestArchive)) {
        let mut inner = self.inner.lock().unwrap();
        update(&mut inner.largest_archive);
    }

    pub fn update_optimal_ortho(&self, update: impl FnOnce(&mut OptimalOrtho)) {
        let mut inner = self.inner.lock().unwrap();
        update(&mut inner.optimal_ortho);
    }

    pub fn record_queue_depth(&self, depth: usize) {
        self.record_sample(depth, |inner| &mut inner.queue_depth_samples);
    }

    pub fn record_seen_size(&self, size: usize) {
        self.record_sample(size, |inner| &mut inner.seen_size_samples);
    }

    pub fn record_results_count(&self, count: usize) {
        self.record_sample(count, |inner| &mut inner.results_count_samples);
    }

    pub fn record_optimal_volume(&self, volume: usize) {
        self.record_sample(volume, |inner| &mut inner.optimal_volume_samples);
    }

    fn record_sample<F>(&self, value: usize, getter: F)
    where
        F: FnOnce(&mut MetricsInner) -> &mut VecDeque<MetricSample>,
    {
        let mut inner = self.inner.lock().unwrap();
        let samples = getter(&mut *inner);
        
        let sample = MetricSample {
            timestamp: Self::current_timestamp(),
            value,
        };
        
        samples.push_back(sample);
        if samples.len() > MAX_SAMPLES {
            samples.pop_front();
        }
    }

    pub fn add_log(&self, message: String) {
        let mut inner = self.inner.lock().unwrap();
        let entry = LogEntry {
            timestamp: Self::current_timestamp(),
            message,
        };
        
        inner.logs.push_back(entry);
        if inner.logs.len() > 100 {
            inner.logs.pop_front();
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let inner = self.inner.lock().unwrap();
        MetricsSnapshot {
            global: inner.global.clone(),
            operation: inner.operation.clone(),
            merge: inner.merge.clone(),
            largest_archive: inner.largest_archive.clone(),
            optimal_ortho: inner.optimal_ortho.clone(),
            queue_depth_samples: inner.queue_depth_samples.iter().cloned().collect(),
            seen_size_samples: inner.seen_size_samples.iter().cloned().collect(),
            results_count_samples: inner.results_count_samples.iter().cloned().collect(),
            optimal_volume_samples: inner.optimal_volume_samples.iter().cloned().collect(),
            logs: inner.logs.iter().cloned().collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MetricsSnapshot {
    pub global: GlobalMetrics,
    pub operation: OperationStatus,
    pub merge: MergeStatus,
    pub largest_archive: LargestArchive,
    pub optimal_ortho: OptimalOrtho,
    pub queue_depth_samples: Vec<MetricSample>,
    pub seen_size_samples: Vec<MetricSample>,
    pub results_count_samples: Vec<MetricSample>,
    pub optimal_volume_samples: Vec<MetricSample>,
    pub logs: Vec<LogEntry>,
}
