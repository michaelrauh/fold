use crate::{
    generation_store, memory_safety, metrics::Metrics, offload_config::OffloadConfig,
    tiered_store::TieredStore,
};
use std::cell::{Cell, RefCell};
#[cfg(unix)]
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};
#[cfg(not(unix))]
use sysinfo::Disks;

const ARCHIVE_PAYLOAD_MAGIC: &[u8; 8] = b"FOLDRSLT";
const ARCHIVE_PAYLOAD_VERSION: u32 = 1;
const DISK_SNAPSHOT_TTL: Duration = Duration::from_millis(250);
const RECLAIM_EXACT_REFRESH_INTERVAL: usize = 8;

#[derive(Clone, Copy, Debug)]
pub(crate) struct DiskSpaceSnapshot {
    pub(crate) total_bytes: u64,
    pub(crate) available_bytes: u64,
    sampled_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SnapshotFreshness {
    CachedOk,
    ForceRefresh,
}

#[derive(Clone)]
struct DiskSafetyContext {
    enabled: bool,
    base_dir: PathBuf,
    floor_bytes: Option<u64>,
    hysteresis_margin_bytes: u64,
    offload_headroom_bytes: usize,
    cache_dir: PathBuf,
    cache_bytes_cap: u64,
    full_runs_dir: PathBuf,
    doubling_runs_dir: PathBuf,
    disk_snapshot: Rc<RefCell<Option<DiskSpaceSnapshot>>>,
    exact_probe_count: Rc<Cell<u64>>,
}

thread_local! {
    static CTX: RefCell<Option<DiskSafetyContext>> = const { RefCell::new(None) };
    static METRICS: RefCell<Option<Metrics>> = const { RefCell::new(None) };
    static IN_RECLAIM: Cell<bool> = const { Cell::new(false) };
}

struct ReclaimGuard;

impl Drop for ReclaimGuard {
    fn drop(&mut self) {
        IN_RECLAIM.with(|flag| flag.set(false));
    }
}

#[derive(Clone)]
struct FileInfo {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

#[derive(Clone, Debug, Default)]
struct ReclaimCandidateStats {
    managed_roots: usize,
    managed_candidates: usize,
    skipped_remote: usize,
    skipped_missing_local: usize,
    skipped_zero_bytes: usize,
    offloaded_files: usize,
    offloaded_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReclaimTargets {
    pub(crate) write_target: u64,
    pub(crate) reclaim_target: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BundleStatus {
    Success,
    Failed,
    Unknown,
}

pub fn configure(base_dir: PathBuf, cfg: &OffloadConfig) {
    CTX.with(|slot| {
        *slot.borrow_mut() = Some(DiskSafetyContext {
            enabled: cfg.enabled,
            base_dir,
            floor_bytes: cfg.disk_free_low_water,
            hysteresis_margin_bytes: cfg.disk_hysteresis_margin_bytes,
            offload_headroom_bytes: cfg.offload_headroom_bytes,
            cache_dir: cfg.cache_dir.clone(),
            cache_bytes_cap: cfg.cache_bytes_cap,
            full_runs_dir: PathBuf::from("fold_history").join("full_runs"),
            doubling_runs_dir: PathBuf::from("fold_history").join("doubling_runs"),
            disk_snapshot: Rc::new(RefCell::new(None)),
            exact_probe_count: Rc::new(Cell::new(0)),
        });
    });
}

pub fn clear() {
    CTX.with(|slot| *slot.borrow_mut() = None);
    METRICS.with(|slot| *slot.borrow_mut() = None);
    IN_RECLAIM.with(|slot| slot.set(false));
}

pub fn set_metrics_handle(handle: Option<Metrics>) {
    METRICS.with(|slot| *slot.borrow_mut() = handle);
}

pub fn ensure_write_budget(bytes_needed: u64, reason: &str) -> io::Result<()> {
    let Some(ctx) = current_ctx() else {
        return Ok(());
    };
    if !ctx.enabled || bytes_needed == 0 {
        return Ok(());
    }
    let Some(floor_bytes) = ctx.floor_bytes else {
        return Ok(());
    };
    if floor_bytes == 0 || IN_RECLAIM.with(|flag| flag.get()) {
        return Ok(());
    }

    let target_free = disk_target(&ctx, bytes_needed);
    let free_before =
        available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::CachedOk)?;
    if free_before < target_free {
        apply_local_cleanup(&ctx)?;
        let free_after_cleanup =
            available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::ForceRefresh)?;
        if free_after_cleanup < target_free {
            let _ = run_reclaim_to_target(
                ReclaimTargets {
                    write_target: target_free,
                    reclaim_target: target_free,
                },
                reason,
            );
        }
    }

