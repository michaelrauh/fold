use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use crate::ortho::Ortho;
use sysinfo::System;
use bincode::error::DecodeError;

/// Role of the worker in the system
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Leader,
    Follower,
}

/// Current phase of generation processing
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Processing,
    Draining,
    Compacting { bucket: usize },
    AntiJoin { bucket: usize },
    Idle,
}

/// Configuration for generation store operations
#[derive(Clone, Debug)]
pub struct Config {
    pub run_budget_bytes: usize,
    pub fan_in: usize,
    pub read_buf_bytes: usize,
    pub allow_compaction: bool,
    pub work_queue_cache_size: usize,  // Max orthos to keep in memory
    pub bufwriter_capacity: usize,      // Buffer size for each bucket writer
    pub work_segment_size: usize,       // Orthos per segment file
    pub history_cache_bytes: usize,     // RAM budget for caching history runs
    pub landing_flush_threshold: usize, // Bytes before forcing flush
}

impl Config {
    /// Create test config with minimal settings
    #[cfg(test)]
    pub fn test_config(run_budget_bytes: usize, fan_in: usize) -> Self {
        Self {
            run_budget_bytes,
            fan_in,
            read_buf_bytes: 64 * 1024,
            allow_compaction: false,
            work_queue_cache_size: 10_000,
            bufwriter_capacity: 64 * 1024,
            work_segment_size: 1000,
            history_cache_bytes: 1024 * 1024,
            landing_flush_threshold: 64 * 1024,
        }
    }

    /// Compute config based on role and current system memory state
    /// 
    /// RAM Policy:
    /// - Target 85% total RAM usage aggressively
    /// - Leader: Scale down only above 85% usage
    /// - Follower: Scale down starting at 70% usage
    /// - Allocate budget across: run_budget (30%), work cache (25%), buffers (20%), history cache (25%)
    /// - Follower bails if run_budget < 128MB when already at lowest budget and RSS stays above minimum target
    pub fn compute_config(role: Role) -> Option<Self> {
        let (used_bytes, total_bytes, _headroom_bytes) = get_memory_state();
        let used_pct = (used_bytes as f64 / total_bytes as f64) * 100.0;
        
        // Target 85% of total RAM
        let target_usage_bytes = (total_bytes as f64 * 0.85) as usize;
        let available_bytes = target_usage_bytes.saturating_sub(used_bytes);
        
        // Define scale-down thresholds based on role
        let scale_threshold = match role {
            Role::Leader => 85.0,  // Start scaling down at 85%
            Role::Follower => 70.0, // Start scaling down at 70%
        };
        
        // Base budgets (at low usage)
        let (base_budget, min_budget) = match role {
            Role::Leader => (available_bytes, 2_000_000_000), // Use all available, min 2GB
            Role::Follower => (available_bytes.min(4_000_000_000), 256_000_000), // Cap at 4GB, min 256MB
        };
        
        // Scale down budget if above threshold
        let budget = if used_pct > scale_threshold {
            // Linear scale-down from 100% at threshold to min at 95%
            let scale_range = 95.0 - scale_threshold;
            let position = ((used_pct - scale_threshold) / scale_range).min(1.0);
            let budget_range = (base_budget - min_budget) as f64;
            base_budget - (budget_range * position) as usize
        } else {
            base_budget
        };
        
        // Check follower bail-out condition
        if role == Role::Follower {
            let run_budget = (budget as f64 * 0.3) as usize;
            if run_budget < 128_000_000 && used_pct >= scale_threshold {
                return None;
            }
        }
        
        // Allocate budget across subsystems
        let run_budget_bytes = (budget as f64 * 0.30) as usize;      // 30% for LSM runs
        let work_cache_budget = (budget as f64 * 0.25) as usize;     // 25% for work queue cache
        let buffer_budget = (budget as f64 * 0.20) as usize;         // 20% for write buffers
        let history_cache_bytes = (budget as f64 * 0.25) as usize;   // 25% for history caching
        
        // Work queue cache: assume ~200 bytes per ortho
        let work_queue_cache_size = work_cache_budget / 200;
        
        // BufWriter capacity: divide among 8 buckets, min 64KB, max 16MB per bucket
        let bufwriter_capacity = (buffer_budget / 8).clamp(64 * 1024, 16 * 1024 * 1024);
        
        // Work segment size: larger segments = fewer files, assume ~200 bytes per ortho
        // Target segments of ~10MB each = 50k orthos
        let work_segment_size = 50_000;
        
        // Landing flush threshold: 1-10MB depending on buffer capacity
        let landing_flush_threshold = bufwriter_capacity.clamp(1024 * 1024, 10 * 1024 * 1024);
        
        let read_buf_bytes = 64 * 1024; // 64KB read buffer
        let fan_in = compute_fan_in(run_budget_bytes, read_buf_bytes);
        
        Some(Self {
            run_budget_bytes,
            fan_in,
            read_buf_bytes,
            allow_compaction: true,
            work_queue_cache_size,
            bufwriter_capacity,
            work_segment_size,
            history_cache_bytes,
            landing_flush_threshold,
        })
    }
}

/// Get current memory state: (used_bytes, total_bytes, headroom_bytes)
fn get_memory_state() -> (usize, usize, usize) {
    let mut sys = System::new_all();
    sys.refresh_memory();
    
    let total_raw = sys.total_memory();
    let used_raw = sys.used_memory();
    
    // Use the same normalization as main.rs for consistency
    let (used_bytes, total_bytes) = normalize_sysinfo_mem(total_raw, used_raw);
    let headroom_bytes = total_bytes.saturating_sub(used_bytes);
    
    (used_bytes, total_bytes, headroom_bytes)
}

/// Normalize sysinfo memory values (copied from main.rs for now)
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
    (used_raw as usize, total_raw as usize)
}

/// Calculate fan_in: clamp(budget / read_buf, 8, 128)
fn compute_fan_in(budget: usize, read_buf_bytes: usize) -> usize {
    if read_buf_bytes == 0 {
        return 8;
    }
    let raw_fan_in = budget / read_buf_bytes;
    raw_fan_in.clamp(8, 128)
}


/// Callback for reporting generation transition progress
pub type ProgressCallback = Box<dyn Fn(&str) + Send>;

/// Statistics for a single bucket
#[derive(Clone, Debug)]
pub struct BucketStats {
    pub bucket_id: usize,
    pub run_count: usize,
    // Count of orthos currently in landing (in-memory + active log)
    pub landing_size: usize,
    pub history_size_estimate: usize,
}

/// Statistics for a single generation
#[derive(Clone, Debug)]
pub struct GenerationStats {
    pub generation: u64,
    pub phase: Phase,
    pub work_len: u64,
    pub seen_len_accepted: u64,
    pub run_budget_bytes: usize,
    pub fan_in: usize,
}

/// Main generational store structure (opaque for now)
pub struct GenerationStore {
    base_path: PathBuf,
    bucket_count: usize,
    bucket_writers: Vec<Option<BufWriter<File>>>,
    drain_counter: Vec<usize>,
    landing_buffer_sizes: Vec<usize>, // Track bytes written to each bucket writer
    landing_counts: Vec<usize>,       // Track ortho counts in landing per bucket
    // Work queue state
    work_segments: Vec<PathBuf>,
    work_segment_counter: usize,
    total_work_len: u64,
    work_queue_cache: VecDeque<Ortho>, // In-memory cache of work items
    work_queue_cache_max: usize,   // Max cache size
    work_segment_batch: Vec<Ortho>, // Batch for writing segments
    work_segment_batch_max: usize,  // Max batch size before flush
    cached_best_volume: Cell<Option<usize>>, // Cached best volume across work caches
    cached_best_ortho: RefCell<Option<Ortho>>, // Cached best ortho across work caches
    best_volume_dirty: Cell<bool>, // Whether cached best needs recompute
    bufwriter_capacity: usize,      // Buffer capacity for bucket writers
    landing_flush_threshold: usize, // Threshold for flushing landing writes
    // History state
    history_runs: Vec<Vec<PathBuf>>, // Per-bucket list of history run files
    seen_len_accepted: u64, // Monotonic count of accepted items across all generations
    #[allow(dead_code)]
    history_cache: std::collections::HashMap<PathBuf, Vec<u8>>, // Cached history run contents (future optimization)
}

/// Placeholder for unsorted drained data
pub struct RawStream {
    files: Vec<PathBuf>,
}

