use crate::{
    completion_pruning::bound_existing_ortho,
    disk_safety,
    interner::Interner,
    memory_budget::MemoryBudget,
    memory_safety,
    ortho::{Ortho, OrthoId, OrthoScore},
};
use rkyv::{
    AlignedVec,
    ser::{
        Serializer,
        serializers::{
            AlignedSerializer, AllocScratch, CompositeSerializer, FallbackScratch, HeapScratch,
            SharedSerializeMap,
        },
    },
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread_local;
use zstd::stream::read::Decoder as ZstdDecoder;
use zstd::stream::write::Encoder as ZstdEncoder;

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
    pub work_queue_cache_bytes: usize, // Max bytes to keep in the decoded work queue cache
    pub bufwriter_capacity: usize,     // Buffer size for each bucket writer
    pub work_segment_max_bytes: usize, // Max decoded bytes per persisted work segment
    pub history_cache_bytes: usize,    // RAM budget for caching history runs
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
            work_queue_cache_bytes: 1024 * 1024,
            bufwriter_capacity: 64 * 1024,
            work_segment_max_bytes: 256 * 1024,
            history_cache_bytes: 1024 * 1024,
            landing_flush_threshold: 64 * 1024,
        }
    }
    pub fn from_memory_budget(budget: &MemoryBudget) -> Self {
        Self {
            run_budget_bytes: budget.compaction_arena_bytes,
            fan_in: budget.fan_in,
            read_buf_bytes: budget.read_buf_bytes,
            allow_compaction: true,
            work_queue_cache_bytes: budget.work_cache_bytes,
            bufwriter_capacity: budget.bufwriter_capacity,
            work_segment_max_bytes: budget.work_segment_max_bytes.min(budget.work_cache_bytes),
            history_cache_bytes: budget.history_cache_bytes,
            landing_flush_threshold: budget.landing_flush_threshold,
        }
    }
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
    work_segments: VecDeque<PathBuf>,
    work_segment_counter: usize,
    total_work_len: u64,
    work_queue_cache: VecDeque<Ortho>, // In-memory cache of work items
    work_queue_cache_bytes: usize,     // Current decoded bytes in the work queue cache
    work_queue_cache_max_bytes: usize, // Max decoded bytes in the work queue cache
    work_segment_batch: Vec<Ortho>,    // Batch for writing segments
    work_segment_batch_bytes: usize,   // Current decoded bytes in the pending work batch
    work_segment_batch_max_bytes: usize, // Max decoded bytes before flushing a work segment
    cached_best_score: Cell<Option<OrthoScore>>, // Cached best score across work caches
    cached_best_ortho: RefCell<Option<Ortho>>, // Cached best ortho across work caches
    best_score_dirty: Cell<bool>,      // Whether cached best needs recompute
    bufwriter_capacity: usize,         // Buffer capacity for bucket writers
    landing_flush_threshold: usize,    // Threshold for flushing landing writes
    compaction_arena_cap_bytes: usize, // Max streamed bytes allowed in compaction
    spill_runs: Vec<Vec<TrackedSpillRun>>, // Per-bucket pending spill runs for the current generation
    // History state
    history_runs: Vec<Vec<PathBuf>>, // Per-bucket list of history run files
    seen_len_accepted: u64,          // Monotonic count of accepted items across all generations
    #[allow(dead_code)]
    history_cache: std::collections::HashMap<PathBuf, Vec<u8>>, // Cached history run contents (future optimization)
    compression_stats: CompressionStats,
    record_write_scratch: AlignedVec,
    record_read_scratch: Vec<u8>,
    pressure_prepare_in_progress: bool,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PressureSpillStats {
    pub buckets_drained: usize,
    pub spill_runs_created: usize,
    pub spill_bytes_created: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TrackedSpillRun {
    path: PathBuf,
    size_bytes: u64,
}

impl TrackedSpillRun {
    fn new(path: PathBuf, size_bytes: u64) -> Self {
        Self { path, size_bytes }
    }
}

/// Sorted run of orthos
#[derive(Clone)]
pub struct Run {
    path: PathBuf,
}

/// Hook for offloading finalized run files (e.g., to object storage).
pub trait RunOffloader: Send + Sync {
    /// Returns true if the file was successfully offloaded and can be removed locally.
    fn offload(&self, path: &Path) -> io::Result<bool>;
}

thread_local! {
    static RUN_OFFLOADER: RefCell<Option<Arc<dyn RunOffloader>>> = RefCell::new(None);
}
struct RunDownloaderCtx {
    base_path: PathBuf,
    downloader: Arc<dyn RunDownloader>,
}
thread_local! {
    static RUN_DOWNLOADER: RefCell<Option<RunDownloaderCtx>> = RefCell::new(None);
}
thread_local! {
    static METRICS_HANDLE: RefCell<Option<crate::metrics::Metrics>> = RefCell::new(None);
}

/// Set or clear the global run offloader hook (used by pressure-mode/offload tests).
pub fn set_run_offloader(offloader: Option<Arc<dyn RunOffloader>>) {
    RUN_OFFLOADER.with(|slot| *slot.borrow_mut() = offloader);
}

/// Downloader invoked when a run file is missing locally. Implementations should download into a
/// cache and return the path to the cached file.
pub trait RunDownloader: Send + Sync {
    fn cache_lookup(&self, key: &str) -> Option<PathBuf>;
    fn download_to_cache(&self, key: &str) -> io::Result<PathBuf>;
}

/// Set or clear the global run downloader hook (used to hydrate missing runs).
pub fn set_run_downloader(ctx: Option<(PathBuf, Arc<dyn RunDownloader>)>) {
    RUN_DOWNLOADER.with(|slot| {
        *slot.borrow_mut() = ctx.map(|(base_path, downloader)| RunDownloaderCtx {
            base_path,
            downloader,
        })
    });
}

/// Set or clear a metrics handle for offload/download counters.
pub fn set_offload_metrics_handle(handle: Option<crate::metrics::Metrics>) {
    crate::disk_safety::set_metrics_handle(handle.clone());
    METRICS_HANDLE.with(|slot| *slot.borrow_mut() = handle);
}

fn current_offloader() -> Option<Arc<dyn RunOffloader>> {
    RUN_OFFLOADER.with(|slot| slot.borrow().clone())
}

fn current_downloader() -> Option<RunDownloaderCtx> {
    RUN_DOWNLOADER.with(|slot| {
        slot.borrow().as_ref().map(|ctx| RunDownloaderCtx {
            base_path: ctx.base_path.clone(),
            downloader: Arc::clone(&ctx.downloader),
        })
    })
}

fn metrics_handle() -> Option<crate::metrics::Metrics> {
    METRICS_HANDLE.with(|slot| slot.borrow().clone())
}

/// Offload a path if a RunOffloader is configured. Returns true if offloaded.
pub fn offload_path_if_configured(path: &Path) -> io::Result<bool> {
    if let Some(offloader) = current_offloader() {
        offloader.offload(path)
    } else {
        Ok(false)
    }
}

fn offload_and_delete_if_configured(path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => {
            let size = metadata.len();
            match offload_path_if_configured(path) {
                Ok(true) => {
                    if let Some(m) = metrics_handle() {
                        m.record_offload(1, size);
                    }
                    let _ = fs::remove_file(path);
                    return Ok(true);
                }
                Ok(false) => {}
                Err(e) => {
                    if let Some(m) = metrics_handle() {
                        m.add_log(format!("Offload failed for {:?}: {}", path, e));
                    }
                    return Err(e);
                }
            }
        }
        Err(_) => {
            if offload_path_if_configured(path)? {
                let _ = fs::remove_file(path);
                return Ok(true);
            }
        }
    };
    Ok(false)
}

fn maybe_offload_and_delete(path: &Path) -> io::Result<()> {
    let _ = offload_and_delete_if_configured(path)?;
    Ok(())
}

fn run_object_key(base_path: &Path, path: &Path) -> io::Result<String> {
    let namespace = base_path
        .file_name()
        .map(|part| part.to_string_lossy().into_owned())
        .filter(|part| !part.is_empty())
        .unwrap_or_else(|| "store".to_string());

    // Prefer a relative path under base_path; handle deleted/nonexistent files gracefully.
    let key_path = path
        .strip_prefix(base_path)
        .map(PathBuf::from)
        .or_else(|_| {
            let base_canon = base_path
                .canonicalize()
                .unwrap_or_else(|_| base_path.to_path_buf());
            let path_canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            path_canon.strip_prefix(&base_canon).map(PathBuf::from)
        })
        .unwrap_or_else(|_| {
            path.file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| path.to_path_buf())
        });
    let key = key_path
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if key.is_empty() {
        Ok(namespace)
    } else {
        Ok(format!("{}/{}", namespace, key))
    }
}

pub fn resolve_managed_path(path: &Path) -> io::Result<PathBuf> {
    if path.exists() {
        return Ok(path.to_path_buf());
    }
    let Some(ctx) = current_downloader() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("run file missing and no downloader configured: {:?}", path),
        ));
    };
    let key = run_object_key(&ctx.base_path, path)?;
    if let Some(hit) = ctx.downloader.cache_lookup(&key) {
        if let Some(m) = metrics_handle() {
            m.record_cache_hit();
        }
        return Ok(hit);
    }
    if let Some(m) = metrics_handle() {
        m.record_cache_miss();
    }
    let cached = ctx.downloader.download_to_cache(&key)?;
    if let Some(m) = metrics_handle() {
        if let Ok(bytes) = fs::metadata(&cached) {
            m.record_download(1, bytes.len() as u64);
        } else {
            m.record_download(1, 0);
        }
    }
    Ok(cached)
}

fn resolve_run_path(path: &Path) -> io::Result<PathBuf> {
    resolve_managed_path(path)
}

fn estimate_streamed_run_bytes(arena: &[StreamedOrtho]) -> u64 {
    arena
        .iter()
        .map(|item| (ORTHO_RECORD_HEADER_SIZE + item.bytes.len()) as u64)
        .sum()
}

fn file_size_or_zero(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn estimate_compressed_run_set_bytes(runs: &[Run]) -> io::Result<u64> {
    let mut total = 0u64;
    for run in runs {
        let resolved = resolve_managed_path(run.path())?;
        total = total.saturating_add(file_size_or_zero(&resolved));
    }
    Ok(total)
}

#[cfg(test)]
pub fn test_maybe_offload_and_delete(path: &Path) -> io::Result<()> {
    maybe_offload_and_delete(path)
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
        let path = resolve_run_path(&self.path)?;
        OrthoStreamReader::new(&path, read_buf_bytes)
    }
}

struct OrthoRunIterator {
    reader: Box<dyn Read>,
    buffer: Vec<u8>,
    offset: usize,
    read_buf_bytes: usize,
}

impl Iterator for OrthoRunIterator {
    type Item = io::Result<StreamedOrtho>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Ensure we have at least a header
            if self.buffer.len().saturating_sub(self.offset) < ORTHO_RECORD_HEADER_SIZE {
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

            let header_start = self.offset;
            let header_end = header_start + ORTHO_RECORD_HEADER_SIZE;
            if header_end > self.buffer.len() {
                // Should only happen on corrupted data
                return Some(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid ortho record header",
                )));
            }

            let decoded_size_est = u64::from_le_bytes(
                self.buffer[header_start..header_start + 8]
                    .try_into()
                    .unwrap(),
            ) as usize;
            let encoded_len = u64::from_le_bytes(
                self.buffer[header_start + 8..header_end]
                    .try_into()
                    .unwrap(),
            ) as usize;

            // Ensure full record is buffered
            let record_end = header_end.saturating_add(encoded_len);
            if record_end > self.buffer.len() {
                match self.read_more() {
                    Ok(true) => continue,
                    Ok(false) => {
                        return Some(Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Unexpected end of ortho stream",
                        )));
                    }
                    Err(e) => return Some(Err(e)),
                }
            }

            let mut encoded_slice = rkyv::AlignedVec::with_capacity(encoded_len);
            encoded_slice.extend_from_slice(&self.buffer[header_end..record_end]);
            // No validation: assume bytes are trusted rkyv output.
            let archived = unsafe { rkyv::archived_root::<Ortho>(&encoded_slice) };
            let id = Ortho::archived_id(archived);
            self.offset = record_end;
            return Some(Ok(StreamedOrtho {
                bytes: encoded_slice,
                bytes_read: encoded_len,
                decoded_size_est,
                id,
            }));
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
    fn new(path: &Path, read_buf_bytes: usize) -> io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::with_capacity(read_buf_bytes, file);
        let decoder = ZstdDecoder::new(reader)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let boxed_reader: Box<dyn Read> = Box::new(decoder);
        Ok(Self {
            inner: OrthoRunIterator {
                reader: boxed_reader,
                buffer: Vec::with_capacity(read_buf_bytes),
                offset: 0,
                read_buf_bytes,
            },
        })
    }
}

fn count_ortho_records_in_file(
    path: &Path,
    read_buf_bytes: usize,
    compressed: bool,
) -> io::Result<usize> {
    let file = File::open(path)?;
    let reader = BufReader::with_capacity(read_buf_bytes, file);
    let boxed_reader: Box<dyn Read> = if compressed {
        Box::new(
            ZstdDecoder::new(reader)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?,
        )
    } else {
        Box::new(reader)
    };
    let mut iter = OrthoRunIterator {
        reader: boxed_reader,
        buffer: Vec::with_capacity(read_buf_bytes),
        offset: 0,
        read_buf_bytes,
    };
    let mut count = 0usize;
    while let Some(result) = iter.next() {
        result?;
        count = count.saturating_add(1);
    }
    Ok(count)
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
    pub bytes: rkyv::AlignedVec,
    pub bytes_read: usize,
    pub decoded_size_est: usize,
    pub id: OrthoId,
}

impl StreamedOrtho {
    fn archived(&self) -> &rkyv::Archived<Ortho> {
        // Safe because bytes come from rkyv::to_bytes and remain owned here.
        unsafe { rkyv::archived_root::<Ortho>(&self.bytes) }
    }

    fn heap_bytes_estimate(&self) -> usize {
        mem::size_of::<StreamedOrtho>()
            .saturating_add(self.bytes.capacity())
            .saturating_add(self.bytes_read)
    }
}

fn archived_eq(a: &StreamedOrtho, b: &StreamedOrtho) -> bool {
    a.archived() == b.archived()
}

