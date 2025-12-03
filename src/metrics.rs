use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_SAMPLES: usize = 2000;
const DOWNSAMPLE_THRESHOLD: usize = 1500;

#[derive(Clone, Debug)]
pub struct MetricSample {
    pub timestamp: u64,
    pub value: usize,
}

#[derive(Clone, Debug)]
pub struct StatusHistoryEntry {
    pub status: String,
    pub start_time: u64,
    pub duration: u64,
}

#[derive(Clone, Debug, Default)]
pub struct StatusDurationStats {
    pub total_count: usize,
    pub total_duration: u64,
    pub min_duration: u64,
    pub max_duration: u64,
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
    pub current_lineage: String,
    pub queue_buffer_size: usize,
    pub bloom_capacity: usize,
    pub num_shards: usize,
    pub max_shards_in_memory: usize,
    pub queue_depth_pk: usize,
    pub seen_size_pk: usize,
    pub distinct_jobs_count: usize,
    pub ram_bytes: usize,
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
            current_lineage: String::new(),
            queue_buffer_size: 0,
            bloom_capacity: 0,
            num_shards: 0,
            max_shards_in_memory: 0,
            queue_depth_pk: 0,
            seen_size_pk: 0,
            distinct_jobs_count: 0,
            ram_bytes: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OperationStatus {
    pub current_file: String,
    pub status: String,
    pub status_start_time: u64,
    pub progress_current: usize,
    pub progress_total: usize,
    pub text_preview: String,
    pub word_count: usize,
    pub new_orthos: usize,
}

impl Default for OperationStatus {
    fn default() -> Self {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        Self {
            current_file: String::new(),
            status: "Idle".to_string(),
            status_start_time: start_time,
            progress_current: 0,
            progress_total: 0,
            text_preview: String::new(),
            word_count: 0,
            new_orthos: 0,
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
    pub new_orthos_from_merge: usize,
    pub text_preview_a: String,
    pub text_preview_b: String,
    pub word_count_a: usize,
    pub word_count_b: usize,
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
            new_orthos_from_merge: 0,
            text_preview_a: String::new(),
            text_preview_b: String::new(),
            word_count_a: 0,
            word_count_b: 0,
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
    pub last_update_time: u64,
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
            last_update_time: 0,
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
    seen_history_samples: VecDeque<MetricSample>,
    optimal_volume_samples: VecDeque<MetricSample>,
    
    status_history: VecDeque<StatusHistoryEntry>,
    status_duration_stats: StatusDurationStats,
    
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
                seen_history_samples: VecDeque::with_capacity(MAX_SAMPLES),
                optimal_volume_samples: VecDeque::with_capacity(MAX_SAMPLES),
                status_history: VecDeque::with_capacity(100),
                status_duration_stats: StatusDurationStats::default(),
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

    pub fn set_operation_status(&self, status: String) {
        let mut inner = self.inner.lock().unwrap();
        let now = Self::current_timestamp();
        let prev_start = inner.operation.status_start_time;
        let duration = now.saturating_sub(prev_start);
        
        // Record previous status if it had non-zero duration
        if duration > 0 && !inner.operation.status.is_empty() {
            let entry = StatusHistoryEntry {
                status: inner.operation.status.clone(),
                start_time: prev_start,
                duration,
            };
            inner.status_history.push_back(entry);
            if inner.status_history.len() > 100 {
                inner.status_history.pop_front();
            }
            
            // Update all-time statistics
            let stats = &mut inner.status_duration_stats;
            stats.total_count += 1;
            stats.total_duration += duration;
            if stats.total_count == 1 {
                stats.min_duration = duration;
                stats.max_duration = duration;
            } else {
                stats.min_duration = stats.min_duration.min(duration);
                stats.max_duration = stats.max_duration.max(duration);
            }
        }
        
        inner.operation.status = status;
        inner.operation.status_start_time = now;
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
        let mut inner = self.inner.lock().unwrap();
        if depth > inner.global.queue_depth_pk {
            inner.global.queue_depth_pk = depth;
        }
        drop(inner);
        self.record_sample(depth, |inner| &mut inner.queue_depth_samples);
    }

    pub fn record_seen_size(&self, size: usize) {
        let mut inner = self.inner.lock().unwrap();
        if size > inner.global.seen_size_pk {
            inner.global.seen_size_pk = size;
        }
        drop(inner);
        self.record_sample(size, |inner| &mut inner.seen_size_samples);
        self.record_sample(size, |inner| &mut inner.seen_history_samples);
    }

    pub fn reset_seen_history(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.seen_history_samples.clear();
    }

    pub fn reset_new_orthos(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.operation.new_orthos = 0;
    }

    pub fn increment_new_orthos(&self, count: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.operation.new_orthos = inner.operation.new_orthos.saturating_add(count);
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
        
        // When we exceed threshold, downsample by time-based bucketing
        // This preserves temporal distribution - always keeping oldest and newest data
        if samples.len() > DOWNSAMPLE_THRESHOLD {
            let target_size = DOWNSAMPLE_THRESHOLD / 2;
            let new_samples = Self::downsample_by_time(samples, target_size);
            *samples = new_samples;
        }
    }
    
    fn downsample_by_time(samples: &VecDeque<MetricSample>, target_size: usize) -> VecDeque<MetricSample> {
        if samples.len() <= target_size {
            return samples.clone();
        }
        
        let mut result = VecDeque::with_capacity(target_size);
        
        // Always keep first sample
        if let Some(first) = samples.front() {
            result.push_back(first.clone());
        }
        
        if target_size <= 2 {
            // Just keep first and last
            if let Some(last) = samples.back() {
                if result.len() < target_size {
                    result.push_back(last.clone());
                }
            }
            return result;
        }
        
        // Divide the timeline into buckets and take one sample per bucket
        let first_ts = samples.front().map(|s| s.timestamp).unwrap_or(0);
        let last_ts = samples.back().map(|s| s.timestamp).unwrap_or(0);
        let time_span = last_ts.saturating_sub(first_ts);
        
        if time_span == 0 {
            // All samples at same timestamp - just take evenly spaced by index
            let step = samples.len() / target_size;
            for i in (step..samples.len()).step_by(step.max(1)) {
                if result.len() < target_size - 1 {
                    result.push_back(samples[i].clone());
                }
            }
        } else {
            // Time-based bucketing
            let bucket_duration = time_span as f64 / (target_size - 1) as f64;
            
            for i in 1..target_size {
                let target_ts = first_ts + (i as f64 * bucket_duration) as u64;
                
                // Find closest sample to this timestamp
                let mut best_idx = 0;
                let mut best_diff = u64::MAX;
                
                for (idx, sample) in samples.iter().enumerate() {
                    let diff = if sample.timestamp >= target_ts {
                        sample.timestamp - target_ts
                    } else {
                        target_ts - sample.timestamp
                    };
                    
                    if diff < best_diff {
                        best_diff = diff;
                        best_idx = idx;
                    }
                }
                
                if best_idx < samples.len() && result.len() < target_size {
                    result.push_back(samples[best_idx].clone());
                }
            }
        }
        
        result
    }

    pub fn clear_chart_history(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.queue_depth_samples.clear();
        inner.seen_size_samples.clear();
        inner.optimal_volume_samples.clear();
        inner.global.queue_depth_pk = 0;
        inner.global.seen_size_pk = 0;
        // Note: seen_history_samples is NOT cleared - it persists across all chunks
    }

    pub fn reset_seen_size(&self, baseline: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.seen_size_samples.clear();
        inner.global.seen_size_pk = baseline;
        let timestamp = Self::current_timestamp();
        inner.seen_size_samples.push_back(MetricSample { timestamp, value: baseline });
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
            seen_history_samples: inner.seen_history_samples.iter().cloned().collect(),
            optimal_volume_samples: inner.optimal_volume_samples.iter().cloned().collect(),
            status_history: inner.status_history.iter().cloned().collect(),
            status_duration_stats: inner.status_duration_stats.clone(),
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
    pub seen_history_samples: Vec<MetricSample>,
    pub optimal_volume_samples: Vec<MetricSample>,
    pub status_history: Vec<StatusHistoryEntry>,
    pub status_duration_stats: StatusDurationStats,
    pub logs: Vec<LogEntry>,
}