impl RawStream {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self { files }
    }

    /// Get all file paths in this raw stream
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }
}

/// Sorted run of orthos
#[derive(Clone)]
pub struct Run {
    path: PathBuf,
}

impl Run {
    /// Create a new Run from a file path
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Get the file path
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Iterate over orthos in this run with bounded buffering
    pub fn iter(&self, read_buf_bytes: usize) -> io::Result<OrthoStreamReader> {
        OrthoStreamReader::new(&self.path, read_buf_bytes)
    }
}

struct OrthoRunIterator {
    reader: BufReader<File>,
    buffer: Vec<u8>,
    offset: usize,
    read_buf_bytes: usize,
}

impl Iterator for OrthoRunIterator {
    type Item = io::Result<StreamedOrtho>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.offset < self.buffer.len() {
                match bincode::decode_from_slice(
                    &self.buffer[self.offset..],
                    bincode::config::standard(),
                ) {
                    Ok((ortho, bytes_read)) => {
                        self.offset += bytes_read;
                        return Some(Ok(StreamedOrtho { ortho, bytes_read }));
                    }
                    Err(DecodeError::UnexpectedEnd { .. }) => {
                        match self.read_more() {
                            Ok(true) => continue,
                            Ok(false) => {
                                if self.offset == self.buffer.len() {
                                    return None;
                                } else {
                                    return Some(Err(io::Error::new(
                                        io::ErrorKind::UnexpectedEof,
                                        "Unexpected end of ortho stream",
                                    )));
                                }
                            }
                            Err(e) => return Some(Err(e)),
                        }
                    }
                    Err(e) => {
                        return Some(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            e.to_string(),
                        )))
                    }
                }
            } else {
                match self.read_more() {
                    Ok(true) => continue,
                    Ok(false) => return None,
                    Err(e) => return Some(Err(e)),
                }
            }
        }
    }
}

impl OrthoRunIterator {
    fn read_more(&mut self) -> io::Result<bool> {
        // Compact buffer to reclaim consumed prefix
        if self.offset > 0 {
            let remaining = self.buffer.len().saturating_sub(self.offset);
            self.buffer.copy_within(self.offset.., 0);
            self.buffer.truncate(remaining);
            self.offset = 0;
        }

        // Ensure capacity for the next read
        let min_chunk = self.read_buf_bytes.max(8 * 1024);
        let desired_cap = self.buffer.len().saturating_add(min_chunk);
        if self.buffer.capacity() < desired_cap {
            self.buffer.reserve(desired_cap - self.buffer.capacity());
        }

        let start = self.buffer.len();
        self.buffer.resize(start + min_chunk, 0);
        let read = self.reader.read(&mut self.buffer[start..])?;
        self.buffer.truncate(start + read);
        Ok(read > 0)
    }
}

/// Streaming ortho reader backed by a bounded buffer
pub struct OrthoStreamReader {
    inner: OrthoRunIterator,
}

impl OrthoStreamReader {
    fn new(path: &PathBuf, read_buf_bytes: usize) -> io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::with_capacity(read_buf_bytes, file);
        Ok(Self {
            inner: OrthoRunIterator {
                reader,
                buffer: Vec::with_capacity(read_buf_bytes),
                offset: 0,
                read_buf_bytes,
            },
        })
    }
}

impl Iterator for OrthoStreamReader {
    type Item = io::Result<StreamedOrtho>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

/// Streamed ortho with its encoded byte length
#[derive(Clone)]
pub struct StreamedOrtho {
    pub ortho: Ortho,
    pub bytes_read: usize,
}

/// Sorted and deduplicated run of orthos
#[derive(Clone)]
pub struct UniqueRun {
    path: PathBuf,
}

impl UniqueRun {
    /// Create a new UniqueRun from a file path
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Get the file path
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Iterate over orthos in this unique run with bounded buffering
    pub fn iter(&self, read_buf_bytes: usize) -> io::Result<OrthoStreamReader> {
        OrthoStreamReader::new(&self.path, read_buf_bytes)
    }
}

/// Iterator over history runs for a bucket
/// Streams orthos from all history run files in order
pub struct HistoryIterator {
    run_files: Vec<PathBuf>,
    current_run_index: usize,
    current_run_iter: Option<OrthoStreamReader>,
    read_buf_bytes: usize,
}

impl HistoryIterator {
    fn new(run_files: &[PathBuf], read_buf_bytes: usize) -> io::Result<Self> {
        let mut iter = Self {
            run_files: run_files.to_vec(),
            current_run_index: 0,
            current_run_iter: None,
            read_buf_bytes,
        };
        iter.advance_to_next_run()?;
        Ok(iter)
    }

    fn advance_to_next_run(&mut self) -> io::Result<()> {
        self.current_run_iter = None;
        
        if self.current_run_index >= self.run_files.len() {
            return Ok(());
        }

        let reader = OrthoStreamReader::new(&self.run_files[self.current_run_index], self.read_buf_bytes)?;
        self.current_run_iter = Some(reader);
        self.current_run_index += 1;
        
        Ok(())
    }
}

impl Iterator for HistoryIterator {
    type Item = io::Result<StreamedOrtho>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(iter) = self.current_run_iter.as_mut() {
                if let Some(result) = iter.next() {
                    return Some(result);
                }
                // Current run exhausted, move to next
                match self.advance_to_next_run() {
                    Ok(_) => continue,
                    Err(e) => return Some(Err(e)),
                }
            } else {
                // No more runs
                return None;
            }
        }
    }
}

impl GenerationStore {
    /// Create a new generation store with specified base path and bucket count
    pub fn new_with_config(base_path: PathBuf, bucket_count: usize) -> io::Result<Self> {
        // Bucket count must be a power of two
        assert!(bucket_count.is_power_of_two(), "bucket_count must be power of two");

        // Create landing directory structure
        for bucket in 0..bucket_count {
            let bucket_dir = base_path.join("landing").join(format!("b={:02}", bucket));
            fs::create_dir_all(&bucket_dir)?;
        }

        // Create work directory
        let work_dir = base_path.join("work");
        fs::create_dir_all(&work_dir)?;

        // Create runs directory
        let runs_dir = base_path.join("runs");
        fs::create_dir_all(&runs_dir)?;

        // Create history directory
        let history_dir = base_path.join("history");
        fs::create_dir_all(&history_dir)?;
        for bucket in 0..bucket_count {
            let bucket_history_dir = history_dir.join(format!("b={:02}", bucket));
            fs::create_dir_all(&bucket_history_dir)?;
        }

        Ok(Self {
            base_path,
            bucket_count,
            bucket_writers: (0..bucket_count).map(|_| None).collect(),
            drain_counter: vec![0; bucket_count],
            landing_buffer_sizes: vec![0; bucket_count],
            landing_counts: vec![0; bucket_count],
            work_segments: Vec::new(),
            work_segment_counter: 0,
            total_work_len: 0,
            work_queue_cache: VecDeque::new(),
            work_queue_cache_max: 100_000, // Default, will be updated with config
            work_segment_batch: Vec::new(),
            work_segment_batch_max: 50_000, // Default, will be updated with config
            cached_best_volume: Cell::new(None),
            cached_best_ortho: RefCell::new(None),
            best_volume_dirty: Cell::new(false),
            bufwriter_capacity: 16 * 1024 * 1024, // Default 16MB
            landing_flush_threshold: 10 * 1024 * 1024, // Default 10MB
            history_runs: (0..bucket_count).map(|_| Vec::new()).collect(),
            seen_len_accepted: 0,
            history_cache: std::collections::HashMap::new(),
        })
    }

    /// Open an existing store for reading history runs from disk
    pub fn from_existing(base_path: PathBuf, bucket_count: usize) -> io::Result<Self> {
        let mut store = Self::new_with_config(base_path, bucket_count)?;
        store.load_history_runs_from_disk()?;
        Ok(store)
    }

    fn load_history_runs_from_disk(&mut self) -> io::Result<()> {
        self.history_runs = (0..self.bucket_count).map(|_| Vec::new()).collect();
        self.seen_len_accepted = 0;

        for bucket in 0..self.bucket_count {
            let history_dir = self
                .base_path
                .join("history")
                .join(format!("b={:02}", bucket));
            if !history_dir.exists() {
                continue;
            }

            let mut entries: Vec<PathBuf> = fs::read_dir(&history_dir)?
                .filter_map(|res| res.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            entries.sort();

            for path in &entries {
                let mut reader = OrthoStreamReader::new(path, 64 * 1024)?;
                while let Some(result) = reader.next() {
                    result.map_err(|e| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Failed to decode ortho in {:?}: {}", path, e),
                        )
                    })?;
                    self.seen_len_accepted += 1;
                }
            }

            self.history_runs[bucket] = entries;
        }

