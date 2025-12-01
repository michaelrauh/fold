use crate::{
    disk_backed_queue::DiskBackedQueue,
    interner::Interner,
    ortho::Ortho,
    s3_state::S3State,
    FoldError,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const HEARTBEAT_GRACE_PERIOD_SECS: u64 = 600; // 10 minutes

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
    
    let first_words = words.iter().take(first_n).cloned().collect::<Vec<_>>().join(" ");
    let last_words = words.iter().rev().take(last_n).cloned().rev().collect::<Vec<_>>().join(" ");
    
    format!("{} ... {}", first_words, last_words)
}

/// Configuration for state directory locations
#[derive(Clone)]
pub struct StateConfig {
    pub base_dir: PathBuf,
    pub remote: Option<Arc<S3State>>,
}

impl StateConfig {
    /// Default configuration for production use
    pub fn default() -> Self {
        Self::with_remote(PathBuf::from("./fold_state")).unwrap_or(Self {
            base_dir: PathBuf::from("./fold_state"),
            remote: None,
        })
    }
    
    /// Attempt to attach remote state from env to a specific base dir
    pub fn with_remote(base_dir: PathBuf) -> Result<Self, FoldError> {
        let remote = S3State::try_from_env()?.map(Arc::new);
        Ok(Self { base_dir, remote })
    }