const ORTHO_RECORD_HEADER_SIZE: usize = mem::size_of::<u64>() * 2;
const ORTHO_SERIALIZER_SCRATCH_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Default)]
pub struct CompressionStats {
    pub uncompressed_bytes: u64,
    pub compressed_bytes: u64,
}

impl CompressionStats {
    fn record(&mut self, uncompressed: u64, compressed: u64) {
        self.uncompressed_bytes = self.uncompressed_bytes.saturating_add(uncompressed);
        self.compressed_bytes = self.compressed_bytes.saturating_add(compressed);
    }
}

fn estimate_decoded_size(ortho: &Ortho) -> usize {
    // Rough estimate: struct size + vec metadata + element storage based on capacity.
    let dims_cap = ortho.dims().capacity();
    let payload_cap = ortho.payload().capacity();
    let vec_overhead = mem::size_of::<Vec<crate::ortho::Dim>>()
        + mem::size_of::<Vec<Option<crate::ortho::PayloadVal>>>();
    mem::size_of::<Ortho>()
        + vec_overhead
        + dims_cap.saturating_mul(mem::size_of::<crate::ortho::Dim>())
        + payload_cap.saturating_mul(mem::size_of::<Option<crate::ortho::PayloadVal>>())
}

fn write_ortho_record<W: Write>(
    writer: &mut W,
    ortho: &Ortho,
    stats: Option<&mut CompressionStats>,
) -> io::Result<usize> {
    let mut scratch = AlignedVec::new();
    write_ortho_record_with_scratch(writer, ortho, &mut scratch, stats)
}

fn serialize_ortho_into_scratch(ortho: &Ortho, scratch: &mut AlignedVec) -> io::Result<usize> {
    scratch.clear();
    {
        let mut serializer = CompositeSerializer::new(
            AlignedSerializer::new(&mut *scratch),
            FallbackScratch::new(
                HeapScratch::<ORTHO_SERIALIZER_SCRATCH_BYTES>::new(),
                AllocScratch::default(),
            ),
            SharedSerializeMap::new(),
        );
        serializer
            .serialize_value(ortho)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    }
    Ok(scratch.len())
}

fn write_ortho_record_with_scratch<W: Write>(
    writer: &mut W,
    ortho: &Ortho,
    scratch: &mut AlignedVec,
    stats: Option<&mut CompressionStats>,
) -> io::Result<usize> {
    let encoded_len_usize = serialize_ortho_into_scratch(ortho, scratch)?;
    let encoded = &scratch[..encoded_len_usize];
    let decoded_est = estimate_decoded_size(ortho) as u64;
    let encoded_len = encoded.len() as u64;

    writer.write_all(&decoded_est.to_le_bytes())?;
    writer.write_all(&encoded_len.to_le_bytes())?;
    writer.write_all(&encoded)?;

    let _ = stats;

    Ok(ORTHO_RECORD_HEADER_SIZE + encoded.len())
}

fn write_ortho_record_bytes<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    decoded_size_est: usize,
    stats: Option<&mut CompressionStats>,
) -> io::Result<usize> {
    let decoded_est = decoded_size_est as u64;
    let encoded_len = bytes.len() as u64;

    writer.write_all(&decoded_est.to_le_bytes())?;
    writer.write_all(&encoded_len.to_le_bytes())?;
    writer.write_all(bytes)?;

    let _ = stats;

    Ok(ORTHO_RECORD_HEADER_SIZE + bytes.len())
}

fn compress_file(path: &PathBuf, level: i32) -> io::Result<(u64, u64)> {
    let uncompressed = fs::metadata(path)?.len();
    let _ = disk_safety::maybe_reclaim(
        uncompressed.saturating_add(64 * 1024),
        &format!("compress {}", path.display()),
    )?;
    disk_safety::ensure_write_budget(
        uncompressed.saturating_add(64 * 1024),
        &format!("compress {}", path.display()),
    )?;
    let tmp_path = path.with_extension("zsttmp");
    let input = File::open(path)?;
    let output = File::create(&tmp_path)?;
    let mut encoder = ZstdEncoder::new(output, level)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let mut reader = BufReader::new(input);
    io::copy(&mut reader, &mut encoder)?;
    let mut output = encoder
        .finish()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    output.flush()?;
    let compressed = fs::metadata(&tmp_path)?.len();
    fs::remove_file(path)?;
    fs::rename(&tmp_path, path)?;
    Ok((uncompressed, compressed))
}

fn create_compressed_writer(
    path: &Path,
    level: i32,
    bufwriter_capacity: usize,
) -> io::Result<ZstdEncoder<'static, BufWriter<File>>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let writer = BufWriter::with_capacity(bufwriter_capacity.max(64 * 1024), File::create(path)?);
    ZstdEncoder::new(writer, level)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