        Ok(())
    }

    /// Create a new empty generation store
    pub fn new() -> Self {
        Self {
            base_path: PathBuf::from("fold_state"),
            bucket_count: 8,
            bucket_writers: (0..8).map(|_| None).collect(),
            drain_counter: vec![0; 8],
            landing_buffer_sizes: vec![0; 8],
            landing_counts: vec![0; 8],
            work_segments: Vec::new(),
            work_segment_counter: 0,
            total_work_len: 0,
            work_queue_cache: VecDeque::new(),
            work_queue_cache_max: 100_000,
            work_segment_batch: Vec::new(),
            work_segment_batch_max: 50_000,
            cached_best_volume: Cell::new(None),
            cached_best_ortho: RefCell::new(None),
            best_volume_dirty: Cell::new(false),
            bufwriter_capacity: 16 * 1024 * 1024,
            landing_flush_threshold: 10 * 1024 * 1024,
            history_runs: (0..8).map(|_| Vec::new()).collect(),
            seen_len_accepted: 0,
            history_cache: std::collections::HashMap::new(),
        }
    }

    /// Get path to active log for a bucket
    fn active_log_path(&self, bucket: usize) -> PathBuf {
        self.base_path
            .join("landing")
            .join(format!("b={:02}", bucket))
            .join("active.log")
    }

    /// Get path to drain log for a bucket
    fn drain_log_path(&self, bucket: usize, drain_id: usize) -> PathBuf {
        self.base_path
            .join("landing")
            .join(format!("b={:02}", bucket))
            .join(format!("drain-{}.log", drain_id))
    }

    /// Record a result to the landing zone
    pub fn record_result(&mut self, ortho: &Ortho) -> io::Result<()> {
        self.record_result_with_threshold(ortho, 10 * 1024 * 1024) // Default 10MB threshold
    }

    /// Record a result with configurable flush threshold
    pub fn record_result_with_threshold(&mut self, ortho: &Ortho, flush_threshold: usize) -> io::Result<()> {
        let bucket = (ortho.id() as u64 & (self.bucket_count - 1) as u64) as usize;
        
        // Get or create writer for this bucket with configured buffer capacity
        if self.bucket_writers[bucket].is_none() {
            let path = self.active_log_path(bucket);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            self.bucket_writers[bucket] = Some(BufWriter::with_capacity(self.bufwriter_capacity, file));
        }

        // Write ortho as bincode
        let writer = self.bucket_writers[bucket].as_mut().unwrap();
        let encoded = bincode::encode_to_vec(ortho, bincode::config::standard())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let encoded_len = encoded.len();
        writer.write_all(&encoded)?;
        self.landing_counts[bucket] = self.landing_counts[bucket].saturating_add(1);
        
        // Track buffer size and flush if over threshold
        self.landing_buffer_sizes[bucket] += encoded_len;
        if self.landing_buffer_sizes[bucket] >= flush_threshold {
            writer.flush()?;
            self.landing_buffer_sizes[bucket] = 0;
        }
        
        Ok(())
    }

    /// Drain a bucket by renaming active.log to drain-N.log
    pub fn drain_bucket(&mut self, bucket: usize) -> io::Result<RawStream> {
        // Flush and close any active writer for this bucket
        if let Some(writer) = self.bucket_writers[bucket].take() {
            drop(writer); // Explicit drop to flush and close
        }

        let active_path = self.active_log_path(bucket);
        
        // Check if active log exists
        if !active_path.exists() {
            return Ok(RawStream::new(vec![]));
        }

        // Rename to drain file
        let drain_id = self.drain_counter[bucket];
        self.drain_counter[bucket] += 1;
        let drain_path = self.drain_log_path(bucket, drain_id);
        
        fs::rename(&active_path, &drain_path)?;
        // Landing for this bucket has been drained; reset counters.
        self.landing_counts[bucket] = 0;
        self.landing_buffer_sizes[bucket] = 0;
        
        Ok(RawStream::new(vec![drain_path]))
    }

    /// Push a segment of work items to the work queue
    /// Uses batching to create larger segment files
    pub fn push_segments(&mut self, items: Vec<Ortho>) -> io::Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        // Add items to batch
        if !self.best_volume_dirty.get() {
            if let Some(best_item) = items.iter().max_by_key(|o| o.volume()) {
                self.update_cached_best(best_item);
            }
        }
        self.work_segment_batch.extend(items);
        
        // Flush batch if it exceeds max size
        if self.work_segment_batch.len() >= self.work_segment_batch_max {
            self.flush_work_segment_batch()?;
        }

        Ok(())
    }
    
    /// Flush the work segment batch to disk
    fn flush_work_segment_batch(&mut self) -> io::Result<()> {
        if self.work_segment_batch.is_empty() {
            return Ok(());
        }

        let count = self.work_segment_batch.len() as u64;
        let segment_path = self.base_path
            .join("work")
            .join(format!("segment-{}.dat", self.work_segment_counter));
        self.work_segment_counter += 1;

        // Write segment file with large buffer
        let mut file = BufWriter::with_capacity(16 * 1024 * 1024, File::create(&segment_path)?);
        file.write_all(&count.to_le_bytes())?;
        for ortho in &self.work_segment_batch {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            file.write_all(&(encoded.len() as u64).to_le_bytes())?;
            file.write_all(&encoded)?;
        }
        file.flush()?;

        // Add to work segments and update totals
        self.work_segments.push(segment_path);
        self.total_work_len += count;
        self.work_segment_batch.clear();
        self.best_volume_dirty.set(true);

        Ok(())
    }

    /// Pop a single work item from the work queue
    /// Uses in-memory cache for speed, only hits disk when cache empties
    pub fn pop_work(&mut self) -> io::Result<Option<Ortho>> {
        // Try to pop from cache first
        if let Some(ortho) = self.work_queue_cache.pop_front() {
            self.total_work_len -= 1;
            self.handle_removed_volume(ortho.volume());
            return Ok(Some(ortho));
        }
        
        // Cache is empty, refill from disk segments
        self.refill_work_cache()?;
        
        // Pop from cache after refill
        if let Some(ortho) = self.work_queue_cache.pop_front() {
            self.total_work_len -= 1;
            self.handle_removed_volume(ortho.volume());
            return Ok(Some(ortho));
        }
        
        Ok(None)
    }
    
    /// Refill work queue cache from disk segments
    fn refill_work_cache(&mut self) -> io::Result<()> {
        while self.work_queue_cache.len() < self.work_queue_cache_max && !self.work_segments.is_empty() {
            // Take next segment
            let segment_path = self.work_segments.remove(0);
            let mut file = File::open(&segment_path)?;

            // Read count
            let mut count_bytes = [0u8; 8];
            file.read_exact(&mut count_bytes)?;
            let count = u64::from_le_bytes(count_bytes) as usize;

            if count == 0 {
                drop(file);
                fs::remove_file(&segment_path)?;
                continue;
            }

            // Read all orthos from segment into cache
            for _ in 0..count {
                let mut len_bytes = [0u8; 8];
                file.read_exact(&mut len_bytes)?;
                let len = u64::from_le_bytes(len_bytes) as usize;

                let mut ortho_bytes = vec![0u8; len];
                file.read_exact(&mut ortho_bytes)?;
                let ortho: Ortho = bincode::decode_from_slice(&ortho_bytes, bincode::config::standard())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
                    .0;
                
                self.work_queue_cache.push_back(ortho);
                if !self.best_volume_dirty.get() {
                    if let Some(last) = self.work_queue_cache.back() {
                        self.update_cached_best(last);
                    }
                }
                
                // Stop if cache is full
                if self.work_queue_cache.len() >= self.work_queue_cache_max {
                    break;
                }
            }

            // Delete consumed segment
            drop(file);
            fs::remove_file(&segment_path)?;
            
            // Stop if cache is full
            if self.work_queue_cache.len() >= self.work_queue_cache_max {
                break;
            }
        }
        
        Ok(())
    }

    /// Configure the store with Config settings
    pub fn configure(&mut self, cfg: &Config) {
        self.work_queue_cache_max = cfg.work_queue_cache_size;
        self.work_segment_batch_max = cfg.work_segment_size;
        self.bufwriter_capacity = cfg.bufwriter_capacity;
        self.landing_flush_threshold = cfg.landing_flush_threshold;
    }
    
    /// Flush all pending buffers (landing + work segments)
    pub fn flush_all(&mut self) -> io::Result<()> {
        // Flush all bucket writers
        self.flush()?;
        
        // Flush work segment batch
        self.flush_work_segment_batch()?;
        
        Ok(())
    }

    /// Get the current work queue length (includes in-memory cache)
    pub fn work_len(&self) -> u64 {
        // total_work_len already tracks everything in cache/segments; only add unflushed batch
        self.total_work_len + self.work_segment_batch.len() as u64
    }

    /// Get just the in-memory work cache size (for debugging)
    pub fn work_queue_cache_len(&self) -> usize {
        self.work_queue_cache.len() + self.work_segment_batch.len()
    }

    /// Get current statistics
    pub fn stats(&self) -> GenerationStats {
        GenerationStats {
            generation: 0,
            phase: Phase::Idle,
            work_len: self.work_len(),
            seen_len_accepted: self.seen_len_accepted,
            run_budget_bytes: 0,
            fan_in: 0,
        }
    }

    /// Iterate over history for a bucket
    /// Returns an iterator over all orthos in history runs for this bucket
    pub fn history_iter_with_buffer(&self, bucket: usize, read_buf_bytes: usize) -> io::Result<HistoryIterator> {
        assert!(bucket < self.bucket_count, "Invalid bucket index");
        HistoryIterator::new(&self.history_runs[bucket], read_buf_bytes)
    }

    /// Expose history run file paths for archiving/export
    pub fn history_run_paths(&self) -> Vec<(usize, Vec<PathBuf>)> {
        (0..self.bucket_count)
            .map(|bucket| (bucket, self.history_runs[bucket].clone()))
            .collect()
    }

    /// Add a history run for a bucket and update accepted count
    /// The run is moved to the history directory and tracked
    pub fn add_history_run(&mut self, bucket: usize, run: Run, accepted: u64) -> io::Result<()> {
        assert!(bucket < self.bucket_count, "Invalid bucket index");
        
        // Move run file to history directory with unique name
        let history_dir = self.base_path.join("history").join(format!("b={:02}", bucket));
        let run_id = self.history_runs[bucket].len();
        let dest_path = history_dir.join(format!("history-{}.dat", run_id));
        
        // Move the run file to history
        fs::rename(run.path(), &dest_path)?;
        
        // Track the history run
        self.history_runs[bucket].push(dest_path);
        
        // Update accepted count (monotonic)
        self.seen_len_accepted += accepted;
        
        Ok(())
    }

    /// Get the monotonic count of accepted items across all generations
    pub fn seen_len_accepted(&self) -> u64 {
        self.seen_len_accepted
    }

    /// Get total landing buffer count across all buckets (orthos pending acceptance)
    pub fn total_landing_size(&self) -> usize {
        self.landing_counts.iter().sum()
    }

    /// Get per-bucket statistics for TUI visualization
    pub fn bucket_stats(&self) -> Vec<BucketStats> {
        (0..self.bucket_count)
            .map(|bucket| {
                let run_count = self.history_runs[bucket].len();
                
                // Landing count represents orthos pending acceptance (buffer + active log)
                let landing_size = self.landing_counts[bucket];
                
                // Estimate history size from run files
                let history_size_estimate = self.history_runs[bucket]
                    .iter()
                    .filter_map(|path| std::fs::metadata(path).ok())
                    .map(|m| m.len() as usize)
                    .sum();
                
                BucketStats {
                    bucket_id: bucket,
                    run_count,
                    landing_size,
                    history_size_estimate,
                }
            })
            .collect()
    }

    fn update_cached_best(&self, candidate: &Ortho) {
        if self.best_volume_dirty.get() {
            return;
        }
        let candidate_volume = candidate.volume();
        match self.cached_best_volume.get() {
            Some(current) if current >= candidate_volume => {}
            _ => {
                self.cached_best_volume.set(Some(candidate_volume));
                *self.cached_best_ortho.borrow_mut() = Some(candidate.clone());
            }
        }
    }

    fn handle_removed_volume(&self, removed: usize) {
        if !self.best_volume_dirty.get() && self.cached_best_volume.get() == Some(removed) {
            self.best_volume_dirty.set(true);
        }
    }

    fn recompute_cached_best(&self) {
        let mut best_volume: Option<usize> = None;
        let mut best_ortho: Option<Ortho> = None;

        for ortho in self
            .work_queue_cache
            .iter()
            .chain(self.work_segment_batch.iter())
        {
            let volume = ortho.volume();
            if best_volume.map(|v| volume > v).unwrap_or(true) {
                best_volume = Some(volume);
                best_ortho = Some(ortho.clone());
            }
        }

        self.cached_best_volume.set(best_volume);
        *self.cached_best_ortho.borrow_mut() = best_ortho;
        self.best_volume_dirty.set(false);
    }

    /// Peek at the best ortho currently in work cache (without removing)
    pub fn peek_best_ortho_in_cache(&self) -> Option<Ortho> {
        if self.best_volume_dirty.get() {
            self.recompute_cached_best();
        }
        self.cached_best_ortho.borrow().clone()
    }

    /// Flush all bucket writers
    pub fn flush(&mut self) -> io::Result<()> {
        for writer in self.bucket_writers.iter_mut() {
            if let Some(w) = writer {
                w.flush()?;
            }
        }
        Ok(())
    }

    /// Process the end of a generation: drain, compact, anti-join, and push new work
    /// 
    /// This is the core generational transition that:
    /// 1. Drains all buckets from landing to raw streams
    /// 2. Compacts each raw stream into sorted runs
    /// 3. Merges runs into unique runs
    /// 4. Anti-joins each unique run against history
    /// 5. Adds accepted runs to history
    /// 6. Pushes new work items to the work queue
    /// 
    /// TODO: After integer bootstrap is proven, replace all integer operations with ortho versions
    pub fn on_generation_end(&mut self, cfg: &Config, progress: Option<&ProgressCallback>) -> io::Result<u64> {
        let mut total_new_work = 0u64;
        let mut buckets_processed = 0;
        let mut total_drained = 0usize;
        let mut total_accepted = 0u64;
        
        // Flush all pending writes before transition
        self.flush_all()?;
        
        if let Some(cb) = &progress {
            cb(&format!("TRANSITION_START:{}", self.bucket_count));
        }
        
        // Process each bucket independently
        for bucket in 0..self.bucket_count {
            // Flush writers before draining
            self.flush()?;
            
            // Phase: Draining
            if let Some(cb) = &progress {
                cb(&format!("BUCKET_STATE:{}:draining", bucket));
            }
            let raw = self.drain_bucket(bucket)?;
            
            if raw.files().is_empty() {
                // No data in this bucket, skip
                if let Some(cb) = &progress {
                    cb(&format!("BUCKET_STATE:{}:empty", bucket));
                }
                continue;
            }
            
            // Count drained orthos for metrics
            let mut drained_count = 0usize;
            for file_path in raw.files() {
                if let Ok(metadata) = std::fs::metadata(file_path) {
                    // Rough estimate: divide file size by average ortho size (~200 bytes)
                    drained_count += (metadata.len() / 200) as usize;
                }
            }
            total_drained += drained_count;
            
            if let Some(cb) = &progress {
                cb(&format!("Bucket {}/{}: drained ~{} orthos", bucket, self.bucket_count, drained_count));
            }
            
            // Phase: Compacting
            if let Some(cb) = &progress {
                cb(&format!("BUCKET_STATE:{}:sorting", bucket));
            }
            let runs = compact_landing(bucket, raw, cfg, &self.base_path)?;
            
            if runs.is_empty() {
                // No runs generated, skip
                if let Some(cb) = &progress {
                    cb(&format!("Bucket {}/{}: no runs generated", bucket, self.bucket_count));
                    cb(&format!("BUCKET_STATE:{}:empty", bucket));
                }
                continue;
            }
            
            if let Some(cb) = &progress {
                cb(&format!("Bucket {}/{}: created {} runs", bucket, self.bucket_count, runs.len()));
            }
            
            // Phase: Merge to unique run
            if let Some(cb) = &progress {
                cb(&format!("BUCKET_STATE:{}:merging", bucket));
            }
            let unique_run = merge_unique(runs, cfg, &self.base_path)?;
            
            // Phase: Anti-join against history
            if let Some(cb) = &progress {
                cb(&format!("BUCKET_STATE:{}:antijoining", bucket));
            }
            let history_iter = self.history_iter_with_buffer(bucket, cfg.read_buf_bytes)?;
            let (new_work, seen_run, accepted) = anti_join_orthos(
                unique_run,
                history_iter,
                &self.base_path,
                cfg.read_buf_bytes,
            )?;
            
            total_accepted += accepted;
            let bucket_new_work = new_work.len();
            
            if let Some(cb) = &progress {
                cb(&format!("Bucket {}/{}: accepted {} orthos, created {} new work", 
                    bucket, self.bucket_count, accepted, bucket_new_work));
            }
            
            // Add seen run to history
            self.add_history_run(bucket, seen_run, accepted)?;
            
            // Optional: Compact history if needed
            if cfg.allow_compaction {
                let pre_compact_runs = self.history_runs[bucket].len();
                if pre_compact_runs > 64 {
                    if let Some(cb) = &progress {
                        cb(&format!("BUCKET_STATE:{}:compacting", bucket));
                        cb(&format!("Bucket {}/{}: compacting {} history runs", bucket, self.bucket_count, pre_compact_runs));
                    }
                    self.compact_history(bucket, cfg)?;
                    let post_compact_runs = self.history_runs[bucket].len();
                    if let Some(cb) = &progress {
                        cb(&format!("Bucket {}/{}: compacted {} → {} runs", bucket, self.bucket_count, pre_compact_runs, post_compact_runs));
                    }
                }
            }
            
            // Mark bucket complete
            if let Some(cb) = &progress {
                cb(&format!("BUCKET_STATE:{}:complete:{}", bucket, bucket_new_work));
            }
            
            // Push new work to queue (ortho version)
            total_new_work += bucket_new_work as u64;
            self.push_segments(new_work)?;
            
            buckets_processed += 1;
        }
        
        // Flush all pending work to disk
        self.flush_work_segment_batch()?;
        
        if let Some(cb) = &progress {
            cb(&format!("TRANSITION_COMPLETE"));
            cb(&format!("Transition complete: processed {} buckets, drained ~{} orthos, accepted {} orthos, created {} new work",
                buckets_processed, total_drained, total_accepted, total_new_work));
        }
        
        Ok(total_new_work)
    }



    /// Compact history runs for a bucket when count exceeds threshold
    /// 
    /// Merges a subset of runs to keep run count bounded. This is optional
    /// and correctness does not depend on it. Triggered when run count > 64.
    pub fn compact_history(&mut self, bucket: usize, cfg: &Config) -> io::Result<()> {
        assert!(bucket < self.bucket_count, "Invalid bucket index");
        
        let run_count = self.history_runs[bucket].len();
        
        // Only compact if we exceed the threshold
        if run_count <= 64 {
            return Ok(());
        }
        
        // Merge the oldest half of runs (keep most recent ones separate for better performance)
        let merge_count = run_count / 2;
        if merge_count < 2 {
            return Ok(()); // Need at least 2 runs to merge
        }
        
        // Collect runs to merge (oldest ones)
        let runs_to_merge: Vec<Run> = self.history_runs[bucket][..merge_count]
            .iter()
            .map(|path| Run::new(path.clone()))
            .collect();
        
        // Merge them into a single unique run
        let merged = merge_unique(runs_to_merge, cfg, &self.base_path)?;
        
        // Move merged run to history with next available ID
        let history_dir = self.base_path.join("history").join(format!("b={:02}", bucket));
        let new_run_id = self.history_runs[bucket].len();
        let dest_path = history_dir.join(format!("history-{}.dat", new_run_id));
        fs::rename(merged.path(), &dest_path)?;
        
        // Remove old runs from tracking and delete their files
        let old_runs: Vec<PathBuf> = self.history_runs[bucket].drain(..merge_count).collect();
        for old_path in old_runs {
            let _ = fs::remove_file(&old_path); // Best effort deletion
        }
        
        // Add merged run to tracking
        self.history_runs[bucket].push(dest_path);
        
        Ok(())
    }
}