    let free_after =
        available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::ForceRefresh)?;
    if free_after >= bytes_needed {
        return Ok(());
    }

    let rss_before = sync_process_rss_metrics();
    log(format!(
        "Disk gate denied inline write: reason={}, bytes_needed={}, free_before={}, free_after={}, disk_threshold={}, rss_before={}",
        reason, bytes_needed, free_before, free_after, target_free, rss_before
    ));
    let rss_after = sync_process_rss_metrics();
    Err(io::Error::other(format!(
        "insufficient local disk for write: reason={}, bytes_needed={}, free_before={}, free_after={}, disk_threshold={}, floor={}, rss_before={}, rss_after={}",
        reason,
        bytes_needed,
        free_before,
        free_after,
        target_free,
        floor_bytes,
        rss_before,
        rss_after
    )))
}

pub(crate) fn reclaim_required(bytes_needed: u64) -> io::Result<Option<ReclaimTargets>> {
    let Some(ctx) = current_ctx() else {
        return Ok(None);
    };
    if !ctx.enabled || bytes_needed == 0 {
        return Ok(None);
    }
    let Some(floor_bytes) = ctx.floor_bytes else {
        return Ok(None);
    };
    if floor_bytes == 0 || IN_RECLAIM.with(|flag| flag.get()) {
        return Ok(None);
    }

    let write_target = disk_target(&ctx, bytes_needed);
    let free_cached =
        available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::CachedOk)?;
    if free_cached >= write_target {
        return Ok(None);
    }
    apply_local_cleanup(&ctx)?;
    let free_now =
        available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::ForceRefresh)?;
    if free_now >= write_target {
        return Ok(None);
    }

    Ok(Some(ReclaimTargets {
        write_target,
        reclaim_target: write_target,
    }))
}

pub fn maybe_reclaim(bytes_needed: u64, reason: &str) -> io::Result<bool> {
    let Some(targets) = reclaim_required(bytes_needed)? else {
        return Ok(false);
    };
    run_reclaim_to_target(targets, reason)?;
    Ok(true)
}

pub(crate) fn run_reclaim_to_target(targets: ReclaimTargets, reason: &str) -> io::Result<()> {
    let Some(ctx) = current_ctx() else {
        return Ok(());
    };
    if !ctx.enabled || IN_RECLAIM.with(|flag| flag.get()) {
        return Ok(());
    }

    let free_before =
        available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::ForceRefresh)?;
    let rss_before = sync_process_rss_metrics();
    log(format!(
        "Tiering start: reason={}, free_before={}, disk_threshold={}, rss_before={}",
        reason, free_before, targets.write_target, rss_before
    ));
    reclaim_until(&ctx, targets.reclaim_target, reason)?;
    let free_after =
        available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::ForceRefresh)?;
    let rss_after = sync_process_rss_metrics();
    log(format!(
        "Tiering finish: reason={}, free_after={}, disk_threshold={}, rss_after={}",
        reason, free_after, targets.write_target, rss_after
    ));
    if free_after < targets.write_target {
        log(format!(
            "Disk still below threshold after offload: reason={}, free_after={}, disk_threshold={}, rss_after={}",
            reason, free_after, targets.write_target, rss_after
        ));
    }
    Ok(())
}

pub fn ensure_archive_results_local(archive_path: &Path) -> io::Result<()> {
    let results_dir = archive_results_dir(archive_path);
    if results_dir.exists() {
        return Ok(());
    }

    let payload_path = archive_payload_path(archive_path);
    let resolved_payload = generation_store::resolve_managed_path(&payload_path)?;
    let mut reader = BufReader::new(File::open(&resolved_payload)?);
    let header = read_archive_payload_header(&mut reader)?;
    let _ = maybe_reclaim(
        header.total_bytes.saturating_add(64 * 1024),
        &format!("restore archive results {}", archive_path.display()),
    )?;
    ensure_write_budget(
        header.total_bytes.saturating_add(64 * 1024),
        &format!("restore archive results {}", archive_path.display()),
    )?;

    fs::create_dir_all(&results_dir)?;
    for _ in 0..header.entry_count {
        let rel_len = read_u32(&mut reader)? as usize;
        let mut rel_buf = vec![0u8; rel_len];
        reader.read_exact(&mut rel_buf)?;
        let rel_path = std::str::from_utf8(&rel_buf)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
        let file_size = read_u64(&mut reader)?;
        let dest_path = results_dir.join(rel_path);
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut writer = BufWriter::new(File::create(&dest_path)?);
        let copied = io::copy(&mut reader.by_ref().take(file_size), &mut writer)?;
        writer.flush()?;
        if copied != file_size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "archive payload truncated while restoring {}",
                    archive_path.display()
                ),
            ));
        }
    }

    log(format!(
        "Restored archive payload into local results: {}",
        archive_path.display()
    ));
    Ok(())
}

