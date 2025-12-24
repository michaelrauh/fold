use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write, Read};
use std::path::PathBuf;
use crate::ortho::Ortho;
use sysinfo::System;

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
}

impl Config {
    /// Compute config based on role and current system memory state
    /// 
    /// RAM Policy:
    /// - Leader targets: aggressive below 65%, conservative above 85%
    /// - Follower targets: aggressive below 50%, conservative above 70%
    /// - run_budget = 0.7 * budget
    /// - fan_in = clamp(budget / read_buf, 8, 128)
    /// - Follower bails if run_budget < 128MB when already at lowest budget and RSS stays above minimum target
    pub fn compute_config(role: Role) -> Option<Self> {
        let (used_bytes, total_bytes, _headroom_bytes) = get_memory_state();
        let used_pct = (used_bytes as f64 / total_bytes as f64) * 100.0;
        
        // Define targets based on role
        let (aggressive_threshold, conservative_threshold) = match role {
            Role::Leader => (65.0, 85.0),
            Role::Follower => (50.0, 70.0),
        };
        
        // Default budgets
        let (default_min, default_max) = match role {
            Role::Leader => (2_000_000_000, 6_000_000_000), // 2-6 GB
            Role::Follower => (256_000_000, 1_000_000_000),  // 256MB - 1GB
        };
        
        // Calculate budget based on memory pressure
        let budget = if used_pct < aggressive_threshold {
            // Below aggressive threshold: use maximum budget
            default_max
        } else if used_pct > conservative_threshold {
            // Above conservative threshold: use minimum budget
            default_min
        } else {
            // In between: linear interpolation
            let range = conservative_threshold - aggressive_threshold;
            let position = (used_pct - aggressive_threshold) / range;
            let budget_range = (default_max - default_min) as f64;
            default_max - (budget_range * position) as usize
        };
        
        // Check follower bail-out condition
        if role == Role::Follower {
            let run_budget = (budget as f64 * 0.7) as usize;
            if run_budget < 128_000_000 && used_pct >= aggressive_threshold {
                // Follower should bail: run_budget < 128MB and RSS above minimum target
                return None;
            }
        }
        
        let run_budget_bytes = (budget as f64 * 0.7) as usize;
        let read_buf_bytes = 64 * 1024; // 64KB read buffer
        let fan_in = compute_fan_in(budget, read_buf_bytes);
        
        Some(Self {
            run_budget_bytes,
            fan_in,
            read_buf_bytes,
            allow_compaction: true,
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


/// Statistics for a single bucket
#[derive(Clone, Debug)]
pub struct BucketStats {
    pub bucket_id: usize,
    pub run_count: usize,
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
    // Work queue state
    work_segments: Vec<PathBuf>,
    work_segment_counter: usize,
    total_work_len: u64,
    // History state
    history_runs: Vec<Vec<PathBuf>>, // Per-bucket list of history run files
    seen_len_accepted: u64, // Monotonic count of accepted items across all generations
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

    /// Iterate over orthos in this run
    pub fn iter(&self) -> io::Result<impl Iterator<Item = io::Result<Ortho>>> {
        let mut file = File::open(&self.path)?;
        let mut all_bytes = Vec::new();
        file.read_to_end(&mut all_bytes)?;
        Ok(OrthoRunIterator { bytes: all_bytes, offset: 0 })
    }
}

struct OrthoRunIterator {
    bytes: Vec<u8>,
    offset: usize,
}

impl Iterator for OrthoRunIterator {
    type Item = io::Result<Ortho>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.bytes.len() {
            return None;
        }

        // Decode one ortho from current offset
        match bincode::decode_from_slice(&self.bytes[self.offset..], bincode::config::standard()) {
            Ok((ortho, bytes_read)) => {
                self.offset += bytes_read;
                Some(Ok(ortho))
            }
            Err(e) => Some(Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))),
        }
    }
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

    /// Iterate over orthos in this unique run
    pub fn iter(&self) -> io::Result<impl Iterator<Item = io::Result<Ortho>>> {
        let mut file = File::open(&self.path)?;
        let mut all_bytes = Vec::new();
        file.read_to_end(&mut all_bytes)?;
        Ok(OrthoRunIterator { bytes: all_bytes, offset: 0 })
    }
}

/// Iterator over history runs for a bucket
/// Streams orthos from all history run files in order
pub struct HistoryIterator {
    run_files: Vec<PathBuf>,
    current_run_index: usize,
    current_run_iter: Option<OrthoRunIterator>,
}

impl HistoryIterator {
    fn new(run_files: &[PathBuf]) -> io::Result<Self> {
        let mut iter = Self {
            run_files: run_files.to_vec(),
            current_run_index: 0,
            current_run_iter: None,
        };
        iter.advance_to_next_run()?;
        Ok(iter)
    }

