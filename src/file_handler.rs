use crate::{disk_backed_queue::DiskBackedQueue, interner::Interner, ortho::Ortho, FoldError};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const HEARTBEAT_GRACE_PERIOD_SECS: u64 = 600; // 10 minutes

pub fn recover_abandoned_files(input_dir: &str, in_process_dir: &str) -> Result<(), FoldError> {
    let in_process_path = std::path::Path::new(in_process_dir);
    
    if !in_process_path.exists() {
        return Ok(());
    }
    
    let mut recovered_count = 0;
    
    // Check for stale heartbeat files
    for entry in fs::read_dir(in_process_path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();
        
        if entry_path.is_file() {
            if let Some(ext) = entry_path.extension() {
                // Check for abandoned .txt files
                if ext == "txt" {
                    let filename = entry_path.file_name().unwrap_or_default();
                    let target_path = format!("{}/{}", input_dir, filename.to_str().unwrap_or("recovered"));
                    fs::rename(&entry_path, &target_path).map_err(|e| FoldError::Io(e))?;
                    println!("[fold] Recovered abandoned file: {:?} -> {}", filename, target_path);
                    recovered_count += 1;
                }
                // Check for stale heartbeat files
                else if ext == "heartbeat" {
                    if is_heartbeat_stale(&entry_path)? {
                        // Find corresponding .txt file and recover it
                        let stem = entry_path.file_stem().unwrap_or_default();
                        let txt_path = in_process_path.join(format!("{}.txt", stem.to_str().unwrap_or("")));
                        
                        if txt_path.exists() {
                            let filename = txt_path.file_name().unwrap_or_default();
                            let target_path = format!("{}/{}", input_dir, filename.to_str().unwrap_or("recovered"));
                            fs::rename(&txt_path, &target_path).map_err(|e| FoldError::Io(e))?;
                            println!("[fold] Recovered file with stale heartbeat: {:?} -> {}", filename, target_path);
                            recovered_count += 1;
                        }
                        
                        // Remove the stale heartbeat file
                        fs::remove_file(&entry_path).map_err(|e| FoldError::Io(e))?;
                    }
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

pub fn create_heartbeat(in_process_path: &str) -> Result<String, FoldError> {
    let path = Path::new(in_process_path);
    let stem = path.file_stem().unwrap_or_default();
    let parent = path.parent().unwrap_or(Path::new("."));
    let heartbeat_path = format!("{}/{}.heartbeat", parent.display(), stem.to_str().unwrap_or("temp"));
    
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

pub fn find_archives(in_process_dir: &str) -> Result<Vec<(String, u64)>, FoldError> {
    let path = std::path::Path::new(in_process_dir);
    
    if !path.exists() {
        return Ok(Vec::new());
    }
    
    let mut archives = Vec::new();
    
    for entry in fs::read_dir(path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let entry_path = entry.path();
        
        if entry_path.is_dir() {
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
                        }
                        
                        if let Some(path_str) = entry_path.to_str() {
                            archives.push((path_str.to_string(), total_size));
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
    best_ortho: Option<&Ortho>
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
        save_archive(archive_path.to_str().unwrap(), &interner, &mut results, results_path.to_str().unwrap(), Some(&ortho1)).unwrap();
        
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
    }
    
    #[test]
    fn test_heartbeat_creation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();
        
        let heartbeat_path = create_heartbeat(test_file.to_str().unwrap()).unwrap();
        
        // Verify heartbeat file was created
        assert!(Path::new(&heartbeat_path).exists());
        
        // Verify heartbeat file is not stale (freshly created)
        assert!(!is_heartbeat_stale(Path::new(&heartbeat_path)).unwrap());
    }
}
