use crate::{
    FoldError, disk_safety, generation_store, interner::Interner, merge_resume, ortho::Ortho,
    stage_planner::PlannerMeta,
};
use std::collections::HashSet;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const HEARTBEAT_GRACE_PERIOD_SECS: u64 = 600; // 10 minutes
const MEM_CLAIM_STALE_GRACE_SECS: u64 = HEARTBEAT_GRACE_PERIOD_SECS;

/// Count words in text (whitespace-separated tokens)
fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Create text preview: first N words and last N words
fn create_text_preview(text: &str, first_n: usize, last_n: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();

    if words.len() <= first_n + last_n {
        return words.join(" ");
    }

    let first_words = words
        .iter()
        .take(first_n)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let last_words = words
        .iter()
        .rev()
        .take(last_n)
        .cloned()
        .rev()
        .collect::<Vec<_>>()
        .join(" ");

    format!("{} ... {}", first_words, last_words)
}

/// Configuration for state directory locations
#[derive(Clone, Debug)]
pub struct StateConfig {
    pub base_dir: PathBuf,
}

impl StateConfig {
    /// Default configuration for production use
    pub fn default() -> Self {
        Self {
            base_dir: PathBuf::from("./fold_state"),
        }
    }

    /// Custom configuration for tests
    pub fn custom(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn input_dir(&self) -> PathBuf {
        self.base_dir.join("input")
    }

    pub fn in_process_dir(&self) -> PathBuf {
        self.base_dir.join("in_process")
    }

    pub fn results_dir(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("results_{}", name))
    }

    pub fn mem_claims_dir(&self) -> PathBuf {
        self.base_dir.join("mem_claims")
    }

    pub fn logs_dir(&self) -> PathBuf {
        PathBuf::from("fold_history").join("logs")
    }
}

/// Initialize the file system: create directories and recover abandoned files
pub fn initialize() -> Result<(), FoldError> {
    initialize_with_config(&StateConfig::default())
}

/// Initialize with custom config (for tests)
pub fn initialize_with_config(config: &StateConfig) -> Result<(), FoldError> {
    let in_process = config.in_process_dir();
    let input = config.input_dir();
    ensure_directory_exists(in_process.to_str().unwrap())?;
    recover_abandoned_files(input.to_str().unwrap(), in_process.to_str().unwrap())?;
    Ok(())
}

/// Check for and recover any stale work from crashed processes
/// This should be called at the beginning of each worker loop iteration
pub fn check_and_recover_stale_work(config: &StateConfig) -> Result<(), FoldError> {
    recover_abandoned_files(
        config.input_dir().to_str().unwrap(),
        config.in_process_dir().to_str().unwrap(),
    )
}

fn load_valid_resume_manifest(merge_work_path: &Path) -> Option<merge_resume::MergeResumeManifest> {
    let manifest = merge_resume::read_manifest(merge_work_path).ok()?;
    merge_resume::validate_manifest(merge_work_path, &manifest, 8)
        .ok()
        .map(|_| manifest)
}

fn protected_archive_paths_for_resumable_merges(in_process_path: &Path) -> HashSet<PathBuf> {
    let _ = in_process_path;
    HashSet::new()
}