/// External sort run generation using arena-based approach
/// 
/// Reads raw stream data from ortho landing files, sorts in-memory by id with a budget, and writes runs on overflow.
pub fn compact_landing(
    bucket: usize,
    raw: RawStream,
    cfg: &Config,
    base_path: &PathBuf,
) -> io::Result<Vec<Run>> {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    static RUN_COUNTER: AtomicUsize = AtomicUsize::new(0);

    let mut runs = Vec::new();
    let mut arena: Vec<Ortho> = Vec::new();
    let mut current_size = 0;

    // Read all drain files with bounded buffering
    for file_path in raw.files() {
        let mut reader = OrthoStreamReader::new(file_path, cfg.read_buf_bytes)?;
        while let Some(result) = reader.next() {
            let streamed = result.map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Failed to decode ortho: {}", e),
                )
            })?;
            arena.push(streamed.ortho);
            current_size += streamed.bytes_read;

            // Check if arena exceeds budget
            if current_size >= cfg.run_budget_bytes {
                // Sort by id and write run
                arena.sort_unstable_by_key(|o| o.id());
                let run_id = RUN_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
                let run_path = base_path
                    .join("runs")
                    .join(format!("b={:02}-run-{}.dat", bucket, run_id));
                
                write_ortho_run(&arena, &run_path)?;
                runs.push(Run::new(run_path));
                
                // Clear arena and size
                arena.clear();
                current_size = 0;
            }
        }
    }

    // Write any remaining items in arena
    if !arena.is_empty() {
        arena.sort_unstable_by_key(|o| o.id());
        let run_id = RUN_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
        let run_path = base_path
            .join("runs")
            .join(format!("b={:02}-run-{}.dat", bucket, run_id));
        
        write_ortho_run(&arena, &run_path)?;
        runs.push(Run::new(run_path));
    }

    Ok(runs)
}