fn reclaim_until(ctx: &DiskSafetyContext, target_free: u64, reason: &str) -> io::Result<()> {
    IN_RECLAIM.with(|flag| flag.set(true));
    let _guard = ReclaimGuard;

    apply_local_cleanup(ctx)?;
    let free_after_cleanup =
        available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::ForceRefresh)?;
    if free_after_cleanup >= target_free {
        return Ok(());
    }

    let free_after_reclaim = reclaim_active_store_files(ctx, target_free, free_after_cleanup)?;
    if free_after_reclaim >= target_free {
        return Ok(());
    }

    log(format!(
        "Disk offload exhausted without reaching threshold: reason={}, target_free={}",
        reason, target_free
    ));
    Ok(())
}

fn apply_local_cleanup(ctx: &DiskSafetyContext) -> io::Result<()> {
    prune_cache_to_cap(&ctx.cache_dir, ctx.cache_bytes_cap)?;
    prune_bundle_root(&ctx.full_runs_dir)?;
    prune_bundle_root(&ctx.doubling_runs_dir)?;
    Ok(())
}

fn reclaim_active_store_files(
    ctx: &DiskSafetyContext,
    target_free: u64,
    mut free_estimate: u64,
) -> io::Result<u64> {
    let mut candidates = Vec::new();
    let mut stats = ReclaimCandidateStats::default();
    collect_active_store_candidates(&ctx.base_dir, &ctx.cache_dir, &mut candidates, &mut stats)?;
    stats.managed_candidates = candidates.len();
    candidates.sort_by(|a, b| {
        a.modified
            .cmp(&b.modified)
            .then_with(|| b.size.cmp(&a.size))
    });

    let mut processed = 0usize;
    for candidate in candidates {
        if free_estimate >= target_free {
            break;
        }
        match offload_and_delete(&candidate.path) {
            Ok(true) => {
                free_estimate = free_estimate.saturating_add(candidate.size);
                stats.offloaded_files = stats.offloaded_files.saturating_add(1);
                stats.offloaded_bytes = stats.offloaded_bytes.saturating_add(candidate.size);
                log(format!(
                    "Offloaded sealed file: {} bytes from {}",
                    candidate.size,
                    candidate.path.display()
                ));
            }
            Ok(false) => {}
            Err(err) => {
                log(format!(
                    "Offload failed for sealed file {}: {}",
                    candidate.path.display(),
                    err
                ));
            }
        }
        processed = processed.saturating_add(1);
        if processed % RECLAIM_EXACT_REFRESH_INTERVAL == 0 {
            free_estimate =
                available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::ForceRefresh)?;
        }
    }

    free_estimate =
        available_space_for_with_freshness(&ctx.base_dir, SnapshotFreshness::ForceRefresh)?;
    log(format!(
        "Catalog reclaim summary: roots={}, candidates={}, offloaded_files={}, offloaded_bytes={}, skipped_remote={}, skipped_missing_local={}, skipped_zero_bytes={}, free_after={}, target_free={}",
        stats.managed_roots,
        stats.managed_candidates,
        stats.offloaded_files,
        stats.offloaded_bytes,
        stats.skipped_remote,
        stats.skipped_missing_local,
        stats.skipped_zero_bytes,
        free_estimate,
        target_free
    ));
    Ok(free_estimate)
}

fn collect_active_store_candidates(
    root: &Path,
    cache_dir: &Path,
    out: &mut Vec<FileInfo>,
    stats: &mut ReclaimCandidateStats,
) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    if root == cache_dir || root.starts_with(cache_dir) {
        return Ok(());
    }

    if is_tiered_store_root(root) {
        let store = TieredStore::open(root)?;
        let scan = store.reclaim_candidates_with_summary()?;
        stats.managed_roots = stats.managed_roots.saturating_add(1);
        stats.skipped_remote = stats.skipped_remote.saturating_add(scan.skipped_remote);
        stats.skipped_missing_local = stats
            .skipped_missing_local
            .saturating_add(scan.skipped_missing_local);
        stats.skipped_zero_bytes = stats
            .skipped_zero_bytes
            .saturating_add(scan.skipped_zero_bytes);
        for candidate in scan.candidates {
            out.push(FileInfo {
                path: candidate.path,
                size: candidate.bytes,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(candidate.last_touch_epoch),
            });
        }
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path == *cache_dir || path.starts_with(cache_dir) {
            continue;
        }
        if path.is_dir() {
            collect_active_store_candidates(&path, cache_dir, out, stats)?;
        }
    }

    Ok(())
}