fn recover_abandoned_files(input_dir: &str, in_process_dir: &str) -> Result<(), FoldError> {
    let in_process_path = std::path::Path::new(in_process_dir);
    let input_path = std::path::Path::new(input_dir);

    if !in_process_path.exists() {
        return Ok(());
    }

    if !input_path.exists() {
        fs::create_dir_all(input_path).map_err(FoldError::Io)?;
    }

    let mut recovered_count = 0;
    let protected_archives = protected_archive_paths_for_resumable_merges(in_process_path);

    // Check for folders with stale heartbeats in in_process
    for entry in fs::read_dir(in_process_path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            let heartbeat_path = entry_path.join("heartbeat");

            // Check if this folder has a heartbeat file
            if heartbeat_path.exists() && is_heartbeat_stale(&heartbeat_path)? {
                // Check if this is a txt.work folder
                let source_txt_path = entry_path.join("source.txt");
                if source_txt_path.exists() {
                    // Recover the txt.work folder back to input as a plain txt file
                    if let Some(folder_name) = entry_path.file_name() {
                        let folder_name_str = folder_name.to_str().unwrap_or("recovered");
                        if folder_name_str.ends_with(".txt.work") {
                            let base_name = &folder_name_str[..folder_name_str.len() - 9]; // Remove ".txt.work"
                            let target_path = format!("{}/{}.txt", input_dir, base_name);
                            if Path::new(&target_path).exists() {
                                // Target already present; drop stale work folder instead of failing
                                let _ = fs::remove_dir_all(&entry_path);
                            } else {
                                fs::rename(&source_txt_path, &target_path)
                                    .map_err(|e| FoldError::Io(e))?;
                            }
                            // println!("[fold] Recovered abandoned txt file: {} -> {}", folder_name_str, target_path);

                            // Delete the results_{filename} directory if it exists
                            let base_dir = Path::new(in_process_dir)
                                .parent()
                                .unwrap_or_else(|| Path::new("."));
                            let results_txt_path = base_dir.join(format!("results_{}", base_name));
                            if results_txt_path.exists() {
                                // println!("[fold] Removing partial txt processing results: {:?}", results_txt_path);
                                let _ = fs::remove_dir_all(&results_txt_path);
                            }

                            // Remove the abandoned work folder (contains queue/ and seen_shards/)
                            fs::remove_dir_all(&entry_path).map_err(|e| FoldError::Io(e))?;
                            recovered_count += 1;
                        }
                    }
                }
                // Check other folders with stale heartbeats
                else if let Some(folder_name) = entry_path.file_name() {
                    let folder_name_str = folder_name.to_str().unwrap_or("");
                    // If it's a merge_*.work folder with stale heartbeat, recover the merge
                    if folder_name_str.starts_with("merge_") && folder_name_str.ends_with(".work") {
                        // Extract PID to find related archives and results
                        if let Some(pid_str) = folder_name_str
                            .strip_prefix("merge_")
                            .and_then(|s| s.strip_suffix(".work"))
                        {
                            // Find and recover the two archive folders back to input
                            // They should be in in_process as .bin folders
                            if let Ok(entries) = fs::read_dir(in_process_path) {
                                for bin_entry in entries {
                                    if let Ok(bin_entry) = bin_entry {
                                        let bin_path = bin_entry.path();
                                        if bin_path.is_dir()
                                            && bin_path
                                                .extension()
                                                .map(|e| e == "bin")
                                                .unwrap_or(false)
                                        {
                                            if protected_archives.contains(&bin_path) {
                                                continue;
                                            }
                                            // Remove heartbeat from archive
                                            let archive_heartbeat = bin_path.join("heartbeat");
                                            if archive_heartbeat.exists() {
                                                let _ = fs::remove_file(&archive_heartbeat);
                                            }

                                            // Move archive back to input unless a live copy already exists there.
                                            let archive_name = bin_path.file_name().unwrap();
                                            let input_path =
                                                Path::new(input_dir).join(archive_name);
                                            if input_path.exists() {
                                                // Target already restored or backed up; drop stale work copy
                                                let _ = fs::remove_dir_all(&bin_path);
                                            } else {
                                                // println!("[fold] Recovering merge archive: {:?} -> {:?}", bin_path, input_path);
                                                let _ = fs::rename(&bin_path, &input_path);
                                            }
                                        }
                                    }
                                }
                            }

                            // Delete the results_merged_{pid} directory if it exists
                            let base_dir = Path::new(in_process_dir)
                                .parent()
                                .unwrap_or_else(|| Path::new("."));
                            let results_merged_path =
                                base_dir.join(format!("results_merged_{}", pid_str));
                            if results_merged_path.exists() {
                                // println!("[fold] Removing partial merge results: {:?}", results_merged_path);
                                let _ = fs::remove_dir_all(&results_merged_path);
                            }
                        }

                        // Delete the merge work folder (contains queue/ and seen_shards/)
                        // println!("[fold] Removing abandoned merge work folder: {}", folder_name_str);
                        fs::remove_dir_all(&entry_path).map_err(|e| FoldError::Io(e))?;
                        recovered_count += 1;
                    }
                    // If it's an archive folder with stale heartbeat (orphaned from failed merge recovery), move it back
                    else if entry_path.to_string_lossy().ends_with(".bin") {
                        if protected_archives.contains(&entry_path) {
                            continue;
                        }
                        // Remove the heartbeat before moving to input
                        let heartbeat_to_remove = entry_path.join("heartbeat");
                        if heartbeat_to_remove.exists() {
                            fs::remove_file(&heartbeat_to_remove).map_err(|e| FoldError::Io(e))?;
                        }

                        let archive_name = entry_path.file_name().unwrap();
                        let input_path = Path::new(input_dir).join(archive_name);
                        if input_path.exists() {
                            // Target already present (e.g., user staged input). Drop stale in_process copy.
                            let _ = fs::remove_dir_all(&entry_path);
                        } else {
                            // println!("[fold] Recovering orphaned archive: {:?} -> {:?}", entry_path, input_path);
                            fs::rename(&entry_path, &input_path).map_err(|e| FoldError::Io(e))?;
                        }
                        recovered_count += 1;
                    }
                }
            }
        }
    }

    // Second pass: recover orphaned archives in in_process that have no heartbeat.
    // These are most likely leftover from interrupted moves before heartbeat creation.
    if in_process_path.exists() {
        for entry in fs::read_dir(in_process_path).map_err(|e| FoldError::Io(e))? {
            let entry = entry.map_err(|e| FoldError::Io(e))?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                if let Some(ext) = entry_path.extension() {
                    if ext == "bin" {
                        if protected_archives.contains(&entry_path) {
                            continue;
                        }
                        let heartbeat = entry_path.join("heartbeat");
                        if !heartbeat.exists() {
                            let archive_name = entry_path.file_name().unwrap();
                            let input_target = Path::new(input_dir).join(archive_name);
                            if input_target.exists() {
                                // Input or backup already present; drop the orphaned copy.
                                let _ = fs::remove_dir_all(&entry_path);
                            } else {
                                let _ = fs::rename(&entry_path, &input_target);
                            }
                        }
                    }
                }
            }
        }
    }

    // Clean up orphaned results_merged_* directories in the parent directory
    // Only delete if the corresponding merge_*.work folder doesn't exist or has stale heartbeat
    let base_dir = Path::new(in_process_dir)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    if base_dir.exists() {
        for entry in fs::read_dir(base_dir).map_err(|e| FoldError::Io(e))? {
            let entry = entry.map_err(|e| FoldError::Io(e))?;
            let entry_path = entry.path();

            if entry_path.is_dir() {
                if let Some(folder_name) = entry_path.file_name() {
                    let folder_name_str = folder_name.to_str().unwrap_or("");
                    if folder_name_str.starts_with("results_merged_") {
                        // Extract PID from results_merged_{pid}
                        if let Some(pid_str) = folder_name_str.strip_prefix("results_merged_") {
                            // Check if corresponding merge_*.work folder exists with fresh heartbeat
                            let merge_work_name = format!("merge_{}.work", pid_str);
                            let merge_work_path = in_process_path.join(&merge_work_name);
                            let merge_heartbeat = merge_work_path.join("heartbeat");

                            // Only delete if merge work doesn't exist or has stale heartbeat
                            let should_delete = !merge_work_path.exists()
                                || !merge_heartbeat.exists()
                                || is_heartbeat_stale(&merge_heartbeat).unwrap_or(true);

                            if should_delete {
                                // println!("[fold] Removing orphaned results directory: {}", folder_name_str);
                                fs::remove_dir_all(&entry_path).map_err(|e| FoldError::Io(e))?;
                                recovered_count += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    if recovered_count > 0 {
        // println!("[fold] Recovered {} abandoned file(s) from previous run", recovered_count);
    }

    Ok(())
}

fn is_heartbeat_stale(heartbeat_path: &Path) -> Result<bool, FoldError> {
    let now = SystemTime::now();
    let metadata = fs::metadata(heartbeat_path).map_err(|e| FoldError::Io(e))?;
    let modified = metadata.modified().map_err(|e| FoldError::Io(e))?;

    // If the file's modification time is beyond the grace period, treat as stale.
    if let Ok(duration) = now.duration_since(modified) {
        if duration.as_secs() > HEARTBEAT_GRACE_PERIOD_SECS {
            return Ok(true);
        }
    }

    // First, attempt to parse the heartbeat contents (timestamp and pid)
    if let Ok(contents) = fs::read_to_string(heartbeat_path) {
        let mut parts = contents
            .split(|c| c == ':' || c == '\n' || c == ' ')
            .filter(|s| !s.is_empty());

        let timestamp_secs = parts.next().and_then(|s| s.parse::<u64>().ok());

        // Fall back to timestamp-based staleness (new format uses timestamp too)
        if let Some(ts) = timestamp_secs {
            if let Ok(duration) =
                now.duration_since(UNIX_EPOCH + std::time::Duration::from_secs(ts))
            {
                if duration.as_secs() > HEARTBEAT_GRACE_PERIOD_SECS {
                    return Ok(true);
                }
            }
        }
    }

    // Compatibility fallback: use file modification time if contents are unreadable/legacy
    Ok(false)
}

fn touch_heartbeat(heartbeat_path: &str) -> Result<(), FoldError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .as_secs();
    let pid = std::process::id();
    let content = format!("{}:{}", now, pid);

    fs::write(heartbeat_path, content.as_bytes()).map_err(|e| FoldError::Io(e))?;
    Ok(())
}

pub fn touch_heartbeat_file(heartbeat_path: &str) -> Result<(), FoldError> {
    touch_heartbeat(heartbeat_path)
}

pub fn is_heartbeat_file_stale(heartbeat_path: &Path) -> Result<bool, FoldError> {
    is_heartbeat_stale(heartbeat_path)
}

fn create_heartbeat(work_folder_path: &str) -> Result<String, FoldError> {
    // Heartbeat is now inside the work folder
    let heartbeat_path = format!("{}/heartbeat", work_folder_path);
    touch_heartbeat(&heartbeat_path)?;
    Ok(heartbeat_path)
}

#[derive(Debug, Clone)]
pub struct MemClaimRecord {
    pub pid: u32,
    pub role: String,
    pub requested_bytes: usize,
    pub granted_bytes: usize,
    pub timestamp_secs: u64,
}

pub struct MemClaimGuard {
    record: MemClaimRecord,
    path: String,
}

impl MemClaimGuard {
    pub fn touch(&self) -> Result<(), FoldError> {
        write_mem_claim_file(&self.path, &self.record, Some(current_timestamp_secs()?))
    }

    pub fn release(self) -> Result<(), FoldError> {
        let _ = fs::remove_file(&self.path);
        Ok(())
    }

    pub fn granted_bytes(&self) -> usize {
        self.record.granted_bytes
    }

    pub fn requested_bytes(&self) -> usize {
        self.record.requested_bytes
    }
}

impl Drop for MemClaimGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn current_timestamp_secs() -> Result<u64, FoldError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .as_secs())
}

fn mem_claim_path(config: &StateConfig, pid: u32) -> PathBuf {
    config.mem_claims_dir().join(format!("{}.claim", pid))
}

fn write_mem_claim_file(
    path: &str,
    record: &MemClaimRecord,
    ts_override: Option<u64>,
) -> Result<(), FoldError> {
    let timestamp = ts_override.unwrap_or(record.timestamp_secs);
    let content = format!(
        "pid:{}\nrole:{}\nrequested:{}\ngranted:{}\ntimestamp:{}\n",
        record.pid, record.role, record.requested_bytes, record.granted_bytes, timestamp
    );
    fs::write(path, content).map_err(FoldError::Io)
}

fn parse_mem_claim(path: &Path) -> Option<MemClaimRecord> {
    let content = fs::read_to_string(path).ok()?;
    let mut pid = None;
    let mut role = None;
    let mut requested_bytes = None;
    let mut granted_bytes = None;
    let mut timestamp_secs = None;

    for line in content.lines() {
        let mut parts = line.splitn(2, ':');
        let key = parts.next()?;
        let value = parts.next().unwrap_or("");
        match key {
            "pid" => pid = value.parse().ok(),
            "role" => role = Some(value.to_string()),
            "requested" => requested_bytes = value.parse().ok(),
            "granted" => granted_bytes = value.parse().ok(),
            "timestamp" => timestamp_secs = value.parse().ok(),
            _ => {}
        }
    }

    Some(MemClaimRecord {
        pid: pid?,
        role: role?,
        requested_bytes: requested_bytes?,
        granted_bytes: granted_bytes?,
        timestamp_secs: timestamp_secs?,
    })
}

fn is_claim_stale(path: &Path) -> bool {
    if let Ok(metadata) = fs::metadata(path) {
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = SystemTime::now().duration_since(modified) {
                if duration.as_secs() > MEM_CLAIM_STALE_GRACE_SECS {
                    return true;
                }
            }
        }
    }
    false
}

pub fn load_active_mem_claims(config: &StateConfig) -> Result<Vec<MemClaimRecord>, FoldError> {
    let mut claims = Vec::new();
    let claims_dir = config.mem_claims_dir();
    fs::create_dir_all(&claims_dir).map_err(FoldError::Io)?;

    for entry in fs::read_dir(&claims_dir).map_err(FoldError::Io)? {
        let entry = entry.map_err(FoldError::Io)?;
        let path = entry.path();
        if path.is_file() && path.extension().map(|ext| ext == "claim").unwrap_or(false) {
            if is_claim_stale(&path) {
                let _ = fs::remove_file(&path);
                continue;
            }

            if let Some(record) = parse_mem_claim(&path) {
                claims.push(record);
            } else {
                // Corrupt claim files should not block work
                let _ = fs::remove_file(&path);
            }
        }
    }

    Ok(claims)
}

pub fn cleanup_stale_mem_claims(config: &StateConfig) -> Result<(), FoldError> {
    let claims_dir = config.mem_claims_dir();
    fs::create_dir_all(&claims_dir).map_err(FoldError::Io)?;

    for entry in fs::read_dir(&claims_dir).map_err(FoldError::Io)? {
        let entry = entry.map_err(FoldError::Io)?;
        let path = entry.path();
        if path.is_file() && path.extension().map(|ext| ext == "claim").unwrap_or(false) {
            if is_claim_stale(&path) {
                let _ = fs::remove_file(&path);
            }
        }
    }
    Ok(())
}

pub fn create_mem_claim(
    config: &StateConfig,
    role: &str,
    requested_bytes: usize,
    granted_bytes: usize,
) -> Result<MemClaimGuard, FoldError> {
    let claims_dir = config.mem_claims_dir();
    fs::create_dir_all(&claims_dir).map_err(FoldError::Io)?;
    let pid = std::process::id();
    let timestamp_secs = current_timestamp_secs()?;
    let record = MemClaimRecord {
        pid,
        role: role.to_string(),
        requested_bytes,
        granted_bytes,
        timestamp_secs,
    };
    let path = mem_claim_path(config, pid);
    // Write via create_new to avoid clobbering if stale file remains; remove stale before overwriting
    let result = OpenOptions::new().write(true).create_new(true).open(&path);
    match result {
        Ok(_) => write_mem_claim_file(path.to_str().unwrap(), &record, Some(timestamp_secs))?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&path);
            write_mem_claim_file(path.to_str().unwrap(), &record, Some(timestamp_secs))?;
        }
        Err(e) => return Err(FoldError::Io(e)),
    };

    Ok(MemClaimGuard {
        record,
        path: path.to_string_lossy().to_string(),
    })
}