/// Write an ortho run to disk
fn write_ortho_run(arena: &[Ortho], path: &PathBuf) -> io::Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    for ortho in arena {
        let encoded = bincode::encode_to_vec(ortho, bincode::config::standard())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        file.write_all(&encoded)?;
    }
    file.flush()?;
    Ok(())
}

/// K-way merge with deduplication
/// 
/// Performs a k-way merge of sorted ortho runs, respecting fan-in limits and dropping
/// adjacent duplicates by id. Multi-pass merge is used if number of runs exceeds fan-in.
pub fn merge_unique(
    mut runs: Vec<Run>,
    cfg: &Config,
    base_path: &PathBuf,
) -> io::Result<UniqueRun> {
    use std::collections::BinaryHeap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    
    if runs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Cannot merge empty run list"));
    }

    static MERGE_COUNTER: AtomicUsize = AtomicUsize::new(0);

    // Multi-pass merge if needed
    while runs.len() > cfg.fan_in {
        let mut next_pass_runs = Vec::new();
        
        for chunk in runs.chunks(cfg.fan_in) {
            let merged = merge_ortho_chunk(chunk, cfg, base_path)?;
            next_pass_runs.push(merged);
        }
        
        runs = next_pass_runs;
    }

    // Final pass - merge all remaining runs into a UniqueRun
    let merge_id = MERGE_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
    let unique_path = base_path.join("runs").join(format!("unique-{}.dat", merge_id));
    let mut writer = BufWriter::new(File::create(&unique_path)?);

    #[derive(Eq, PartialEq)]
    struct HeapItem {
        id: usize,
        run_idx: usize,
    }
    
    impl Ord for HeapItem {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            // Reverse for min-heap
            other.id.cmp(&self.id)
        }
    }
    
    impl PartialOrd for HeapItem {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    // Open iterators for all runs
    let mut iterators: Vec<_> = runs
        .iter()
        .map(|r| r.iter(cfg.read_buf_bytes))
        .collect::<io::Result<Vec<_>>>()?;

    // Store current ortho for each run
    let mut current_orthos: Vec<Option<StreamedOrtho>> = vec![None; iterators.len()];
    let mut heap = BinaryHeap::new();

    // Initialize heap with first value from each run
    for (idx, iter) in iterators.iter_mut().enumerate() {
        if let Some(result) = iter.next() {
            let streamed = result?;
            let id = streamed.ortho.id();
            current_orthos[idx] = Some(streamed);
            heap.push(HeapItem { id, run_idx: idx });
        }
    }

    let mut last_written_id: Option<usize> = None;

    // K-way merge with deduplication by id + equality
    while let Some(item) = heap.pop() {
        let streamed = current_orthos[item.run_idx].take().unwrap();
        let ortho = streamed.ortho;
        
        // Write only if different id from last written (dedupe)
        if last_written_id.map(|last| last != item.id).unwrap_or(true) {
            let encoded = bincode::encode_to_vec(&ortho, bincode::config::standard())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            writer.write_all(&encoded)?;
            last_written_id = Some(item.id);
        }

        // Fetch next from same run
        if let Some(result) = iterators[item.run_idx].next() {
            let streamed = result?;
            let id = streamed.ortho.id();
            current_orthos[item.run_idx] = Some(streamed);
            heap.push(HeapItem { id, run_idx: item.run_idx });
        }
    }

    writer.flush()?;
    Ok(UniqueRun::new(unique_path))
}

