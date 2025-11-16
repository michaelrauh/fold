use crate::{disk_backed_queue::DiskBackedQueue, interner::Interner, ortho::Ortho, FoldError};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const HEARTBEAT_GRACE_PERIOD_SECS: u64 = 600; // 10 minutes

// Directory structure constants
const INPUT_DIR: &str = "./fold_state/input";
const IN_PROCESS_DIR: &str = "./fold_state/in_process";

/// Initialize the file system: create directories and recover abandoned files
pub fn initialize() -> Result<(), FoldError> {
    ensure_directory_exists(IN_PROCESS_DIR)?;
    recover_abandoned_files(INPUT_DIR, IN_PROCESS_DIR)?;
    Ok(())
}

pub fn recover_abandoned_files(input_dir: &str, in_process_dir: &str) -> Result<(), FoldError> {
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
                            println!("[fold] Recovered abandoned txt file: {} -> {}", folder_name_str, target_path);
                            
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
                    println!("[fold] Recovering stale archive: {:?} -> {:?}", entry_path, input_path);
                    fs::rename(&entry_path, &input_path).map_err(|e| FoldError::Io(e))?;
                    recovered_count += 1;
                }
            }
        }
    }
    
    if recovered_count > 0 {
        println!("[fold] Recovered {} abandoned file(s) from previous run", recovered_count);
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

pub fn touch_heartbeat(heartbeat_path: &str) -> Result<(), FoldError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .as_secs();
    
    fs::write(heartbeat_path, now.to_string().as_bytes()).map_err(|e| FoldError::Io(e))?;
    Ok(())
}

pub fn create_heartbeat(work_folder_path: &str) -> Result<String, FoldError> {
    // Heartbeat is now inside the work folder
    let heartbeat_path = format!("{}/heartbeat", work_folder_path);
    touch_heartbeat(&heartbeat_path)?;
    Ok(heartbeat_path)
}

pub fn create_merge_heartbeat() -> Result<String, FoldError> {
    let heartbeat_path = format!("{}/merge_{}.heartbeat", IN_PROCESS_DIR, std::process::id());
    touch_heartbeat(&heartbeat_path)?;
    Ok(heartbeat_path)
}