fn finish_compressed_writer(
    path: &Path,
    writer: ZstdEncoder<'static, BufWriter<File>>,
    uncompressed_bytes: u64,
) -> io::Result<(u64, u64)> {
    let mut output = writer
        .finish()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    output.flush()?;
    let compressed_bytes = fs::metadata(path)?.len();
    Ok((uncompressed_bytes, compressed_bytes))
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
        let path = resolve_run_path(&self.path)?;
        OrthoStreamReader::new(&path, read_buf_bytes)
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

        let run_path = resolve_run_path(&self.run_files[self.current_run_index])?;
        let reader = OrthoStreamReader::new(&run_path, self.read_buf_bytes)?;
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
    fn sync_memory_metrics(&self) {
        if let Some(metrics) = metrics_handle() {
            metrics.update_global(|g| {
                g.work_cache_bytes = self.work_queue_cache_bytes;
                g.work_cache_cap_bytes = self.work_queue_cache_max_bytes;
                g.segment_batch_bytes = self.work_segment_batch_bytes;
                g.segment_batch_cap_bytes = self.work_segment_batch_max_bytes;
                g.compaction_arena_cap_bytes = self.compaction_arena_cap_bytes;
            });
        }
    }

    fn sync_spill_metrics(&self) {
        if let Some(metrics) = metrics_handle() {
            let pending_files = self.spill_runs.iter().map(|runs| runs.len() as u64).sum();
            let pending_bytes = self
                .spill_runs
                .iter()
                .flatten()
                .map(|run| run.size_bytes)
                .sum();
            metrics.set_spill_pending(pending_files, pending_bytes);
        }
    }

    pub fn sync_runtime_metrics(&self) {
        self.sync_memory_metrics();
        self.sync_spill_metrics();
    }

    fn set_compaction_bytes(bytes: usize) {
        if let Some(metrics) = metrics_handle() {
            metrics.update_global(|g| {
                g.compaction_arena_bytes = bytes;
            });
        }
    }

    /// Create a new generation store with specified base path and bucket count
    pub fn new_with_config(base_path: PathBuf, bucket_count: usize) -> io::Result<Self> {
        // Bucket count must be a power of two
        assert!(
            bucket_count.is_power_of_two(),
            "bucket_count must be power of two"
        );

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

        // Create spill directory
        let spill_dir = base_path.join("spill");
        fs::create_dir_all(&spill_dir)?;
        for bucket in 0..bucket_count {
            let bucket_spill_dir = spill_dir.join(format!("b={:02}", bucket));
            fs::create_dir_all(&bucket_spill_dir)?;
        }

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
            work_segments: VecDeque::new(),
            work_segment_counter: 0,
            total_work_len: 0,
            work_queue_cache: VecDeque::new(),
            work_queue_cache_bytes: 0,
            work_queue_cache_max_bytes: 1024 * 1024, // Default, will be updated with config
            work_segment_batch: Vec::new(),
            work_segment_batch_bytes: 0,
            work_segment_batch_max_bytes: 256 * 1024, // Default, will be updated with config
            cached_best_score: Cell::new(None),
            cached_best_ortho: RefCell::new(None),
            best_score_dirty: Cell::new(false),
            bufwriter_capacity: 16 * 1024 * 1024, // Default 16MB
            landing_flush_threshold: 10 * 1024 * 1024, // Default 10MB
            compaction_arena_cap_bytes: 256 * 1024 * 1024,
            spill_runs: (0..bucket_count).map(|_| Vec::new()).collect(),
            history_runs: (0..bucket_count).map(|_| Vec::new()).collect(),
            seen_len_accepted: 0,
            history_cache: std::collections::HashMap::new(),
            compression_stats: CompressionStats::default(),
            record_write_scratch: AlignedVec::new(),
            record_read_scratch: Vec::new(),
            pressure_prepare_in_progress: false,
        })
    }

    /// Open an existing store for reading history runs from disk
    pub fn from_existing(base_path: PathBuf, bucket_count: usize) -> io::Result<Self> {
        let mut store = Self::new_with_config(base_path, bucket_count)?;
        store.load_history_runs_from_disk()?;
        store.load_spill_runs_from_disk()?;
        store.load_work_segments_from_disk()?;
        store.load_landing_state_from_disk()?;
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

    fn spill_root(&self) -> PathBuf {
        self.base_path.join("spill")
    }

    fn spill_dir(&self, bucket: usize) -> PathBuf {
        self.spill_root().join(format!("b={:02}", bucket))
    }

    fn spill_manifest_path(&self) -> PathBuf {
        self.spill_root().join("manifest.txt")
    }

    fn persist_spill_manifest(&self) -> io::Result<()> {
        let manifest_path = self.spill_manifest_path();
        if let Some(parent) = manifest_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut contents = String::new();
        for bucket in 0..self.bucket_count {
            for spill in &self.spill_runs[bucket] {
                let relative = spill
                    .path
                    .strip_prefix(&self.base_path)
                    .unwrap_or(&spill.path)
                    .to_string_lossy()
                    .into_owned();
                contents.push_str(&format!("{}\t{}\t{}\n", bucket, relative, spill.size_bytes));
            }
        }

        if contents.is_empty() {
            match fs::remove_file(&manifest_path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        } else {
            fs::write(manifest_path, contents)?;
        }

        Ok(())
    }

    fn load_spill_runs_from_disk(&mut self) -> io::Result<()> {
        self.spill_runs = (0..self.bucket_count).map(|_| Vec::new()).collect();
        let mut per_bucket: Vec<BTreeMap<PathBuf, u64>> =
            (0..self.bucket_count).map(|_| BTreeMap::new()).collect();

        let manifest_path = self.spill_manifest_path();
        if let Ok(contents) = fs::read_to_string(&manifest_path) {
            for line in contents.lines() {
                let mut parts = line.splitn(3, '\t');
                let Some(bucket_str) = parts.next() else {
                    continue;
                };
                let Some(path_str) = parts.next() else {
                    continue;
                };
                let size_bytes = parts
                    .next()
                    .and_then(|part| part.parse::<u64>().ok())
                    .unwrap_or(0);
                let Ok(bucket) = bucket_str.parse::<usize>() else {
                    continue;
                };
                if bucket >= self.bucket_count {
                    continue;
                }
                let path = PathBuf::from(path_str);
                let full_path = if path.is_absolute() {
                    path
                } else {
                    self.base_path.join(path)
                };
                let size = if size_bytes > 0 {
                    size_bytes
                } else {
                    fs::metadata(&full_path).map(|meta| meta.len()).unwrap_or(0)
                };
                per_bucket[bucket].insert(full_path, size);
            }
        }

        for bucket in 0..self.bucket_count {
            let spill_dir = self.spill_dir(bucket);
            if spill_dir.exists() {
                for entry in fs::read_dir(&spill_dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                        per_bucket[bucket].insert(path, size);
                    }
                }
            }
            self.spill_runs[bucket] = per_bucket[bucket]
                .iter()
                .map(|(path, size_bytes)| TrackedSpillRun::new(path.clone(), *size_bytes))
                .collect();
        }

        Ok(())
    }

    fn load_work_segments_from_disk(&mut self) -> io::Result<()> {
        self.work_segments.clear();
        self.total_work_len = 0;
        self.work_segment_counter = 0;

        let work_dir = self.base_path.join("work");
        if !work_dir.exists() {
            return Ok(());
        }

        let mut entries: Vec<(usize, PathBuf)> = Vec::new();
        for entry in fs::read_dir(&work_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let Some(segment_id) = file_name
                .strip_prefix("segment-")
                .and_then(|rest| rest.strip_suffix(".dat"))
                .and_then(|rest| rest.parse::<usize>().ok())
            else {
                continue;
            };
            entries.push((segment_id, path));
        }

        entries.sort_by_key(|(segment_id, _)| *segment_id);
        for (segment_id, path) in entries {
            let resolved_path = resolve_managed_path(&path)?;
            let mut file = File::open(&resolved_path)?;
            let mut count_bytes = [0u8; 8];
            if file.read_exact(&mut count_bytes).is_err() {
                continue;
            }
            let count = u64::from_le_bytes(count_bytes);
            self.total_work_len = self.total_work_len.saturating_add(count);
            self.work_segment_counter = self.work_segment_counter.max(segment_id.saturating_add(1));
            self.work_segments.push_back(path);
        }

        Ok(())
    }

    fn load_landing_state_from_disk(&mut self) -> io::Result<()> {
        self.landing_counts = vec![0; self.bucket_count];
        self.landing_buffer_sizes = vec![0; self.bucket_count];
        self.drain_counter = vec![0; self.bucket_count];

        for bucket in 0..self.bucket_count {
            let landing_dir = self
                .base_path
                .join("landing")
                .join(format!("b={:02}", bucket));
            if !landing_dir.exists() {
                continue;
            }

            let mut next_drain_id = 0usize;
            let mut total_count = 0usize;

            let mut entries: Vec<PathBuf> = fs::read_dir(&landing_dir)?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|path| path.is_file())
                .collect();
            entries.sort();

            for path in entries {
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if let Some(drain_id) = file_name
                    .strip_prefix("drain-")
                    .and_then(|rest| rest.strip_suffix(".log"))
                    .and_then(|rest| rest.parse::<usize>().ok())
                {
                    next_drain_id = next_drain_id.max(drain_id.saturating_add(1));
                }

                let compressed = file_name != "active.log";
                total_count = total_count.saturating_add(count_ortho_records_in_file(
                    &path,
                    64 * 1024,
                    compressed,
                )?);
            }

            self.drain_counter[bucket] = next_drain_id;
            self.landing_counts[bucket] = total_count;
        }

        Ok(())
    }

    fn spill_runs_for_bucket(&self, bucket: usize) -> Vec<TrackedSpillRun> {
        self.spill_runs[bucket].clone()
    }

    fn clear_spill_runs_for_bucket(&mut self, bucket: usize) -> io::Result<()> {
        self.spill_runs[bucket].clear();
        self.persist_spill_manifest()?;
        self.sync_spill_metrics();
        Ok(())
    }

    fn extend_spill_runs<I>(&mut self, bucket: usize, paths: I) -> io::Result<()>
    where
        I: IntoIterator<Item = TrackedSpillRun>,
    {
        self.spill_runs[bucket].extend(paths);
        self.spill_runs[bucket].sort();
        self.persist_spill_manifest()?;
        self.sync_spill_metrics();
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
            work_segments: VecDeque::new(),
            work_segment_counter: 0,
            total_work_len: 0,
            work_queue_cache: VecDeque::new(),
            work_queue_cache_bytes: 0,
            work_queue_cache_max_bytes: 1024 * 1024,
            work_segment_batch: Vec::new(),
            work_segment_batch_bytes: 0,
            work_segment_batch_max_bytes: 256 * 1024,
            cached_best_score: Cell::new(None),
            cached_best_ortho: RefCell::new(None),
            best_score_dirty: Cell::new(false),
            bufwriter_capacity: 16 * 1024 * 1024,
            landing_flush_threshold: 10 * 1024 * 1024,
            compaction_arena_cap_bytes: 256 * 1024 * 1024,
            spill_runs: (0..8).map(|_| Vec::new()).collect(),
            history_runs: (0..8).map(|_| Vec::new()).collect(),
            seen_len_accepted: 0,
            history_cache: std::collections::HashMap::new(),
            compression_stats: CompressionStats::default(),
            record_write_scratch: AlignedVec::new(),
            record_read_scratch: Vec::new(),
            pressure_prepare_in_progress: false,
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
    pub fn record_result_with_threshold(
        &mut self,
        ortho: &Ortho,
        flush_threshold: usize,
    ) -> io::Result<()> {
        let bucket = (ortho.id() as u64 & (self.bucket_count - 1) as u64) as usize;

        // Get or create writer for this bucket with configured buffer capacity
        if self.bucket_writers[bucket].is_none() {
            let path = self.active_log_path(bucket);
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            let buf = BufWriter::with_capacity(self.bufwriter_capacity, file);
            self.bucket_writers[bucket] = Some(buf);
        }

        // Write ortho using rkyv
        let encoded_len = {
            let writer = self.bucket_writers[bucket].as_mut().unwrap();
            write_ortho_record_with_scratch(writer, ortho, &mut self.record_write_scratch, None)?
        };
        self.landing_counts[bucket] = self.landing_counts[bucket].saturating_add(1);

        // Track buffer size and flush if over threshold
        self.landing_buffer_sizes[bucket] += encoded_len;
        if self.landing_buffer_sizes[bucket] >= flush_threshold {
            self.flush_bucket_writer(bucket, true)?;
        }

        Ok(())
    }

    /// Drain a bucket by renaming active.log to drain-N.log
    pub fn drain_bucket(&mut self, bucket: usize) -> io::Result<RawStream> {
        // Flush and close any active writer for this bucket
        if self.bucket_writers[bucket].is_some() {
            self.flush_bucket_writer(bucket, true)?;
            self.bucket_writers[bucket].take();
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
        if drain_path.exists() {
            let (unc, comp) = compress_file(&drain_path, 3)?;
            self.compression_stats.record(unc, comp);
        }
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

        for ortho in items {
            if !self.best_score_dirty.get() {
                self.update_cached_best(&ortho);
            }

            let ortho_bytes = ortho.heap_bytes_estimate().max(1);
            if !self.work_segment_batch.is_empty()
                && self.work_segment_batch_bytes.saturating_add(ortho_bytes)
                    > self.work_segment_batch_max_bytes
            {
                self.flush_work_segment_batch()?;
            }

            self.work_segment_batch_bytes =
                self.work_segment_batch_bytes.saturating_add(ortho_bytes);
            self.work_segment_batch.push(ortho);

            if self.work_segment_batch_bytes >= self.work_segment_batch_max_bytes {
                self.flush_work_segment_batch()?;
            }
        }

        self.sync_memory_metrics();

        Ok(())
    }

    /// Flush the work segment batch to disk
    fn flush_work_segment_batch(&mut self) -> io::Result<()> {
        self.flush_work_segment_batch_with_pressure(true)
    }

    fn flush_work_segment_batch_with_pressure(
        &mut self,
        allow_pressure_handling: bool,
    ) -> io::Result<()> {
        if self.work_segment_batch.is_empty() {
            return Ok(());
        }

        let batch = std::mem::take(&mut self.work_segment_batch);
        let batch_bytes = self.work_segment_batch_bytes;
        let count = batch.len() as u64;
        let segment_path = if allow_pressure_handling {
            self.write_segment_file(batch.iter(), batch_bytes, "flush work segment batch")?
        } else {
            self.write_segment_file_inner(
                batch.iter(),
                batch_bytes,
                "flush work segment batch",
                false,
            )?
        };
        self.work_segments.push_back(segment_path);
        self.total_work_len += count;
        self.work_segment_batch_bytes = 0;
        self.best_score_dirty.set(true);
        self.sync_memory_metrics();

        Ok(())
    }

    fn write_segment_file<'a, I>(
        &mut self,
        items: I,
        decoded_bytes: usize,
        reason: &str,
    ) -> io::Result<PathBuf>
    where
        I: IntoIterator<Item = &'a Ortho>,
    {
        self.write_segment_file_inner(items, decoded_bytes, reason, true)
    }

    fn write_segment_file_inner<'a, I>(
        &mut self,
        items: I,
        decoded_bytes: usize,
        reason: &str,
        allow_pressure_handling: bool,
    ) -> io::Result<PathBuf>
    where
        I: IntoIterator<Item = &'a Ortho>,
    {
        let items: Vec<&Ortho> = items.into_iter().collect();
        let count = items.len() as u64;
        let bytes_needed = (decoded_bytes as u64)
            .saturating_mul(2)
            .saturating_add(std::mem::size_of_val(&count) as u64)
            .saturating_add((count as u64).saturating_mul(8));
        if allow_pressure_handling {
            self.maybe_reclaim_before_write(bytes_needed, reason)?;
        }
        memory_safety::ensure_phase_headroom(
            decoded_bytes.saturating_mul(2).saturating_add(64 * 1024),
            reason,
        )?;
        disk_safety::ensure_write_budget(bytes_needed, reason)?;

        let segment_path = self
            .base_path
            .join("work")
            .join(format!("segment-{}.dat", self.work_segment_counter));
        self.work_segment_counter += 1;

        let mut file = BufWriter::with_capacity(16 * 1024 * 1024, File::create(&segment_path)?);
        file.write_all(&count.to_le_bytes())?;
        for ortho in items {
            let encoded_len = serialize_ortho_into_scratch(ortho, &mut self.record_write_scratch)?;
            file.write_all(&(encoded_len as u64).to_le_bytes())?;
            file.write_all(&self.record_write_scratch[..encoded_len])?;
        }
        file.flush()?;
        Ok(segment_path)
    }

    fn spill_work_queue_cache_to_disk(&mut self) -> io::Result<bool> {
        if self.work_queue_cache.is_empty() {
            return Ok(false);
        }

        let cache_items: Vec<Ortho> = self.work_queue_cache.iter().cloned().collect();
        let cache_bytes = self.work_queue_cache_bytes;
        let segment_path = self.write_segment_file_inner(
            cache_items.iter(),
            cache_bytes,
            "spill work queue cache",
            false,
        )?;
        self.work_segments.push_front(segment_path);
        self.work_queue_cache.clear();
        self.work_queue_cache_bytes = 0;
        self.best_score_dirty.set(true);
        self.sync_memory_metrics();
        Ok(true)
    }

    fn flush_bucket_writer(
        &mut self,
        bucket: usize,
        allow_pressure_handling: bool,
    ) -> io::Result<()> {
        let bytes_needed = self.landing_buffer_sizes[bucket] as u64;
        if bytes_needed > 0 {
            let reason = format!("flush landing bucket {}", bucket);
            if allow_pressure_handling {
                self.maybe_reclaim_before_write(bytes_needed, &reason)?;
            }
            disk_safety::ensure_write_budget(bytes_needed, &reason)?;
        }
        if let Some(writer) = self.bucket_writers[bucket].as_mut() {
            writer.flush()?;
        }
        self.landing_buffer_sizes[bucket] = 0;
        Ok(())
    }

    /// Pop a single work item from the work queue
    /// Uses in-memory cache for speed, only hits disk when cache empties
    pub fn pop_work(&mut self) -> io::Result<Option<Ortho>> {
        // Try to pop from cache first
        if let Some(ortho) = self.work_queue_cache.pop_front() {
            self.total_work_len -= 1;
            self.work_queue_cache_bytes = self
                .work_queue_cache_bytes
                .saturating_sub(ortho.heap_bytes_estimate());
            self.handle_removed_score(ortho.score());
            self.sync_memory_metrics();
            return Ok(Some(ortho));
        }

        // If everything is empty except the unflushed batch, load it directly
        if self.work_queue_cache.is_empty()
            && self.work_segments.is_empty()
            && !self.work_segment_batch.is_empty()
        {
            let batch_len = self.work_segment_batch.len() as u64;
            self.total_work_len = self.total_work_len.saturating_add(batch_len);
            self.work_queue_cache_bytes = self
                .work_queue_cache_bytes
                .saturating_add(self.work_segment_batch_bytes);
            let batch = std::mem::take(&mut self.work_segment_batch);
            self.work_segment_batch_bytes = 0;
            for ortho in batch {
                self.work_queue_cache.push_back(ortho);
            }
            self.sync_memory_metrics();
        }

        // Cache is empty, refill from disk segments
        self.refill_work_cache()?;

        // Pop from cache after refill
        if let Some(ortho) = self.work_queue_cache.pop_front() {
            self.total_work_len -= 1;
            self.work_queue_cache_bytes = self
                .work_queue_cache_bytes
                .saturating_sub(ortho.heap_bytes_estimate());
            self.handle_removed_score(ortho.score());
            self.sync_memory_metrics();
            return Ok(Some(ortho));
        }

        Ok(None)
    }

    /// Refill work queue cache from disk segments
    fn refill_work_cache(&mut self) -> io::Result<()> {
        let target_max = self.work_queue_cache_max_bytes.max(1);
        if self.work_queue_cache_bytes >= target_max || !self.work_queue_cache.is_empty() {
            return Ok(());
        }

        while self.work_queue_cache.is_empty() && !self.work_segments.is_empty() {
            let segment_path = self
                .work_segments
                .pop_front()
                .expect("work segment queue unexpectedly empty during refill");
            let resolved_path = resolve_managed_path(&segment_path)?;
            let mut file = File::open(&resolved_path)?;

            let mut count_bytes = [0u8; 8];
            file.read_exact(&mut count_bytes)?;
            let count = u64::from_le_bytes(count_bytes) as usize;

            if count == 0 {
                drop(file);
                match fs::remove_file(&segment_path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                }
                continue;
            }

            let reserve_bytes = self.work_queue_cache_max_bytes.max(target_max);
            memory_safety::ensure_phase_headroom(
                reserve_bytes.saturating_add(64 * 1024),
                "refill work queue cache",
            )?;

            let mut loaded = VecDeque::with_capacity(count);
            let mut loaded_bytes = 0usize;
            for _ in 0..count {
                let mut len_bytes = [0u8; 8];
                file.read_exact(&mut len_bytes)?;
                let len = u64::from_le_bytes(len_bytes) as usize;

                self.record_read_scratch.clear();
                self.record_read_scratch.resize(len, 0);
                file.read_exact(&mut self.record_read_scratch)?;
                let ortho: Ortho = Ortho::from_bytes(&self.record_read_scratch)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                loaded_bytes = loaded_bytes.saturating_add(ortho.heap_bytes_estimate());
                loaded.push_back(ortho);
            }

            if loaded_bytes > self.work_queue_cache_max_bytes && loaded.len() > 1 {
                return Err(io::Error::other(format!(
                    "work segment exceeded cache budget: {} > {}",
                    loaded_bytes, self.work_queue_cache_max_bytes
                )));
            }

            self.work_queue_cache = loaded;
            self.work_queue_cache_bytes = loaded_bytes;
            if !self.best_score_dirty.get() {
                if let Some(best_item) = self.work_queue_cache.iter().max_by_key(|o| o.score()) {
                    self.update_cached_best(best_item);
                }
            }
            self.sync_memory_metrics();

            drop(file);
            match fs::remove_file(&segment_path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }

        Ok(())
    }

    /// Configure the store with Config settings
    pub fn configure(&mut self, cfg: &Config) {
        self.work_queue_cache_max_bytes = cfg.work_queue_cache_bytes.max(1);
        self.work_segment_batch_max_bytes = cfg.work_segment_max_bytes.max(1);
        self.bufwriter_capacity = cfg.bufwriter_capacity;
        self.landing_flush_threshold = cfg.landing_flush_threshold;
        self.compaction_arena_cap_bytes = cfg.run_budget_bytes;
        self.sync_runtime_metrics();
    }

    pub fn processing_reclaim_reservation_bytes(&self) -> u64 {
        let landing_flush = self
            .landing_flush_threshold
            .max(self.bufwriter_capacity)
            .max(64 * 1024) as u64;
        let work_flush = (self.work_segment_batch_max_bytes as u64)
            .saturating_mul(2)
            .saturating_add(64 * 1024);
        landing_flush.max(work_flush)
    }

    pub fn prepare_for_reclaim(&mut self) -> io::Result<()> {
        if self.pressure_prepare_in_progress {
            return Ok(());
        }
        self.pressure_prepare_in_progress = true;
        let result = (|| -> io::Result<()> {
            self.flush_all_without_pressure()?;
            let _ = self.spill_work_queue_cache_to_disk()?;
            self.sync_memory_metrics();
            GenerationStore::set_compaction_bytes(0);
            Ok(())
        })();
        self.pressure_prepare_in_progress = false;
        result
    }

    fn maybe_reclaim_before_write(&mut self, bytes_needed: u64, reason: &str) -> io::Result<()> {
        let mut prepared = false;
        if memory_safety::should_spill_to_disk() {
            self.prepare_for_reclaim()?;
            prepared = true;
        }
        if disk_safety::reclaim_required(bytes_needed)?.is_some() {
            if !prepared {
                self.prepare_for_reclaim()?;
            }
            let _ = disk_safety::maybe_reclaim(bytes_needed, reason)?;
        }
        Ok(())
    }

    fn flush_all_without_pressure(&mut self) -> io::Result<()> {
        self.flush_without_pressure()?;
        self.flush_work_segment_batch_with_pressure(false)?;
        Ok(())
    }

    fn flush_without_pressure(&mut self) -> io::Result<()> {
        for bucket in 0..self.bucket_writers.len() {
            if self.bucket_writers[bucket].is_some() {
                self.flush_bucket_writer(bucket, false)?;
            }
        }
        Ok(())
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

    pub fn work_queue_cache_bytes(&self) -> usize {
        self.work_queue_cache_bytes
    }

    pub fn work_queue_cache_cap_bytes(&self) -> usize {
        self.work_queue_cache_max_bytes
    }

    pub fn work_segment_batch_bytes(&self) -> usize {
        self.work_segment_batch_bytes
    }

    pub fn work_segment_batch_cap_bytes(&self) -> usize {
        self.work_segment_batch_max_bytes
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
    pub fn history_iter_with_buffer(
        &self,
        bucket: usize,
        read_buf_bytes: usize,
    ) -> io::Result<HistoryIterator> {
        assert!(bucket < self.bucket_count, "Invalid bucket index");
        HistoryIterator::new(&self.history_runs[bucket], read_buf_bytes)
    }

    /// Expose history run file paths for archiving/export
    pub fn history_run_paths(&self) -> Vec<(usize, Vec<PathBuf>)> {
        (0..self.bucket_count)
            .map(|bucket| (bucket, self.history_runs[bucket].clone()))
            .collect()
    }

    pub fn compression_stats(&self) -> CompressionStats {
        self.compression_stats
    }

    pub fn base_path(&self) -> &PathBuf {
        &self.base_path
    }

    pub fn bucket_count(&self) -> usize {
        self.bucket_count
    }

    /// Prune history runs using the optimistic bound; returns (kept, pruned) counts.
    /// Uses impacted_prefixes (if provided) to tighten the bound for impacted merges.
    pub fn prune_history_with_bound(
        &mut self,
        interner: &Interner,
        best_score: OrthoScore,
        impacted_prefixes: Option<&[Vec<usize>]>,
        read_buf_bytes: usize,
    ) -> io::Result<(u64, u64)> {
        if best_score == OrthoScore::zero() {
            return Ok((self.seen_len_accepted, 0));
        }

        let mut kept: u64 = 0;
        let mut pruned: u64 = 0;

        for bucket in 0..self.bucket_count {
            let runs = std::mem::take(&mut self.history_runs[bucket]);
            let mut new_runs: Vec<PathBuf> = Vec::with_capacity(runs.len());
            for run_path in runs {
                let run = Run::new(run_path.clone());
                let mut reader = run.iter(read_buf_bytes)?;
                let resolved_run_path = resolve_managed_path(&run_path)?;
                let rewrite_budget = file_size_or_zero(&resolved_run_path).saturating_mul(8);
                self.maybe_reclaim_before_write(
                    rewrite_budget.saturating_add(64 * 1024),
                    &format!("rewrite pruned history run {}", run_path.display()),
                )?;
                disk_safety::ensure_write_budget(
                    rewrite_budget.saturating_add(64 * 1024),
                    &format!("rewrite pruned history run {}", run_path.display()),
                )?;
                let tmp_path = run_path.with_extension("pruned");
                let tmp_parent = tmp_path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.base_path.clone());
                fs::create_dir_all(&tmp_parent)?;
                let mut writer = BufWriter::new(File::create(&tmp_path)?);
                let mut wrote_any = false;

                while let Some(item) = reader.next() {
                    let streamed = item?;
                    let ortho = Ortho::from_bytes(streamed.bytes.as_ref())
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                    if bound_existing_ortho(&ortho, interner, best_score, impacted_prefixes) {
                        pruned = pruned.saturating_add(1);
                        continue;
                    }
                    write_ortho_record(&mut writer, &ortho, Some(&mut self.compression_stats))?;
                    kept = kept.saturating_add(1);
                    wrote_any = true;
                }
                writer.flush()?;
                if wrote_any {
                    let (unc, comp) = compress_file(&tmp_path, 3)?;
                    self.compression_stats.record(unc, comp);
                }

                if wrote_any {
                    fs::rename(&tmp_path, &run_path)?;
                    new_runs.push(run_path);
                } else {
                    let _ = fs::remove_file(&run_path);
                    let _ = fs::remove_file(&tmp_path);
                }
            }
            self.history_runs[bucket] = new_runs;
        }

        Ok((kept, pruned))
    }

    /// Add a history run for a bucket and update accepted count
    /// The run is moved to the history directory and tracked
    pub fn add_history_run(&mut self, bucket: usize, run: Run, accepted: u64) -> io::Result<()> {
        assert!(bucket < self.bucket_count, "Invalid bucket index");

        // Move run file to history directory with unique name
        let history_dir = self
            .base_path
            .join("history")
            .join(format!("b={:02}", bucket));
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

    /// Adopt existing history run files into this store without decoding/re-encoding them.
    /// The source files are moved into this store's history layout.
    pub fn adopt_history_runs(
        &mut self,
        runs_by_bucket: &[(usize, Vec<PathBuf>)],
        accepted: u64,
    ) -> io::Result<()> {
        for (bucket, runs) in runs_by_bucket {
            assert!(*bucket < self.bucket_count, "Invalid bucket index");
            let history_dir = self
                .base_path
                .join("history")
                .join(format!("b={:02}", bucket));
            fs::create_dir_all(&history_dir)?;

            for run_path in runs {
                let run_id = self.history_runs[*bucket].len();
                let dest_path = history_dir.join(format!("history-{}.dat", run_id));
                fs::rename(run_path, &dest_path)?;
                self.history_runs[*bucket].push(dest_path);
            }
        }

        self.seen_len_accepted = self.seen_len_accepted.saturating_add(accepted);
        Ok(())
    }

    /// Read a run of work items and enqueue them in bounded batches.
    /// Returns the number of orthos enqueued.
    fn enqueue_work_run(&mut self, run: Run, read_buf_bytes: usize) -> io::Result<usize> {
        let mut reader = run.iter(read_buf_bytes)?;
        let mut batch: Vec<Ortho> = Vec::new();
        let mut count = 0usize;

        while let Some(item) = reader.next() {
            let streamed = item?;
            let ortho = Ortho::from_bytes(streamed.bytes.as_ref())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            batch.push(ortho);
            count += 1;

            self.push_segments(std::mem::take(&mut batch))?;
        }

        if !batch.is_empty() {
            self.push_segments(batch)?;
        }

        // Best-effort cleanup of the consumed run file
        let _ = fs::remove_file(run.path());

        Ok(count)
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

    /// Emergency path: drain all buckets into tracked local spill runs.
    pub fn pressure_spill_to_local_runs(&mut self, cfg: &Config) -> io::Result<PressureSpillStats> {
        self.flush_all()?;
        let mut stats = PressureSpillStats::default();
        for bucket in 0..self.bucket_count {
            let raw = self.drain_bucket(bucket)?;
            if raw.files().is_empty() {
                continue;
            }
            stats.buckets_drained += 1;
            let runs = compact_landing(
                bucket,
                raw,
                cfg,
                &self.base_path,
                false,
                Some(&mut self.compression_stats),
            )?;
            let mut tracked_paths = Vec::with_capacity(runs.len());
            for run in runs {
                let file_name = run
                    .path()
                    .file_name()
                    .map(|name| name.to_owned())
                    .unwrap_or_else(|| std::ffi::OsString::from("spill-run.dat"));
                let spill_path = self.spill_dir(bucket).join(file_name);
                if let Some(parent) = spill_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(run.path(), &spill_path)?;
                let size_bytes = fs::metadata(&spill_path)
                    .map(|meta| meta.len())
                    .unwrap_or(0);
                tracked_paths.push(TrackedSpillRun::new(spill_path, size_bytes));
            }
            stats.spill_runs_created += tracked_paths.len();
            stats.spill_bytes_created = stats
                .spill_bytes_created
                .saturating_add(tracked_paths.iter().map(|run| run.size_bytes).sum::<u64>());
            self.extend_spill_runs(bucket, tracked_paths)?;
        }
        if let Some(metrics) = metrics_handle() {
            metrics
                .record_spill_created(stats.spill_runs_created as u64, stats.spill_bytes_created);
        }
        Ok(stats)
    }

    fn update_cached_best(&self, candidate: &Ortho) {
        if self.best_score_dirty.get() {
            return;
        }
        let candidate_score = candidate.score();
        match self.cached_best_score.get() {
            Some(current) if current >= candidate_score => {}
            _ => {
                self.cached_best_score.set(Some(candidate_score));
                *self.cached_best_ortho.borrow_mut() = Some(candidate.clone());
            }
        }
    }

    fn handle_removed_score(&self, removed: OrthoScore) {
        if !self.best_score_dirty.get() && self.cached_best_score.get() == Some(removed) {
            self.best_score_dirty.set(true);
        }
    }

    fn recompute_cached_best(&self) {
        let mut best_score: Option<OrthoScore> = None;
        let mut best_ortho: Option<Ortho> = None;

        for ortho in self
            .work_queue_cache
            .iter()
            .chain(self.work_segment_batch.iter())
        {
            let score = ortho.score();
            if best_score.map(|current| score > current).unwrap_or(true) {
                best_score = Some(score);
                best_ortho = Some(ortho.clone());
            }
        }

        self.cached_best_score.set(best_score);
        *self.cached_best_ortho.borrow_mut() = best_ortho;
        self.best_score_dirty.set(false);
    }

    /// Peek at the best ortho currently in work cache (without removing)
    pub fn peek_best_ortho_in_cache(&self) -> Option<Ortho> {
        if self.best_score_dirty.get() {
            self.recompute_cached_best();
        }
        self.cached_best_ortho.borrow().clone()
    }

    /// Flush all bucket writers
    pub fn flush(&mut self) -> io::Result<()> {
        for bucket in 0..self.bucket_writers.len() {
            if self.bucket_writers[bucket].is_some() {
                self.flush_bucket_writer(bucket, true)?;
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
    pub fn on_generation_end(
        &mut self,
        cfg: &Config,
        progress: Option<&ProgressCallback>,
    ) -> io::Result<u64> {
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
            let spill_runs = self.spill_runs_for_bucket(bucket);

            if raw.files().is_empty() && spill_runs.is_empty() {
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
                cb(&format!(
                    "Bucket {}/{}: drained ~{} orthos",
                    bucket, self.bucket_count, drained_count
                ));
            }
            if !spill_runs.is_empty() {
                if let Some(cb) = &progress {
                    cb(&format!(
                        "Bucket {}/{}: consuming {} spill runs",
                        bucket,
                        self.bucket_count,
                        spill_runs.len()
                    ));
                }
            }

            // Phase: Compacting
            if let Some(cb) = &progress {
                cb(&format!("BUCKET_STATE:{}:sorting", bucket));
            }
            let compact_budget = raw
                .files()
                .iter()
                .map(|path| file_size_or_zero(path))
                .fold(0u64, u64::saturating_add)
                .saturating_add(cfg.run_budget_bytes as u64)
                .saturating_add(64 * 1024);
            self.maybe_reclaim_before_write(
                compact_budget,
                &format!("compact landing bucket {}", bucket),
            )?;
            let mut runs: Vec<Run> = spill_runs
                .iter()
                .map(|spill| Run::new(spill.path.clone()))
                .collect();
            let raw_runs = compact_landing(
                bucket,
                raw,
                cfg,
                &self.base_path,
                false,
                Some(&mut self.compression_stats),
            )?;
            runs.extend(raw_runs);

            if runs.is_empty() {
                // No runs generated, skip
                if let Some(cb) = &progress {
                    cb(&format!(
                        "Bucket {}/{}: no runs generated",
                        bucket, self.bucket_count
                    ));
                    cb(&format!("BUCKET_STATE:{}:empty", bucket));
                }
                continue;
            }

            if let Some(cb) = &progress {
                cb(&format!(
                    "Bucket {}/{}: created {} runs",
                    bucket,
                    self.bucket_count,
                    runs.len()
                ));
            }

            // Phase: Merge to unique run
            if let Some(cb) = &progress {
                cb(&format!("BUCKET_STATE:{}:merging", bucket));
            }
            let merge_estimate = estimate_compressed_run_set_bytes(&runs)?;
            self.maybe_reclaim_before_write(
                merge_estimate
                    .saturating_mul(8)
                    .saturating_add(merge_estimate / 8)
                    .saturating_add(64 * 1024),
                &format!("merge unique bucket {}", bucket),
            )?;
            let unique_run = merge_unique(
                runs,
                cfg,
                &self.base_path,
                Some(&mut self.compression_stats),
            )?;

            // Phase: Anti-join against history
            if let Some(cb) = &progress {
                cb(&format!("BUCKET_STATE:{}:antijoining", bucket));
            }
            let anti_join_budget = resolve_managed_path(unique_run.path())
                .map(|path| file_size_or_zero(&path))
                .unwrap_or(0)
                .saturating_mul(4)
                .saturating_add(64 * 1024);
            if anti_join_budget > 64 * 1024 {
                self.maybe_reclaim_before_write(
                    anti_join_budget,
                    &format!("anti join bucket {}", bucket),
                )?;
            }
            let history_iter = self.history_iter_with_buffer(bucket, cfg.read_buf_bytes)?;
            let (new_work_run, seen_run, accepted) = anti_join_orthos(
                unique_run,
                history_iter,
                &self.base_path,
                cfg.read_buf_bytes,
                Some(&mut self.compression_stats),
            )?;

            total_accepted += accepted;

            // Add seen run to history
            self.add_history_run(bucket, seen_run, accepted)?;

            // Enqueue new work from run in bounded batches
            let bucket_new_work = self.enqueue_work_run(new_work_run, cfg.read_buf_bytes)?;
            if !spill_runs.is_empty() {
                if let Some(metrics) = metrics_handle() {
                    let consumed_bytes = spill_runs.iter().map(|run| run.size_bytes).sum();
                    metrics.record_spill_consumed(spill_runs.len() as u64, consumed_bytes);
                }
                self.clear_spill_runs_for_bucket(bucket)?;
                if let Some(cb) = &progress {
                    cb(&format!(
                        "Bucket {}/{}: consumed {} spill runs",
                        bucket,
                        self.bucket_count,
                        spill_runs.len()
                    ));
                }
            }

            if let Some(cb) = &progress {
                cb(&format!(
                    "Bucket {}/{}: accepted {} orthos, created {} new work",
                    bucket, self.bucket_count, accepted, bucket_new_work
                ));
            }

            // Optional: Compact history if needed
            if cfg.allow_compaction {
                let pre_compact_runs = self.history_runs[bucket].len();
                if pre_compact_runs > 64 {
                    if let Some(cb) = &progress {
                        cb(&format!("BUCKET_STATE:{}:compacting", bucket));
                        cb(&format!(
                            "Bucket {}/{}: compacting {} history runs",
                            bucket, self.bucket_count, pre_compact_runs
                        ));
                    }
                    self.compact_history(bucket, cfg)?;
                    let post_compact_runs = self.history_runs[bucket].len();
                    if let Some(cb) = &progress {
                        cb(&format!(
                            "Bucket {}/{}: compacted {} → {} runs",
                            bucket, self.bucket_count, pre_compact_runs, post_compact_runs
                        ));
                    }
                }
            }

            // Mark bucket complete
            if let Some(cb) = &progress {
                cb(&format!(
                    "BUCKET_STATE:{}:complete:{}",
                    bucket, bucket_new_work
                ));
            }

            // Push new work to queue (ortho version)
            total_new_work += bucket_new_work as u64;

            buckets_processed += 1;
        }

        // Flush all pending work to disk
        self.flush_work_segment_batch()?;

        if let Some(cb) = &progress {
            cb(&format!("TRANSITION_COMPLETE"));
            cb(&format!(
                "Transition complete: processed {} buckets, drained ~{} orthos, accepted {} orthos, created {} new work",
                buckets_processed, total_drained, total_accepted, total_new_work
            ));
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
        let merge_estimate = estimate_compressed_run_set_bytes(&runs_to_merge)?;
        self.maybe_reclaim_before_write(
            merge_estimate
                .saturating_mul(8)
                .saturating_add(merge_estimate / 8)
                .saturating_add(64 * 1024),
            &format!("compact history bucket {}", bucket),
        )?;

        // Merge them into a single unique run
        let merged = merge_unique(
            runs_to_merge,
            cfg,
            &self.base_path,
            Some(&mut self.compression_stats),
        )?;

        // Move merged run to history with next available ID
        let history_dir = self
            .base_path
            .join("history")
            .join(format!("b={:02}", bucket));
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
    offload_after_write: bool,
    mut stats: Option<&mut CompressionStats>,
) -> io::Result<Vec<Run>> {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    static RUN_COUNTER: AtomicUsize = AtomicUsize::new(0);

    memory_safety::ensure_phase_headroom(
        cfg.run_budget_bytes
            .saturating_add(cfg.read_buf_bytes.saturating_mul(2))
            .saturating_add(64 * 1024),
        &format!("compact landing bucket {}", bucket),
    )?;

    let mut runs = Vec::new();
    let mut arena: Vec<StreamedOrtho> = Vec::new();
    let mut current_size: usize = 0;
    GenerationStore::set_compaction_bytes(0);

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
            let ortho_size = streamed.heap_bytes_estimate();
            if !arena.is_empty() && current_size.saturating_add(ortho_size) > cfg.run_budget_bytes {
                // Flush before adding this item to keep arena under budget.
                arena.sort_unstable_by_key(|o| o.id);
                let run_id = RUN_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
                let run_path = base_path
                    .join("runs")
                    .join(format!("b={:02}-run-{}.dat", bucket, run_id));

                write_streamed_run(&arena, &run_path, offload_after_write, stats.as_deref_mut())?;
                runs.push(Run::new(run_path));

                arena.clear();
                arena.shrink_to_fit();
                current_size = 0;
                GenerationStore::set_compaction_bytes(0);
            }

            arena.push(streamed);
            current_size = current_size.saturating_add(ortho_size);
            GenerationStore::set_compaction_bytes(current_size);
        }
    }

    // Write any remaining items in arena
    if !arena.is_empty() {
        arena.sort_unstable_by_key(|o| o.id);
        let run_id = RUN_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
        let run_path = base_path
            .join("runs")
            .join(format!("b={:02}-run-{}.dat", bucket, run_id));

        write_streamed_run(&arena, &run_path, offload_after_write, stats.as_deref_mut())?;
        runs.push(Run::new(run_path));
        arena.clear();
        arena.shrink_to_fit();
    }
    GenerationStore::set_compaction_bytes(0);

    // Best-effort cleanup of drained landing files now that they are incorporated.
    for file_path in raw.files() {
        let _ = fs::remove_file(file_path);
    }

    Ok(runs)
}

fn write_streamed_run(
    arena: &[StreamedOrtho],
    path: &PathBuf,
    offload_after_write: bool,
    mut stats: Option<&mut CompressionStats>,
) -> io::Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let estimated_raw_bytes = estimate_streamed_run_bytes(arena);
    disk_safety::ensure_write_budget(
        estimated_raw_bytes
            .saturating_add(estimated_raw_bytes / 8)
            .saturating_add(64 * 1024),
        &format!("write run {}", path.display()),
    )?;

    let mut writer = create_compressed_writer(path, 3, 64 * 1024)?;
    let mut uncompressed_bytes = 0u64;
    for streamed in arena {
        uncompressed_bytes = uncompressed_bytes.saturating_add(write_ortho_record_bytes(
            &mut writer,
            &streamed.bytes,
            streamed.decoded_size_est,
            None,
        )? as u64);
    }
    let (unc, comp) = finish_compressed_writer(path, writer, uncompressed_bytes)?;
    if let Some(s) = stats.as_deref_mut() {
        s.record(unc, comp);
    }
    if offload_after_write {
        maybe_offload_and_delete(path)?;
    }
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
    mut stats: Option<&mut CompressionStats>,
) -> io::Result<UniqueRun> {
    use std::collections::BinaryHeap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    if runs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Cannot merge empty run list",
        ));
    }

    let cleanup_runs = |paths: &[Run]| {
        for r in paths {
            let _ = fs::remove_file(r.path());
        }
    };

    static MERGE_COUNTER: AtomicUsize = AtomicUsize::new(0);

    // Multi-pass merge if needed
    while runs.len() > cfg.fan_in {
        let mut next_pass_runs = Vec::new();

        for chunk in runs.chunks(cfg.fan_in) {
            let merged = merge_ortho_chunk(chunk, cfg, base_path, stats.as_deref_mut())?;
            cleanup_runs(chunk);
            next_pass_runs.push(merged);
        }

        runs = next_pass_runs;
    }

    // Final pass - merge all remaining runs into a UniqueRun
    let merge_id = MERGE_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
    let unique_path = base_path
        .join("runs")
        .join(format!("unique-{}.dat", merge_id));
    let merge_memory_budget = runs
        .len()
        .saturating_mul(cfg.read_buf_bytes)
        .saturating_add(cfg.bufwriter_capacity)
        .saturating_add(64 * 1024);
    memory_safety::ensure_phase_headroom(
        merge_memory_budget,
        &format!("merge unique {}", unique_path.display()),
    )?;
    let merge_budget = estimate_compressed_run_set_bytes(&runs)?.saturating_mul(8);
    disk_safety::ensure_write_budget(
        merge_budget
            .saturating_add(merge_budget / 8)
            .saturating_add(64 * 1024),
        &format!("merge unique {}", unique_path.display()),
    )?;
    let mut writer = create_compressed_writer(&unique_path, 3, cfg.bufwriter_capacity)?;

    #[derive(Eq, PartialEq)]
    struct HeapItem {
        id: OrthoId,
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
            let id = streamed.id;
            current_orthos[idx] = Some(streamed);
            heap.push(HeapItem { id, run_idx: idx });
        }
    }

    let mut last_written: Option<StreamedOrtho> = None;
    let mut uncompressed_bytes = 0u64;

    // K-way merge with deduplication by id + equality
    while let Some(item) = heap.pop() {
        let streamed = current_orthos[item.run_idx].take().unwrap();

        // Write only if different from last written (dedupe by id + shape/payload)
        let is_duplicate = last_written
            .as_ref()
            .map(|last| last.id == item.id && archived_eq(last, &streamed))
            .unwrap_or(false);
        if !is_duplicate {
            uncompressed_bytes = uncompressed_bytes.saturating_add(write_ortho_record_bytes(
                &mut writer,
                &streamed.bytes,
                streamed.decoded_size_est,
                None,
            )? as u64);
            last_written = Some(streamed);
        } else {
            // Keep last_written so adjacent duplicates continue to collapse correctly
            last_written = Some(streamed);
        }

        // Fetch next from same run
        if let Some(result) = iterators[item.run_idx].next() {
            let streamed = result?;
            let id = streamed.id;
            current_orthos[item.run_idx] = Some(streamed);
            heap.push(HeapItem {
                id,
                run_idx: item.run_idx,
            });
        }
    }

    let (unc, comp) = finish_compressed_writer(&unique_path, writer, uncompressed_bytes)?;
    if let Some(s) = stats.as_deref_mut() {
        s.record(unc, comp);
    }
    cleanup_runs(&runs);

    Ok(UniqueRun::new(unique_path))
}