fn is_tiered_store_root(path: &Path) -> bool {
    path.join("catalog.json").is_file() && path.join("segments").is_dir()
}

fn prune_cache_to_cap(cache_dir: &Path, cap: u64) -> io::Result<()> {
    if !cache_dir.exists() {
        return Ok(());
    }
    let mut files = Vec::new();
    collect_files(cache_dir, &mut files)?;
    let mut total: u64 = files.iter().map(|info| info.size).sum();
    if total <= cap {
        return Ok(());
    }

    files.sort_by_key(|info| info.modified);
    for file in files {
        if total <= cap {
            break;
        }
        fs::remove_file(&file.path)?;
        remove_empty_dirs_upwards(&file.path, cache_dir)?;
        total = total.saturating_sub(file.size);
    }

    log(format!(
        "Pruned offload cache to cap: cache_dir={}, cap={}",
        cache_dir.display(),
        cap
    ));
    Ok(())
}

fn prune_bundle_root(root: &Path) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }

    let mut dirs = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(file_info(&path)?);
        }
    }
    if dirs.len() <= 3 {
        return Ok(());
    }
    dirs.sort_by_key(|info| info.modified);

    let mut keep: Vec<PathBuf> = Vec::new();
    if let Some(newest) = dirs.last() {
        keep.push(newest.path.clone());
    }
    if let Some(path) = newest_bundle_with_status(&dirs, BundleStatus::Success) {
        keep.push(path);
    }
    if let Some(path) = newest_bundle_with_status(&dirs, BundleStatus::Failed) {
        keep.push(path);
    }
    keep.sort();
    keep.dedup();

    for dir in dirs {
        if keep.iter().any(|kept| kept == &dir.path) {
            continue;
        }
        fs::remove_dir_all(&dir.path)?;
        log(format!("Pruned local run bundle: {}", dir.path.display()));
    }

    Ok(())
}

fn newest_bundle_with_status(dirs: &[FileInfo], wanted: BundleStatus) -> Option<PathBuf> {
    dirs.iter()
        .rev()
        .find(|info| classify_bundle(&info.path).ok() == Some(wanted))
        .map(|info| info.path.clone())
}

fn classify_bundle(path: &Path) -> io::Result<BundleStatus> {
    let status_path = path.join("status.txt");
    if status_path.exists() {
        return Ok(parse_status_file(&status_path));
    }

    let mut saw_success = false;
    let mut saw_failure = false;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let child = entry.path();
        if child.is_dir() {
            let child_status = child.join("status.txt");
            if child_status.exists() {
                match parse_status_file(&child_status) {
                    BundleStatus::Success => saw_success = true,
                    BundleStatus::Failed => saw_failure = true,
                    BundleStatus::Unknown => {}
                }
            }
        }
    }

    if saw_failure {
        Ok(BundleStatus::Failed)
    } else if saw_success {
        Ok(BundleStatus::Success)
    } else {
        Ok(BundleStatus::Unknown)
    }
}

fn parse_status_file(path: &Path) -> BundleStatus {
    match fs::read_to_string(path) {
        Ok(contents) if contents.contains("status: success") => BundleStatus::Success,
        Ok(contents) if contents.contains("status: failed") => BundleStatus::Failed,
        _ => BundleStatus::Unknown,
    }
}

fn collect_files(root: &Path, out: &mut Vec<FileInfo>) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if path.is_file() {
            out.push(file_info(&path)?);
        }
    }

    Ok(())
}

fn file_info(path: &Path) -> io::Result<FileInfo> {
    let metadata = fs::metadata(path)?;
    Ok(FileInfo {
        path: path.to_path_buf(),
        size: generation_store::managed_file_size(path).unwrap_or_else(|_| metadata.len()),
        modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    })
}