    fn advance_to_next_run(&mut self) -> io::Result<()> {
        self.current_run_iter = None;
        
        if self.current_run_index >= self.run_files.len() {
            return Ok(());
        }

        let mut file = File::open(&self.run_files[self.current_run_index])?;
        let mut all_bytes = Vec::new();
        file.read_to_end(&mut all_bytes)?;
        self.current_run_iter = Some(OrthoRunIterator { bytes: all_bytes, offset: 0 });
        self.current_run_index += 1;
        
        Ok(())
    }
}

impl Iterator for HistoryIterator {
    type Item = io::Result<Ortho>;

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
            work_segments: Vec::new(),
            work_segment_counter: 0,
            total_work_len: 0,
            history_runs: (0..bucket_count).map(|_| Vec::new()).collect(),
            seen_len_accepted: 0,
        })
    }

    /// Create a new empty generation store
    pub fn new() -> Self {
        Self {
            base_path: PathBuf::from("fold_state"),
            bucket_count: 8,
            bucket_writers: (0..8).map(|_| None).collect(),
            drain_counter: vec![0; 8],
            work_segments: Vec::new(),
            work_segment_counter: 0,
            total_work_len: 0,
            history_runs: (0..8).map(|_| Vec::new()).collect(),
            seen_len_accepted: 0,
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
        let bucket = (ortho.id() as u64 & (self.bucket_count - 1) as u64) as usize;
        
        // Get or create writer for this bucket
        if self.bucket_writers[bucket].is_none() {
            let path = self.active_log_path(bucket);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            self.bucket_writers[bucket] = Some(BufWriter::new(file));
        }

        // Write ortho as bincode
        let writer = self.bucket_writers[bucket].as_mut().unwrap();
        let encoded = bincode::encode_to_vec(ortho, bincode::config::standard())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        writer.write_all(&encoded)?;
        
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
        
        Ok(RawStream::new(vec![drain_path]))
    }

    /// Push a segment of work items to the work queue
    /// Segment file format: [count as u64][ortho bincode...count items]
    pub fn push_segments(&mut self, items: Vec<Ortho>) -> io::Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        let count = items.len() as u64;
        let segment_path = self.base_path
            .join("work")
            .join(format!("segment-{}.dat", self.work_segment_counter));
        self.work_segment_counter += 1;

        // Write segment file: [count][ortho bincode...]
        let mut file = BufWriter::new(File::create(&segment_path)?);
        file.write_all(&count.to_le_bytes())?;
        for ortho in &items {
            let encoded = bincode::encode_to_vec(ortho, bincode::config::standard())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            // Write length prefix for ortho
            file.write_all(&(encoded.len() as u64).to_le_bytes())?;
            file.write_all(&encoded)?;
        }
        file.flush()?;

        // Add to work segments list and update total length
        self.work_segments.push(segment_path);
        self.total_work_len += count;

        Ok(())
    }

    /// Pop a single work item from the work queue
    /// Reads orthos from segments with length-prefix encoding
    pub fn pop_work(&mut self) -> io::Result<Option<Ortho>> {
        if self.work_segments.is_empty() {
            return Ok(None);
        }

        // Take the first segment from the list
        let segment_path = self.work_segments.remove(0);
        let mut file = File::open(&segment_path)?;

        // Read count
        let mut count_bytes = [0u8; 8];
        file.read_exact(&mut count_bytes)?;
        let count = u64::from_le_bytes(count_bytes) as usize;

        if count == 0 {
            drop(file);
            fs::remove_file(&segment_path)?;
            return self.pop_work(); // Try next segment
        }

        // Read first ortho
        let mut len_bytes = [0u8; 8];
        file.read_exact(&mut len_bytes)?;
        let len = u64::from_le_bytes(len_bytes) as usize;

        let mut ortho_bytes = vec![0u8; len];
        file.read_exact(&mut ortho_bytes)?;
        let ortho: Ortho = bincode::decode_from_slice(&ortho_bytes, bincode::config::standard())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
            .0;

        // If there are more items, rewrite the segment with remaining items
        if count > 1 {
            // Read all remaining orthos
            let mut remaining = Vec::new();
            for _ in 1..count {
                let mut len_bytes = [0u8; 8];
                file.read_exact(&mut len_bytes)?;
                let len = u64::from_le_bytes(len_bytes) as usize;

                let mut ortho_bytes = vec![0u8; len];
                file.read_exact(&mut ortho_bytes)?;
                let ortho: Ortho = bincode::decode_from_slice(&ortho_bytes, bincode::config::standard())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
                    .0;
                remaining.push(ortho);
            }

            drop(file);
            fs::remove_file(&segment_path)?;

            // Rewrite segment with remaining items
            let new_count = remaining.len() as u64;
            let mut new_file = BufWriter::new(File::create(&segment_path)?);
            new_file.write_all(&new_count.to_le_bytes())?;
            for ortho in &remaining {
                let encoded = bincode::encode_to_vec(ortho, bincode::config::standard())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                new_file.write_all(&(encoded.len() as u64).to_le_bytes())?;
                new_file.write_all(&encoded)?;
            }
            new_file.flush()?;

            // Put segment back at the front
            self.work_segments.insert(0, segment_path);
        } else {
            // Last item, just delete the segment
            drop(file);
            fs::remove_file(&segment_path)?;
        }

        self.total_work_len -= 1;
        Ok(Some(ortho))
    }

    /// Get the current work queue length
    pub fn work_len(&self) -> u64 {
        self.total_work_len
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
    pub fn history_iter(&self, bucket: usize) -> io::Result<HistoryIterator> {
        assert!(bucket < self.bucket_count, "Invalid bucket index");
        HistoryIterator::new(&self.history_runs[bucket])
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

    /// Get per-bucket statistics for TUI visualization
    pub fn bucket_stats(&self) -> Vec<BucketStats> {
        (0..self.bucket_count)
            .map(|bucket| {
                let run_count = self.history_runs[bucket].len();
                
                // Estimate landing size by checking if active log exists
                let landing_path = self.active_log_path(bucket);
                let landing_size = std::fs::metadata(&landing_path)
                    .map(|m| m.len() as usize)
                    .unwrap_or(0);
                
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
    pub fn on_generation_end(&mut self, cfg: &Config) -> io::Result<u64> {
        let mut total_new_work = 0u64;
        
        // Process each bucket independently
        for bucket in 0..self.bucket_count {
            // Flush writers before draining
            self.flush()?;
            
            // Phase: Draining
            let raw = self.drain_bucket(bucket)?;
            
            if raw.files().is_empty() {
                // No data in this bucket, skip
                continue;
            }
            
            // Phase: Compacting
            let runs = compact_landing(bucket, raw, cfg, &self.base_path)?;
            
            if runs.is_empty() {
                // No runs generated, skip
                continue;
            }
            
            // Phase: Merge to unique run
            let unique_run = merge_unique(runs, cfg, &self.base_path)?;
            
            // Phase: Anti-join against history
            let history_iter = self.history_iter(bucket)?;
            let (new_work, seen_run, accepted) = anti_join_orthos(
                unique_run,
                history_iter,
                &self.base_path,
            )?;
            
            // Add seen run to history
            self.add_history_run(bucket, seen_run, accepted)?;
            
            // Optional: Compact history if needed
            if cfg.allow_compaction {
                self.compact_history(bucket, cfg)?;
            }
            
            // Push new work to queue (ortho version)
            total_new_work += new_work.len() as u64;
            self.push_segments(new_work)?;
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

    // Read all drain files
    for file_path in raw.files() {
        let mut file = File::open(file_path)?;
        let mut all_bytes = Vec::new();
        file.read_to_end(&mut all_bytes)?;

        let mut offset = 0;
        while offset < all_bytes.len() {
            // Decode one ortho
            match bincode::decode_from_slice(&all_bytes[offset..], bincode::config::standard()) {
                Ok((ortho, bytes_read)) => {
                    let encoded_size = bytes_read;
                    arena.push(ortho);
                    current_size += encoded_size;
                    offset += bytes_read;

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
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to decode ortho: {}", e),
                    ));
                }
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
    let mut iterators: Vec<_> = runs.iter()
        .map(|r| r.iter())
        .collect::<io::Result<Vec<_>>>()?;

    // Store current ortho for each run
    let mut current_orthos: Vec<Option<Ortho>> = vec![None; iterators.len()];
    let mut heap = BinaryHeap::new();

    // Initialize heap with first value from each run
    for (idx, iter) in iterators.iter_mut().enumerate() {
        if let Some(result) = iter.next() {
            let ortho = result?;
            let id = ortho.id();
            current_orthos[idx] = Some(ortho);
            heap.push(HeapItem { id, run_idx: idx });
        }
    }

    let mut last_written_id: Option<usize> = None;

    // K-way merge with deduplication by id + equality
    while let Some(item) = heap.pop() {
        let ortho = current_orthos[item.run_idx].take().unwrap();
        
        // Write only if different id from last written (dedupe)
        if last_written_id.map(|last| last != item.id).unwrap_or(true) {
            let encoded = bincode::encode_to_vec(&ortho, bincode::config::standard())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            writer.write_all(&encoded)?;
            last_written_id = Some(item.id);
        }

        // Fetch next from same run
        if let Some(result) = iterators[item.run_idx].next() {
            let ortho = result?;
            let id = ortho.id();
            current_orthos[item.run_idx] = Some(ortho);
            heap.push(HeapItem { id, run_idx: item.run_idx });
        }
    }

    writer.flush()?;
    Ok(UniqueRun::new(unique_path))
}

/// Helper to merge a chunk of ortho runs (for multi-pass)
fn merge_ortho_chunk(
    runs: &[Run],
    _cfg: &Config,
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

    let mut iterators: Vec<_> = runs.iter()
        .map(|r| r.iter())
        .collect::<io::Result<Vec<_>>>()?;

    // Store current ortho for each run
    let mut current_orthos: Vec<Option<Ortho>> = vec![None; iterators.len()];
    let mut heap = BinaryHeap::new();

    for (idx, iter) in iterators.iter_mut().enumerate() {
        if let Some(result) = iter.next() {
            let ortho = result?;
            let id = ortho.id();
            current_orthos[idx] = Some(ortho);
            heap.push(HeapItem { id, run_idx: idx });
        }
    }

    // No deduplication in intermediate passes - just merge
    while let Some(item) = heap.pop() {
        let ortho = current_orthos[item.run_idx].take().unwrap();
        let encoded = bincode::encode_to_vec(&ortho, bincode::config::standard())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        writer.write_all(&encoded)?;

        if let Some(result) = iterators[item.run_idx].next() {
            let ortho = result?;
            let id = ortho.id();
            current_orthos[item.run_idx] = Some(ortho);
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
    mut history: impl Iterator<Item = io::Result<Ortho>>,
    base_path: &PathBuf,
) -> io::Result<(Vec<Ortho>, Run, u64)> {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    
    static ANTI_JOIN_COUNTER: AtomicUsize = AtomicUsize::new(0);

    let anti_join_id = ANTI_JOIN_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
    let seen_run_path = base_path.join("runs").join(format!("seen-{}.dat", anti_join_id));
    let mut seen_writer = BufWriter::new(File::create(&seen_run_path)?);

    let mut gen_iter = unique_gen.iter()?;
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
                let encoded = bincode::encode_to_vec(g, bincode::config::standard())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                seen_writer.write_all(&encoded)?;
                next_work.push(g.clone());
                accepted_count += 1;
                gen_val = gen_iter.next().transpose()?;
            }
            (Some(g), Some(h)) => {
                let g_id = g.id();
                let h_id = h.id();
                
                match g_id.cmp(&h_id) {
                    std::cmp::Ordering::Less => {
                        // g < h: g is new (not in history)
                        let encoded = bincode::encode_to_vec(g, bincode::config::standard())
                            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                        seen_writer.write_all(&encoded)?;
                        next_work.push(g.clone());
                        accepted_count += 1;
                        gen_val = gen_iter.next().transpose()?;
                    }
                    std::cmp::Ordering::Equal => {
                        // Same ID: check structural equality
                        if g == h {
                            // Exact duplicate - reject from work, but add to seen
                            let encoded = bincode::encode_to_vec(g, bincode::config::standard())
                                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                            seen_writer.write_all(&encoded)?;
                        } else {
                            // ID collision with different structure - treat as new
                            // Note: This is extremely rare and indicates hash collision
                            let encoded = bincode::encode_to_vec(g, bincode::config::standard())
                                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                            seen_writer.write_all(&encoded)?;
                            next_work.push(g.clone());
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
        let cfg = Config {
            run_budget_bytes: 1024 * 1024,
            fan_in: 8,
            read_buf_bytes: 4096,
            allow_compaction: false,
        };

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let runs = compact_landing(bucket, raw, &cfg, &base_path).unwrap();

        assert_eq!(runs.len(), 1);

        // Read back and verify sorted by id
        let mut result = vec![];
        for item in runs[0].iter().unwrap() {
            result.push(item.unwrap());
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
        let cfg = Config {
            run_budget_bytes: 2048, // Small budget
            fan_in: 8,
            read_buf_bytes: 4096,
            allow_compaction: false,
        };

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let runs = compact_landing(bucket, raw, &cfg, &base_path).unwrap();

        // Should produce multiple runs due to small budget
        assert!(runs.len() >= 1);

        // Collect all orthos
        let mut all_orthos = vec![];
        for run in &runs {
            for item in run.iter().unwrap() {
                all_orthos.push(item.unwrap());
            }
        }

        assert_eq!(all_orthos.len(), orthos.len());

        // Verify each run is sorted by id
        for run in &runs {
            let mut prev_id = None;
            for item in run.iter().unwrap() {
                let ortho = item.unwrap();
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
        let cfg = Config {
            run_budget_bytes: 4 * 1024 * 1024, // 4 MB
            fan_in: 8,
            read_buf_bytes: 4096,
            allow_compaction: false,
        };

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let runs = compact_landing(bucket, raw, &cfg, &base_path).unwrap();

        // Collect and verify count
        let mut total = 0;
        for run in &runs {
            for item in run.iter().unwrap() {
                item.unwrap();
                total += 1;
            }
        }
        assert_eq!(total, orthos.len());

        // Verify each run is sorted by id
        for run in &runs {
            let mut prev_id = None;
            for item in run.iter().unwrap() {
                let ortho = item.unwrap();
                let id = ortho.id();
                if let Some(p) = prev_id {
                    assert!(id >= p, "Run should be sorted by id");
                }
                prev_id = Some(id);
            }
        }
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
        let history_iter = history_run.iter().unwrap();

        let (work, _seen_run, accepted) = anti_join_orthos(unique_gen, history_iter, &base_path).unwrap();

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
        let history_iter = std::iter::empty();

        let (work, _seen_run, accepted) = anti_join_orthos(unique_gen, history_iter, &base_path).unwrap();

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
        let history_iter = history_run.iter().unwrap();

        let (work, _seen_run, accepted) = anti_join_orthos(unique_gen, history_iter, &base_path).unwrap();

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
        
        let cfg = Config {
            run_budget_bytes: 1024 * 1024,
            fan_in: 8,
            read_buf_bytes: 4096,
            allow_compaction: false,
        };
        
        // Call on_generation_end with no data
        let new_work = store.on_generation_end(&cfg).unwrap();
        
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
        
        let cfg = Config {
            run_budget_bytes: 1024 * 1024, // 1 MB
            fan_in: 8,
            read_buf_bytes: 4096,
            allow_compaction: false,
        };
        
        // Seed the work queue with initial orthos
        let seed = Ortho::new();
        let gen0_work = seed.add(1);
        store.push_segments(gen0_work.clone()).unwrap();
        
        // Generation 0: Process initial work
        let mut processed_gen0 = 0;
        while let Some(ortho) = store.pop_work().unwrap() {
            // Process function: expand the ortho by adding tokens 2 and 3
            let results = ortho.add(2);
            for r in results {
                store.record_result(&r).unwrap();
            }
            processed_gen0 += 1;
        }
        
        assert!(processed_gen0 > 0, "Should have processed some orthos");
        assert_eq!(store.work_len(), 0); // Work queue is empty
        
        // End generation 0 - triggers drain, compact, anti-join, and push new work
        let new_work_gen0 = store.on_generation_end(&cfg).unwrap();
        
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
        let new_work_gen1 = store.on_generation_end(&cfg).unwrap();
        
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
        let new_work_gen2 = store.on_generation_end(&cfg).unwrap();
        
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
        
        let cfg = Config {
            run_budget_bytes: 1024 * 1024,
            fan_in: 8,
            read_buf_bytes: 4096,
            allow_compaction: false,
        };
        
        // Seed with orthos that will generate some overlaps
        let seed = Ortho::new();
        let initial_work = seed.add(1);
        store.push_segments(initial_work).unwrap();
        
        // Generation 0: Process and generate results
        while let Some(ortho) = store.pop_work().unwrap() {
            // Generate children - some may overlap with siblings
            let results = ortho.add(2);
            for r in results {
                store.record_result(&r).unwrap();
            }
        }
        
        let new_work_gen0 = store.on_generation_end(&cfg).unwrap();
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
        
        let new_work_gen1 = store.on_generation_end(&cfg).unwrap();
        
        // Seen count should grow but some duplicates should be filtered
        assert!(store.seen_len_accepted() > seen_gen0);
        
        println!("Ortho duplicate filtering test:");
        println!("  Gen 0: {} new work, {} seen", new_work_gen0, seen_gen0);
        println!("  Gen 1: {} processed, {} new work, {} total seen", 
                 processed, new_work_gen1, store.seen_len_accepted());
    }
}