fn count_txt_files(input_dir: &str) -> Result<usize, FoldError> {
    let path = std::path::Path::new(input_dir);

    if !path.exists() {
        return Ok(0);
    }

    let mut count = 0;
    for entry in fs::read_dir(path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();

        if entry_path.is_file() {
            if let Some(ext) = entry_path.extension() {
                if ext == "txt" {
                    count += 1;
                }
            }
        }
    }

    Ok(count)
}

fn find_next_txt_file(input_dir: &str, in_process_dir: &str) -> Result<Option<String>, FoldError> {
    let path = std::path::Path::new(input_dir);

    if !path.exists() {
        fs::create_dir_all(path).map_err(|e| FoldError::Io(e))?;
        return Ok(None);
    }

    let mut largest_file: Option<(String, u64)> = None;

    for entry in fs::read_dir(path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();

        // Look for plain .txt files in input directory
        if entry_path.is_file() {
            if let Some(ext) = entry_path.extension() {
                if ext == "txt" {
                    // Skip if there's an active .txt.work folder with fresh heartbeat for this file
                    if let Some(stem) = entry_path.file_stem().and_then(|s| s.to_str()) {
                        let work_folder =
                            Path::new(in_process_dir).join(format!("{}.txt.work", stem));
                        let heartbeat = work_folder.join("heartbeat");
                        if heartbeat.exists() {
                            let stale = is_heartbeat_stale(&heartbeat).unwrap_or(false);
                            if !stale {
                                continue;
                            }
                        }
                    }

                    if let Some(path_str) = entry_path.to_str() {
                        if let Ok(metadata) = entry_path.metadata() {
                            let size = metadata.len();
                            if let Some((_, current_largest_size)) = largest_file {
                                if size > current_largest_size {
                                    largest_file = Some((path_str.to_string(), size));
                                }
                            } else {
                                largest_file = Some((path_str.to_string(), size));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(largest_file.map(|(path, _)| path))
}

fn find_archives(input_dir: &str) -> Result<Vec<(String, u64)>, FoldError> {
    let path = std::path::Path::new(input_dir);

    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut archives = Vec::new();

    for entry in fs::read_dir(path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            // Check if this is a .bin archive directory (archives in input don't have heartbeats yet)
            if let Some(ext) = entry_path.extension() {
                if ext == "bin" {
                    // Get size of results directory
                    let results_path = entry_path.join("results");
                    if results_path.exists() && results_path.is_dir() {
                        // Calculate total size of results directory
                        let mut total_size = 0u64;
                        if let Ok(entries) = fs::read_dir(&results_path) {
                            for result_entry in entries {
                                if let Ok(result_entry) = result_entry {
                                    if let Ok(metadata) = result_entry.metadata() {
                                        total_size += metadata.len();
                                    }
                                }
                            }

                            if let Some(path_str) = entry_path.to_str() {
                                archives.push((path_str.to_string(), total_size));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(archives)
}

/// Load interner from an archive
pub fn load_interner(archive_path: &str) -> Result<Interner, FoldError> {
    let interner_bytes = read_archive_artifact_bytes(archive_path, "interner.bin")?;
    Interner::from_bytes(&interner_bytes)
}

fn get_results_path(archive_path: &str) -> String {
    format!("{}/results", archive_path)
}

fn resolve_archive_artifact_path(
    archive_path: &str,
    relative_path: &str,
) -> Result<PathBuf, FoldError> {
    let path = Path::new(archive_path).join(relative_path);
    generation_store::resolve_managed_path(&path).map_err(FoldError::Io)
}

fn read_archive_artifact_bytes(
    archive_path: &str,
    relative_path: &str,
) -> Result<Vec<u8>, FoldError> {
    let path = resolve_archive_artifact_path(archive_path, relative_path)?;
    fs::read(path).map_err(FoldError::Io)
}

fn read_archive_artifact_string(
    archive_path: &str,
    relative_path: &str,
) -> Result<String, FoldError> {
    let path = resolve_archive_artifact_path(archive_path, relative_path)?;
    fs::read_to_string(path).map_err(FoldError::Io)
}

fn load_lineage(archive_path: &str) -> Result<String, FoldError> {
    read_archive_artifact_string(archive_path, "lineage.txt")
}

fn load_metadata(archive_path: &str) -> Result<usize, FoldError> {
    let content = read_archive_artifact_string(archive_path, "metadata.txt")?;
    content
        .trim()
        .parse::<usize>()
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

/// Load text metadata (word count and preview) from archive
/// Returns (word_count, text_preview)
fn load_text_metadata(archive_path: &str) -> Result<(usize, String), FoldError> {
    let content = read_archive_artifact_string(archive_path, "text_meta.txt")?;
    let mut lines = content.lines();

    let word_count = lines
        .next()
        .ok_or_else(|| {
            FoldError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Missing word count",
            ))
        })?
        .parse::<usize>()
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;

    let text_preview = lines
        .next()
        .ok_or_else(|| {
            FoldError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Missing text preview",
            ))
        })?
        .to_string();

    Ok((word_count, text_preview))
}

/// Ensures a directory exists by creating it if needed
fn ensure_directory_exists(path: &str) -> Result<(), FoldError> {
    fs::create_dir_all(path).map_err(|e| FoldError::Io(e))
}

/// Sets up processing for a txt file: creates work folder, moves file to source.txt
/// Returns (work_folder_path, source_txt_path, heartbeat_path, filename)
fn setup_txt_processing(
    file_path: &str,
    in_process_dir: &str,
) -> Result<(String, String, String, String), FoldError> {
    // Extract filename from path
    let filename = Path::new(file_path).file_stem().unwrap_or_default();
    let filename_str = filename.to_str().unwrap_or("temp").to_string();
    let work_folder = format!("{}/{}.txt.work", in_process_dir, &filename_str);

    // Create work folder
    fs::create_dir_all(&work_folder).map_err(|e| FoldError::Io(e))?;

    // Move txt file to source.txt inside work folder
    let source_txt_path = format!("{}/source.txt", work_folder);
    fs::rename(file_path, &source_txt_path).map_err(|e| FoldError::Io(e))?;

    // Create heartbeat file inside work folder
    let heartbeat_path = create_heartbeat(&work_folder)?;

    Ok((work_folder, source_txt_path, heartbeat_path, filename_str))
}

/// Reads the text content from source.txt in a work folder
fn read_source_text(source_txt_path: &str) -> Result<String, FoldError> {
    fs::read_to_string(source_txt_path).map_err(|e| FoldError::Io(e))
}

/// Cleans up a txt processing work folder by removing it entirely
/// This removes the work folder which contains: source.txt, heartbeat, queue/, seen_shards/
fn cleanup_txt_processing(work_folder: &str) -> Result<(), FoldError> {
    fs::remove_dir_all(work_folder).map_err(|e| FoldError::Io(e))
}

/// Sets up archive merging by moving archives to in_process directory
/// Returns (work_path_a, work_path_b)
fn setup_archive_merge(
    archive_a_path: &str,
    archive_b_path: &str,
    in_process_dir: &str,
) -> Result<(String, String), FoldError> {
    let archive_a_name = Path::new(archive_a_path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let archive_b_name = Path::new(archive_b_path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let work_a_path = format!("{}/{}", in_process_dir, archive_a_name);
    let work_b_path = format!("{}/{}", in_process_dir, archive_b_name);

    fs::rename(archive_a_path, &work_a_path).map_err(FoldError::Io)?;
    fs::rename(archive_b_path, &work_b_path).map_err(FoldError::Io)?;

    // Create heartbeats for archives now that they're in_process
    touch_heartbeat(&format!("{}/heartbeat", work_a_path))?;
    touch_heartbeat(&format!("{}/heartbeat", work_b_path))?;

    Ok((work_a_path, work_b_path))
}

/// Cleans up archives by removing them if they exist
fn cleanup_archives(archive_paths: &[&str]) -> Result<(), FoldError> {
    for archive_path in archive_paths {
        if Path::new(archive_path).exists() {
            fs::remove_dir_all(archive_path).map_err(|e| FoldError::Io(e))?;
        }
    }
    Ok(())
}

// ============================================================================
// High-level API - encapsulates directory paths and provides clean operations
// ============================================================================

/// Result of ingesting a text file, containing text and metadata for processing
pub struct TxtIngestion {
    work_folder: String,
    heartbeat_path: String,
    pub filename: String,
    pub text: String,
    pub text_preview: String,
    pub word_count: usize,
    config: StateConfig,
}

impl TxtIngestion {
    /// Touch the heartbeat file (zero-arity as requested)
    pub fn touch_heartbeat(&self) -> Result<(), FoldError> {
        touch_heartbeat(&self.heartbeat_path)
    }

    /// Get the results path for this ingestion
    pub fn results_path(&self) -> String {
        self.config
            .results_dir(&self.filename)
            .to_string_lossy()
            .to_string()
    }

    /// Get the work queue path for this ingestion (isolated per file)
    pub fn work_queue_path(&self) -> String {
        format!("{}/queue", self.work_folder)
    }

    /// Get the seen shards path for this ingestion (isolated per file)
    pub fn seen_shards_path(&self) -> String {
        format!("{}/seen_shards", self.work_folder)
    }

    /// Get the config for this ingestion
    pub fn config(&self) -> &StateConfig {
        &self.config
    }

    /// Cleanup work folder after processing
    pub fn cleanup(self) -> Result<(), FoldError> {
        cleanup_txt_processing(&self.work_folder)
    }
}

/// Result of ingesting archives for merging
pub struct ArchiveIngestion {
    work_a_path: String,
    work_b_path: String,
    merge_work_folder: String,
    heartbeat_path: String,
    pub text_preview_a: String,
    pub text_preview_b: String,
    pub word_count_a: usize,
    pub word_count_b: usize,
    config: StateConfig,
}

#[derive(Clone, Debug)]
pub struct ResumableMergeClaim {
    pub merge_work_folder: String,
    pub heartbeat_path: String,
    pub archive_a_path: String,
    pub archive_b_path: String,
    pub text_preview_a: String,
    pub text_preview_b: String,
    pub word_count_a: usize,
    pub word_count_b: usize,
    pub manifest: merge_resume::MergeResumeManifest,
    config: StateConfig,
}

impl ArchiveIngestion {
    /// Touch the heartbeat file (zero-arity as requested)
    pub fn touch_heartbeat(&self) -> Result<(), FoldError> {
        touch_heartbeat(&self.heartbeat_path)
    }

    /// Load both interners
    pub fn load_interners(&self) -> Result<(Interner, Interner), FoldError> {
        let interner_a = load_interner(&self.work_a_path)?;
        let interner_b = load_interner(&self.work_b_path)?;
        Ok((interner_a, interner_b))
    }

    /// Load lineages from both archives
    pub fn load_lineages(&self) -> Result<(String, String), FoldError> {
        let lineage_a = load_lineage(&self.work_a_path)?;
        let lineage_b = load_lineage(&self.work_b_path)?;
        Ok((lineage_a, lineage_b))
    }

    /// Get results paths for both archives
    pub fn get_results_paths(&self) -> (String, String) {
        (
            get_results_path(&self.work_a_path),
            get_results_path(&self.work_b_path),
        )
    }

    /// Get the work queue path for this merge (isolated to merge work folder)
    pub fn work_queue_path(&self) -> String {
        format!("{}/queue", self.merge_work_folder)
    }

    /// Get the seen shards path for this merge (isolated to merge work folder)
    pub fn seen_shards_path(&self) -> String {
        format!("{}/seen_shards", self.merge_work_folder)
    }

    pub fn merge_work_folder(&self) -> &str {
        &self.merge_work_folder
    }

    pub fn archive_paths(&self) -> (&str, &str) {
        (&self.work_a_path, &self.work_b_path)
    }

    /// Get the config for this ingestion
    pub fn config(&self) -> &StateConfig {
        &self.config
    }

    /// Cleanup original archives and merge work folder
    pub fn cleanup(self) -> Result<(), FoldError> {
        // Clean up merge work folder (contains queue/, seen_shards/, heartbeat)
        if Path::new(&self.merge_work_folder).exists() {
            fs::remove_dir_all(&self.merge_work_folder).map_err(FoldError::Io)?;
        }
        // Clean up the work paths (archives in in_process), not the original paths
        cleanup_archives(&[&self.work_a_path, &self.work_b_path])
    }
}

impl ResumableMergeClaim {
    pub fn config(&self) -> &StateConfig {
        &self.config
    }
}

/// Count remaining text files in input (uses default config)
pub fn count_txt_files_remaining() -> Result<usize, FoldError> {
    count_txt_files_remaining_with_config(&StateConfig::default())
}

/// Count remaining text files in input with custom config
pub fn count_txt_files_remaining_with_config(config: &StateConfig) -> Result<usize, FoldError> {
    count_txt_files(config.input_dir().to_str().unwrap())
}

/// Count all chunks (txt + archives) including those currently in_process (uses default config)
pub fn count_all_chunks() -> Result<usize, FoldError> {
    count_all_chunks_with_config(&StateConfig::default())
}

/// Count all chunks (txt + archives) including those currently in_process
pub fn count_all_chunks_with_config(config: &StateConfig) -> Result<usize, FoldError> {
    let input_dir = config.input_dir();
    let in_process_dir = config.in_process_dir();

    // Count txt files waiting in input
    let mut total = count_txt_files(input_dir.to_str().unwrap())?;

    // Count archives waiting in input
    total = total.saturating_add(find_archives(input_dir.to_str().unwrap())?.len());

    // Count work that is currently checked out in in_process (txt.work and .bin)
    if in_process_dir.exists() {
        for entry in fs::read_dir(&in_process_dir).map_err(|e| FoldError::Io(e))? {
            let entry = entry.map_err(|e| FoldError::Io(e))?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.ends_with(".txt.work") {
                total = total.saturating_add(1);
                continue;
            }

            if path.extension().map(|ext| ext == "bin").unwrap_or(false) {
                total = total.saturating_add(1);
            }
        }
    }

    Ok(total)
}

/// Count distinct running jobs in the in_process folder
pub fn count_running_jobs_with_config(config: &StateConfig) -> Result<usize, FoldError> {
    let in_process_path = config.in_process_dir();

    if !in_process_path.exists() {
        return Ok(0);
    }

    let mut job_count = 0;

    for entry in fs::read_dir(&in_process_path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();

        if entry_path.is_dir() {
            let heartbeat_path = entry_path.join("heartbeat");

            if heartbeat_path.exists() {
                if let Some(folder_name) = entry_path.file_name() {
                    let folder_name_str = folder_name.to_str().unwrap_or("");

                    // Count txt.work and merge.work folders (these are distinct jobs)
                    if folder_name_str.ends_with(".txt.work")
                        || (folder_name_str.starts_with("merge_")
                            && folder_name_str.ends_with(".work"))
                    {
                        job_count += 1;
                    }
                }
            }
        }
    }

    Ok(job_count)
}

/// Find the next text file to process (uses default config)
pub fn find_txt_file() -> Result<Option<String>, FoldError> {
    find_txt_file_with_config(&StateConfig::default())
}

/// Find the next text file to process with custom config
pub fn find_txt_file_with_config(config: &StateConfig) -> Result<Option<String>, FoldError> {
    find_next_txt_file(
        config.input_dir().to_str().unwrap(),
        config.in_process_dir().to_str().unwrap(),
    )
}

pub fn claim_resumable_merge_with_config(
    config: &StateConfig,
) -> Result<Option<ResumableMergeClaim>, FoldError> {
    let in_process = config.in_process_dir();
    if !in_process.exists() {
        return Ok(None);
    }

    let mut merge_dirs: Vec<PathBuf> = fs::read_dir(&in_process)
        .map_err(FoldError::Io)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_dir())
        .filter(|path| {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            name.starts_with("merge_") && name.ends_with(".work")
        })
        .collect();
    merge_dirs.sort();

    for merge_work_path in merge_dirs {
        let heartbeat = merge_work_path.join("heartbeat");
        let stale_or_missing = !heartbeat.exists() || is_heartbeat_stale(&heartbeat)?;
        if !stale_or_missing {
            continue;
        }

        let Some(manifest) = load_valid_resume_manifest(&merge_work_path) else {
            continue;
        };
        if manifest.phase == merge_resume::ResumePhase::Archived {
            continue;
        }

        touch_heartbeat(heartbeat.to_str().unwrap())?;
        touch_heartbeat(&format!("{}/heartbeat", manifest.archive_a_path))?;
        touch_heartbeat(&format!("{}/heartbeat", manifest.archive_b_path))?;
        disk_safety::ensure_archive_results_local(Path::new(&manifest.archive_a_path))
            .map_err(FoldError::Io)?;
        disk_safety::ensure_archive_results_local(Path::new(&manifest.archive_b_path))
            .map_err(FoldError::Io)?;

        let (word_count_a_orig, text_preview_a_orig) =
            load_text_metadata(&manifest.archive_a_path).unwrap_or_else(|_| (0, String::new()));
        let (word_count_b_orig, text_preview_b_orig) =
            load_text_metadata(&manifest.archive_b_path).unwrap_or_else(|_| (0, String::new()));

        return Ok(Some(ResumableMergeClaim {
            merge_work_folder: merge_work_path.to_string_lossy().to_string(),
            heartbeat_path: heartbeat.to_string_lossy().to_string(),
            archive_a_path: manifest.archive_a_path.clone(),
            archive_b_path: manifest.archive_b_path.clone(),
            text_preview_a: if word_count_a_orig > 0 {
                create_text_preview(&text_preview_a_orig, 2, 2)
            } else {
                String::new()
            },
            text_preview_b: if word_count_b_orig > 0 {
                create_text_preview(&text_preview_b_orig, 2, 2)
            } else {
                String::new()
            },
            word_count_a: word_count_a_orig,
            word_count_b: word_count_b_orig,
            manifest,
            config: config.clone(),
        }));
    }

    Ok(None)
}

/// Get the two largest archives (uses default config)
pub fn get_two_largest_archives() -> Result<Option<(String, String)>, FoldError> {
    get_two_largest_archives_with_config(&StateConfig::default())
}

/// Get the two smallest archives (uses default config)
pub fn get_two_smallest_archives() -> Result<Option<(String, String)>, FoldError> {
    get_two_smallest_archives_with_config(&StateConfig::default())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchivePairPolicy {
    LargestLargest,
    SmallestSmallest,
    LargestSmallest,
    AdjacentBalanced,
}

impl ArchivePairPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LargestLargest => "largest_largest",
            Self::SmallestSmallest => "smallest_smallest",
            Self::LargestSmallest => "largest_smallest",
            Self::AdjacentBalanced => "adjacent_balanced",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchivePairSelection {
    pub pair: Option<(String, String)>,
    pub effective_policy: ArchivePairPolicy,
    pub warning: Option<String>,
    pub selection_log: Option<String>,
}

#[derive(Clone, Debug)]
struct ArchivePlannerRecord {
    path: String,
    ortho_count: usize,
    planner_meta: Option<PlannerMeta>,
}

fn select_archive_pair_from_counts(
    archives_with_counts: &[(String, usize)],
    policy: ArchivePairPolicy,
) -> Option<(String, String)> {
    if archives_with_counts.len() < 2 {
        return None;
    }

    let mut sorted = archives_with_counts.to_vec();
    sorted.sort_by_key(|(_, count)| *count);

    match policy {
        ArchivePairPolicy::LargestLargest => Some((
            sorted[sorted.len() - 2].0.clone(),
            sorted[sorted.len() - 1].0.clone(),
        )),
        ArchivePairPolicy::SmallestSmallest => Some((sorted[0].0.clone(), sorted[1].0.clone())),
        ArchivePairPolicy::LargestSmallest => {
            Some((sorted[0].0.clone(), sorted[sorted.len() - 1].0.clone()))
        }
        ArchivePairPolicy::AdjacentBalanced => None,
    }
}

fn adjacent_priority_tuple(
    left: &PlannerMeta,
    right: &PlannerMeta,
) -> (usize, usize, u128, u64, usize) {
    let left_cost = left.planner_cost.max(1);
    let right_cost = right.planner_cost.max(1);
    let cost_imbalance = if left_cost >= right_cost {
        (left_cost as u128) * 1_000 / (right_cost as u128)
    } else {
        (right_cost as u128) * 1_000 / (left_cost as u128)
    };

    (
        left.merge_level.max(right.merge_level),
        left.merge_level.abs_diff(right.merge_level),
        cost_imbalance,
        left.planner_cost.saturating_add(right.planner_cost),
        left.range_start,
    )
}

fn select_adjacent_balanced_pair(records: &[ArchivePlannerRecord]) -> ArchivePairSelection {
    if records.len() < 2 {
        return ArchivePairSelection {
            pair: None,
            effective_policy: ArchivePairPolicy::AdjacentBalanced,
            warning: None,
            selection_log: None,
        };
    }

    if records.iter().any(|record| record.planner_meta.is_none()) {
        let pair = select_archive_pair_from_counts(
            &records
                .iter()
                .map(|record| (record.path.clone(), record.ortho_count))
                .collect::<Vec<_>>(),
            ArchivePairPolicy::SmallestSmallest,
        );
        return ArchivePairSelection {
            pair,
            effective_policy: ArchivePairPolicy::SmallestSmallest,
            warning: Some(
                "adjacent_balanced fallback: missing planner_meta.json; using smallest_smallest for this selection cycle"
                    .to_string(),
            ),
            selection_log: None,
        };
    }

    let mut sorted = records.to_vec();
    sorted.sort_by_key(|record| {
        record
            .planner_meta
            .as_ref()
            .map(|meta| meta.range_start)
            .unwrap_or(usize::MAX)
    });

    let mut best_index = None;
    let mut best_priority = None;

    for index in 0..sorted.len().saturating_sub(1) {
        let left = sorted[index].planner_meta.as_ref().unwrap();
        let right = sorted[index + 1].planner_meta.as_ref().unwrap();
        if left.range_end_exclusive != right.range_start {
            continue;
        }

        let priority = adjacent_priority_tuple(left, right);
        if best_priority
            .as_ref()
            .is_none_or(|current| priority < *current)
        {
            best_priority = Some(priority);
            best_index = Some(index);
        }
    }

    if let Some(index) = best_index {
        let left_record = &sorted[index];
        let right_record = &sorted[index + 1];
        let left = left_record.planner_meta.as_ref().unwrap();
        let right = right_record.planner_meta.as_ref().unwrap();
        let priority = best_priority.unwrap();
        return ArchivePairSelection {
            pair: Some((left_record.path.clone(), right_record.path.clone())),
            effective_policy: ArchivePairPolicy::AdjacentBalanced,
            warning: None,
            selection_log: Some(format!(
                "Adjacent planner selected: left=[{}, {}) right=[{}, {}) leaves=({}, {}) levels=({}, {}) costs=({}, {}) priority=({},{},{},{},{})",
                left.range_start,
                left.range_end_exclusive,
                right.range_start,
                right.range_end_exclusive,
                left.leaf_count,
                right.leaf_count,
                left.merge_level,
                right.merge_level,
                left.planner_cost,
                right.planner_cost,
                priority.0,
                priority.1,
                priority.2,
                priority.3,
                priority.4
            )),
        };
    }

    let pair = select_archive_pair_from_counts(
        &sorted
            .iter()
            .map(|record| (record.path.clone(), record.ortho_count))
            .collect::<Vec<_>>(),
        ArchivePairPolicy::SmallestSmallest,
    );
    ArchivePairSelection {
        pair,
        effective_policy: ArchivePairPolicy::SmallestSmallest,
        warning: Some(
            "adjacent_balanced fallback: no adjacent planner ranges available; using smallest_smallest for this selection cycle"
                .to_string(),
        ),
        selection_log: None,
    }
}

pub fn get_archive_pair_with_config(
    config: &StateConfig,
    policy: ArchivePairPolicy,
) -> Result<Option<(String, String)>, FoldError> {
    Ok(get_archive_pair_selection_with_config(config, policy)?.pair)
}

pub fn get_archive_pair_selection_with_config(
    config: &StateConfig,
    policy: ArchivePairPolicy,
) -> Result<ArchivePairSelection, FoldError> {
    let archives = find_archives(config.input_dir().to_str().unwrap())?;

    if archives.len() < 2 {
        return Ok(ArchivePairSelection {
            pair: None,
            effective_policy: policy,
            warning: None,
            selection_log: None,
        });
    }

    let records: Vec<ArchivePlannerRecord> = archives
        .into_iter()
        .filter_map(|(path, _size)| {
            load_metadata(&path).ok().map(|count| ArchivePlannerRecord {
                planner_meta: load_archive_planner_meta(&path).ok(),
                path,
                ortho_count: count,
            })
        })
        .collect();

    let selection = match policy {
        ArchivePairPolicy::AdjacentBalanced => select_adjacent_balanced_pair(&records),
        _ => ArchivePairSelection {
            pair: select_archive_pair_from_counts(
                &records
                    .iter()
                    .map(|record| (record.path.clone(), record.ortho_count))
                    .collect::<Vec<_>>(),
                policy,
            ),
            effective_policy: policy,
            warning: None,
            selection_log: None,
        },
    };

    Ok(selection)
}

/// Get the two largest archives with custom config
pub fn get_two_largest_archives_with_config(
    config: &StateConfig,
) -> Result<Option<(String, String)>, FoldError> {
    get_archive_pair_with_config(config, ArchivePairPolicy::LargestLargest)
}

/// Get the two smallest archives with custom config
pub fn get_two_smallest_archives_with_config(
    config: &StateConfig,
) -> Result<Option<(String, String)>, FoldError> {
    get_archive_pair_with_config(config, ArchivePairPolicy::SmallestSmallest)
}

/// Archive metadata for initialization
pub struct ArchiveMetadata {
    pub path: String,
    pub ortho_count: usize,
    pub lineage: String,
}

/// Load archive metadata (ortho count) - public wrapper
pub fn load_archive_metadata(archive_path: &str) -> Result<usize, FoldError> {
    load_metadata(archive_path)
}

pub fn load_archive_planner_meta(archive_path: &str) -> Result<PlannerMeta, FoldError> {
    let bytes =
        read_archive_artifact_bytes(archive_path, crate::stage_planner::PLANNER_META_FILENAME)?;
    serde_json::from_slice(&bytes).map_err(|e| {
        FoldError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        ))
    })
}

/// Load the optimal ortho from an archive (required - will error if missing)
pub fn load_optimal_ortho(archive_path: &str) -> Result<Ortho, FoldError> {
    let optimal_bytes = read_archive_artifact_bytes(archive_path, "optimal.bin")?;
    Ortho::from_bytes(&optimal_bytes)
}

/// Find the largest archive by ortho count (uses default config)
pub fn find_largest_archive() -> Result<Option<ArchiveMetadata>, FoldError> {
    find_largest_archive_with_config(&StateConfig::default())
}

/// Find the largest archive by ortho count with custom config
pub fn find_largest_archive_with_config(
    config: &StateConfig,
) -> Result<Option<ArchiveMetadata>, FoldError> {
    let archives = find_archives(config.input_dir().to_str().unwrap())?;

    if archives.is_empty() {
        return Ok(None);
    }

    let mut largest: Option<ArchiveMetadata> = None;

    for (archive_path, _size_bytes) in archives {
        if let Ok(ortho_count) = load_metadata(&archive_path) {
            if let Ok(lineage) = load_lineage(&archive_path) {
                if let Some(ref current) = largest {
                    if ortho_count > current.ortho_count {
                        largest = Some(ArchiveMetadata {
                            path: archive_path,
                            ortho_count,
                            lineage,
                        });
                    }
                } else {
                    largest = Some(ArchiveMetadata {
                        path: archive_path,
                        ortho_count,
                        lineage,
                    });
                }
            }
        }
    }

    Ok(largest)
}

/// Ingest a text file (uses default config)
pub fn ingest_txt_file(file_path: &str) -> Result<TxtIngestion, FoldError> {
    ingest_txt_file_with_config(file_path, &StateConfig::default())
}

/// Ingest a text file with custom config
pub fn ingest_txt_file_with_config(
    file_path: &str,
    config: &StateConfig,
) -> Result<TxtIngestion, FoldError> {
    let (work_folder, source_txt_path, heartbeat_path, filename) =
        setup_txt_processing(file_path, config.in_process_dir().to_str().unwrap())?;

    // Read the text immediately as part of ingestion
    let text = read_source_text(&source_txt_path)?;

    // Compute text metadata
    let word_count = count_words(&text);
    let text_preview = create_text_preview(&text, 4, 4); // First 4 and last 4 words for text blobs

    Ok(TxtIngestion {
        work_folder,
        heartbeat_path,
        filename,
        text,
        text_preview,
        word_count,
        config: config.clone(),
    })
}

/// Ingest archives for merging (uses default config)
pub fn ingest_archives(
    archive_a_path: &str,
    archive_b_path: &str,
) -> Result<ArchiveIngestion, FoldError> {
    ingest_archives_with_config(archive_a_path, archive_b_path, &StateConfig::default())
}

/// Ingest archives for merging with custom config
pub fn ingest_archives_with_config(
    archive_a_path: &str,
    archive_b_path: &str,
    config: &StateConfig,
) -> Result<ArchiveIngestion, FoldError> {
    let in_process = config.in_process_dir();

    // Load text metadata before moving archives
    let (word_count_a_orig, text_preview_a_orig) =
        load_text_metadata(archive_a_path).unwrap_or_else(|_| (0, String::new()));
    let (word_count_b_orig, text_preview_b_orig) =
        load_text_metadata(archive_b_path).unwrap_or_else(|_| (0, String::new()));

    // Truncate previews to first 2 and last 2 words for merging display
    let text_preview_a = if word_count_a_orig > 0 {
        create_text_preview(&text_preview_a_orig, 2, 2)
    } else {
        String::new()
    };
    let text_preview_b = if word_count_b_orig > 0 {
        create_text_preview(&text_preview_b_orig, 2, 2)
    } else {
        String::new()
    };

    let (work_a_path, work_b_path) =
        setup_archive_merge(archive_a_path, archive_b_path, in_process.to_str().unwrap())?;

    // Create merge work folder for isolated queue and seen_shards
    let merge_work_folder = in_process.join(format!("merge_{}.work", std::process::id()));
    fs::create_dir_all(&merge_work_folder).map_err(FoldError::Io)?;

    // Create heartbeat for merge operation
    let heartbeat_path = merge_work_folder.join("heartbeat");
    touch_heartbeat(heartbeat_path.to_str().unwrap())?;

    disk_safety::ensure_archive_results_local(Path::new(&work_a_path)).map_err(FoldError::Io)?;
    disk_safety::ensure_archive_results_local(Path::new(&work_b_path)).map_err(FoldError::Io)?;

    Ok(ArchiveIngestion {
        work_a_path,
        work_b_path,
        merge_work_folder: merge_work_folder.to_string_lossy().to_string(),
        heartbeat_path: heartbeat_path.to_string_lossy().to_string(),
        text_preview_a,
        text_preview_b,
        word_count_a: word_count_a_orig,
        word_count_b: word_count_b_orig,
        config: config.clone(),
    })
}

pub fn resume_archives_with_config(
    claim: &ResumableMergeClaim,
) -> Result<ArchiveIngestion, FoldError> {
    let merge_work_folder = PathBuf::from(&claim.merge_work_folder);
    fs::create_dir_all(&merge_work_folder).map_err(FoldError::Io)?;
    touch_heartbeat(&claim.heartbeat_path)?;
    disk_safety::ensure_archive_results_local(Path::new(&claim.archive_a_path))
        .map_err(FoldError::Io)?;
    disk_safety::ensure_archive_results_local(Path::new(&claim.archive_b_path))
        .map_err(FoldError::Io)?;

    Ok(ArchiveIngestion {
        work_a_path: claim.archive_a_path.clone(),
        work_b_path: claim.archive_b_path.clone(),
        merge_work_folder: claim.merge_work_folder.clone(),
        heartbeat_path: claim.heartbeat_path.clone(),
        text_preview_a: claim.text_preview_a.clone(),
        text_preview_b: claim.text_preview_b.clone(),
        word_count_a: claim.word_count_a,
        word_count_b: claim.word_count_b,
        config: claim.config.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generation_store::{self, GenerationStore},
        merge_resume::{self, MergeResumeManifest, ResumePhase},
        offload_config::OffloadConfig,
        offload_runtime::configure_offload_runtime,
        ortho::{Ortho, OrthoScore},
    };
    use filetime::{FileTime, set_file_mtime};
    use std::sync::{Arc, Barrier};
    use sysinfo::Disks;

    fn available_space_for_test(path: &Path) -> u64 {
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
        best.expect("no disk mount found").0
    }

    #[test]
    fn test_heartbeat_creation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let work_folder = temp_dir.path().join("test.txt.work");
        fs::create_dir_all(&work_folder).unwrap();

        let heartbeat_path = create_heartbeat(work_folder.to_str().unwrap()).unwrap();

        // Verify heartbeat file was created inside the folder
        assert!(Path::new(&heartbeat_path).exists());
        assert_eq!(
            heartbeat_path,
            work_folder.join("heartbeat").to_str().unwrap()
        );

        // Verify heartbeat file is not stale (freshly created)
        assert!(!is_heartbeat_stale(Path::new(&heartbeat_path)).unwrap());
    }

    #[test]
    fn test_ensure_directory_exists() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_path = temp_dir.path().join("new_dir");

        // Directory should not exist initially
        assert!(!test_path.exists());

        // Call ensure_directory_exists
        ensure_directory_exists(test_path.to_str().unwrap()).unwrap();

        // Directory should now exist
        assert!(test_path.exists());
        assert!(test_path.is_dir());

        // Calling again should be idempotent
        ensure_directory_exists(test_path.to_str().unwrap()).unwrap();
        assert!(test_path.exists());
    }

    #[test]
    fn test_setup_and_cleanup_txt_processing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let input_dir = temp_dir.path().join("input");
        let in_process_dir = temp_dir.path().join("in_process");
        fs::create_dir_all(&input_dir).unwrap();
        fs::create_dir_all(&in_process_dir).unwrap();

        // Create a test txt file
        let txt_path = input_dir.join("test.txt");
        fs::write(&txt_path, "test content").unwrap();

        // Setup processing
        let (work_folder, source_txt_path, heartbeat_path, filename) =
            setup_txt_processing(txt_path.to_str().unwrap(), in_process_dir.to_str().unwrap())
                .unwrap();

        // Verify filename extraction
        assert_eq!(filename, "test");

        // Verify work folder was created
        assert!(Path::new(&work_folder).exists());
        assert!(work_folder.ends_with("test.txt.work"));

        // Verify source.txt exists in work folder
        assert!(Path::new(&source_txt_path).exists());
        let content = fs::read_to_string(&source_txt_path).unwrap();
        assert_eq!(content, "test content");

        // Verify heartbeat was created
        assert!(Path::new(&heartbeat_path).exists());

        // Verify original file was moved
        assert!(!txt_path.exists());

        // Test read_source_text
        let read_content = read_source_text(&source_txt_path).unwrap();
        assert_eq!(read_content, "test content");

        // Cleanup
        cleanup_txt_processing(&work_folder).unwrap();

        // Verify work folder was deleted
        assert!(!Path::new(&work_folder).exists());
    }

    #[test]
    fn test_cleanup_archives() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create test archive directories
        let archive1 = temp_dir.path().join("archive1.bin");
        let archive2 = temp_dir.path().join("archive2.bin");
        fs::create_dir_all(&archive1).unwrap();
        fs::create_dir_all(&archive2).unwrap();

        // Verify they exist
        assert!(archive1.exists());
        assert!(archive2.exists());

        // Cleanup
        cleanup_archives(&[archive1.to_str().unwrap(), archive2.to_str().unwrap()]).unwrap();

        // Verify they were deleted
        assert!(!archive1.exists());
        assert!(!archive2.exists());

        // Test cleanup with non-existent archive (should not error)
        let non_existent = temp_dir.path().join("non_existent.bin");
        cleanup_archives(&[non_existent.to_str().unwrap()]).unwrap();
    }

    #[test]
    fn test_count_running_jobs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());

        // Initialize directories
        initialize_with_config(&config).unwrap();

        // Initially, no jobs running
        let count = count_running_jobs_with_config(&config).unwrap();
        assert_eq!(count, 0);

        // Create a txt.work folder with heartbeat
        let txt_work = config.in_process_dir().join("test.txt.work");
        fs::create_dir_all(&txt_work).unwrap();
        create_heartbeat(txt_work.to_str().unwrap()).unwrap();

        // Should count 1 job
        let count = count_running_jobs_with_config(&config).unwrap();
        assert_eq!(count, 1);

        // Create a merge.work folder with heartbeat
        let merge_work = config.in_process_dir().join("merge_12345.work");
        fs::create_dir_all(&merge_work).unwrap();
        create_heartbeat(merge_work.to_str().unwrap()).unwrap();

        // Should count 2 jobs
        let count = count_running_jobs_with_config(&config).unwrap();
        assert_eq!(count, 2);

        // Create an archive .bin folder with heartbeat (should NOT be counted as a job)
        let archive_bin = config.in_process_dir().join("archive_test.bin");
        fs::create_dir_all(&archive_bin).unwrap();
        create_heartbeat(archive_bin.to_str().unwrap()).unwrap();

        // Should still count 2 jobs (archive folders don't count)
        let count = count_running_jobs_with_config(&config).unwrap();
        assert_eq!(count, 2);

        // Create a folder without a heartbeat
        let no_heartbeat = config.in_process_dir().join("test2.txt.work");
        fs::create_dir_all(&no_heartbeat).unwrap();

        // Should still count 2 jobs (no heartbeat means not active)
        let count = count_running_jobs_with_config(&config).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn orphaned_in_process_archives_without_heartbeat_are_recovered() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());
        initialize_with_config(&config).unwrap();

        // Create orphaned archive in in_process without heartbeat
        let orphan = config.in_process_dir().join("archive_orphan.bin");
        fs::create_dir_all(&orphan).unwrap();

        // No copy exists in input; should be moved back to input
        recover_abandoned_files(
            config.input_dir().to_str().unwrap(),
            config.in_process_dir().to_str().unwrap(),
        )
        .unwrap();
        assert!(
            !orphan.exists(),
            "orphaned copy should be removed from in_process"
        );
        assert!(
            config.input_dir().join("archive_orphan.bin").exists(),
            "archive should be restored to input"
        );

        // Now create a second orphan when input already has a copy; it should be dropped
        let orphan_dup = config.in_process_dir().join("archive_orphan.bin");
        fs::create_dir_all(&orphan_dup).unwrap();
        recover_abandoned_files(
            config.input_dir().to_str().unwrap(),
            config.in_process_dir().to_str().unwrap(),
        )
        .unwrap();
        assert!(!orphan_dup.exists(), "duplicate orphan should be deleted");
    }

    #[test]
    fn stale_merge_rewinds_to_input_even_with_valid_resume_manifest() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());
        initialize_with_config(&config).unwrap();

        let archive_a =
            create_archive_with_meta(&config.in_process_dir(), "archive_a.bin", 10, None);
        let archive_b =
            create_archive_with_meta(&config.in_process_dir(), "archive_b.bin", 20, None);
        touch_heartbeat(archive_a.join("heartbeat").to_str().unwrap()).unwrap();
        touch_heartbeat(archive_b.join("heartbeat").to_str().unwrap()).unwrap();

        let merge_work = config.in_process_dir().join("merge_4242.work");
        fs::create_dir_all(&merge_work).unwrap();
        let merge_heartbeat = create_heartbeat(merge_work.to_str().unwrap()).unwrap();

        let checkpoint = merge_resume::prepare_working_checkpoint(&merge_work, None).unwrap();
        let mut store =
            GenerationStore::new_with_config(checkpoint.working_dir.clone(), 8).unwrap();
        store.push_segments(vec![Ortho::new()]).unwrap();
        store.flush_all().unwrap();

        let manifest = MergeResumeManifest::new(
            ResumePhase::Claimed,
            archive_a.to_string_lossy().to_string(),
            archive_b.to_string_lossy().to_string(),
            true,
            "adjacent_balanced".to_string(),
            OrthoScore::zero(),
        );
        let mut manifest = merge_resume::finalize_checkpoint_commit(
            &merge_work,
            manifest,
            checkpoint,
            &Ortho::new(),
        )
        .unwrap();
        manifest.phase = ResumePhase::LargerLoaded;
        merge_resume::write_manifest_atomic(&merge_work, &manifest).unwrap();

        let stale_secs = (HEARTBEAT_GRACE_PERIOD_SECS + 5) as i64;
        let stale_time =
            FileTime::from_unix_time(current_timestamp_secs().unwrap() as i64 - stale_secs, 0);
        set_file_mtime(&merge_heartbeat, stale_time).unwrap();
        set_file_mtime(archive_a.join("heartbeat"), stale_time).unwrap();
        set_file_mtime(archive_b.join("heartbeat"), stale_time).unwrap();

        recover_abandoned_files(
            config.input_dir().to_str().unwrap(),
            config.in_process_dir().to_str().unwrap(),
        )
        .unwrap();

        assert!(!merge_work.exists(), "stale merge work should be removed");
        assert!(
            config.input_dir().join("archive_a.bin").exists(),
            "archive A should be rewound to input"
        );
        assert!(
            config.input_dir().join("archive_b.bin").exists(),
            "archive B should be rewound to input"
        );
    }

    #[test]
    fn stale_invalid_resumable_merge_rewinds_to_input() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());
        initialize_with_config(&config).unwrap();

        let archive_a =
            create_archive_with_meta(&config.in_process_dir(), "archive_a.bin", 10, None);
        let archive_b =
            create_archive_with_meta(&config.in_process_dir(), "archive_b.bin", 20, None);
        touch_heartbeat(archive_a.join("heartbeat").to_str().unwrap()).unwrap();
        touch_heartbeat(archive_b.join("heartbeat").to_str().unwrap()).unwrap();

        let merge_work = config.in_process_dir().join("merge_9898.work");
        fs::create_dir_all(&merge_work).unwrap();
        let merge_heartbeat = create_heartbeat(merge_work.to_str().unwrap()).unwrap();
        fs::write(
            merge_work.join(merge_resume::MANIFEST_FILENAME),
            br#"{"schema_version":1,"phase":"LargerLoaded","generation":0,"active_store_dir":"checkpoints/store-9999","archive_a_path":"missing_a","archive_b_path":"missing_b","a_is_smaller":true,"merge_policy":"adjacent_balanced","best_score":{"volume":0,"variance_num":0,"variance_den":1,"fullness":0},"best_ortho_file":null,"archive_temp_path":null,"updated_at":0}"#,
        )
        .unwrap();

        let stale_secs = (HEARTBEAT_GRACE_PERIOD_SECS + 5) as i64;
        let stale_time =
            FileTime::from_unix_time(current_timestamp_secs().unwrap() as i64 - stale_secs, 0);
        set_file_mtime(&merge_heartbeat, stale_time).unwrap();
        set_file_mtime(archive_a.join("heartbeat"), stale_time).unwrap();
        set_file_mtime(archive_b.join("heartbeat"), stale_time).unwrap();

        recover_abandoned_files(
            config.input_dir().to_str().unwrap(),
            config.in_process_dir().to_str().unwrap(),
        )
        .unwrap();

        assert!(
            !merge_work.exists(),
            "invalid resumable merge should be removed"
        );
        assert!(
            config.input_dir().join("archive_a.bin").exists(),
            "archive A should be rewound to input"
        );
        assert!(
            config.input_dir().join("archive_b.bin").exists(),
            "archive B should be rewound to input"
        );
    }

    #[test]
    fn archive_root_artifact_loaders_use_managed_paths() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().join("fold_state"));
        initialize_with_config(&config).unwrap();

        let archive = config.input_dir().join("archive_a.bin");
        fs::create_dir_all(archive.join("results")).unwrap();

        let interner = Interner::from_text("foo bar");
        let optimal = Ortho::new();
        let planner_meta = PlannerMeta {
            range_start: 0,
            range_end_exclusive: 1,
            leaf_count: 1,
            merge_level: 0,
            planner_cost: 10,
            word_count: 2,
        };

        fs::write(archive.join("metadata.txt"), "7").unwrap();
        fs::write(archive.join("lineage.txt"), "\"x\"").unwrap();
        fs::write(archive.join("text_meta.txt"), "2\npreview").unwrap();
        fs::write(archive.join("interner.bin"), interner.to_bytes().unwrap()).unwrap();
        fs::write(archive.join("optimal.bin"), optimal.to_bytes().unwrap()).unwrap();
        crate::stage_planner::write_planner_meta(&archive, &planner_meta).unwrap();

        let mut cfg = OffloadConfig::with_base_dir(&config.base_dir);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = config.base_dir.join("offload_cache");
        let _guard = configure_offload_runtime(&config.base_dir, &cfg)
            .unwrap()
            .unwrap();

        for rel in [
            "metadata.txt",
            "lineage.txt",
            "text_meta.txt",
            "interner.bin",
            "optimal.bin",
            crate::stage_planner::PLANNER_META_FILENAME,
        ] {
            let path = archive.join(rel);
            assert!(generation_store::offload_path_if_configured(&path).unwrap());
            fs::remove_file(&path).unwrap();
        }

        let archive_str = archive.to_string_lossy().to_string();
        assert_eq!(load_metadata(&archive_str).unwrap(), 7);
        assert_eq!(load_lineage(&archive_str).unwrap(), "\"x\"");
        assert_eq!(
            load_text_metadata(&archive_str).unwrap(),
            (2, "preview".to_string())
        );
        assert_eq!(
            load_interner(&archive_str).unwrap().vocabulary(),
            interner.vocabulary()
        );
        assert_eq!(load_optimal_ortho(&archive_str).unwrap(), optimal);
        assert_eq!(
            load_archive_planner_meta(&archive_str).unwrap(),
            planner_meta
        );
    }

    #[test]
    fn reclaim_pressure_keeps_live_merge_files_local_and_replayable() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().join("fold_state"));
        initialize_with_config(&config).unwrap();

        let archive_a =
            create_archive_with_meta(&config.in_process_dir(), "archive_a.bin", 10, None);
        let archive_b =
            create_archive_with_meta(&config.in_process_dir(), "archive_b.bin", 20, None);
        touch_heartbeat(archive_a.join("heartbeat").to_str().unwrap()).unwrap();
        touch_heartbeat(archive_b.join("heartbeat").to_str().unwrap()).unwrap();

        let reclaimable_spill = archive_a
            .join("results")
            .join("spill")
            .join("b=00")
            .join("spill-0.dat");
        fs::create_dir_all(reclaimable_spill.parent().unwrap()).unwrap();
        const FILE_BYTES: usize = 64 * 1024 * 1024;
        const RESERVATION_BYTES: u64 = 8 * 1024 * 1024;
        fs::write(&reclaimable_spill, vec![5u8; FILE_BYTES]).unwrap();

        let merge_work = config.in_process_dir().join("merge_4242.work");
        fs::create_dir_all(&merge_work).unwrap();
        let merge_heartbeat = create_heartbeat(merge_work.to_str().unwrap()).unwrap();

        let checkpoint = merge_resume::prepare_working_checkpoint(&merge_work, None).unwrap();
        let mut store =
            GenerationStore::new_with_config(checkpoint.working_dir.clone(), 8).unwrap();
        store.push_segments(vec![Ortho::new()]).unwrap();
        store.flush_all().unwrap();

        let manifest = MergeResumeManifest::new(
            ResumePhase::Claimed,
            archive_a.to_string_lossy().to_string(),
            archive_b.to_string_lossy().to_string(),
            true,
            "adjacent_balanced".to_string(),
            OrthoScore::zero(),
        );
        let mut manifest = merge_resume::finalize_checkpoint_commit(
            &merge_work,
            manifest,
            checkpoint,
            &Ortho::new(),
        )
        .unwrap();
        manifest.phase = ResumePhase::LargerLoaded;
        merge_resume::write_manifest_atomic(&merge_work, &manifest).unwrap();

        let free_now = available_space_for_test(&config.base_dir);
        let mut cfg = OffloadConfig::with_base_dir(&config.base_dir);
        cfg.enabled = true;
        cfg.in_memory_store = true;
        cfg.cache_dir = config.base_dir.join("offload_cache");
        cfg.disk_hysteresis_margin_bytes = 0;
        cfg.disk_free_low_water = Some(free_now.saturating_add((FILE_BYTES / 2) as u64));
        let _guard = configure_offload_runtime(&config.base_dir, &cfg)
            .unwrap()
            .unwrap();
        disk_safety::configure(config.base_dir.clone(), &cfg);

        assert!(disk_safety::maybe_reclaim(RESERVATION_BYTES, "test resumable reclaim").unwrap());
        assert!(merge_work.join(merge_resume::MANIFEST_FILENAME).exists());
        assert!(archive_a.join("metadata.txt").exists());
        assert!(archive_a.join("text_meta.txt").exists());

        let stale_secs = (HEARTBEAT_GRACE_PERIOD_SECS + 5) as i64;
        let stale_time =
            FileTime::from_unix_time(current_timestamp_secs().unwrap() as i64 - stale_secs, 0);
        set_file_mtime(&merge_heartbeat, stale_time).unwrap();
        set_file_mtime(archive_a.join("heartbeat"), stale_time).unwrap();
        set_file_mtime(archive_b.join("heartbeat"), stale_time).unwrap();

        recover_abandoned_files(
            config.input_dir().to_str().unwrap(),
            config.in_process_dir().to_str().unwrap(),
        )
        .unwrap();
        assert!(!merge_work.exists(), "stale merge work should be removed");
        assert!(
            config.input_dir().join("archive_a.bin").exists(),
            "archive A should be rewound to input after reclaim pressure"
        );
        assert!(
            config.input_dir().join("archive_b.bin").exists(),
            "archive B should be rewound to input after reclaim pressure"
        );
    }

    #[test]
    fn mem_claim_create_load_and_cleanup() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());

        let guard = create_mem_claim(&config, "leader", 10_000, 8_000).unwrap();
        let claim_path = config
            .mem_claims_dir()
            .join(format!("{}.claim", std::process::id()));
        assert!(claim_path.exists(), "claim file should be created");

        let claims = load_active_mem_claims(&config).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].granted_bytes, guard.granted_bytes());
        assert_eq!(claims[0].requested_bytes, guard.requested_bytes());
        assert_eq!(claims[0].role, "leader");

        // Touch claim to ensure non-stale
        guard.touch().unwrap();

        // Make the claim stale and ensure cleanup removes it
        let stale_time = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(
                MEM_CLAIM_STALE_GRACE_SECS + 5,
            ))
            .unwrap();
        let stale_filetime = filetime::FileTime::from_system_time(stale_time);
        filetime::set_file_mtime(&claim_path, stale_filetime).unwrap();

        cleanup_stale_mem_claims(&config).unwrap();
        assert!(
            !claim_path.exists(),
            "stale claim should be removed by cleanup"
        );

        drop(guard); // should be a no-op even if file already removed
    }

    #[test]
    fn concurrent_ingest_only_allows_one_claim() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());
        initialize_with_config(&config).unwrap();

        let input_dir = config.input_dir();
        fs::create_dir_all(&input_dir).unwrap();

        let txt_path = input_dir.join("race.txt");
        fs::write(&txt_path, "concurrent ingestion test").unwrap();
        let txt_path_str = txt_path.to_string_lossy().to_string();

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let config_clone = config.clone();
                let barrier = Arc::clone(&barrier);
                let path = txt_path_str.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    ingest_txt_file_with_config(&path, &config_clone)
                })
            })
            .collect();

        let mut successes = 0;
        let mut concurrency_errors = 0;

        for handle in handles {
            match handle.join().unwrap() {
                Ok(ingestion) => {
                    successes += 1;
                    ingestion.cleanup().unwrap();
                }
                Err(FoldError::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::AlreadyExists
                    ) =>
                {
                    concurrency_errors += 1;
                }
                Err(other) => panic!("unexpected error: {:?}", other),
            }
        }

        assert_eq!(successes, 1, "exactly one worker should claim the file");
        assert_eq!(
            concurrency_errors, 1,
            "the losing worker should see an atomic-move race error"
        );
    }

    #[test]
    fn find_txt_skips_active_work_folder() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());
        initialize_with_config(&config).unwrap();

        // Two input files
        fs::create_dir_all(config.input_dir()).unwrap();
        let first = config.input_dir().join("a.txt");
        let second = config.input_dir().join("b.txt");
        fs::write(&first, "one").unwrap();
        fs::write(&second, "two").unwrap();

        // Active work folder for a.txt with fresh heartbeat
        let work_a = config.in_process_dir().join("a.txt.work");
        fs::create_dir_all(&work_a).unwrap();
        create_heartbeat(work_a.to_str().unwrap()).unwrap();

        // Should return b.txt (skip a.txt because its work folder is active)
        let next = find_txt_file_with_config(&config).unwrap();
        assert_eq!(next.as_deref(), Some(second.to_str().unwrap()));
    }

    #[test]
    fn archive_pair_policy_selects_expected_pairs() {
        let archives = vec![
            ("archive_small".to_string(), 10usize),
            ("archive_mid".to_string(), 30usize),
            ("archive_large".to_string(), 50usize),
        ];

        assert_eq!(
            select_archive_pair_from_counts(&archives, ArchivePairPolicy::LargestLargest),
            Some(("archive_mid".to_string(), "archive_large".to_string()))
        );
        assert_eq!(
            select_archive_pair_from_counts(&archives, ArchivePairPolicy::SmallestSmallest),
            Some(("archive_small".to_string(), "archive_mid".to_string()))
        );
        assert_eq!(
            select_archive_pair_from_counts(&archives, ArchivePairPolicy::LargestSmallest),
            Some(("archive_small".to_string(), "archive_large".to_string()))
        );
    }

    fn create_archive_with_meta(
        root: &Path,
        name: &str,
        ortho_count: usize,
        planner_meta: Option<PlannerMeta>,
    ) -> PathBuf {
        let archive = root.join(name);
        fs::create_dir_all(archive.join("results")).unwrap();
        fs::write(archive.join("metadata.txt"), ortho_count.to_string()).unwrap();
        fs::write(archive.join("lineage.txt"), name).unwrap();
        fs::write(archive.join("text_meta.txt"), "1\nx").unwrap();
        if let Some(planner_meta) = planner_meta.as_ref() {
            crate::stage_planner::write_planner_meta(&archive, planner_meta).unwrap();
        }
        archive
    }

    #[test]
    fn adjacent_balanced_selects_adjacent_pair_by_balance() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());
        initialize_with_config(&config).unwrap();

        create_archive_with_meta(
            &config.input_dir(),
            "archive_a.bin",
            10,
            Some(PlannerMeta {
                range_start: 0,
                range_end_exclusive: 1,
                leaf_count: 1,
                merge_level: 0,
                planner_cost: 10,
                word_count: 10,
            }),
        );
        create_archive_with_meta(
            &config.input_dir(),
            "archive_b.bin",
            12,
            Some(PlannerMeta {
                range_start: 1,
                range_end_exclusive: 2,
                leaf_count: 1,
                merge_level: 0,
                planner_cost: 12,
                word_count: 12,
            }),
        );
        create_archive_with_meta(
            &config.input_dir(),
            "archive_c.bin",
            40,
            Some(PlannerMeta {
                range_start: 2,
                range_end_exclusive: 4,
                leaf_count: 2,
                merge_level: 1,
                planner_cost: 40,
                word_count: 40,
            }),
        );

        let selection =
            get_archive_pair_selection_with_config(&config, ArchivePairPolicy::AdjacentBalanced)
                .unwrap();
        let pair = selection.pair.unwrap();
        assert_eq!(
            selection.effective_policy,
            ArchivePairPolicy::AdjacentBalanced
        );
        assert!(selection.warning.is_none());
        assert!(selection.selection_log.is_some());
        assert!(pair.0.ends_with("archive_a.bin"));
        assert!(pair.1.ends_with("archive_b.bin"));
    }

    #[test]
    fn adjacent_balanced_falls_back_when_planner_meta_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = StateConfig::custom(temp_dir.path().to_path_buf());
        initialize_with_config(&config).unwrap();

        create_archive_with_meta(&config.input_dir(), "archive_a.bin", 10, None);
        create_archive_with_meta(&config.input_dir(), "archive_b.bin", 20, None);

        let selection =
            get_archive_pair_selection_with_config(&config, ArchivePairPolicy::AdjacentBalanced)
                .unwrap();
        assert_eq!(
            selection.effective_policy,
            ArchivePairPolicy::SmallestSmallest
        );
        assert!(selection.warning.is_some());
        let pair = selection.pair.unwrap();
        assert!(pair.0.ends_with("archive_a.bin"));
        assert!(pair.1.ends_with("archive_b.bin"));
    }
}