    /// Custom configuration for tests
    pub fn custom(base_dir: PathBuf) -> Self {
        Self { base_dir, remote: None }
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
}

/// Initialize the file system: create directories and recover abandoned files
pub fn initialize() -> Result<(), FoldError> {
    initialize_with_config(&StateConfig::default())
}

/// Initialize with custom config (for tests)
pub fn initialize_with_config(config: &StateConfig) -> Result<(), FoldError> {
    let in_process = config.in_process_dir();
    let input = config.input_dir();

    if let Some(remote) = &config.remote {
        remote.recover_stale_leases(HEARTBEAT_GRACE_PERIOD_SECS)?;
    }

    ensure_directory_exists(in_process.to_str().unwrap())?;
    recover_abandoned_files(input.to_str().unwrap(), in_process.to_str().unwrap(), config.remote.clone())?;
    Ok(())
}

/// Check for and recover any stale work from crashed processes
/// This should be called at the beginning of each worker loop iteration
pub fn check_and_recover_stale_work(config: &StateConfig) -> Result<(), FoldError> {
    if let Some(remote) = &config.remote {
        remote.recover_stale_leases(HEARTBEAT_GRACE_PERIOD_SECS)?;
    }
    recover_abandoned_files(
        config.input_dir().to_str().unwrap(),
        config.in_process_dir().to_str().unwrap(),
        config.remote.clone(),
    )
}

fn recover_abandoned_files(
    input_dir: &str,
    in_process_dir: &str,
    remote: Option<Arc<S3State>>,
) -> Result<(), FoldError> {
    let in_process_path = std::path::Path::new(in_process_dir);
    
    if !in_process_path.exists() {
        return Ok(());
    }
    
    let mut recovered_count = 0;
    
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
                            fs::rename(&source_txt_path, &target_path).map_err(|e| FoldError::Io(e))?;
                            // println!("[fold] Recovered abandoned txt file: {} -> {}", folder_name_str, target_path);

                            if let Some(r) = &remote {
                                let _ = r.release_lease(&format!("input/{}.txt", base_name));
                            }
                            
                            // Delete the results_{filename} directory if it exists
                            let base_dir = Path::new(in_process_dir).parent().unwrap_or_else(|| Path::new("."));
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
                        if let Some(pid_str) = folder_name_str.strip_prefix("merge_").and_then(|s| s.strip_suffix(".work")) {
                            // Find and recover the two archive folders back to input
                            // They should be in in_process as .bin folders
                            if let Ok(entries) = fs::read_dir(in_process_path) {
                                for bin_entry in entries {
                                    if let Ok(bin_entry) = bin_entry {
                                        let bin_path = bin_entry.path();
                                        if bin_path.is_dir() && bin_path.extension().map(|e| e == "bin").unwrap_or(false) {
                                            // Remove heartbeat from archive
                                            let archive_heartbeat = bin_path.join("heartbeat");
                                            if archive_heartbeat.exists() {
                                                let _ = fs::remove_file(&archive_heartbeat);
                                            }
                                            
                                            // Move archive back to input
                                            let archive_name = bin_path.file_name().unwrap();
                                            let input_path = Path::new(input_dir).join(archive_name);
                                            // println!("[fold] Recovering merge archive: {:?} -> {:?}", bin_path, input_path);
                                            let _ = fs::rename(&bin_path, &input_path);

                                            if let Some(r) = &remote {
                                                if let Some(name_str) = archive_name.to_str() {
                                                    let _ = r.release_lease(&format!("input/{}", name_str));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            
                            // Delete the results_merged_{pid} directory if it exists
                            let base_dir = Path::new(in_process_dir).parent().unwrap_or_else(|| Path::new("."));
                            let results_merged_path = base_dir.join(format!("results_merged_{}", pid_str));
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
                        // Remove the heartbeat before moving to input
                        let heartbeat_to_remove = entry_path.join("heartbeat");
                        if heartbeat_to_remove.exists() {
                            fs::remove_file(&heartbeat_to_remove).map_err(|e| FoldError::Io(e))?;
                        }
                        
                        let archive_name = entry_path.file_name().unwrap();
                        let input_path = Path::new(input_dir).join(archive_name);
                        // println!("[fold] Recovering orphaned archive: {:?} -> {:?}", entry_path, input_path);
                        fs::rename(&entry_path, &input_path).map_err(|e| FoldError::Io(e))?;
                        if let Some(r) = &remote {
                            if let Some(name_str) = archive_name.to_str() {
                                let _ = r.release_lease(&format!("input/{}", name_str));
                            }
                        }
                        recovered_count += 1;
                    }
                }
            }
        }
    }
    
    // Clean up orphaned results_merged_* directories in the parent directory
    // Only delete if the corresponding merge_*.work folder doesn't exist or has stale heartbeat
    let base_dir = Path::new(in_process_dir).parent().unwrap_or_else(|| Path::new("."));
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
                            let should_delete = !merge_work_path.exists() || 
                                !merge_heartbeat.exists() ||
                                is_heartbeat_stale(&merge_heartbeat).unwrap_or(true);
                            
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
    let metadata = fs::metadata(heartbeat_path).map_err(|e| FoldError::Io(e))?;
    let modified = metadata.modified().map_err(|e| FoldError::Io(e))?;
    let now = SystemTime::now();
    
    if let Ok(duration) = now.duration_since(modified) {
        Ok(duration.as_secs() > HEARTBEAT_GRACE_PERIOD_SECS)
    } else {
        Ok(false)
    }
}

fn touch_heartbeat(heartbeat_path: &str) -> Result<(), FoldError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .as_secs();
    
    fs::write(heartbeat_path, now.to_string().as_bytes()).map_err(|e| FoldError::Io(e))?;
    Ok(())
}

fn create_heartbeat(work_folder_path: &str) -> Result<String, FoldError> {
    // Heartbeat is now inside the work folder
    let heartbeat_path = format!("{}/heartbeat", work_folder_path);
    touch_heartbeat(&heartbeat_path)?;
    Ok(heartbeat_path)
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

fn find_next_txt_file(input_dir: &str) -> Result<Option<String>, FoldError> {
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


fn save_archive(
    archive_path: &str, 
    interner: &Interner, 
    mut results: DiskBackedQueue, 
    results_path: &str,
    best_ortho: Option<&Ortho>,
    lineage: &str,
    ortho_count: usize,
    text_preview: &str,
    word_count: usize
) -> Result<(), FoldError> {
    // Flush and drop results to close file handles before rename
    results.flush()?;
    drop(results);
    
    // Create archive directory
    fs::create_dir_all(archive_path).map_err(|e| FoldError::Io(e))?;
    
    // Move the DiskBackedQueue directory to the archive
    let archive_results_path = format!("{}/results", archive_path);
    let results_path_obj = Path::new(results_path);
    if results_path_obj.exists() {
        fs::rename(results_path, &archive_results_path).map_err(|e| FoldError::Io(e))?;
    }
    
    // Write the interner to the archive folder
    let interner_path = format!("{}/interner.bin", archive_path);
    let interner_bytes = bincode::encode_to_vec(interner, bincode::config::standard())?;
    fs::write(interner_path, interner_bytes).map_err(|e| FoldError::Io(e))?;
    
    // Write the optimal ortho as text if provided
    if let Some(ortho) = best_ortho {
        let optimal_path = format!("{}/optimal.txt", archive_path);
        let optimal_text = format_optimal_ortho(ortho, interner);
        fs::write(optimal_path, optimal_text).map_err(|e| FoldError::Io(e))?;
        
        // Also save the optimal ortho in binary format for recovery
        let optimal_bin_path = format!("{}/optimal.bin", archive_path);
        let optimal_bytes = bincode::encode_to_vec(ortho, bincode::config::standard())?;
        fs::write(optimal_bin_path, optimal_bytes).map_err(|e| FoldError::Io(e))?;
    }
    
    // Write the lineage tracking as S-expression
    let lineage_path = format!("{}/lineage.txt", archive_path);
    fs::write(lineage_path, lineage).map_err(|e| FoldError::Io(e))?;
    
    // Write metadata (ortho count)
    let metadata_path = format!("{}/metadata.txt", archive_path);
    fs::write(metadata_path, ortho_count.to_string()).map_err(|e| FoldError::Io(e))?;
    
    // Write text metadata (preview and word count)
    let text_meta_path = format!("{}/text_meta.txt", archive_path);
    let text_meta_content = format!("{}\n{}", word_count, text_preview);
    fs::write(text_meta_path, text_meta_content).map_err(|e| FoldError::Io(e))?;
    
    Ok(())
}

fn format_optimal_ortho(ortho: &Ortho, interner: &Interner) -> String {
    let volume = ortho.dims().iter().map(|&d| d - 1).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    
    let mut output = String::new();
    output.push_str("===== OPTIMAL ORTHO =====\n");
    output.push_str(&format!("Ortho ID: {}\n", ortho.id()));
    output.push_str(&format!("Dimensions: {:?}\n", ortho.dims()));
    output.push_str(&format!("Score: (volume={}, fullness={})\n", volume, fullness));
    output.push_str("\nGeometry:\n");
    
    for line in format!("{}", ortho.display(interner)).lines() {
        output.push_str(&format!("  {}\n", line));
    }
    
    output
}

/// Load interner from an archive
pub fn load_interner(archive_path: &str) -> Result<Interner, FoldError> {
    let interner_path = format!("{}/interner.bin", archive_path);
    let interner_bytes = fs::read(&interner_path).map_err(|e| FoldError::Io(e))?;
    let (interner, _): (Interner, usize) = 
        bincode::decode_from_slice(&interner_bytes, bincode::config::standard())?;
    Ok(interner)
}

fn get_results_path(archive_path: &str) -> String {
    format!("{}/results", archive_path)
}

fn load_lineage(archive_path: &str) -> Result<String, FoldError> {
    let lineage_path = format!("{}/lineage.txt", archive_path);
    fs::read_to_string(&lineage_path).map_err(|e| FoldError::Io(e))
}

fn load_metadata(archive_path: &str) -> Result<usize, FoldError> {
    let metadata_path = format!("{}/metadata.txt", archive_path);
    let content = fs::read_to_string(&metadata_path).map_err(|e| FoldError::Io(e))?;
    content.trim().parse::<usize>()
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
}

/// Load text metadata (word count and preview) from archive
/// Returns (word_count, text_preview)
fn load_text_metadata(archive_path: &str) -> Result<(usize, String), FoldError> {
    let text_meta_path = format!("{}/text_meta.txt", archive_path);
    let content = fs::read_to_string(&text_meta_path).map_err(|e| FoldError::Io(e))?;
    let mut lines = content.lines();
    
    let word_count = lines.next()
        .ok_or_else(|| FoldError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing word count")))?
        .parse::<usize>()
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
    
    let text_preview = lines.next()
        .ok_or_else(|| FoldError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing text preview")))?
        .to_string();
    
    Ok((word_count, text_preview))
}

/// Ensures a directory exists by creating it if needed
fn ensure_directory_exists(path: &str) -> Result<(), FoldError> {
    fs::create_dir_all(path).map_err(|e| FoldError::Io(e))
}

/// Sets up processing for a txt file: creates work folder, moves file to source.txt
/// Returns (work_folder_path, source_txt_path, heartbeat_path, filename)
fn setup_txt_processing(file_path: &str, in_process_dir: &str) -> Result<(String, String, String, String), FoldError> {
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
fn setup_archive_merge(archive_a_path: &str, archive_b_path: &str, in_process_dir: &str) -> Result<(String, String), FoldError> {
    let archive_a_name = Path::new(archive_a_path).file_name().unwrap().to_str().unwrap();
    let archive_b_name = Path::new(archive_b_path).file_name().unwrap().to_str().unwrap();
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
    remote: Option<RemoteTxtHandle>,
}

struct RemoteTxtHandle {
    s3: Arc<S3State>,
    job_key: String,
}

impl TxtIngestion {
    /// Touch the heartbeat file (zero-arity as requested)
    pub fn touch_heartbeat(&self) -> Result<(), FoldError> {
        touch_heartbeat(&self.heartbeat_path)?;

        if let Some(remote) = &self.remote {
            remote
                .s3
                .refresh_lease(&remote.job_key, HEARTBEAT_GRACE_PERIOD_SECS)?;
        }

        Ok(())
    }
    
    /// Get the results path for this ingestion
    pub fn results_path(&self) -> String {
        self.config.results_dir(&self.filename).to_string_lossy().to_string()
    }
    
    /// Get the work queue path for this ingestion (isolated per file)
    pub fn work_queue_path(&self) -> String {
        format!("{}/queue", self.work_folder)
    }
    
    /// Get the seen shards path for this ingestion (isolated per file)
    pub fn seen_shards_path(&self) -> String {
        format!("{}/seen_shards", self.work_folder)
    }
    
    /// Save the processing result as an archive
    pub fn save_result(
        &self,
        interner: &Interner,
        results: DiskBackedQueue,
        best_ortho: Option<&Ortho>,
        ortho_count: usize,
    ) -> Result<(String, String), FoldError> {
        let lineage = format!("\"{}\"", self.filename);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        // Use timestamp + nanos + filename for guaranteed uniqueness
        let archive_path = self.config.input_dir().join(format!("archive_{}_{}_{}=.bin", 
            self.filename, now.as_secs(), now.subsec_nanos()));
        let results_path = self.results_path();
        
        save_archive(archive_path.to_str().unwrap(), interner, results, &results_path, best_ortho, &lineage, ortho_count, &self.text_preview, self.word_count)?;

        if let Some(remote) = &self.remote {
            remote
                .s3
                .upload_archive(archive_path.as_path())?;
            remote.s3.finalize_txt_job(&remote.job_key)?;
        }
        Ok((archive_path.to_string_lossy().to_string(), lineage))
    }
    
    /// Cleanup work folder after processing
    pub fn cleanup(self) -> Result<(), FoldError> {
        if let Some(remote) = &self.remote {
            let _ = remote.s3.release_lease(&remote.job_key);
        }
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
    remote: Option<RemoteMergeHandle>,
}

struct RemoteMergeHandle {
    s3: Arc<S3State>,
    archive_a_key: String,
    archive_b_key: String,
}

impl ArchiveIngestion {
    /// Touch the heartbeat file (zero-arity as requested)
    pub fn touch_heartbeat(&self) -> Result<(), FoldError> {
        touch_heartbeat(&self.heartbeat_path)?;

        if let Some(remote) = &self.remote {
            remote
                .s3
                .refresh_lease(&remote.archive_a_key, HEARTBEAT_GRACE_PERIOD_SECS)?;
            remote
                .s3
                .refresh_lease(&remote.archive_b_key, HEARTBEAT_GRACE_PERIOD_SECS)?;
        }

        Ok(())
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
            get_results_path(&self.work_b_path)
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
    
    /// Save merged result
    pub fn save_result(
        &self,
        interner: &Interner,
        results: DiskBackedQueue,
        results_path: &str,
        best_ortho: Option<&Ortho>,
        lineage_a: &str,
        lineage_b: &str,
        ortho_count: usize,
    ) -> Result<(String, String), FoldError> {
        let merged_lineage = format!("({} {})", lineage_a, lineage_b);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        // Use timestamp + nanos + process ID for guaranteed uniqueness across merges
        let archive_path = self.config.input_dir().join(format!("archive_merged_{}_{}_{}.bin", 
            now.as_secs(), now.subsec_nanos(), std::process::id()));
        
        // Compute merged text metadata (sum of word counts, combined previews)
        let merged_word_count = self.word_count_a + self.word_count_b;
        let merged_preview = format!("{} ... {}", self.text_preview_a, self.text_preview_b);
        
        save_archive(archive_path.to_str().unwrap(), interner, results, results_path, best_ortho, &merged_lineage, ortho_count, &merged_preview, merged_word_count)?;

        if let Some(remote) = &self.remote {
            remote.s3.upload_archive(archive_path.as_path())?;
            remote.s3.delete_remote_archive(&remote.archive_a_key)?;
            remote.s3.delete_remote_archive(&remote.archive_b_key)?;
            remote.s3.release_lease(&remote.archive_a_key)?;
            remote.s3.release_lease(&remote.archive_b_key)?;
        }
        Ok((archive_path.to_string_lossy().to_string(), merged_lineage))
    }
    
    /// Cleanup original archives and merge work folder
    pub fn cleanup(self) -> Result<(), FoldError> {
        if let Some(remote) = &self.remote {
            let _ = remote.s3.release_lease(&remote.archive_a_key);
            let _ = remote.s3.release_lease(&remote.archive_b_key);
        }
        // Clean up merge work folder (contains queue/, seen_shards/, heartbeat)
        if Path::new(&self.merge_work_folder).exists() {
            fs::remove_dir_all(&self.merge_work_folder).map_err(FoldError::Io)?;
        }
        // Clean up the work paths (archives in in_process), not the original paths
        cleanup_archives(&[&self.work_a_path, &self.work_b_path])
    }
}

/// Count remaining text files in input (uses default config)
pub fn count_txt_files_remaining() -> Result<usize, FoldError> {
    count_txt_files_remaining_with_config(&StateConfig::default())
}

/// Count remaining text files in input with custom config
pub fn count_txt_files_remaining_with_config(config: &StateConfig) -> Result<usize, FoldError> {
    if let Some(remote) = &config.remote {
        return remote.count_available_txt(HEARTBEAT_GRACE_PERIOD_SECS);
    }
    count_txt_files(config.input_dir().to_str().unwrap())
}

/// Count distinct running jobs in the in_process folder
pub fn count_running_jobs_with_config(config: &StateConfig) -> Result<usize, FoldError> {
    let in_process_path = config.in_process_dir();
    
    if !in_process_path.exists() {
        if let Some(remote) = &config.remote {
            if let Ok(count) = remote.count_active_leases(HEARTBEAT_GRACE_PERIOD_SECS) {
                return Ok(count);
            }
        }
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
                    if folder_name_str.ends_with(".txt.work") || 
                       (folder_name_str.starts_with("merge_") && folder_name_str.ends_with(".work")) {
                        job_count += 1;
                    }
                }
            }
        }
    }

    if let Some(remote) = &config.remote {
        if let Ok(remote_count) = remote.count_active_leases(HEARTBEAT_GRACE_PERIOD_SECS) {
            return Ok(remote_count.max(job_count));
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
    if let Some(remote) = &config.remote {
        if let Some(job) = remote.checkout_next_txt(&config.input_dir(), HEARTBEAT_GRACE_PERIOD_SECS)? {
            return Ok(Some(job.local_path.to_string_lossy().to_string()));
        }
        return Ok(None);
    }
    find_next_txt_file(config.input_dir().to_str().unwrap())
}

/// Get the two largest archives (uses default config)
pub fn get_two_largest_archives() -> Result<Option<(String, String)>, FoldError> {
    get_two_largest_archives_with_config(&StateConfig::default())
}

/// Get the two largest archives with custom config
pub fn get_two_largest_archives_with_config(config: &StateConfig) -> Result<Option<(String, String)>, FoldError> {
    if let Some(remote) = &config.remote {
        if let Some(pair) = remote.checkout_two_archives(&config.input_dir(), HEARTBEAT_GRACE_PERIOD_SECS)? {
            return Ok(Some((
                pair.local_b.to_string_lossy().to_string(),
                pair.local_a.to_string_lossy().to_string(),
            )));
        }
        return Ok(None);
    }
    let archives = find_archives(config.input_dir().to_str().unwrap())?;
    
    if archives.len() < 2 {
        return Ok(None);
    }
    
    // Collect archives with valid metadata (ortho counts)
    let mut archives_with_counts: Vec<(String, usize)> = archives
        .into_iter()
        .filter_map(|(path, _size)| {
            load_metadata(&path).ok().map(|count| (path, count))
        })
        .collect();
    
    if archives_with_counts.len() < 2 {
        return Ok(None);
    }
    
    archives_with_counts.sort_by_key(|(_, count)| *count);
    let largest = archives_with_counts[archives_with_counts.len() - 1].0.clone();
    let second_largest = archives_with_counts[archives_with_counts.len() - 2].0.clone();
    
    Ok(Some((second_largest, largest)))
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

/// Load the optimal ortho from an archive (required - will error if missing)
pub fn load_optimal_ortho(archive_path: &str) -> Result<Ortho, FoldError> {
    let optimal_bin_path = format!("{}/optimal.bin", archive_path);
    let optimal_bytes = fs::read(&optimal_bin_path).map_err(|e| FoldError::Io(e))?;
    let (ortho, _): (Ortho, usize) = 
        bincode::decode_from_slice(&optimal_bytes, bincode::config::standard())?;
    Ok(ortho)
}

/// Find the largest archive by ortho count (uses default config)
pub fn find_largest_archive() -> Result<Option<ArchiveMetadata>, FoldError> {
    find_largest_archive_with_config(&StateConfig::default())
}

/// Find the largest archive by ortho count with custom config
pub fn find_largest_archive_with_config(config: &StateConfig) -> Result<Option<ArchiveMetadata>, FoldError> {
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
pub fn ingest_txt_file_with_config(file_path: &str, config: &StateConfig) -> Result<TxtIngestion, FoldError> {
    let (work_folder, source_txt_path, heartbeat_path, filename) = 
        setup_txt_processing(file_path, config.in_process_dir().to_str().unwrap())?;
    
    // Read the text immediately as part of ingestion
    let text = read_source_text(&source_txt_path)?;
    
    // Compute text metadata
    let word_count = count_words(&text);
    let text_preview = create_text_preview(&text, 4, 4); // First 4 and last 4 words for text blobs

    let remote = config.remote.as_ref().map(|s3| RemoteTxtHandle {
        s3: Arc::clone(s3),
        job_key: format!("input/{}.txt", filename),
    });
    
    Ok(TxtIngestion {
        work_folder,
        heartbeat_path,
        filename,
        text,
        text_preview,
        word_count,
        config: config.clone(),
        remote,
    })
}

/// Ingest archives for merging (uses default config)
pub fn ingest_archives(archive_a_path: &str, archive_b_path: &str) -> Result<ArchiveIngestion, FoldError> {
    ingest_archives_with_config(archive_a_path, archive_b_path, &StateConfig::default())
}

/// Ingest archives for merging with custom config
pub fn ingest_archives_with_config(archive_a_path: &str, archive_b_path: &str, config: &StateConfig) -> Result<ArchiveIngestion, FoldError> {
    let in_process = config.in_process_dir();
    
    // Load text metadata before moving archives
    let (word_count_a_orig, text_preview_a_orig) = load_text_metadata(archive_a_path)
        .unwrap_or_else(|_| (0, String::new()));
    let (word_count_b_orig, text_preview_b_orig) = load_text_metadata(archive_b_path)
        .unwrap_or_else(|_| (0, String::new()));
    
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
    
    let (work_a_path, work_b_path) = setup_archive_merge(archive_a_path, archive_b_path, in_process.to_str().unwrap())?;
    
    // Create merge work folder for isolated queue and seen_shards
    let merge_work_folder = in_process.join(format!("merge_{}.work", std::process::id()));
    fs::create_dir_all(&merge_work_folder).map_err(FoldError::Io)?;
    
    // Create heartbeat for merge operation
    let heartbeat_path = merge_work_folder.join("heartbeat");
    touch_heartbeat(heartbeat_path.to_str().unwrap())?;

    let remote = config.remote.as_ref().map(|s3| RemoteMergeHandle {
        s3: Arc::clone(s3),
        archive_a_key: format!(
            "input/{}",
            Path::new(archive_a_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ),
        archive_b_key: format!(
            "input/{}",
            Path::new(archive_b_path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ),
    });
    
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
        remote,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;
    
    #[test]
    fn test_save_and_load_archive() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.bin");
        let results_path = temp_dir.path().join("test_results");
        
        let interner = Interner::from_text("hello world test");
        let ortho1 = Ortho::new();
        let ortho2 = Ortho::new();
        
        // Create a DiskBackedQueue and add orthos
        let mut results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), 10).unwrap();
        results.push(ortho1.clone()).unwrap();
        results.push(ortho2).unwrap();
        
        // Save archive
        save_archive(archive_path.to_str().unwrap(), &interner, results, results_path.to_str().unwrap(), Some(&ortho1), "\"test\"", 2, "hello world ... test", 3).unwrap();
        
        // Verify archive directory exists
        assert!(archive_path.exists());
        assert!(archive_path.is_dir());
        
        // Verify interner.bin exists
        let interner_path = archive_path.join("interner.bin");
        assert!(interner_path.exists());
        
        // Load and verify interner
        let interner_bytes = fs::read(&interner_path).unwrap();
        let (loaded_interner, _): (Interner, usize) = 
            bincode::decode_from_slice(&interner_bytes, bincode::config::standard()).unwrap();
        
        assert_eq!(loaded_interner.version(), interner.version());
        assert_eq!(loaded_interner.vocabulary().len(), interner.vocabulary().len());
        
        // Verify results directory was moved
        let archive_results_path = archive_path.join("results");
        assert!(archive_results_path.exists());
        assert!(archive_results_path.is_dir());
        
        // Load results from the archive and verify by popping (len() is not reliable for reloaded queues)
        let mut loaded_results = DiskBackedQueue::new_from_path(archive_results_path.to_str().unwrap(), 10).unwrap();
        let mut count = 0;
        while loaded_results.pop().unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 2);
        
        // Verify lineage.txt exists and contains expected value
        let lineage_path = archive_path.join("lineage.txt");
        assert!(lineage_path.exists());
        let lineage = fs::read_to_string(&lineage_path).unwrap();
        assert_eq!(lineage, "\"test\"");
    }
    
    #[test]
    fn test_heartbeat_creation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let work_folder = temp_dir.path().join("test.txt.work");
        fs::create_dir_all(&work_folder).unwrap();
        
        let heartbeat_path = create_heartbeat(work_folder.to_str().unwrap()).unwrap();
        
        // Verify heartbeat file was created inside the folder
        assert!(Path::new(&heartbeat_path).exists());
        assert_eq!(heartbeat_path, work_folder.join("heartbeat").to_str().unwrap());
        
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
            setup_txt_processing(txt_path.to_str().unwrap(), in_process_dir.to_str().unwrap()).unwrap();
        
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
        cleanup_archives(&[
            archive1.to_str().unwrap(),
            archive2.to_str().unwrap()
        ]).unwrap();
        
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
}