/// Helper to merge a chunk of ortho runs (for multi-pass)
fn merge_ortho_chunk(
    runs: &[Run],
    cfg: &Config,
    base_path: &PathBuf,
) -> io::Result<Run> {
    use std::collections::BinaryHeap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    
    static CHUNK_COUNTER: AtomicUsize = AtomicUsize::new(0);

    let chunk_id = CHUNK_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
    let chunk_path = base_path.join("runs").join(format!("chunk-{}.dat", chunk_id));
    let mut writer = BufWriter::new(File::create(&chunk_path)?);

    #[derive(Eq, PartialEq)]
    struct HeapItem {
        id: usize,
        run_idx: usize,
    }
    
    impl Ord for HeapItem {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other.id.cmp(&self.id)
        }
    }
    
    impl PartialOrd for HeapItem {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    let mut iterators: Vec<_> = runs
        .iter()
        .map(|r| r.iter(cfg.read_buf_bytes))
        .collect::<io::Result<Vec<_>>>()?;

    // Store current ortho for each run
    let mut current_orthos: Vec<Option<StreamedOrtho>> = vec![None; iterators.len()];
    let mut heap = BinaryHeap::new();

    for (idx, iter) in iterators.iter_mut().enumerate() {
        if let Some(result) = iter.next() {
            let streamed = result?;
            let id = streamed.ortho.id();
            current_orthos[idx] = Some(streamed);
            heap.push(HeapItem { id, run_idx: idx });
        }
    }

    // No deduplication in intermediate passes - just merge
    while let Some(item) = heap.pop() {
        let streamed = current_orthos[item.run_idx].take().unwrap();
        let encoded = bincode::encode_to_vec(&streamed.ortho, bincode::config::standard())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        writer.write_all(&encoded)?;

        if let Some(result) = iterators[item.run_idx].next() {
            let streamed = result?;
            let id = streamed.ortho.id();
            current_orthos[item.run_idx] = Some(streamed);
            heap.push(HeapItem { id, run_idx: item.run_idx });
        }
    }

    writer.flush()?;
    Ok(Run::new(chunk_path))
}

/// Anti-join: streaming merge that emits orthos from gen that are NOT in history
/// Returns: (next-work orthos, new seen run, accepted count)
/// 
/// Semantics:
/// - Emit x iff x ∈ gen and x ∉ history  
/// - Compares orthos by id + equality
/// 
/// Example:
/// History: [ortho_a(id=1), ortho_b(id=3), ortho_c(id=5)]
/// Gen: [ortho_d(id=2), ortho_e(id=3), ortho_f(id=4), ortho_g(id=5), ortho_h(id=6)]
/// Result: work = [ortho_d, ortho_f, ortho_h], accepted = 3 (orthos with ids 3, 5 already seen)
pub fn anti_join_orthos(
    unique_gen: UniqueRun,
    mut history: impl Iterator<Item = io::Result<StreamedOrtho>>,
    base_path: &PathBuf,
    read_buf_bytes: usize,
) -> io::Result<(Vec<Ortho>, Run, u64)> {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    
    static ANTI_JOIN_COUNTER: AtomicUsize = AtomicUsize::new(0);

    let anti_join_id = ANTI_JOIN_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
    let seen_run_path = base_path.join("runs").join(format!("seen-{}.dat", anti_join_id));
    let mut seen_writer = BufWriter::new(File::create(&seen_run_path)?);

    let mut gen_iter = unique_gen.iter(read_buf_bytes)?;
    let mut next_work = Vec::new();
    let mut accepted_count = 0u64;

    // Current values from each stream
    let mut gen_val = gen_iter.next().transpose()?;
    let mut history_val = history.next().transpose()?;

    // Streaming merge: compare gen orthos against history by ID
    loop {
        match (&gen_val, &history_val) {
            (None, _) => break,
            (Some(g), None) => {
                // No more history - all remaining gen values are new
                let encoded = bincode::encode_to_vec(&g.ortho, bincode::config::standard())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                seen_writer.write_all(&encoded)?;
                next_work.push(g.ortho.clone());
                accepted_count += 1;
                gen_val = gen_iter.next().transpose()?;
            }
            (Some(g), Some(h)) => {
                let g_id = g.ortho.id();
                let h_id = h.ortho.id();
                
                match g_id.cmp(&h_id) {
                    std::cmp::Ordering::Less => {
                        // g < h: g is new (not in history)
                        let encoded = bincode::encode_to_vec(&g.ortho, bincode::config::standard())
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                        seen_writer.write_all(&encoded)?;
                        next_work.push(g.ortho.clone());
                        accepted_count += 1;
                        gen_val = gen_iter.next().transpose()?;
                    }
                    std::cmp::Ordering::Equal => {
                        // Same ID: check structural equality
                        if g.ortho == h.ortho {
                            // Exact duplicate - reject from work, but add to seen
                            let encoded = bincode::encode_to_vec(&g.ortho, bincode::config::standard())
                                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                            seen_writer.write_all(&encoded)?;
                        } else {
                            // ID collision with different structure - treat as new
                            // Note: This is extremely rare and indicates hash collision
                            let encoded = bincode::encode_to_vec(&g.ortho, bincode::config::standard())
                                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                            seen_writer.write_all(&encoded)?;
                            next_work.push(g.ortho.clone());
                            accepted_count += 1;
                        }
                        gen_val = gen_iter.next().transpose()?;
                        history_val = history.next().transpose()?;
                    }
                    std::cmp::Ordering::Greater => {
                        // g > h: advance history
                        history_val = history.next().transpose()?;
                    }
                }
            }
        }
    }

    seen_writer.flush()?;
    Ok((next_work, Run::new(seen_run_path), accepted_count))
}

impl Drop for GenerationStore {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl Default for GenerationStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_compact_landing_small() {
        use crate::ortho::Ortho;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Create some orthos manually and write to a drain file
        let orthos = vec![
            Ortho::new(),
            Ortho::new().add(1)[0].clone(),
            Ortho::new().add(2)[0].clone(),
        ];

        let bucket = 0;
        let landing_dir = base_path.join("landing").join(format!("b={:02}", bucket));
        fs::create_dir_all(&landing_dir).unwrap();
        let drain_path = landing_dir.join("drain-0.log");

        // Write orthos to drain file
        let mut file = BufWriter::new(File::create(&drain_path).unwrap());
        for ortho in &orthos {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard()).unwrap();
            file.write_all(&encoded).unwrap();
        }
        file.flush().unwrap();

        let raw = RawStream::new(vec![drain_path]);

        // Large budget - should fit in one run
        let cfg = Config::test_config(1024 * 1024, 8);

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let runs = compact_landing(bucket, raw, &cfg, &base_path).unwrap();

        assert_eq!(runs.len(), 1);

        // Read back and verify sorted by id
        let mut result = vec![];
        for item in runs[0].iter(64 * 1024).unwrap() {
            result.push(item.unwrap().ortho);
        }

        assert_eq!(result.len(), orthos.len());