fn offload_and_delete(path: &Path) -> io::Result<bool> {
    let size = generation_store::managed_file_size(path).unwrap_or(0);
    let offload_headroom = current_ctx()
        .map(|ctx| ctx.offload_headroom_bytes)
        .unwrap_or(16 * 1024 * 1024);
    memory_safety::ensure_phase_headroom(
        offload_headroom,
        &format!("disk reclaim offload {}", path.display()),
    )?;
    let rss_before = sync_process_rss_metrics();
    match generation_store::offload_managed_path_if_configured(path)? {
        true => {
            let rss_after = sync_process_rss_metrics();
            log(format!(
                "Disk reclaim offloaded: {} bytes from {} (rss_before={} rss_after={})",
                size,
                path.display(),
                rss_before,
                rss_after
            ));
            Ok(true)
        }
        false => Err(io::Error::other(format!(
            "offload refused while reclaiming {}",
            path.display()
        ))),
    }
}

fn archive_results_dir(archive_path: &Path) -> PathBuf {
    archive_path.join("results")
}

fn archive_payload_path(archive_path: &Path) -> PathBuf {
    archive_path.join("results.payload.bin")
}

#[cfg(test)]
fn pack_and_offload_archive_results(archive_path: &Path) -> io::Result<u64> {
    let results_dir = archive_results_dir(archive_path);
    if !results_dir.exists() {
        return Ok(0);
    }

    let mut entries = Vec::new();
    collect_archive_entries(&results_dir, &results_dir, &mut entries)?;
    if entries.is_empty() {
        fs::remove_dir_all(&results_dir)?;
        return Ok(0);
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let total_bytes: u64 = entries.iter().map(|(_, info)| info.size).sum();
    let scratch_bytes = entries.iter().map(|(_, info)| info.size).max().unwrap_or(0);
    let free_now = current_ctx()
        .map(|ctx| available_space_for(&ctx.base_dir))
        .transpose()?
        .unwrap_or(0);
    let floor = current_ctx().and_then(|ctx| ctx.floor_bytes).unwrap_or(0);
    if scratch_bytes > 0 && free_now < floor.saturating_add(scratch_bytes) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "not enough scratch to pack archive payload: need {} free bytes",
                floor.saturating_add(scratch_bytes)
            ),
        ));
    }

    let payload_path = archive_payload_path(archive_path);
    let temp_path = payload_path.with_extension("bin.tmp");
    let mut writer = BufWriter::new(File::create(&temp_path)?);
    writer.write_all(ARCHIVE_PAYLOAD_MAGIC)?;
    writer.write_all(&ARCHIVE_PAYLOAD_VERSION.to_le_bytes())?;
    writer.write_all(&total_bytes.to_le_bytes())?;
    writer.write_all(&(entries.len() as u64).to_le_bytes())?;

    for (relative, info) in &entries {
        let rel_str = relative.to_string_lossy();
        let rel_bytes = rel_str.as_bytes();
        writer.write_all(&(rel_bytes.len() as u32).to_le_bytes())?;
        writer.write_all(rel_bytes)?;
        writer.write_all(&info.size.to_le_bytes())?;

        let mut reader = BufReader::new(File::open(&info.path)?);
        io::copy(&mut reader, &mut writer)?;
        writer.flush()?;
        fs::remove_file(&info.path)?;
    }

    writer.flush()?;
    fs::rename(&temp_path, &payload_path)?;
    let _ = fs::remove_dir_all(&results_dir);

    offload_and_delete(&payload_path)?;
    Ok(total_bytes)
}

fn disk_target(ctx: &DiskSafetyContext, bytes_needed: u64) -> u64 {
    ctx.floor_bytes
        .unwrap_or(0)
        .saturating_add(ctx.hysteresis_margin_bytes)
        .max(bytes_needed)
}

#[cfg(test)]
fn collect_archive_entries(
    root: &Path,
    current: &Path,
    out: &mut Vec<(PathBuf, FileInfo)>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_archive_entries(root, &path, out)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|err| io::Error::other(err.to_string()))?
                .to_path_buf();
            out.push((relative, file_info(&path)?));
        }
    }
    Ok(())
}

struct ArchivePayloadHeader {
    total_bytes: u64,
    entry_count: u64,
}