pub fn find_next_txt_file(input_dir: &str) -> Result<Option<String>, FoldError> {
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

pub fn find_archives(input_dir: &str) -> Result<Vec<(String, u64)>, FoldError> {
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

pub fn get_archive_path(input_file_path: &str) -> String {
    let path = Path::new(input_file_path);
    let parent = path.parent().unwrap_or(Path::new("."));
    let filename = path.file_stem().unwrap_or_default().to_str().unwrap_or("output");
    format!("{}/{}.bin", parent.display(), filename)
}

pub fn save_archive(
    archive_path: &str, 
    interner: &Interner, 
    results: &mut DiskBackedQueue, 
    results_path: &str,
    best_ortho: Option<&Ortho>,
    lineage: &str
) -> Result<(), FoldError> {
    // Flush results to ensure all are on disk
    results.flush()?;
    
    // Create archive directory
    fs::create_dir_all(archive_path).map_err(|e| FoldError::Io(e))?;
    
    // Move the DiskBackedQueue directory to the archive
    let archive_results_path = format!("{}/results", archive_path);
    if Path::new(results_path).exists() {
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
    }
    
    // Write the lineage tracking as S-expression
    let lineage_path = format!("{}/lineage.txt", archive_path);
    fs::write(lineage_path, lineage).map_err(|e| FoldError::Io(e))?;
    
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
    output.push_str(&format!("Version: {}\n", ortho.version()));
    output.push_str(&format!("Dimensions: {:?}\n", ortho.dims()));
    output.push_str(&format!("Score: (volume={}, fullness={})\n", volume, fullness));
    output.push_str("\nGeometry:\n");
    
    for line in format!("{}", ortho.display(interner)).lines() {
        output.push_str(&format!("  {}\n", line));
    }
    
    output
}

pub fn load_interner(archive_path: &str) -> Result<Interner, FoldError> {
    let interner_path = format!("{}/interner.bin", archive_path);
    let interner_bytes = fs::read(&interner_path).map_err(|e| FoldError::Io(e))?;
    let (interner, _): (Interner, usize) = 
        bincode::decode_from_slice(&interner_bytes, bincode::config::standard())?;
    Ok(interner)
}

pub fn get_results_path(archive_path: &str) -> String {
    format!("{}/results", archive_path)
}

pub fn load_lineage(archive_path: &str) -> Result<String, FoldError> {
    let lineage_path = format!("{}/lineage.txt", archive_path);
    fs::read_to_string(&lineage_path).map_err(|e| FoldError::Io(e))
}

/// Ensures a directory exists by creating it if needed
pub fn ensure_directory_exists(path: &str) -> Result<(), FoldError> {
    fs::create_dir_all(path).map_err(|e| FoldError::Io(e))
}

/// Sets up processing for a txt file: creates work folder, moves file to source.txt
/// Returns (work_folder_path, source_txt_path, heartbeat_path, filename)
pub fn setup_txt_processing(file_path: &str, in_process_dir: &str) -> Result<(String, String, String, String), FoldError> {
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
pub fn read_source_text(source_txt_path: &str) -> Result<String, FoldError> {
    fs::read_to_string(source_txt_path).map_err(|e| FoldError::Io(e))
}

/// Cleans up a txt processing work folder by removing it entirely
pub fn cleanup_txt_processing(work_folder: &str) -> Result<(), FoldError> {
    fs::remove_dir_all(work_folder).map_err(|e| FoldError::Io(e))
}

/// Sets up archive merging by moving archives to in_process directory
/// Returns (work_path_a, work_path_b)
pub fn setup_archive_merge(archive_a_path: &str, archive_b_path: &str, in_process_dir: &str) -> Result<(String, String), FoldError> {
    let archive_a_name = Path::new(archive_a_path).file_name().unwrap().to_str().unwrap();
    let archive_b_name = Path::new(archive_b_path).file_name().unwrap().to_str().unwrap();
    let work_a_path = format!("{}/{}", in_process_dir, archive_a_name);
    let work_b_path = format!("{}/{}", in_process_dir, archive_b_name);
    
    fs::rename(archive_a_path, &work_a_path).map_err(FoldError::Io)?;
    fs::rename(archive_b_path, &work_b_path).map_err(FoldError::Io)?;
    
    Ok((work_a_path, work_b_path))
}

/// Cleans up archives by removing them if they exist
pub fn cleanup_archives(archive_paths: &[&str]) -> Result<(), FoldError> {
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

/// Result of ingesting a text file, containing paths needed for processing
pub struct TxtIngestion {
    pub work_folder: String,
    pub source_txt_path: String,
    pub heartbeat_path: String,
    pub filename: String,
}

/// Result of ingesting archives for merging
pub struct ArchiveIngestion {
    pub work_a_path: String,
    pub work_b_path: String,
    pub original_a_path: String,
    pub original_b_path: String,
}

/// Find the next text file to process (uses internal INPUT_DIR)
pub fn find_txt_file() -> Result<Option<String>, FoldError> {
    find_next_txt_file(INPUT_DIR)
}

/// Find all archives in the in-process directory (uses internal IN_PROCESS_DIR)
pub fn find_all_archives() -> Result<Vec<(String, u64)>, FoldError> {
    find_archives(IN_PROCESS_DIR)
}

/// Get the smallest and largest archives, or None if less than 2 exist
pub fn get_smallest_and_largest_archives() -> Result<Option<(String, String)>, FoldError> {
    let mut archives = find_all_archives()?;
    
    if archives.len() < 2 {
        return Ok(None);
    }
    
    archives.sort_by_key(|(_, size)| *size);
    let smallest = archives[0].0.clone();
    let largest = archives[archives.len() - 1].0.clone();
    
    Ok(Some((smallest, largest)))
}

/// Ingest a text file: setup work folder and prepare for processing
pub fn ingest_txt_file(file_path: &str) -> Result<TxtIngestion, FoldError> {
    let (work_folder, source_txt_path, heartbeat_path, filename) = 
        setup_txt_processing(file_path, IN_PROCESS_DIR)?;
    
    Ok(TxtIngestion {
        work_folder,
        source_txt_path,
        heartbeat_path,
        filename,
    })
}

/// Save a text processing result as an archive
pub fn save_txt_result(
    filename: &str,
    interner: &Interner,
    results: &mut DiskBackedQueue,
    results_path: &str,
    best_ortho: Option<&Ortho>,
) -> Result<String, FoldError> {
    let lineage = format!("\"{}\"", filename);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| FoldError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
        .as_secs();
    let archive_path = format!("{}/archive_{}.bin", INPUT_DIR, timestamp);
    
    save_archive(&archive_path, interner, results, results_path, best_ortho, &lineage)?;
    Ok(archive_path)
}

/// Ingest archives for merging: move them to in-process and prepare
pub fn ingest_archives(archive_a_path: &str, archive_b_path: &str) -> Result<ArchiveIngestion, FoldError> {
    let (work_a_path, work_b_path) = setup_archive_merge(archive_a_path, archive_b_path, IN_PROCESS_DIR)?;
    
    Ok(ArchiveIngestion {
        work_a_path,
        work_b_path,
        original_a_path: archive_a_path.to_string(),
        original_b_path: archive_b_path.to_string(),
    })
}

/// Save a merged archive result
pub fn save_merge_result(
    interner: &Interner,
    results: &mut DiskBackedQueue,
    results_path: &str,
    best_ortho: Option<&Ortho>,
    lineage_a: &str,
    lineage_b: &str,
) -> Result<String, FoldError> {
    let merged_lineage = format!("({} {})", lineage_a, lineage_b);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let archive_path = format!("{}/archive_{}.bin", INPUT_DIR, timestamp);
    
    save_archive(&archive_path, interner, results, results_path, best_ortho, &merged_lineage)?;
    Ok(archive_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ortho::Ortho;
    
    #[test]
    fn test_get_archive_path() {
        let input = "./fold_state/input/test_chunk_0001.txt";
        let archive_path = get_archive_path(input);
        assert_eq!(archive_path, "./fold_state/input/test_chunk_0001.bin");
    }
    
    #[test]
    fn test_save_and_load_archive() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.bin");
        let results_path = temp_dir.path().join("test_results");
        
        let interner = Interner::from_text("hello world test");
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(2);
        
        // Create a DiskBackedQueue and add orthos
        let mut results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), 10).unwrap();
        results.push(ortho1.clone()).unwrap();
        results.push(ortho2).unwrap();
        
        // Save archive
        save_archive(archive_path.to_str().unwrap(), &interner, &mut results, results_path.to_str().unwrap(), Some(&ortho1), "\"test\"").unwrap();
        
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
