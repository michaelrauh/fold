use crate::{disk_backed_queue::DiskBackedQueue, interner::Interner, ortho::Ortho, FoldError};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const HEARTBEAT_GRACE_PERIOD_SECS: u64 = 600; // 10 minutes

/// Configuration for state directory locations
#[derive(Clone)]
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

fn recover_abandoned_files(input_dir: &str, in_process_dir: &str) -> Result<(), FoldError> {
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
                            
                            // Remove the abandoned work folder
                            fs::remove_dir_all(&entry_path).map_err(|e| FoldError::Io(e))?;
                            recovered_count += 1;
                        }
                    }
                }
                // If it's an archive folder with stale heartbeat, move it back to input
                else if entry_path.to_string_lossy().ends_with(".bin") {
                    let archive_name = entry_path.file_name().unwrap();
                    let input_path = Path::new(input_dir).join(archive_name);
                    // println!("[fold] Recovering stale archive: {:?} -> {:?}", entry_path, input_path);
                    fs::rename(&entry_path, &input_path).map_err(|e| FoldError::Io(e))?;
                    recovered_count += 1;
                }
                // If it's a merge_*.work folder with stale heartbeat, delete it
                else if let Some(folder_name) = entry_path.file_name() {
                    let folder_name_str = folder_name.to_str().unwrap_or("");
                    if folder_name_str.starts_with("merge_") && folder_name_str.ends_with(".work") {
                        // println!("[fold] Removing abandoned merge work folder: {}", folder_name_str);
                        fs::remove_dir_all(&entry_path).map_err(|e| FoldError::Io(e))?;
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
    
    for entry in fs::read_dir(path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();
        
        // Look for plain .txt files in input directory
        if entry_path.is_file() {
            if let Some(ext) = entry_path.extension() {
                if ext == "txt" {
                    if let Some(path_str) = entry_path.to_str() {
                        return Ok(Some(path_str.to_string()));
                    }
                }
            }
        }
    }
    
    Ok(None)
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
    ortho_count: usize
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
    
    // Create heartbeat file in the archive
    let heartbeat_path = format!("{}/heartbeat", archive_path);
    touch_heartbeat(&heartbeat_path)?;
    
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
    config: StateConfig,
}

impl TxtIngestion {
    /// Touch the heartbeat file (zero-arity as requested)
    pub fn touch_heartbeat(&self) -> Result<(), FoldError> {
        touch_heartbeat(&self.heartbeat_path)
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
        
        save_archive(archive_path.to_str().unwrap(), interner, results, &results_path, best_ortho, &lineage, ortho_count)?;
        Ok((archive_path.to_string_lossy().to_string(), lineage))
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
    original_a_path: String,
    original_b_path: String,
    merge_work_folder: String,
    heartbeat_path: String,
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
        
        save_archive(archive_path.to_str().unwrap(), interner, results, results_path, best_ortho, &merged_lineage, ortho_count)?;
        Ok((archive_path.to_string_lossy().to_string(), merged_lineage))
    }
    
    /// Cleanup original archives and merge work folder
    pub fn cleanup(self) -> Result<(), FoldError> {
        // Clean up merge work folder (contains queue/, seen_shards/, heartbeat)
        if Path::new(&self.merge_work_folder).exists() {
            fs::remove_dir_all(&self.merge_work_folder).map_err(FoldError::Io)?;
        }
        cleanup_archives(&[&self.original_a_path, &self.original_b_path])
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

/// Find the next text file to process (uses default config)
pub fn find_txt_file() -> Result<Option<String>, FoldError> {
    find_txt_file_with_config(&StateConfig::default())
}

/// Find the next text file to process with custom config
pub fn find_txt_file_with_config(config: &StateConfig) -> Result<Option<String>, FoldError> {
    find_next_txt_file(config.input_dir().to_str().unwrap())
}

/// Get the smallest and largest archives (uses default config)
pub fn get_smallest_and_largest_archives() -> Result<Option<(String, String)>, FoldError> {
    get_smallest_and_largest_archives_with_config(&StateConfig::default())
}

/// Get the smallest and largest archives with custom config
pub fn get_smallest_and_largest_archives_with_config(config: &StateConfig) -> Result<Option<(String, String)>, FoldError> {
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
    let smallest = archives_with_counts[0].0.clone();
    let largest = archives_with_counts[archives_with_counts.len() - 1].0.clone();
    
    Ok(Some((smallest, largest)))
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
    
    Ok(TxtIngestion {
        work_folder,
        heartbeat_path,
        filename,
        text,
        config: config.clone(),
    })
}

/// Ingest archives for merging (uses default config)
pub fn ingest_archives(archive_a_path: &str, archive_b_path: &str) -> Result<ArchiveIngestion, FoldError> {
    ingest_archives_with_config(archive_a_path, archive_b_path, &StateConfig::default())
}

/// Ingest archives for merging with custom config
pub fn ingest_archives_with_config(archive_a_path: &str, archive_b_path: &str, config: &StateConfig) -> Result<ArchiveIngestion, FoldError> {
    let in_process = config.in_process_dir();
    let (work_a_path, work_b_path) = setup_archive_merge(archive_a_path, archive_b_path, in_process.to_str().unwrap())?;
    
    // Create merge work folder for isolated queue and seen_shards
    let merge_work_folder = in_process.join(format!("merge_{}.work", std::process::id()));
    fs::create_dir_all(&merge_work_folder).map_err(FoldError::Io)?;
    
    // Create heartbeat for merge operation
    let heartbeat_path = merge_work_folder.join("heartbeat");
    touch_heartbeat(heartbeat_path.to_str().unwrap())?;
    
    Ok(ArchiveIngestion {
        work_a_path,
        work_b_path,
        original_a_path: archive_a_path.to_string(),
        original_b_path: archive_b_path.to_string(),
        merge_work_folder: merge_work_folder.to_string_lossy().to_string(),
        heartbeat_path: heartbeat_path.to_string_lossy().to_string(),
        config: config.clone(),
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
        save_archive(archive_path.to_str().unwrap(), &interner, results, results_path.to_str().unwrap(), Some(&ortho1), "\"test\"", 2).unwrap();
        
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
        
        // Load results from the archive
        let loaded_results = DiskBackedQueue::new_from_path(archive_results_path.to_str().unwrap(), 10).unwrap();
        assert_eq!(loaded_results.len(), 2);
        
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
}