fn read_archive_payload_header<R: Read>(reader: &mut R) -> io::Result<ArchivePayloadHeader> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != ARCHIVE_PAYLOAD_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid archive payload magic",
        ));
    }

    let version = read_u32(reader)?;
    if version != ARCHIVE_PAYLOAD_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported archive payload version {}", version),
        ));
    }

    let total_bytes = read_u64(reader)?;
    let entry_count = read_u64(reader)?;
    Ok(ArchivePayloadHeader {
        total_bytes,
        entry_count,
    })
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn current_ctx() -> Option<DiskSafetyContext> {
    CTX.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
fn available_space_for(path: &Path) -> io::Result<u64> {
    available_space_for_with_freshness(path, SnapshotFreshness::CachedOk)
}

fn available_space_for_with_freshness(
    path: &Path,
    freshness: SnapshotFreshness,
) -> io::Result<u64> {
    Ok(disk_usage_snapshot_for_path(path, freshness)?.available_bytes)
}

pub(crate) fn disk_usage_snapshot_for_metrics(path: &Path) -> io::Result<DiskSpaceSnapshot> {
    disk_usage_snapshot_for_path(path, SnapshotFreshness::CachedOk)
}

fn disk_usage_snapshot_for_path(
    path: &Path,
    freshness: SnapshotFreshness,
) -> io::Result<DiskSpaceSnapshot> {
    let ctx = current_ctx().filter(|ctx| path.starts_with(&ctx.base_dir) || ctx.base_dir == path);
    if let Some(ctx) = ctx {
        if freshness == SnapshotFreshness::CachedOk {
            if let Some(snapshot) = *ctx.disk_snapshot.borrow() {
                if snapshot.sampled_at.elapsed() < DISK_SNAPSHOT_TTL {
                    return Ok(snapshot);
                }
            }
        }

        let snapshot = exact_disk_usage_snapshot(path)?;
        ctx.disk_snapshot.borrow_mut().replace(snapshot);
        ctx.exact_probe_count
            .set(ctx.exact_probe_count.get().saturating_add(1));
        return Ok(snapshot);
    }

    exact_disk_usage_snapshot(path)
}

fn exact_disk_usage_snapshot(path: &Path) -> io::Result<DiskSpaceSnapshot> {
    let (total_bytes, available_bytes) = probe_disk_usage(path)?;
    Ok(DiskSpaceSnapshot {
        total_bytes,
        available_bytes,
        sampled_at: Instant::now(),
    })
}

#[cfg(unix)]
fn probe_disk_usage(path: &Path) -> io::Result<(u64, u64)> {
    let probe_path = existing_probe_path(path);
    let c_path = CString::new(probe_path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path contains interior NUL: {}", probe_path.display()),
        )
    })?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let stats = unsafe { stats.assume_init() };
    let fragment_size = if stats.f_frsize > 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    } as u64;
    Ok((
        (stats.f_blocks as u64).saturating_mul(fragment_size),
        (stats.f_bavail as u64).saturating_mul(fragment_size),
    ))
}

#[cfg(not(unix))]
fn probe_disk_usage(path: &Path) -> io::Result<(u64, u64)> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut disks = Disks::new_with_refreshed_list();
    disks.refresh_list();
    disks.refresh();

    let mut best: Option<(u64, u64, usize)> = None;
    for disk in disks.iter() {
        let mount = disk.mount_point();
        if canonical.starts_with(mount) {
            let score = mount.as_os_str().to_string_lossy().len();
            if best.map_or(true, |(_, _, best_score)| score > best_score) {
                best = Some((disk.total_space(), disk.available_space(), score));
            }
        }
    }

    best.map(|(total, available, _)| (total, available))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no disk mount found for {}", canonical.display()),
            )
        })
}

fn existing_probe_path(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            return current;
        }
        if !current.pop() {
            return path.to_path_buf();
        }
    }
}

#[cfg(test)]
pub(crate) fn debug_exact_disk_probe_count() -> u64 {
    current_ctx()
        .map(|ctx| ctx.exact_probe_count.get())
        .unwrap_or(0)
}

