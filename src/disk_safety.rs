use crate::{generation_store, memory_safety, metrics::Metrics, offload_config::OffloadConfig};
use std::cell::{Cell, RefCell};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use sysinfo::Disks;

const ARCHIVE_PAYLOAD_MAGIC: &[u8; 8] = b"FOLDRSLT";
const ARCHIVE_PAYLOAD_VERSION: u32 = 1;
const RECLAIM_IO_HEADROOM_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone)]
struct DiskSafetyContext {
    enabled: bool,
    base_dir: PathBuf,
    floor_bytes: Option<u64>,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum BundleStatus {
    Success,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ActiveReclaimClass {
    Spill,
    History,
    Runs,
    WorkSegment,
}

pub fn configure(base_dir: PathBuf, cfg: &OffloadConfig) {
    CTX.with(|slot| {
        *slot.borrow_mut() = Some(DiskSafetyContext {
            enabled: cfg.enabled,
            base_dir,
            floor_bytes: cfg.disk_free_low_water,
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

    let target_free = floor_bytes.saturating_add(bytes_needed);
    let free_before = available_space_for(&ctx.base_dir)?;
    if free_before >= target_free {
        return Ok(());
    }

    let rss_before = sync_process_rss_metrics();
    log(format!(
        "Disk gate: reason={}, bytes_needed={}, free_before={}, target_free={}, rss_before={}",
        reason, bytes_needed, free_before, target_free, rss_before
    ));
    reclaim_until(&ctx, target_free, reason)?;
    let free_after = available_space_for(&ctx.base_dir)?;
    let rss_after = sync_process_rss_metrics();
    if free_after < target_free {
        return Err(io::Error::other(format!(
            "disk safety denied write: reason={}, bytes_needed={}, free_before={}, free_after={}, floor={}, rss_before={}, rss_after={}",
            reason, bytes_needed, free_before, free_after, floor_bytes, rss_before, rss_after
        )));
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

    if available_space_for(&ctx.base_dir)? >= target_free {
        return Ok(());
    }

    prune_cache_to_cap(&ctx.cache_dir, ctx.cache_bytes_cap)?;
    if available_space_for(&ctx.base_dir)? >= target_free {
        return Ok(());
    }

    prune_bundle_root(&ctx.full_runs_dir)?;
    if available_space_for(&ctx.base_dir)? >= target_free {
        return Ok(());
    }
    prune_bundle_root(&ctx.doubling_runs_dir)?;
    if available_space_for(&ctx.base_dir)? >= target_free {
        return Ok(());
    }

    reclaim_cold_archives(ctx, target_free)?;
    if available_space_for(&ctx.base_dir)? >= target_free {
        return Ok(());
    }

    reclaim_active_store_files(ctx, target_free)?;
    if available_space_for(&ctx.base_dir)? >= target_free {
        return Ok(());
    }

    log(format!(
        "Disk reclaim exhausted without reaching target: reason={}, target_free={}",
        reason, target_free
    ));
    Ok(())
}

fn reclaim_cold_archives(ctx: &DiskSafetyContext, target_free: u64) -> io::Result<()> {
    let input_dir = ctx.base_dir.join("input");
    let mut archives = Vec::new();
    if input_dir.exists() {
        for entry in fs::read_dir(&input_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.extension().map(|ext| ext == "bin").unwrap_or(false)
                && archive_results_dir(&path).exists()
            {
                archives.push(file_info(&path)?);
            }
        }
    }
    archives.sort_by_key(|info| info.modified);

    for archive in archives {
        if available_space_for(&ctx.base_dir)? >= target_free {
            break;
        }
        match pack_and_offload_archive_results(&archive.path) {
            Ok(reclaimed) if reclaimed > 0 => log(format!(
                "Reclaimed cold archive results: {} bytes from {}",
                reclaimed,
                archive.path.display()
            )),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                log(format!(
                    "Skipped archive reclaim due to insufficient scratch space: {} ({})",
                    archive.path.display(),
                    err
                ));
            }
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

fn reclaim_active_store_files(ctx: &DiskSafetyContext, target_free: u64) -> io::Result<()> {
    let mut candidates = Vec::new();
    collect_active_store_candidates(&ctx.base_dir, &ctx.cache_dir, &mut candidates)?;
    candidates.sort_by(|a, b| {
        active_reclaim_class(&a.path)
            .cmp(&active_reclaim_class(&b.path))
            .then_with(|| {
                a.modified
                    .cmp(&b.modified)
                    .then_with(|| b.size.cmp(&a.size))
            })
    });

    for candidate in candidates {
        if available_space_for(&ctx.base_dir)? >= target_free {
            break;
        }
        if offload_and_delete(&candidate.path)? {
            log(format!(
                "Reclaimed active file: {} bytes from {}",
                candidate.size,
                candidate.path.display()
            ));
        }
    }

    Ok(())
}

fn active_reclaim_class(path: &Path) -> ActiveReclaimClass {
    if path
        .components()
        .any(|component| component.as_os_str() == "spill")
        && path.extension().map(|ext| ext == "dat").unwrap_or(false)
    {
        return ActiveReclaimClass::Spill;
    }

    if path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .starts_with("history")
    }) && path.extension().map(|ext| ext == "dat").unwrap_or(false)
    {
        return ActiveReclaimClass::History;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let parent_name = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("");

    if parent_name == "runs" && file_name.ends_with(".dat") {
        return ActiveReclaimClass::Runs;
    }

    if parent_name == "work" && file_name.starts_with("segment-") && file_name.ends_with(".dat") {
        return ActiveReclaimClass::WorkSegment;
    }

    ActiveReclaimClass::Runs
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
    if path
        .components()
        .any(|component| component.as_os_str() == "input")
    {
        return false;
    }
    if path
        .components()
        .any(|component| component.as_os_str() == "landing")
    {
        return false;
    }

    matches!(
        active_reclaim_class(path),
        ActiveReclaimClass::Spill
            | ActiveReclaimClass::History
            | ActiveReclaimClass::Runs
            | ActiveReclaimClass::WorkSegment
    )
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
    memory_safety::ensure_phase_headroom(
        RECLAIM_IO_HEADROOM_BYTES,
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
    use tempfile::TempDir;

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
    fn reclaim_prioritizes_spill_runs_before_other_active_files() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path().join("fold_state");
        let store_root = base.join("in_process").join("job.work");
        let spill_path = store_root.join("spill").join("b=00").join("spill-0.dat");
        let history_path = store_root
            .join("history")
            .join("b=00")
            .join("history-0.dat");
        let run_path = store_root.join("runs").join("unique-0.dat");
        let segment_path = store_root.join("work").join("segment-0.dat");

        for path in [&spill_path, &history_path, &run_path, &segment_path] {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, vec![7u8; 4 * 1024 * 1024]).unwrap();
        }

        let free_now = available_space_for(&base).unwrap();
        let mut cfg = OffloadConfig::with_base_dir(&base);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = base.join("offload_cache");
        cfg.disk_free_low_water = Some(free_now.saturating_add(512 * 1024));

        let _guard = configure_offload_runtime(&base, &cfg).unwrap().unwrap();
        configure(base.clone(), &cfg);

        let metrics = Metrics::new();
        set_metrics_handle(Some(metrics.clone_handle()));
        ensure_write_budget(1, "test reclaim ordering").unwrap();

        assert!(!spill_path.exists(), "spill file should be reclaimed first");
        assert!(history_path.exists());
        assert!(run_path.exists());
        assert!(segment_path.exists());

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.global.offloaded_files, 1);
        assert!(
            snapshot
                .logs
                .iter()
                .any(|log| log.message.contains("Disk reclaim offloaded"))
        );
        set_metrics_handle(None);
    }
}
