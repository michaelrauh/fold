use crate::{generation_store, memory_safety, metrics::Metrics, offload_config::OffloadConfig};
use std::cell::{Cell, RefCell};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use sysinfo::Disks;

const ARCHIVE_PAYLOAD_MAGIC: &[u8; 8] = b"FOLDRSLT";
const ARCHIVE_PAYLOAD_VERSION: u32 = 1;
#[derive(Clone)]
struct DiskSafetyContext {
    enabled: bool,
    base_dir: PathBuf,
    floor_bytes: Option<u64>,
    offload_headroom_bytes: usize,
    cache_dir: PathBuf,
    cache_bytes_cap: u64,
    full_runs_dir: PathBuf,
    doubling_runs_dir: PathBuf,
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
            offload_headroom_bytes: cfg.offload_headroom_bytes,
            cache_dir: cfg.cache_dir.clone(),
            cache_bytes_cap: cfg.cache_bytes_cap,
            full_runs_dir: PathBuf::from("fold_history").join("full_runs"),
            doubling_runs_dir: PathBuf::from("fold_history").join("doubling_runs"),
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
    let free_before = available_space_for(&ctx.base_dir)?;
    if free_before < target_free {
        apply_local_cleanup(&ctx)?;
        let free_after_cleanup = available_space_for(&ctx.base_dir)?;
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

    let free_after = available_space_for(&ctx.base_dir)?;
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
    apply_local_cleanup(&ctx)?;
    let free_now = available_space_for(&ctx.base_dir)?;
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

    let free_before = available_space_for(&ctx.base_dir)?;
    let rss_before = sync_process_rss_metrics();
    log(format!(
        "Tiering start: reason={}, free_before={}, disk_threshold={}, rss_before={}",
        reason, free_before, targets.write_target, rss_before
    ));
    reclaim_until(&ctx, targets.reclaim_target, reason)?;
    let free_after = available_space_for(&ctx.base_dir)?;
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
    if available_space_for(&ctx.base_dir)? >= target_free {
        return Ok(());
    }

    reclaim_active_store_files(ctx, target_free)?;
    if available_space_for(&ctx.base_dir)? >= target_free {
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

fn reclaim_active_store_files(ctx: &DiskSafetyContext, target_free: u64) -> io::Result<()> {
    let mut candidates = Vec::new();
    collect_active_store_candidates(&ctx.base_dir, &ctx.cache_dir, &mut candidates)?;
    candidates.sort_by(|a, b| a.modified.cmp(&b.modified).then_with(|| b.size.cmp(&a.size)));

    for candidate in candidates {
        if available_space_for(&ctx.base_dir)? >= target_free {
            break;
        }
        match offload_and_delete(&candidate.path) {
            Ok(true) => log(format!(
                "Offloaded sealed file: {} bytes from {}",
                candidate.size,
                candidate.path.display()
            )),
            Ok(false) => {}
            Err(err) => {
                log(format!(
                    "Offload failed for sealed file {}: {}",
                    candidate.path.display(),
                    err
                ));
            }
        }
    }

    Ok(())
}

fn collect_active_store_candidates(
    root: &Path,
    cache_dir: &Path,
    out: &mut Vec<FileInfo>,
) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path == *cache_dir {
            continue;
        }
        if path.is_dir() {
            collect_active_store_candidates(&path, cache_dir, out)?;
            continue;
        }
        if is_active_reclaim_candidate(&path) {
            out.push(file_info(&path)?);
        }
    }

    Ok(())
}

fn is_active_reclaim_candidate(path: &Path) -> bool {
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if matches!(
        file_name,
        "heartbeat" | "leader.lock" | "resume_manifest.json" | "source.txt" | "active.log"
    ) {
        return false;
    }
    if file_name.ends_with(".claim")
        || file_name.ends_with(".tmp")
        || file_name.ends_with(".partial")
    {
        return false;
    }

    let under_input = path.components().any(|component| component.as_os_str() == "input");
    if under_input && path.extension().map(|ext| ext == "txt").unwrap_or(false) {
        return false;
    }

    let under_results = path
        .components()
        .any(|component| component.as_os_str() == "results");
    let under_checkpoints = path
        .components()
        .any(|component| component.as_os_str() == "checkpoints");
    let under_merge_work = path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .ends_with(".work")
    });
    let under_in_process_archive = path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .ends_with(".bin")
    }) && path.components().any(|component| component.as_os_str() == "in_process");

    if under_in_process_archive {
        return false;
    }
    if under_checkpoints {
        return false;
    }

    if under_merge_work {
        return under_results
            || path.components().any(|component| {
                matches!(
                    component.as_os_str().to_string_lossy().as_ref(),
                    "history" | "runs" | "work" | "spill" | "landing"
                )
            });
    }

    under_results || path.is_file()
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
        size: metadata.len(),
        modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
    })
}