fn remove_empty_dirs_upwards(path: &Path, root: &Path) -> io::Result<()> {
    let mut current = path.parent();
    while let Some(dir) = current {
        if dir == root {
            break;
        }
        match fs::remove_dir(dir) {
            Ok(()) => current = dir.parent(),
            Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty => break,
            Err(err) if err.kind() == io::ErrorKind::NotFound => current = dir.parent(),
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn log(message: String) {
    METRICS.with(|slot| {
        if let Some(metrics) = slot.borrow().clone() {
            metrics.add_log(message);
        }
    });
}

fn sync_process_rss_metrics() -> usize {
    let rss_bytes = memory_safety::current_process_rss_bytes();
    METRICS.with(|slot| {
        if let Some(metrics) = slot.borrow().clone() {
            metrics.update_global(|g| g.process_rss_bytes = rss_bytes);
        }
    });
    rss_bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Metrics;
    use crate::offload_runtime::configure_offload_runtime;
    use crate::tiered_store::TieredStore;
    use tempfile::TempDir;

    fn write_test_file(path: &Path, size: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, vec![7u8; size]).unwrap();
    }

    #[test]
    fn archive_payload_round_trip_restores_results() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        let archive = base.join("input").join("archive_test.bin");
        let results_dir = archive.join("results").join("history").join("b=00");
        fs::create_dir_all(&results_dir).unwrap();
        let results_file = results_dir.join("history-0.dat");
        fs::write(&results_file, b"hello archive").unwrap();
        fs::write(archive.join("metadata.txt"), "1").unwrap();
        fs::write(archive.join("lineage.txt"), "\"x\"").unwrap();
        fs::write(archive.join("text_meta.txt"), "1\nx").unwrap();
        fs::write(archive.join("interner.bin"), b"int").unwrap();

        let local_store = temp_dir.path().join("store");
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.local_store_dir = Some(local_store);
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_free_low_water = Some(1);

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);

        let reclaimed = pack_and_offload_archive_results(&archive).unwrap();
        assert!(reclaimed > 0);
        assert!(!archive.join("results").exists());
        assert!(archive_payload_path(&archive).exists());
        assert!(generation_store::is_offload_marker(&archive_payload_path(
            &archive
        )));

        ensure_archive_results_local(&archive).unwrap();
        assert_eq!(fs::read(results_file).unwrap(), b"hello archive");
    }

    #[test]
    fn offload_prefers_oldest_sealed_candidate() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        let store =
            TieredStore::open(base.join("in_process").join("merge_1.work").join("store")).unwrap();

        const FILE_BYTES: usize = 32 * 1024 * 1024;
        const RESERVATION_BYTES: u64 = 8 * 1024 * 1024;

        store.begin_epoch(1).unwrap();
        let cold_segment = store.allocate_segment("history", "zstd", "sorted").unwrap();
        fs::write(&cold_segment.path, vec![7u8; FILE_BYTES]).unwrap();
        store
            .commit_allocated_segment(&cold_segment, FILE_BYTES as u64, 1)
            .unwrap();

        store.begin_epoch(2).unwrap();
        let warm_segment = store.allocate_segment("work", "raw", "fifo").unwrap();
        fs::write(&warm_segment.path, vec![8u8; FILE_BYTES]).unwrap();
        store
            .commit_allocated_segment(&warm_segment, FILE_BYTES as u64, 1)
            .unwrap();

        let free_now = available_space_for(&base).unwrap();
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_hysteresis_margin_bytes = 0;
        cfg.disk_free_low_water = Some(free_now.saturating_add((FILE_BYTES / 4) as u64));

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);

        let metrics = Metrics::new();
        set_metrics_handle(Some(metrics.clone_handle()));
        assert!(maybe_reclaim(RESERVATION_BYTES, "test reclaim ordering").unwrap());

        let snapshot = metrics.snapshot();
        let first_offload = snapshot
            .logs
            .iter()
            .find(|log| {
                log.message.contains("Disk reclaim offloaded")
                    || log.message.contains("Offloaded sealed file")
            })
            .expect("expected disk reclaim offload log");
        assert!(
            first_offload
                .message
                .contains(&cold_segment.path.display().to_string()),
            "oldest sealed file should be offloaded first"
        );
        assert!(!cold_segment.path.exists());
        set_metrics_handle(None);
    }

    #[test]
    fn repeated_reclaim_required_without_pressure_uses_cached_probe() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        fs::create_dir_all(&base).unwrap();

        let free_now = available_space_for(&base).unwrap();
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_hysteresis_margin_bytes = 0;
        cfg.disk_free_low_water = Some(free_now.saturating_sub(512 * 1024 * 1024));

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);

        assert!(reclaim_required(64 * 1024).unwrap().is_none());
        let probes_after_first = debug_exact_disk_probe_count();
        assert!(reclaim_required(64 * 1024).unwrap().is_none());
        assert_eq!(debug_exact_disk_probe_count(), probes_after_first);
    }

    #[test]
    fn reclaim_required_force_refreshes_when_cached_probe_shows_pressure() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        fs::create_dir_all(&base).unwrap();

        let free_now = available_space_for(&base).unwrap();
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_hysteresis_margin_bytes = 0;
        cfg.disk_free_low_water = Some(free_now.saturating_add(64 * 1024 * 1024));

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);

        assert!(reclaim_required(64 * 1024).unwrap().is_some());
        let probes_after_first = debug_exact_disk_probe_count();
        assert!(reclaim_required(64 * 1024).unwrap().is_some());
        assert!(debug_exact_disk_probe_count() > probes_after_first);
    }

    #[test]
    fn ensure_write_budget_force_refreshes_before_failure() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        fs::create_dir_all(&base).unwrap();

        let free_now = available_space_for(&base).unwrap();
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_hysteresis_margin_bytes = 0;
        cfg.disk_free_low_water = Some(free_now.saturating_add(512 * 1024 * 1024));

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);

        let _ = reclaim_required(64 * 1024).unwrap();
        let probes_before_failure = debug_exact_disk_probe_count();

        let err = ensure_write_budget(
            free_now.saturating_add(512 * 1024 * 1024),
            "test force refresh failure",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("insufficient local disk for write")
        );
        assert!(debug_exact_disk_probe_count() > probes_before_failure);
    }

    #[test]
    fn ensure_write_budget_succeeds_when_actual_write_fits_below_floor() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        fs::create_dir_all(&base).unwrap();
        const WRITE_BYTES: u64 = 8 * 1024 * 1024;

        let free_now = available_space_for(&base).unwrap();
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_hysteresis_margin_bytes = 0;
        cfg.disk_free_low_water = Some(free_now.saturating_add(64 * 1024 * 1024));

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);
        ensure_write_budget(WRITE_BYTES, "test small write below floor").unwrap();
    }

    #[test]
    fn ensure_write_budget_fails_when_actual_write_cannot_fit() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        fs::create_dir_all(&base).unwrap();

        let free_now = available_space_for(&base).unwrap();
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_hysteresis_margin_bytes = 0;
        cfg.disk_free_low_water = Some(free_now.saturating_add(512 * 1024 * 1024));

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);

        let err = ensure_write_budget(
            free_now.saturating_add(512 * 1024 * 1024),
            "test hard write failure",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("insufficient local disk for write")
        );
    }

    #[test]
    fn active_reclaim_candidates_are_catalog_segments_not_legacy_paths() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        let cache_dir = base.join("offload_cache");
        let store_root = base.join("in_process").join("merge_123.work").join("store");
        let store = TieredStore::open(&store_root).unwrap();

        let active_work = store.allocate_segment("work", "raw", "fifo").unwrap();
        fs::write(&active_work.path, b"active work").unwrap();
        store.commit_allocated_segment(&active_work, 11, 1).unwrap();

        let active_history = store.allocate_segment("history", "zstd", "sorted").unwrap();
        fs::write(&active_history.path, b"active history").unwrap();
        store
            .commit_allocated_segment(&active_history, 14, 1)
            .unwrap();

        let already_remote = store.allocate_segment("spill", "zstd", "sorted").unwrap();
        fs::write(&already_remote.path, b"remote").unwrap();
        store
            .commit_allocated_segment(&already_remote, 6, 1)
            .unwrap();
        store
            .set_remote_state_for_path(&already_remote.path, true, Some("remote-key".to_string()))
            .unwrap();

        let missing_local = store.allocate_segment("chunk", "zstd", "sorted").unwrap();
        fs::write(&missing_local.path, b"missing").unwrap();
        store
            .commit_allocated_segment(&missing_local, 7, 1)
            .unwrap();
        fs::remove_file(&missing_local.path).unwrap();

        let _zero_byte_allocation = store.allocate_segment("new-work", "raw", "fifo").unwrap();
        write_test_file(
            &store_root
                .join("heads")
                .join("default")
                .join("landing-b=00.head"),
            16,
        );
        write_test_file(
            &store_root
                .join("collections")
                .join("default")
                .join("work.json"),
            16,
        );
        write_test_file(
            &store_root.join("blobs").join("default").join("control"),
            16,
        );
        write_test_file(
            &base
                .join("in_process")
                .join("merge_123.work")
                .join("heartbeat"),
            16,
        );

        let mut candidates = Vec::new();
        let mut stats = ReclaimCandidateStats::default();
        collect_active_store_candidates(&base, &cache_dir, &mut candidates, &mut stats).unwrap();
        let mut candidate_paths: Vec<PathBuf> =
            candidates.into_iter().map(|info| info.path).collect();
        candidate_paths.sort();

        assert_eq!(stats.managed_roots, 1);
        assert_eq!(stats.skipped_remote, 1);
        assert_eq!(stats.skipped_missing_local, 1);
        assert_eq!(stats.skipped_zero_bytes, 1);
        assert_eq!(
            candidate_paths,
            vec![active_work.path.clone(), active_history.path.clone()]
        );
    }
}