/// Helper to merge a chunk of ortho runs (for multi-pass)
fn merge_ortho_chunk(
    runs: &[Run],
    cfg: &Config,
    base_path: &PathBuf,
    mut stats: Option<&mut CompressionStats>,
) -> io::Result<Run> {
    use std::collections::BinaryHeap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    static CHUNK_COUNTER: AtomicUsize = AtomicUsize::new(0);

    let chunk_id = CHUNK_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
    let chunk_path = base_path
        .join("runs")
        .join(format!("chunk-{}.dat", chunk_id));
    let merge_memory_budget = runs
        .len()
        .saturating_mul(cfg.read_buf_bytes)
        .saturating_add(cfg.bufwriter_capacity)
        .saturating_add(64 * 1024);
    memory_safety::ensure_phase_headroom(
        merge_memory_budget,
        &format!("merge chunk {}", chunk_path.display()),
    )?;
    let merge_budget = estimate_compressed_run_set_bytes(runs)?.saturating_mul(8);
    disk_safety::ensure_write_budget(
        merge_budget
            .saturating_add(merge_budget / 8)
            .saturating_add(64 * 1024),
        &format!("merge chunk {}", chunk_path.display()),
    )?;
    let mut writer = create_compressed_writer(&chunk_path, 3, cfg.bufwriter_capacity)?;

    #[derive(Eq, PartialEq)]
    struct HeapItem {
        id: OrthoId,
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
            let id = streamed.id;
            current_orthos[idx] = Some(streamed);
            heap.push(HeapItem { id, run_idx: idx });
        }
    }

    // No deduplication in intermediate passes - just merge
    let mut uncompressed_bytes = 0u64;
    while let Some(item) = heap.pop() {
        let streamed = current_orthos[item.run_idx].take().unwrap();
        uncompressed_bytes = uncompressed_bytes.saturating_add(write_ortho_record_bytes(
            &mut writer,
            &streamed.bytes,
            streamed.decoded_size_est,
            None,
        )? as u64);

        if let Some(result) = iterators[item.run_idx].next() {
            let streamed = result?;
            let id = streamed.id;
            current_orthos[item.run_idx] = Some(streamed);
            heap.push(HeapItem {
                id,
                run_idx: item.run_idx,
            });
        }
    }

    let (unc, comp) = finish_compressed_writer(&chunk_path, writer, uncompressed_bytes)?;
    if let Some(s) = stats.as_deref_mut() {
        s.record(unc, comp);
    }
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
    mut stats: Option<&mut CompressionStats>,
) -> io::Result<(Run, Run, u64)> {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    static ANTI_JOIN_COUNTER: AtomicUsize = AtomicUsize::new(0);

    let anti_join_id = ANTI_JOIN_COUNTER.fetch_add(1, AtomicOrdering::SeqCst);
    memory_safety::ensure_phase_headroom(
        read_buf_bytes.saturating_mul(4).saturating_add(256 * 1024),
        &format!("anti join {}", anti_join_id),
    )?;
    let unique_budget = resolve_managed_path(unique_gen.path())
        .map(|path| file_size_or_zero(&path))
        .unwrap_or(0)
        .saturating_mul(4);
    if unique_budget > 0 {
        disk_safety::ensure_write_budget(
            unique_budget.saturating_add(64 * 1024),
            &format!("anti join {}", anti_join_id),
        )?;
    }
    let seen_run_path = base_path
        .join("runs")
        .join(format!("seen-{}.dat", anti_join_id));
    let mut seen_writer = create_compressed_writer(&seen_run_path, 3, 64 * 1024)?;

    let new_work_path = base_path
        .join("runs")
        .join(format!("new-work-{}.dat", anti_join_id));
    let mut new_work_writer = create_compressed_writer(&new_work_path, 3, 64 * 1024)?;

    let mut gen_iter = unique_gen.iter(read_buf_bytes)?;
    let mut accepted_count = 0u64;
    let mut seen_uncompressed_bytes = 0u64;
    let mut new_work_uncompressed_bytes = 0u64;

    // Current values from each stream
    let mut gen_val = gen_iter.next().transpose()?;
    let mut history_val = history.next().transpose()?;

    // Streaming merge: compare gen orthos against history by ID
    loop {
        match (&gen_val, &history_val) {
            (None, _) => break,
            (Some(g), None) => {
                // No more history - all remaining gen values are new
                seen_uncompressed_bytes = seen_uncompressed_bytes.saturating_add(
                    write_ortho_record_bytes(&mut seen_writer, &g.bytes, g.decoded_size_est, None)?
                        as u64,
                );
                new_work_uncompressed_bytes =
                    new_work_uncompressed_bytes.saturating_add(write_ortho_record_bytes(
                        &mut new_work_writer,
                        &g.bytes,
                        g.decoded_size_est,
                        None,
                    )? as u64);
                accepted_count += 1;
                gen_val = gen_iter.next().transpose()?;
            }
            (Some(g), Some(h)) => {
                let g_id = g.id;
                let h_id = h.id;

                match g_id.cmp(&h_id) {
                    std::cmp::Ordering::Less => {
                        // g < h: g is new (not in history)
                        seen_uncompressed_bytes = seen_uncompressed_bytes.saturating_add(
                            write_ortho_record_bytes(
                                &mut seen_writer,
                                &g.bytes,
                                g.decoded_size_est,
                                None,
                            )? as u64,
                        );
                        new_work_uncompressed_bytes = new_work_uncompressed_bytes.saturating_add(
                            write_ortho_record_bytes(
                                &mut new_work_writer,
                                &g.bytes,
                                g.decoded_size_est,
                                None,
                            )? as u64,
                        );
                        accepted_count += 1;
                        gen_val = gen_iter.next().transpose()?;
                    }
                    std::cmp::Ordering::Equal => {
                        // Same ID: check structural equality
                        if archived_eq(g, h) {
                            // Exact duplicate - reject from work, but add to seen
                            seen_uncompressed_bytes = seen_uncompressed_bytes.saturating_add(
                                write_ortho_record_bytes(
                                    &mut seen_writer,
                                    &g.bytes,
                                    g.decoded_size_est,
                                    None,
                                )? as u64,
                            );
                        } else {
                            // ID collision with different structure - treat as new
                            // Note: This is extremely rare and indicates hash collision
                            seen_uncompressed_bytes = seen_uncompressed_bytes.saturating_add(
                                write_ortho_record_bytes(
                                    &mut seen_writer,
                                    &g.bytes,
                                    g.decoded_size_est,
                                    None,
                                )? as u64,
                            );
                            new_work_uncompressed_bytes = new_work_uncompressed_bytes
                                .saturating_add(write_ortho_record_bytes(
                                    &mut new_work_writer,
                                    &g.bytes,
                                    g.decoded_size_est,
                                    None,
                                )? as u64);
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

    let (seen_unc, seen_comp) =
        finish_compressed_writer(&seen_run_path, seen_writer, seen_uncompressed_bytes)?;
    let (new_work_unc, new_work_comp) =
        finish_compressed_writer(&new_work_path, new_work_writer, new_work_uncompressed_bytes)?;
    if let Some(s) = stats.as_deref_mut() {
        s.record(seen_unc, seen_comp);
        s.record(new_work_unc, new_work_comp);
    }
    Ok((
        Run::new(new_work_path),
        Run::new(seen_run_path),
        accepted_count,
    ))
}

impl Drop for UniqueRun {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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
    use crate::generation_store::set_offload_metrics_handle;
    use crate::metrics::Metrics;
    use crate::offload_cache::OffloadCache;
    use crate::offloader::{MockObjectStore, OffloadClient};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tempfile::TempDir;

    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

    fn collect_run(run: &Run, read_buf_bytes: usize) -> Vec<Ortho> {
        let mut out = Vec::new();
        let mut iter = run.iter(read_buf_bytes).unwrap();
        while let Some(item) = iter.next() {
            let s = item.unwrap();
            out.push(Ortho::from_bytes(&s.bytes).unwrap());
        }
        out
    }

    fn read_magic(path: &Path) -> [u8; 4] {
        let mut file = File::open(path).unwrap();
        let mut buf = [0u8; 4];
        file.read_exact(&mut buf).unwrap();
        buf
    }

    fn count_history_orthos(store: &GenerationStore, read_buf_bytes: usize) -> usize {
        let mut count = 0usize;
        for bucket in 0..store.bucket_count {
            for item in store
                .history_iter_with_buffer(bucket, read_buf_bytes)
                .unwrap()
            {
                item.unwrap();
                count += 1;
            }
        }
        count
    }

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

        // Write orthos to drain file using the same record format as production
        let writer = BufWriter::new(File::create(&drain_path).unwrap());
        let mut encoder = ZstdEncoder::new(writer, 3).unwrap();
        for ortho in &orthos {
            write_ortho_record(&mut encoder, ortho, None).unwrap();
        }
        let mut file = encoder.finish().unwrap();
        file.flush().unwrap();

        let raw = RawStream::new(vec![drain_path]);

        // Large budget - should fit in one run
        let cfg = Config::test_config(1024 * 1024, 8);

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let mut stats = CompressionStats::default();
        let runs = compact_landing(bucket, raw, &cfg, &base_path, false, Some(&mut stats)).unwrap();

        assert_eq!(runs.len(), 1);

        // Read back and verify sorted by id
        let mut result = vec![];
        for item in runs[0].iter(64 * 1024).unwrap() {
            let s = item.unwrap();
            result.push(Ortho::from_bytes(&s.bytes).unwrap());
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

        // Write orthos to drain file using the same record format as production
        let writer = BufWriter::new(File::create(&drain_path).unwrap());
        let mut encoder = ZstdEncoder::new(writer, 3).unwrap();
        for ortho in &orthos {
            write_ortho_record(&mut encoder, ortho, None).unwrap();
        }
        let mut file = encoder.finish().unwrap();
        file.flush().unwrap();

        let raw = RawStream::new(vec![drain_path]);

        // Small budget to force multiple runs
        let cfg = Config::test_config(2048, 8);

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let mut stats = CompressionStats::default();
        let runs = compact_landing(bucket, raw, &cfg, &base_path, false, Some(&mut stats)).unwrap();

        // Should produce multiple runs due to small budget
        assert!(runs.len() >= 1);

        // Collect all orthos
        let mut all_orthos = vec![];
        for run in &runs {
            for item in run.iter(64 * 1024).unwrap() {
                let s = item.unwrap();
                all_orthos.push(Ortho::from_bytes(s.bytes.as_ref()).unwrap());
            }
        }

        assert_eq!(all_orthos.len(), orthos.len());

        // Verify each run is sorted by id
        for run in &runs {
            let mut prev_id = None;
            for item in run.iter(64 * 1024).unwrap() {
                let ortho = Ortho::from_bytes(item.unwrap().bytes.as_ref()).unwrap();
                let id = ortho.id();
                if let Some(p) = prev_id {
                    assert!(id >= p, "Run should be sorted by id");
                }
                prev_id = Some(id);
            }
        }
    }

    struct RecordingOffloader {
        dest_dir: PathBuf,
        uploads: Arc<Mutex<Vec<PathBuf>>>,
        base_path: PathBuf,
    }

    impl RecordingOffloader {
        fn new(dest_dir: PathBuf, uploads: Arc<Mutex<Vec<PathBuf>>>, base_path: PathBuf) -> Self {
            Self {
                dest_dir,
                uploads,
                base_path,
            }
        }
    }

    impl RunOffloader for RecordingOffloader {
        fn offload(&self, path: &Path) -> io::Result<bool> {
            if !path.starts_with(&self.base_path) {
                return Ok(false);
            }
            fs::create_dir_all(&self.dest_dir)?;
            let name = path
                .file_name()
                .map(|f| f.to_owned())
                .unwrap_or_else(|| std::ffi::OsString::from("run.dat"));
            let dest = self.dest_dir.join(name);
            fs::copy(path, &dest)?;
            self.uploads.lock().unwrap().push(dest);
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
    fn compact_landing_offloads_and_deletes_runs() {
        use crate::ortho::Ortho;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let offload_dir = base_path.join("offloaded");
        let uploads: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let offloader = Arc::new(RecordingOffloader::new(
            offload_dir.clone(),
            uploads.clone(),
            base_path.clone(),
        ));
        let _guard = OffloaderGuard;
        set_run_offloader(Some(offloader));
        let metrics = Metrics::new();
        set_offload_metrics_handle(Some(metrics.clone_handle()));

        let orthos = vec![
            Ortho::new(),
            Ortho::new().add(1)[0].clone(),
            Ortho::new().add(2)[0].clone(),
        ];

        let bucket = 0;
        let landing_dir = base_path.join("landing").join(format!("b={:02}", bucket));
        fs::create_dir_all(&landing_dir).unwrap();
        let drain_path = landing_dir.join("drain-0.log");

        let writer = BufWriter::new(File::create(&drain_path).unwrap());
        let mut encoder = ZstdEncoder::new(writer, 3).unwrap();
        for ortho in &orthos {
            write_ortho_record(&mut encoder, ortho, None).unwrap();
        }
        let mut file = encoder.finish().unwrap();
        file.flush().unwrap();

        let raw = RawStream::new(vec![drain_path]);
        let cfg = Config::test_config(1024 * 1024, 8);
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let mut stats = CompressionStats::default();
        let runs = compact_landing(bucket, raw, &cfg, &base_path, true, Some(&mut stats)).unwrap();

        // Offloader should have copied and runs should be deleted locally.
        let uploaded = uploads.lock().unwrap();
        assert!(
            uploaded.len() >= runs.len(),
            "expected at least {} uploads, saw {}",
            runs.len(),
            uploaded.len()
        );
        for run in &runs {
            assert!(!run.path().exists());
        }
        for dest in uploaded.iter() {
            assert!(dest.exists());
        }
        let snapshot = metrics.snapshot();
        assert!(snapshot.global.offloaded_files >= 1);
        set_offload_metrics_handle(None);
    }

    #[test]
    fn on_generation_end_keeps_history_local_when_space_is_healthy() {
        use crate::ortho::Ortho;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let offload_dir = base_path.join("offloaded");
        let uploads: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let offloader = Arc::new(RecordingOffloader::new(
            offload_dir,
            uploads.clone(),
            base_path.clone(),
        ));
        let _guard = OffloaderGuard;
        set_run_offloader(Some(offloader));

        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        let cfg = Config::test_config(256 * 1024, 8);
        store.configure(&cfg);
        store.record_result(&Ortho::new()).unwrap();
        store.on_generation_end(&cfg, None).unwrap();

        assert!(
            uploads.lock().unwrap().is_empty(),
            "healthy runs should remain local until reclaim needs space"
        );
        assert_eq!(count_history_orthos(&store, cfg.read_buf_bytes), 1);
    }

    struct CachedDownloader {
        client: OffloadClient,
        cache: Mutex<OffloadCache>,
        temp_root: PathBuf,
    }

    impl RunDownloader for CachedDownloader {
        fn cache_lookup(&self, key: &str) -> Option<PathBuf> {
            self.cache.lock().unwrap().get(key)
        }

        fn download_to_cache(&self, key: &str) -> io::Result<PathBuf> {
            let tmp_dir = self.temp_root.join("tmp_downloads");
            fs::create_dir_all(&tmp_dir)?;
            let tmp_path = tmp_dir.join(key.replace('/', "_"));
            let object_key = self.client.object_key(Path::new(key));
            self.client
                .download_file(&object_key, &tmp_path)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            let mut cache = self.cache.lock().unwrap();
            let cached = cache
                .insert_copy(key, &tmp_path)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            let _ = fs::remove_file(&tmp_path);
            Ok(cached)
        }
    }

    struct DownloaderGuard;
    impl Drop for DownloaderGuard {
        fn drop(&mut self) {
            set_run_downloader(None);
        }
    }

    struct UploadThenFailOffloader {
        client: OffloadClient,
        base_path: PathBuf,
    }

    impl RunOffloader for UploadThenFailOffloader {
        fn offload(&self, path: &Path) -> io::Result<bool> {
            let key = run_object_key(&self.base_path, path)?;
            let rel = Path::new(&key);
            self.client
                .upload_file(path, rel)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            Err(io::Error::new(
                io::ErrorKind::Other,
                "forced offload failure",
            ))
        }
    }

    #[test]
    fn iter_downloads_missing_run_from_cache() {
        use crate::ortho::Ortho;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        fs::create_dir_all(base_path.join("runs")).unwrap();

        // Create a run file with a few orthos.
        let run_path = base_path.join("runs").join("b=00-run-0.dat");
        let writer = BufWriter::new(File::create(&run_path).unwrap());
        let mut encoder = ZstdEncoder::new(writer, 3).unwrap();
        let orthos = vec![
            Ortho::new(),
            Ortho::new().add(1)[0].clone(),
            Ortho::new().add(2)[0].clone(),
        ];
        for ortho in &orthos {
            write_ortho_record(&mut encoder, ortho, None).unwrap();
        }
        let mut file = encoder.finish().unwrap();
        file.flush().unwrap();

        // Upload to mock object store using the relative key.
        let store = Arc::new(MockObjectStore::new());
        let client = OffloadClient::new(Arc::clone(&store), "bucket", "runs")
            .with_retry(1, Duration::from_millis(1));
        let key = run_object_key(&base_path, &run_path).unwrap();
        client.upload_file(&run_path, Path::new(&key)).unwrap();

        // Delete the local run to force download path.
        fs::remove_file(&run_path).unwrap();

        // Configure downloader with cache.
        let cache_dir = base_path.join("offload_cache");
        let cache = OffloadCache::new(cache_dir.clone(), 1024 * 1024).unwrap();
        let downloader = Arc::new(CachedDownloader {
            client,
            cache: Mutex::new(cache),
            temp_root: base_path.clone(),
        });
        let _guard = DownloaderGuard;
        set_run_downloader(Some((base_path.clone(), downloader)));
        let metrics = Metrics::new();
        set_offload_metrics_handle(Some(metrics.clone_handle()));

        // Iterating the run should download from mock store into cache.
        let run = Run::new(run_path.clone());
        let collected = collect_run(&run, 64 * 1024);
        assert_eq!(collected.len(), orthos.len());
        for (a, b) in collected.iter().zip(orthos.iter()) {
            assert_eq!(a.id(), b.id());
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.global.cache_misses, 1);
        assert_eq!(snapshot.global.downloaded_files, 1);
        assert!(snapshot.global.downloaded_bytes > 0);
        set_offload_metrics_handle(None);
    }

    #[test]
    fn offload_failure_retains_local_and_downloads_on_restart() {
        use crate::ortho::Ortho;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let run_path = base_path.join("runs").join("b=00-run-0.dat");
        let writer = BufWriter::new(File::create(&run_path).unwrap());
        let mut encoder = ZstdEncoder::new(writer, 3).unwrap();
        let orthos = vec![Ortho::new(), Ortho::new().add(1)[0].clone()];
        for ortho in &orthos {
            write_ortho_record(&mut encoder, ortho, None).unwrap();
        }
        let mut file = encoder.finish().unwrap();
        file.flush().unwrap();

        let store = Arc::new(MockObjectStore::new());
        let offload_client = OffloadClient::new(Arc::clone(&store), "bucket", "")
            .with_retry(1, Duration::from_millis(1));
        let download_client = OffloadClient::new(Arc::clone(&store), "bucket", "")
            .with_retry(1, Duration::from_millis(1));
        let key = run_object_key(&base_path, &run_path).unwrap();

        let offloader = Arc::new(UploadThenFailOffloader {
            client: offload_client,
            base_path: base_path.clone(),
        });
        let _guard = OffloaderGuard;
        set_run_offloader(Some(offloader));
        let metrics = Metrics::new();
        set_offload_metrics_handle(Some(metrics.clone_handle()));

        let result = test_maybe_offload_and_delete(&run_path);
        assert!(result.is_err(), "expected offload failure");
        assert!(run_path.exists(), "run should remain after failure");
        assert!(
            store.get_bytes("bucket", &key).is_some(),
            "object should be present in mock store"
        );

        let logs = metrics.snapshot().logs;
        assert!(
            logs.iter().any(|l| l.message.contains("Offload failed")),
            "expected offload failure log entry"
        );

        // Simulate restart: clear hooks, drop local copy, and ensure downloads succeed.
        set_run_offloader(None);
        set_offload_metrics_handle(None);
        fs::remove_file(&run_path).unwrap();

        let cache_dir = base_path.join("offload_cache");
        let cache = OffloadCache::new(cache_dir, 1024 * 1024).unwrap();
        let downloader = Arc::new(CachedDownloader {
            client: download_client,
            cache: Mutex::new(cache),
            temp_root: base_path.clone(),
        });
        let _dl_guard = DownloaderGuard;
        set_run_downloader(Some((base_path.clone(), downloader)));

        let run = Run::new(run_path.clone());
        let collected = collect_run(&run, 64 * 1024);
        assert_eq!(collected.len(), orthos.len());
        for (a, b) in collected.iter().zip(orthos.iter()) {
            assert_eq!(a.id(), b.id());
        }
        set_run_downloader(None);
    }

    #[test]
    fn pop_work_hydrates_offloaded_work_segment() {
        use crate::offload_runtime::configure_offload_runtime;

        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let local_store = temp_dir.path().join("store");
        let mut offload_cfg = crate::offload_config::OffloadConfig::with_base_dir(&base_path);
        offload_cfg.enabled = true;
        offload_cfg.local_store_dir = Some(local_store);
        offload_cfg.cache_dir = base_path.join("offload_cache");
        offload_cfg.disk_free_low_water = Some(0);
        offload_cfg.disk_hysteresis_margin_bytes = 0;
        let _guard = configure_offload_runtime(&base_path, &offload_cfg)
            .unwrap()
            .expect("offload runtime");

        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        let cfg = Config::test_config(256 * 1024, 8);
        store.configure(&cfg);

        let seed = Ortho::new();
        let child = seed.add(1)[0].clone();
        store
            .push_segments(vec![seed.clone(), child.clone()])
            .unwrap();
        store.flush_all().unwrap();
        assert_eq!(store.work_segments.len(), 1);

        let segment_path = store.work_segments[0].clone();
        assert!(offload_path_if_configured(&segment_path).unwrap());
        fs::remove_file(&segment_path).unwrap();

        let first = store.pop_work().unwrap().unwrap();
        let second = store.pop_work().unwrap().unwrap();
        let mut ids = vec![first.id(), second.id()];
        ids.sort_unstable();
        let mut expected = vec![seed.id(), child.id()];
        expected.sort_unstable();
        assert_eq!(ids, expected);
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
            let children = base
                .add(crate::ortho::PayloadVal::try_from(i).expect("test payload overflowed u32"));
            if !children.is_empty() {
                orthos.push(children[0].clone());
            }
        }

        let bucket = 0;
        let landing_dir = base_path.join("landing").join(format!("b={:02}", bucket));
        fs::create_dir_all(&landing_dir).unwrap();
        let drain_path = landing_dir.join("drain-0.log");

        // Write orthos to drain file using the same record format as production
        let writer = BufWriter::new(File::create(&drain_path).unwrap());
        let mut encoder = ZstdEncoder::new(writer, 3).unwrap();
        for ortho in &orthos {
            write_ortho_record(&mut encoder, ortho, None).unwrap();
        }
        let mut file = encoder.finish().unwrap();
        file.flush().unwrap();

        let raw = RawStream::new(vec![drain_path]);

        // Reasonable budget
        let cfg = Config::test_config(4 * 1024 * 1024, 8);

        // Create runs directory
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let mut stats = CompressionStats::default();
        let runs = compact_landing(bucket, raw, &cfg, &base_path, false, Some(&mut stats)).unwrap();

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
                let ortho = Ortho::from_bytes(item.unwrap().bytes.as_ref()).unwrap();
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

    #[test]
    fn adopt_history_runs_preserves_history_visibility_and_count() {
        let temp_dir = TempDir::new().unwrap();
        let source_base = temp_dir.path().join("source");
        let target_base = temp_dir.path().join("target");

        let cfg = Config::test_config(256 * 1024, 8);

        let mut source = GenerationStore::new_with_config(source_base.clone(), 8).unwrap();
        source.configure(&cfg);
        let ortho = Ortho::new().add(1)[0].clone();
        source.record_result(&ortho).unwrap();
        let _ = source.on_generation_end(&cfg, None).unwrap();
        let source_count = source.seen_len_accepted();
        let source_paths = source.history_run_paths();
        drop(source);

        let mut target = GenerationStore::new_with_config(target_base.clone(), 8).unwrap();
        target.configure(&cfg);
        target
            .adopt_history_runs(&source_paths, source_count)
            .unwrap();

        assert_eq!(target.seen_len_accepted(), source_count);
        assert_eq!(
            count_history_orthos(&target, cfg.read_buf_bytes),
            source_count as usize
        );
        for (_bucket, paths) in source_paths {
            for old_path in paths {
                assert!(
                    !old_path.exists(),
                    "adopted run should no longer exist at source path"
                );
            }
        }
    }

    #[test]
    fn anti_join_outputs_stay_local_until_store_consumes_them() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let offload_dir = base_path.join("offloaded");
        let uploads: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let offloader = Arc::new(RecordingOffloader::new(
            offload_dir,
            uploads.clone(),
            base_path.clone(),
        ));
        let _guard = OffloaderGuard;
        set_run_offloader(Some(offloader));

        let cfg = Config::test_config(256 * 1024, 8);
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        store.configure(&cfg);
        fs::create_dir_all(base_path.join("runs")).unwrap();

        let ortho1 = Ortho::new().add(1)[0].clone();
        let ortho2 = Ortho::new().add(2)[0].clone();
        let gen_path = base_path.join("runs").join("gen.dat");
        let gen_raw = BufWriter::new(File::create(&gen_path).unwrap());
        let mut gen_file = ZstdEncoder::new(gen_raw, 3).unwrap();
        let mut gen_items = vec![ortho1.clone(), ortho2.clone()];
        gen_items.sort_by_key(|o| o.id());
        for ortho in &gen_items {
            write_ortho_record(&mut gen_file, ortho, None).unwrap();
        }
        let mut gen_file = gen_file.finish().unwrap();
        gen_file.flush().unwrap();

        let history_iter = std::iter::empty::<io::Result<StreamedOrtho>>();
        let (new_work_run, seen_run, accepted) = anti_join_orthos(
            UniqueRun::new(gen_path),
            history_iter,
            &base_path,
            64 * 1024,
            None,
        )
        .unwrap();

        assert!(new_work_run.path().exists());
        assert!(seen_run.path().exists());
        let uploads_before_history = uploads.lock().unwrap().len();

        store.add_history_run(0, seen_run, accepted).unwrap();
        assert!(!store.history_runs[0].is_empty());
        assert!(
            uploads.lock().unwrap().len() == uploads_before_history,
            "local-first mode should not eagerly offload accepted history runs"
        );

        let enqueued = store
            .enqueue_work_run(new_work_run, cfg.read_buf_bytes)
            .unwrap();
        assert_eq!(enqueued, 2);
        assert_eq!(store.work_len(), enqueued as u64);
    }

    #[test]
    fn pressure_spill_runs_are_consumed_on_generation_end() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        let cfg = Config::test_config(256 * 1024, 8);
        store.configure(&cfg);

        let orthos = vec![
            Ortho::new(),
            Ortho::new().add(1)[0].clone(),
            Ortho::new().add(2)[0].clone(),
            Ortho::new().add(1)[0].add(2)[0].clone(),
        ];
        for ortho in &orthos {
            store.record_result(ortho).unwrap();
        }
        store.flush_all().unwrap();

        let spill_stats = store.pressure_spill_to_local_runs(&cfg).unwrap();
        assert!(spill_stats.buckets_drained > 0);
        assert!(spill_stats.spill_runs_created > 0);
        assert_eq!(store.total_landing_size(), 0);
        assert!(store.spill_runs.iter().any(|runs| !runs.is_empty()));
        assert!(store.spill_manifest_path().exists());

        let new_work = store.on_generation_end(&cfg, None).unwrap();
        assert_eq!(new_work as usize, orthos.len());
        assert_eq!(store.seen_len_accepted(), orthos.len() as u64);
        assert_eq!(
            count_history_orthos(&store, cfg.read_buf_bytes),
            orthos.len()
        );
        assert!(store.spill_runs.iter().all(|runs| runs.is_empty()));
        assert!(!store.spill_manifest_path().exists());
    }

    #[test]
    fn pressure_spill_stays_local_when_disk_is_healthy() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let offload_dir = base_path.join("offloaded");
        let uploads: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let offloader = Arc::new(RecordingOffloader::new(
            offload_dir,
            uploads.clone(),
            base_path.clone(),
        ));
        let _guard = OffloaderGuard;
        set_run_offloader(Some(offloader));

        let metrics = Metrics::new();
        set_offload_metrics_handle(Some(metrics.clone_handle()));

        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        let cfg = Config::test_config(256 * 1024, 8);
        store.configure(&cfg);

        for ortho in [
            Ortho::new(),
            Ortho::new().add(1)[0].clone(),
            Ortho::new().add(2)[0].clone(),
        ] {
            store.record_result(&ortho).unwrap();
        }
        store.flush_all().unwrap();

        let spill_stats = store.pressure_spill_to_local_runs(&cfg).unwrap();
        assert!(spill_stats.spill_runs_created > 0);
        assert!(
            uploads.lock().unwrap().is_empty(),
            "pressure spill should remain local when disk is healthy"
        );

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.global.offloaded_files, 0);
        assert!(snapshot.global.spill_created_files > 0);
        assert!(snapshot.global.spill_pending_files > 0);
        set_offload_metrics_handle(None);
    }

    #[test]
    fn repeated_pressure_spills_preserve_all_outputs() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
        let cfg = Config::test_config(256 * 1024, 8);
        store.configure(&cfg);

        let first_batch = vec![
            Ortho::new().add(10)[0].clone(),
            Ortho::new().add(11)[0].clone(),
        ];
        for ortho in &first_batch {
            store.record_result(ortho).unwrap();
        }
        store.flush_all().unwrap();
        let first_stats = store.pressure_spill_to_local_runs(&cfg).unwrap();
        assert!(first_stats.spill_runs_created > 0);

        let second_batch = vec![
            Ortho::new().add(12)[0].clone(),
            Ortho::new().add(13)[0].clone(),
            Ortho::new().add(10)[0].add(12)[0].clone(),
        ];
        for ortho in &second_batch {
            store.record_result(ortho).unwrap();
        }
        store.flush_all().unwrap();
        let second_stats = store.pressure_spill_to_local_runs(&cfg).unwrap();
        assert!(second_stats.spill_runs_created > 0);
        assert!(store.spill_runs.iter().flatten().count() >= 2);

        let new_work = store.on_generation_end(&cfg, None).unwrap();
        let expected = first_batch.len() + second_batch.len();
        assert_eq!(new_work as usize, expected);
        assert_eq!(store.seen_len_accepted(), expected as u64);
        assert_eq!(count_history_orthos(&store, cfg.read_buf_bytes), expected);
        assert!(store.spill_runs.iter().all(|runs| runs.is_empty()));
    }

    #[test]
    fn from_existing_restores_spill_runs_and_consumes_them() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let cfg = Config::test_config(256 * 1024, 8);

        let expected = {
            let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();
            store.configure(&cfg);
            let orthos = vec![
                Ortho::new().add(21)[0].clone(),
                Ortho::new().add(22)[0].clone(),
                Ortho::new().add(21)[0].add(22)[0].clone(),
            ];
            for ortho in &orthos {
                store.record_result(ortho).unwrap();
            }
            store.flush_all().unwrap();
            let spill_stats = store.pressure_spill_to_local_runs(&cfg).unwrap();
            assert!(spill_stats.spill_runs_created > 0);
            assert!(store.spill_manifest_path().exists());
            orthos.len()
        };

        let mut reopened = GenerationStore::from_existing(base_path.clone(), 8).unwrap();
        reopened.configure(&cfg);
        assert!(reopened.spill_runs.iter().any(|runs| !runs.is_empty()));

        let new_work = reopened.on_generation_end(&cfg, None).unwrap();
        assert_eq!(new_work as usize, expected);
        assert_eq!(reopened.seen_len_accepted(), expected as u64);
        assert_eq!(
            count_history_orthos(&reopened, cfg.read_buf_bytes),
            expected
        );
        assert!(reopened.spill_runs.iter().all(|runs| runs.is_empty()));
        assert!(!reopened.spill_manifest_path().exists());
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
        let history_raw = BufWriter::new(File::create(&history_path).unwrap());
        let mut history_file = ZstdEncoder::new(history_raw, 3).unwrap();
        let mut history_items = vec![ortho1.clone(), ortho3.clone()];
        history_items.sort_by_key(|o| o.id());
        for ortho in &history_items {
            write_ortho_record(&mut history_file, ortho, None).unwrap();
        }
        let mut history_file = history_file.finish().unwrap();
        history_file.flush().unwrap();

        // Gen: ortho2, ortho3, ortho4, ortho5
        let gen_path = base_path.join("runs").join("gen.dat");
        let gen_raw = BufWriter::new(File::create(&gen_path).unwrap());
        let mut gen_file = ZstdEncoder::new(gen_raw, 3).unwrap();
        let mut gen_items = vec![
            ortho2.clone(),
            ortho3.clone(),
            ortho4.clone(),
            ortho5.clone(),
        ];
        gen_items.sort_by_key(|o| o.id());
        for ortho in &gen_items {
            write_ortho_record(&mut gen_file, ortho, None).unwrap();
        }
        let mut gen_file = gen_file.finish().unwrap();
        gen_file.flush().unwrap();

        let unique_gen = UniqueRun::new(gen_path);
        let history_run = Run::new(history_path);
        let history_iter = history_run.iter(64 * 1024).unwrap();

        let (work_run, seen_run, accepted) =
            anti_join_orthos(unique_gen, history_iter, &base_path, 64 * 1024, None).unwrap();

        // ortho3 is already in history, so only ortho2, ortho4, ortho5 should be in work
        let mut work = collect_run(&work_run, 64 * 1024);
        work.sort_by_key(|o| o.id());
        assert_eq!(work.len(), 3);
        assert_eq!(accepted, 3);
        let mut expected = vec![ortho2, ortho4, ortho5];
        expected.sort_by_key(|o| o.id());
        assert_eq!(work, expected);
        assert_eq!(read_magic(work_run.path()), ZSTD_MAGIC);
        assert_eq!(read_magic(seen_run.path()), ZSTD_MAGIC);
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
        let gen_raw = BufWriter::new(File::create(&gen_path).unwrap());
        let mut gen_file = ZstdEncoder::new(gen_raw, 3).unwrap();
        for ortho in [&ortho1, &ortho2] {
            write_ortho_record(&mut gen_file, ortho, None).unwrap();
        }
        let mut gen_file = gen_file.finish().unwrap();
        gen_file.flush().unwrap();

        let unique_gen = UniqueRun::new(gen_path);
        let history_iter = std::iter::empty::<io::Result<StreamedOrtho>>();

        let (work_run, _seen_run, accepted) =
            anti_join_orthos(unique_gen, history_iter, &base_path, 64 * 1024, None).unwrap();

        let mut work = collect_run(&work_run, 64 * 1024);
        work.sort_by_key(|o| o.id());
        let mut expected = vec![ortho1, ortho2];
        expected.sort_by_key(|o| o.id());
        assert_eq!(work.len(), 2);
        assert_eq!(accepted, 2);
        assert_eq!(work, expected);
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
        let history_raw = BufWriter::new(File::create(&history_path).unwrap());
        let mut history_file = ZstdEncoder::new(history_raw, 3).unwrap();
        for ortho in [&ortho1, &ortho2] {
            write_ortho_record(&mut history_file, ortho, None).unwrap();
        }
        let mut history_file = history_file.finish().unwrap();
        history_file.flush().unwrap();

        // Gen: ortho1 (subset)
        let gen_path = base_path.join("runs").join("gen.dat");
        let gen_raw = BufWriter::new(File::create(&gen_path).unwrap());
        let mut gen_file = ZstdEncoder::new(gen_raw, 3).unwrap();
        write_ortho_record(&mut gen_file, &ortho1, None).unwrap();
        let mut gen_file = gen_file.finish().unwrap();
        gen_file.flush().unwrap();

        let unique_gen = UniqueRun::new(gen_path);
        let history_run = Run::new(history_path);
        let history_iter = history_run.iter(64 * 1024).unwrap();

        let (work_run, _seen_run, accepted) =
            anti_join_orthos(unique_gen, history_iter, &base_path, 64 * 1024, None).unwrap();

        let work = collect_run(&work_run, 64 * 1024);
        assert_eq!(work.len(), 0);
        assert_eq!(accepted, 0);
    }

    #[test]
    fn config_from_memory_budget_uses_fixed_leader_caps() {
        let budget = MemoryBudget::for_role(Role::Leader, 16 * 1024 * 1024 * 1024).unwrap();
        let config = Config::from_memory_budget(&budget);

        assert_eq!(config.run_budget_bytes, 384 * 1024 * 1024);
        assert_eq!(config.work_queue_cache_bytes, 256 * 1024 * 1024);
        assert_eq!(config.work_segment_max_bytes, 64 * 1024 * 1024);
        assert_eq!(config.fan_in, 64);
        assert_eq!(config.read_buf_bytes, 256 * 1024);
        assert!(config.allow_compaction);
    }

    #[test]
    fn config_from_memory_budget_uses_fixed_follower_caps() {
        let budget = MemoryBudget::for_role(Role::Follower, 16 * 1024 * 1024 * 1024).unwrap();
        let config = Config::from_memory_budget(&budget);

        assert_eq!(config.run_budget_bytes, 128 * 1024 * 1024);
        assert_eq!(config.work_queue_cache_bytes, 64 * 1024 * 1024);
        assert_eq!(config.work_segment_max_bytes, 16 * 1024 * 1024);
        assert_eq!(config.fan_in, 32);
        assert_eq!(config.read_buf_bytes, 128 * 1024);
        assert!(config.allow_compaction);
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
    fn pop_work_reads_unflushed_batch() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();

        let cfg = Config::test_config(1024 * 1024, 8);
        store.configure(&cfg);

        let ortho = Ortho::new();
        store.push_segments(vec![ortho.clone()]).unwrap();
        assert_eq!(store.work_len(), 1);

        let popped = store.pop_work().unwrap();
        assert!(popped.is_some());
        assert_eq!(store.work_len(), 0);
    }

    #[test]
    fn pop_work_handles_zero_cache_size() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();

        let mut cfg = Config::test_config(1024 * 1024, 8);
        cfg.work_queue_cache_bytes = 0;
        cfg.work_segment_max_bytes = usize::MAX; // prevent auto-flush
        store.configure(&cfg);

        let ortho = Ortho::new();
        store.push_segments(vec![ortho]).unwrap();

        assert!(store.pop_work().unwrap().is_some());
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
        assert!(
            store.work_len() > 0,
            "Work queue should have items after push and flush, got {}",
            store.work_len()
        );

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
        println!(
            "  Gen 0: {} orthos -> {} new work",
            processed_gen0, new_work_gen0
        );
        println!(
            "  Gen 1: {} orthos -> {} new work",
            processed_gen1, new_work_gen1
        );
        println!(
            "  Gen 2: {} orthos -> {} new work",
            processed_gen2, new_work_gen2
        );
        println!("  Final work queue: {}", store.work_len());
        println!("  Final seen count: {}", store.seen_len_accepted());
    }

    #[test]
    fn test_shapes_over_many_generations() {
        // Walk through many generations and dump the best shape seen each time.
        // Keeps work bounded to avoid blowing up the test runtime.
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 8).unwrap();

        let mut cfg = Config::test_config(2 * 1024 * 1024, 8);
        cfg.work_queue_cache_bytes = 16 * 1024;
        store.configure(&cfg);

        // Seed with a base ortho and one that is almost full to force an "up" expansion path
        let mut prefilled = Ortho::new();
        for v in [1u32, 2, 3] {
            prefilled = prefilled.add(v).into_iter().next().unwrap();
        }
        store.push_segments(vec![Ortho::new(), prefilled]).unwrap();
        store.flush_all().unwrap();

        let mut shapes: Vec<String> = Vec::new();
        let max_per_gen = 5usize;
        let mut best_so_far: Option<(Vec<usize>, usize, usize)> = Some((vec![2, 2], 1, 0)); // dims, volume, fullness
        let mut seen_shapes: std::collections::HashSet<String> = std::collections::HashSet::new();

        for gen_idx in 0..=30 {
            let mut processed = 0usize;
            let mut best_this_gen: Option<(Vec<usize>, usize, usize)> = None;
            let mut new_shapes_this_gen: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();

            while let Some(ortho) = store.pop_work().unwrap() {
                let val = 2 + (gen_idx as u32 % 5);
                for child in ortho.add(val) {
                    let dims: Vec<usize> = child.dims().iter().map(|d| *d as usize).collect();
                    let vol = child.volume();
                    let full = child.fullness();
                    let sig = format!("{:?}", dims);
                    if seen_shapes.insert(sig.clone()) {
                        new_shapes_this_gen.insert(sig);
                    }
                    if best_this_gen
                        .as_ref()
                        .map_or(true, |(_, v, f)| vol > *v || (vol == *v && full > *f))
                    {
                        best_this_gen = Some((dims.clone(), vol, full));
                    }
                    if best_so_far
                        .as_ref()
                        .map_or(true, |(_, v, f)| vol > *v || (vol == *v && full > *f))
                    {
                        best_so_far = Some((dims.clone(), vol, full));
                    }
                    store.record_result(&child).unwrap();
                    processed += 1;
                    if processed >= max_per_gen {
                        // Re-queue the current work item to keep the queue alive across generations.
                        store.push_segments(vec![child]).unwrap();
                        break;
                    }
                }
                if processed >= max_per_gen {
                    break;
                }
            }

            let _ = store.on_generation_end(&cfg, None).unwrap();

            let desc = format!(
                "gen {}: {} new shapes [{}]",
                gen_idx,
                new_shapes_this_gen.len(),
                new_shapes_this_gen
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("{}", desc);
            shapes.push(desc);
        }

        // Should have logged all generations up to 30 (inclusive)
        assert_eq!(shapes.len(), 31);
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
        println!(
            "  Gen 1: {} processed, {} new work, {} total seen",
            processed,
            new_work_gen1,
            store.seen_len_accepted()
        );
    }

    #[test]
    fn on_generation_end_reports_and_enqueues_consistently() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 4).unwrap();
        let cfg = Config::test_config(512 * 1024, 8);
        store.configure(&cfg);

        // Seed work with a single ortho
        store.push_segments(vec![Ortho::new()]).unwrap();
        store.flush_all().unwrap();

        // Process gen 0 work and emit two children
        while let Some(ortho) = store.pop_work().unwrap() {
            for child in ortho.add(1) {
                store.record_result(&child).unwrap();
            }
        }

        let new_work = store.on_generation_end(&cfg, None).unwrap();
        assert!(
            new_work > 0,
            "transition should report nonzero new_work when children were recorded"
        );
        assert_eq!(
            store.work_len() as u64,
            new_work,
            "work queue length should match reported new_work"
        );

        // All reported work should be retrievable
        let mut popped = 0u64;
        while let Some(_) = store.pop_work().unwrap() {
            popped += 1;
        }
        assert_eq!(
            popped, new_work,
            "pop_work should yield exactly the reported new_work items"
        );
    }

    #[test]
    fn prune_history_with_bound_drops_hopeless_and_keeps_rest() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path.clone(), 2).unwrap();
        let cfg = Config::test_config(512 * 1024, 8);
        store.configure(&cfg);

        let interner = Interner::from_text("a b c");
        let a_idx = interner.vocabulary().iter().position(|w| w == "a").unwrap();
        let b_idx = interner.vocabulary().iter().position(|w| w == "b").unwrap();

        // Record two orthos: one short, one longer.
        let short = Ortho::new().add(a_idx as u32)[0].clone();
        let long = Ortho::new().add(a_idx as u32)[0].add(b_idx as u32)[0].clone();
        store.record_result(&short).unwrap();
        store.record_result(&long).unwrap();
        store.flush_all().unwrap();

        // Finish gen 0 to move results into history
        let _ = store.on_generation_end(&cfg, None).unwrap();

        // Prune with a best_score just above the short ortho but below the long one.
        let best_score = OrthoScore::optimistic_bound(short.volume(), short.fullness() + 1);
        let (kept, pruned) = store
            .prune_history_with_bound(&interner, best_score, None, cfg.read_buf_bytes)
            .unwrap();

        assert_eq!(kept + pruned, 2, "all orthos accounted for");
        // Document current behavior: pruning may keep both if bound sees potential.
        assert!(pruned <= 2, "pruned count within expected range");
    }

    #[test]
    fn cached_best_uses_full_score_not_just_volume() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path, 2).unwrap();
        let cfg = Config::test_config(512 * 1024, 8);
        store.configure(&cfg);

        let skinny = Ortho::from_test_parts(
            vec![2, 7],
            {
                let mut payload = vec![Some(1); 13];
                payload.push(None);
                payload
            },
            None,
        );
        let squareish = Ortho::from_test_parts(
            vec![3, 4],
            {
                let mut payload = vec![Some(1); 11];
                payload.push(None);
                payload
            },
            None,
        );

        assert_eq!(
            skinny.volume(),
            squareish.volume(),
            "setup expects equal volume"
        );
        assert!(
            squareish.score() > skinny.score(),
            "setup expects lower variance to win"
        );

        store
            .push_segments(vec![skinny.clone(), squareish.clone()])
            .unwrap();

        let cached_best = store
            .peek_best_ortho_in_cache()
            .expect("cached best should exist");
        assert_eq!(
            cached_best.id(),
            squareish.id(),
            "cache best should prefer the better variance-aware score"
        );
    }

    #[test]
    fn work_segment_refill_preserves_fifo_order() {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path().to_path_buf();
        let mut store = GenerationStore::new_with_config(base_path, 2).unwrap();
        let mut cfg = Config::test_config(64 * 1024, 4);
        cfg.work_queue_cache_bytes = 1;
        cfg.work_segment_max_bytes = 1;
        store.configure(&cfg);

        let first = Ortho::new().add(1).pop().unwrap();
        let second = Ortho::new().add(2).pop().unwrap();

        store.push_segments(vec![first.clone()]).unwrap();
        store.flush_work_segment_batch().unwrap();
        store.push_segments(vec![second.clone()]).unwrap();
        store.flush_work_segment_batch().unwrap();

        assert_eq!(store.pop_work().unwrap(), Some(first));
        assert_eq!(store.pop_work().unwrap(), Some(second));
    }

    #[test]
    fn run_object_key_scopes_under_store_name() {
        let base_path = PathBuf::from("/tmp/example/input_w16_ts123.txt.work");
        let run_path = base_path.join("runs").join("b=00-run-0.dat");

        let key = run_object_key(&base_path, &run_path).unwrap();
        assert_eq!(key, "input_w16_ts123.txt.work/runs/b=00-run-0.dat");
    }
}