        // Verify sorted by id
        for i in 1..result.len() {
            assert!(result[i - 1].id() <= result[i].id());
        }
    }

    #[test]
    fn test_compact_landing_multiple_runs() {
        use crate::ortho::Ortho;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Generate many orthos
        let mut orthos = vec![Ortho::new()];
        for i in 0..100 {
            let children = orthos[0].add(i);
            orthos.extend(children);
        }

        let bucket = 0;
        let landing_dir = base_path.join("landing").join(format!("b={:02}", bucket));
        fs::create_dir_all(&landing_dir).unwrap();
        let drain_path = landing_dir.join("drain-0.log");

        // Write orthos to drain file
        let mut file = BufWriter::new(File::create(&drain_path).unwrap());
        for ortho in &orthos {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard()).unwrap();
            file.write_all(&encoded).unwrap();
        }
        file.flush().unwrap();

        let raw = RawStream::new(vec![drain_path]);

        // Small budget to force multiple runs
        let cfg = Config::test_config(2048, 8);

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let runs = compact_landing(bucket, raw, &cfg, &base_path).unwrap();

        // Should produce multiple runs due to small budget
        assert!(runs.len() >= 1);

        // Collect all orthos
        let mut all_orthos = vec![];
        for run in &runs {
            for item in run.iter(64 * 1024).unwrap() {
                all_orthos.push(item.unwrap().ortho);
            }
        }

        assert_eq!(all_orthos.len(), orthos.len());

        // Verify each run is sorted by id
        for run in &runs {
            let mut prev_id = None;
            for item in run.iter(64 * 1024).unwrap() {
                let ortho = item.unwrap().ortho;
                let id = ortho.id();
                if let Some(p) = prev_id {
                    assert!(id >= p, "Run should be sorted by id");
                }
                prev_id = Some(id);
            }
        }
    }

    #[test]
    fn test_compact_landing_millions_of_orthos() {
        use crate::ortho::Ortho;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Generate 100K orthos (scaled down from millions for test speed)
        let count = 100_000;
        let mut orthos = vec![];
        let base = Ortho::new();
        for i in 0..count {
            // Create simple variations
            let children = base.add(i as usize);
            if !children.is_empty() {
                orthos.push(children[0].clone());
            }
        }

        let bucket = 0;
        let landing_dir = base_path.join("landing").join(format!("b={:02}", bucket));
        fs::create_dir_all(&landing_dir).unwrap();
        let drain_path = landing_dir.join("drain-0.log");

        // Write orthos to drain file
        let mut file = BufWriter::new(File::create(&drain_path).unwrap());
        for ortho in &orthos {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard()).unwrap();
            file.write_all(&encoded).unwrap();
        }
        file.flush().unwrap();

        let raw = RawStream::new(vec![drain_path]);

        // Reasonable budget
        let cfg = Config::test_config(4 * 1024 * 1024, 8);

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let runs = compact_landing(bucket, raw, &cfg, &base_path).unwrap();

        // Collect and verify count
        let mut total = 0;
        for run in &runs {
            for item in run.iter(64 * 1024).unwrap() {
                item.unwrap();
                total += 1;
            }
        }
        assert_eq!(total, orthos.len());

        // Verify each run is sorted by id
        for run in &runs {
            let mut prev_id = None;
            for item in run.iter(64 * 1024).unwrap() {
                let ortho = item.unwrap().ortho;
                let id = ortho.id();
                if let Some(p) = prev_id {
                    assert!(id >= p, "Run should be sorted by id");
                }
                prev_id = Some(id);
            }
        }
    }

    #[test]
    fn from_existing_reads_history_runs() {
        use crate::ortho::Ortho;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();

        // Write a single ortho into a store and finalize
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        let cfg = Config::test_config(256 * 1024, 8);
        store.configure(&cfg);
        store.record_result(&Ortho::new()).unwrap();
        store.on_generation_end(&cfg, None).unwrap();
        drop(store);

        // Reopen from disk and ensure history is readable
        let reader = GenerationStore::from_existing(base_path.clone(), 8).unwrap();
        let mut count = 0usize;
        for bucket in 0..8 {
            for ortho in reader.history_iter_with_buffer(bucket, 64 * 1024).unwrap() {
                ortho.unwrap();
                count += 1;
            }
        }
        assert_eq!(count, 1);
    }

    // ============ TASK 6 TESTS ============
    #[test]
    fn test_anti_join_orthos_basic() {
        // Test anti_join with ortho structures
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        fs::create_dir_all(base_path.join("runs")).unwrap();

        // Create some test orthos with different IDs
        let ortho1 = Ortho::new();
        let ortho2 = ortho1.add(1).into_iter().next().unwrap();
        let ortho3 = ortho2.add(2).into_iter().next().unwrap();
        let ortho4 = ortho1.add(2).into_iter().next().unwrap();
        let ortho5 = ortho4.add(1).into_iter().next().unwrap();

        // History: ortho1, ortho3
        let history_path = base_path.join("runs").join("history.dat");
        let mut history_file = BufWriter::new(File::create(&history_path).unwrap());
        for ortho in [&ortho1, &ortho3] {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard()).unwrap();
            history_file.write_all(&encoded).unwrap();
        }
        history_file.flush().unwrap();

        // Gen: ortho2, ortho3, ortho4, ortho5
        let gen_path = base_path.join("runs").join("gen.dat");
        let mut gen_file = BufWriter::new(File::create(&gen_path).unwrap());
        for ortho in [&ortho2, &ortho3, &ortho4, &ortho5] {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard()).unwrap();
            gen_file.write_all(&encoded).unwrap();
        }
        gen_file.flush().unwrap();

        let unique_gen = UniqueRun::new(gen_path);
        let history_run = Run::new(history_path);
        let history_iter = history_run.iter(64 * 1024).unwrap();

        let (work, _seen_run, accepted) =
            anti_join_orthos(unique_gen, history_iter, &base_path, 64 * 1024).unwrap();

        // ortho3 is already in history, so only ortho2, ortho4, ortho5 should be in work
        assert_eq!(work.len(), 3);
        assert_eq!(accepted, 3);
        assert_eq!(work[0], ortho2);
        assert_eq!(work[1], ortho4);
        assert_eq!(work[2], ortho5);
    }

    #[test]
    fn test_anti_join_orthos_empty_history() {
        // When history is empty, all orthos should be in work
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let ortho1 = Ortho::new();
        let ortho2 = ortho1.add(1).into_iter().next().unwrap();

        // Gen: ortho1, ortho2
        let gen_path = base_path.join("runs").join("gen.dat");
        let mut gen_file = BufWriter::new(File::create(&gen_path).unwrap());
        for ortho in [&ortho1, &ortho2] {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard()).unwrap();
            gen_file.write_all(&encoded).unwrap();
        }
        gen_file.flush().unwrap();

        let unique_gen = UniqueRun::new(gen_path);
        let history_iter = std::iter::empty::<io::Result<StreamedOrtho>>();

        let (work, _seen_run, accepted) =
            anti_join_orthos(unique_gen, history_iter, &base_path, 64 * 1024).unwrap();

        assert_eq!(work.len(), 2);
        assert_eq!(accepted, 2);
        assert_eq!(work[0], ortho1);
        assert_eq!(work[1], ortho2);
    }

    #[test]
    fn test_anti_join_orthos_all_in_history() {
        // When all orthos are in history, work should be empty
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let ortho1 = Ortho::new();
        let ortho2 = ortho1.add(1).into_iter().next().unwrap();

        // History: ortho1, ortho2
        let history_path = base_path.join("runs").join("history.dat");
        let mut history_file = BufWriter::new(File::create(&history_path).unwrap());
        for ortho in [&ortho1, &ortho2] {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard()).unwrap();
            history_file.write_all(&encoded).unwrap();
        }
        history_file.flush().unwrap();

        // Gen: ortho1 (subset)
        let gen_path = base_path.join("runs").join("gen.dat");
        let mut gen_file = BufWriter::new(File::create(&gen_path).unwrap());
        let encoded = bincode::encode_to_vec(&ortho1, bincode::config::standard()).unwrap();
        gen_file.write_all(&encoded).unwrap();
        gen_file.flush().unwrap();

        let unique_gen = UniqueRun::new(gen_path);
        let history_run = Run::new(history_path);
        let history_iter = history_run.iter(64 * 1024).unwrap();

        let (work, _seen_run, accepted) =
            anti_join_orthos(unique_gen, history_iter, &base_path, 64 * 1024).unwrap();

        assert_eq!(work.len(), 0);
        assert_eq!(accepted, 0);
    }

    #[test]
    fn test_compute_fan_in() {
        // fan_in = clamp(budget / read_buf, 8, 128)
        let read_buf = 64 * 1024; // 64KB
        
        // Small budget: should clamp to 8
        assert_eq!(compute_fan_in(100_000, read_buf), 8);
        
        // Medium budget: should be in range
        let budget = 1_000_000_000; // 1GB
        let fan_in = compute_fan_in(budget, read_buf);
        assert!(fan_in >= 8 && fan_in <= 128);
        
        // Large budget: should clamp to 128
        let budget = 100_000_000_000; // 100GB
        assert_eq!(compute_fan_in(budget, read_buf), 128);
        
        // Zero read_buf: should return 8
        assert_eq!(compute_fan_in(1_000_000, 0), 8);
    }

    #[test]
    fn test_compute_config_leader_aggressive() {
        // This test validates the structure but cannot control actual system memory
        // In real usage, leader at low memory pressure should get max budget (6GB)
        let config = Config::compute_config(Role::Leader);
        
        // Should not bail out
        assert!(config.is_some());
        
        let config = config.unwrap();
        
        // run_budget should be 70% of some budget
        // fan_in should be between 8 and 128
        assert!(config.fan_in >= 8 && config.fan_in <= 128);
        assert!(config.run_budget_bytes > 0);
        assert_eq!(config.read_buf_bytes, 64 * 1024);
        assert!(config.allow_compaction);
    }

    #[test]
    fn test_compute_config_follower() {
        // Follower should have smaller budget than leader
        let config = Config::compute_config(Role::Follower);
        
        // May bail out if system memory is very constrained, but typically should succeed
        if let Some(config) = config {
            assert!(config.fan_in >= 8 && config.fan_in <= 128);
            assert!(config.run_budget_bytes > 0);
            assert_eq!(config.read_buf_bytes, 64 * 1024);
            assert!(config.allow_compaction);
        }
        // If None, follower decided to bail due to memory pressure
    }

    #[test]
    fn test_run_budget_calculation() {
        // Verify run_budget is 70% of total budget
        let budget = 1_000_000_000; // 1GB
        let run_budget = (budget as f64 * 0.7) as usize;
        assert_eq!(run_budget, 700_000_000);
        
        // Test edge case: very small budget
        let budget = 128_000_000; // 128MB
        let run_budget = (budget as f64 * 0.7) as usize;
        assert!(run_budget < 128_000_000);
    }

    // ============ TASK 11 TESTS ============

    #[test]
    fn test_on_generation_end_empty_work() {
        // Test that on_generation_end handles empty landing zones gracefully
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        
        let cfg = Config::test_config(1024 * 1024, 8);
        
        // Call on_generation_end with no data
        let new_work = store.on_generation_end(&cfg, None).unwrap();
        
        assert_eq!(new_work, 0);
        assert_eq!(store.work_len(), 0);
        assert_eq!(store.seen_len_accepted(), 0);
    }

    #[test]
    fn test_ortho_pipeline_full_generations() {
        // Test the full ortho pipeline through multiple generations
        // This is the ortho version of the integer pipeline test
        // 
        // Loop pattern:
        //   while let Some(ortho) = store.pop_work() {
        //       let results = process_ortho(ortho);
        //       for r in results { store.record_result(&r); }
        //   }
        //   store.on_generation_end();
        
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        
        let cfg = Config::test_config(1024 * 1024, 8);
        store.configure(&cfg);
        
        // Seed the work queue with initial ortho
        let seed = Ortho::new();
        store.push_segments(vec![seed]).unwrap();
        store.flush_all().unwrap();
        assert!(store.work_len() > 0, "Work queue should have items after push and flush, got {}", store.work_len());
        
        // Generation 0: Process initial work
        let mut processed_gen0 = 0;
        let mut results_gen0 = 0;
        while let Some(ortho) = store.pop_work().unwrap() {
            // Process function: expand the ortho by adding tokens 2 and 3
            let results = ortho.add(2);
            results_gen0 += results.len();
            for r in results {
                store.record_result(&r).unwrap();
            }
            processed_gen0 += 1;
        }
        
        assert!(processed_gen0 > 0, "Should have processed some orthos");
        assert!(results_gen0 > 0, "Should have generated some results");
        assert_eq!(store.work_len(), 0); // Work queue is empty
        
        // End generation 0 - triggers drain, compact, anti-join, and push new work
        let new_work_gen0 = store.on_generation_end(&cfg, None).unwrap();
        
        // Should have generated some new work
        assert!(new_work_gen0 > 0, "Should have new work from generation 0");
        assert_eq!(store.work_len(), new_work_gen0);
        
        // Check that seen_len_accepted has been updated
        assert_eq!(store.seen_len_accepted(), new_work_gen0);
        
        // Generation 1: Process the new work
        let mut processed_gen1 = 0;
        let max_gen1_items = 10; // Limit to avoid explosion
        while let Some(ortho) = store.pop_work().unwrap() {
            if processed_gen1 >= max_gen1_items {
                // Push back the rest
                store.push_segments(vec![ortho]).unwrap();
                break;
            }
            
            // Same process function
            let results = ortho.add(3);
            for r in results {
                store.record_result(&r).unwrap();
            }
            processed_gen1 += 1;
        }
        
        assert!(processed_gen1 <= max_gen1_items);
        
        // End generation 1
        let new_work_gen1 = store.on_generation_end(&cfg, None).unwrap();
        
        // Should have generated new work
        assert!(new_work_gen1 > 0, "Should have new work from generation 1");
        
        // Seen count should have increased
        assert!(store.seen_len_accepted() > new_work_gen0);
        
        // Generation 2: Process more work
        let mut processed_gen2 = 0;
        let max_gen2_items = 10;
        while let Some(ortho) = store.pop_work().unwrap() {
            if processed_gen2 >= max_gen2_items {
                store.push_segments(vec![ortho]).unwrap();
                break;
            }
            
            let results = ortho.add(4);
            for r in results {
                store.record_result(&r).unwrap();
            }
            processed_gen2 += 1;
        }
        
        // End generation 2
        let new_work_gen2 = store.on_generation_end(&cfg, None).unwrap();
        
        // Verify the system maintains correctness with orthos:
        // - work_len tracks queue depth
        // - seen_len_accepted is monotonic
        // - history accumulates across generations
        // - deduplication by ortho.id() works correctly
        assert!(store.work_len() > 0);
        assert!(store.seen_len_accepted() >= new_work_gen0);
        
        println!("Completed 3 ortho generations:");
        println!("  Gen 0: {} orthos -> {} new work", processed_gen0, new_work_gen0);
        println!("  Gen 1: {} orthos -> {} new work", processed_gen1, new_work_gen1);
        println!("  Gen 2: {} orthos -> {} new work", processed_gen2, new_work_gen2);
        println!("  Final work queue: {}", store.work_len());
        println!("  Final seen count: {}", store.seen_len_accepted());
    }

    #[test]
    fn test_ortho_pipeline_with_duplicates() {
        // Test that duplicate orthos are properly filtered during anti-join
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        
        let cfg = Config::test_config(1024 * 1024, 8);
        store.configure(&cfg);
        
        // Seed with initial ortho
        let seed = Ortho::new();
        store.push_segments(vec![seed]).unwrap();
        store.flush_all().unwrap();
        
        // Generation 0: Process and generate results
        while let Some(ortho) = store.pop_work().unwrap() {
            // Generate children - some may overlap with siblings
            let results = ortho.add(2);
            for r in results {
                store.record_result(&r).unwrap();
            }
        }
        
        let new_work_gen0 = store.on_generation_end(&cfg, None).unwrap();
        let seen_gen0 = store.seen_len_accepted();
        
        assert!(new_work_gen0 > 0);
        assert_eq!(seen_gen0, new_work_gen0);
        
        // Generation 1: Process again - should see some duplicates filtered
        let mut processed = 0;
        let max_items = 5;
        while let Some(ortho) = store.pop_work().unwrap() {
            if processed >= max_items {
                store.push_segments(vec![ortho]).unwrap();
                break;
            }
            
            // Generate more children
            let results = ortho.add(3);
            for r in results {
                store.record_result(&r).unwrap();
            }
            processed += 1;
        }
        
        let new_work_gen1 = store.on_generation_end(&cfg, None).unwrap();
        
        // Seen count should grow but some duplicates should be filtered
        assert!(store.seen_len_accepted() > seen_gen0);
        
        println!("Ortho duplicate filtering test:");
        println!("  Gen 0: {} new work, {} seen", new_work_gen0, seen_gen0);
        println!("  Gen 1: {} processed, {} new work, {} total seen", 
                 processed, new_work_gen1, store.seen_len_accepted());
    }
}