fn offload_and_delete(path: &Path) -> io::Result<bool> {
    let size = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let offload_headroom = current_ctx()
        .map(|ctx| ctx.offload_headroom_bytes)
        .unwrap_or(16 * 1024 * 1024);
    memory_safety::ensure_phase_headroom(
        offload_headroom,
        &format!("disk reclaim offload {}", path.display()),
    )?;
    let rss_before = sync_process_rss_metrics();
    match generation_store::offload_path_if_configured(path)? {
        true => {
            let _ = fs::remove_file(path);
            METRICS.with(|slot| {
                if let Some(metrics) = slot.borrow().clone() {
                    metrics.record_offload(1, size);
                }
            });
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
    ctx.floor_bytes.unwrap_or(0).max(bytes_needed)
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

fn available_space_for(path: &Path) -> io::Result<u64> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut disks = Disks::new_with_refreshed_list();
    disks.refresh_list();
    disks.refresh();

    let mut best: Option<(u64, usize)> = None;
    for disk in disks.iter() {
        let mount = disk.mount_point();
        if canonical.starts_with(mount) {
            let score = mount.as_os_str().to_string_lossy().len();
            if best.map_or(true, |(_, best_score)| score > best_score) {
                best = Some((disk.available_space(), score));
            }
        }
    }

    best.map(|(available, _)| available).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no disk mount found for {}", canonical.display()),
        )
    })
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
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

    struct CwdGuard(std::path::PathBuf);

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

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
        assert!(!archive_payload_path(&archive).exists());

        ensure_archive_results_local(&archive).unwrap();
        assert_eq!(fs::read(results_file).unwrap(), b"hello archive");
    }

    #[test]
    fn offload_prefers_oldest_sealed_candidate() {
        static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _cwd_guard = CWD_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let temp_dir = TempDir::new().unwrap();
        let prev_cwd = CwdGuard(std::env::current_dir().unwrap());
        std::env::set_current_dir(temp_dir.path()).unwrap();
        let base = temp_dir.path().join("fold_state");
        let cold_archive_path = base
            .join("input")
            .join("archive_test.bin")
            .join("results")
            .join("history")
            .join("b=00")
            .join("history-0.dat");
        let merge_segment_path = base
            .join("in_process")
            .join("merge_1.work")
            .join("work")
            .join("segment-0.dat");

        const FILE_BYTES: usize = 32 * 1024 * 1024;
        const RESERVATION_BYTES: u64 = 8 * 1024 * 1024;

        write_test_file(&cold_archive_path, FILE_BYTES);
        write_test_file(&merge_segment_path, FILE_BYTES);
        let old_time = filetime::FileTime::from_unix_time(1, 0);
        let new_time = filetime::FileTime::from_unix_time(2, 0);
        filetime::set_file_mtime(&cold_archive_path, old_time).unwrap();
        filetime::set_file_mtime(&merge_segment_path, new_time).unwrap();

        let free_now = available_space_for(&base).unwrap();
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_hysteresis_margin_bytes = 0;
        cfg.disk_free_low_water = Some(free_now.saturating_add((3 * FILE_BYTES) as u64));

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);

        let metrics = Metrics::new();
        set_metrics_handle(Some(metrics.clone_handle()));
        assert!(maybe_reclaim(RESERVATION_BYTES, "test reclaim ordering").unwrap());

        let snapshot = metrics.snapshot();
        let first_offload = snapshot
            .logs
            .iter()
            .find(|log| log.message.contains("Disk reclaim offloaded") || log.message.contains("Offloaded sealed file"))
            .expect("expected disk reclaim offload log");
        assert!(
            first_offload
                .message
                .contains(&cold_archive_path.display().to_string()),
            "oldest sealed file should be offloaded first"
        );
        set_metrics_handle(None);
        drop(prev_cwd);
    }

    #[test]
    fn ensure_write_budget_succeeds_when_actual_write_fits_below_floor() {
        static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _cwd_guard = CWD_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let temp_dir = TempDir::new().unwrap();
        let prev_cwd = CwdGuard(std::env::current_dir().unwrap());
        std::env::set_current_dir(temp_dir.path()).unwrap();
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
        drop(prev_cwd);
    }

    #[test]
    fn ensure_write_budget_fails_when_actual_write_cannot_fit() {
        static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _cwd_guard = CWD_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let temp_dir = TempDir::new().unwrap();
        let prev_cwd = CwdGuard(std::env::current_dir().unwrap());
        std::env::set_current_dir(temp_dir.path()).unwrap();
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

        drop(prev_cwd);
    }

    #[test]
    fn active_reclaim_candidates_exclude_live_merge_and_archive_control_files() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        let cache_dir = base.join("offload_cache");

        let reclaimable_archive_history = base
            .join("input")
            .join("archive_a.bin")
            .join("results")
            .join("history")
            .join("b=00")
            .join("history-0.dat");
        let reclaimable_merge_segment = base
            .join("in_process")
            .join("merge_123.work")
            .join("work")
            .join("segment-0.dat");
        let reclaimable_landing_drain = base
            .join("in_process")
            .join("merge_123.work")
            .join("landing")
            .join("b=00")
            .join("drain-0.log");

        let protected_manifest = base
            .join("in_process")
            .join("merge_123.work")
            .join("resume_manifest.json");
        let protected_heartbeat = base
            .join("in_process")
            .join("merge_123.work")
            .join("heartbeat");
        let protected_lock = base.join("in_process").join("leader.lock");
        let protected_claim = base.join("mem_claims").join("123.claim");
        let protected_metadata = base
            .join("in_process")
            .join("archive_b.bin")
            .join("metadata.txt");
        let protected_text_meta = base
            .join("in_process")
            .join("archive_b.bin")
            .join("text_meta.txt");
        let protected_lineage = base
            .join("in_process")
            .join("archive_b.bin")
            .join("lineage.txt");
        let protected_interner = base
            .join("in_process")
            .join("archive_b.bin")
            .join("interner.bin");
        let protected_optimal = base
            .join("in_process")
            .join("archive_b.bin")
            .join("optimal.bin");
        let protected_planner = base
            .join("in_process")
            .join("archive_b.bin")
            .join("planner_meta.json");
        let protected_archive_history = base
            .join("in_process")
            .join("archive_b.bin")
            .join("results")
            .join("history")
            .join("b=00")
            .join("history-0.dat");
        let protected_checkpoint_history = base
            .join("in_process")
            .join("merge_123.work")
            .join("checkpoints")
            .join("store-0000")
            .join("history")
            .join("b=00")
            .join("history-0.dat");

        for path in [
            &reclaimable_archive_history,
            &reclaimable_merge_segment,
            &reclaimable_landing_drain,
            &protected_manifest,
            &protected_heartbeat,
            &protected_lock,
            &protected_claim,
            &protected_metadata,
            &protected_text_meta,
            &protected_lineage,
            &protected_interner,
            &protected_optimal,
            &protected_planner,
            &protected_archive_history,
            &protected_checkpoint_history,
        ] {
            write_test_file(path, 16);
        }

        let mut candidates = Vec::new();
        collect_active_store_candidates(&base, &cache_dir, &mut candidates).unwrap();
        let candidate_paths: HashSet<PathBuf> =
            candidates.into_iter().map(|info| info.path).collect();

        assert!(candidate_paths.contains(&reclaimable_archive_history));
        assert!(candidate_paths.contains(&reclaimable_merge_segment));
        assert!(candidate_paths.contains(&reclaimable_landing_drain));

        for protected in [
            protected_manifest,
            protected_heartbeat,
            protected_lock,
            protected_claim,
            protected_metadata,
            protected_text_meta,
            protected_lineage,
            protected_interner,
            protected_optimal,
            protected_planner,
            protected_archive_history,
            protected_checkpoint_history,
        ] {
            assert!(
                !candidate_paths.contains(&protected),
                "protected path should not be reclaimable: {}",
                protected.display()
            );
        }
    }
}
