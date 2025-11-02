use fold::interner::Interner;
use fold::ortho::Ortho;
use fold::error::FoldError;
use std::collections::{VecDeque, HashSet};
use std::fs;
use std::path::Path;
use bincode::{encode_to_vec, decode_from_slice, config::standard};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "fold_single")]
#[command(about = "Single-node fold optimizer", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Add text to the interner from a file
    Ingest {
        /// Path to the text file
        path: String,
    },
    /// Run the worker loop
    Run,
    /// Process all files in the input folder
    Process {
        /// Path to the state folder (default: ./fold_state)
        #[arg(long, default_value = "./fold_state")]
        state_dir: String,
    },
}

struct ResumeFile {
    frontier: Vec<Ortho>,
    interner: Interner,
}

impl bincode::Encode for ResumeFile {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), bincode::error::EncodeError> {
        self.frontier.encode(encoder)?;
        self.interner.encode(encoder)?;
        Ok(())
    }
}

impl<Context> bincode::Decode<Context> for ResumeFile {
    fn decode<D: bincode::de::Decoder>(decoder: &mut D) -> Result<Self, bincode::error::DecodeError> {
        let frontier = Vec::<Ortho>::decode(decoder)?;
        let interner = Interner::decode(decoder)?;
        Ok(ResumeFile { frontier, interner })
    }
}

fn main() -> Result<(), FoldError> {
    let cli = Cli::parse();
    let resume_path = "fold_resume.bin";
    
    // Ensure resume file exists
    if !Path::new(resume_path).exists() {
        println!("[fold_single] No resume file found, creating blank state");
        let blank = create_blank_resume()?;
        save_resume(resume_path, &blank)?;
        println!("[fold_single] Blank resume file created at {}", resume_path);
    }
    
    match cli.command {
        Some(Commands::Ingest { path }) => {
            ingest_text(resume_path, &path)?;
        }
        Some(Commands::Run) | None => {
            run_worker(resume_path)?;
        }
        Some(Commands::Process { state_dir }) => {
            process_input_folder(&state_dir)?;
        }
    }
    
    Ok(())
}

fn ingest_text(resume_path: &str, text_path: &str) -> Result<(), FoldError> {
    println!("[fold_single] Ingesting text from {}", text_path);
    
    let text = fs::read_to_string(text_path)
        .map_err(|e| FoldError::Interner(format!("Failed to read text file: {}", e)))?;
    
    let mut resume = load_resume(resume_path)?;
    let old_interner = resume.interner.clone();
    
    println!("[fold_single] Current interner version: {}, vocab size: {}", 
             old_interner.version(), old_interner.vocabulary().len());
    
    // Add text to interner
    let new_interner = old_interner.add_text(&text);
    println!("[fold_single] New interner version: {}, vocab size: {}", 
             new_interner.version(), new_interner.vocabulary().len());
    
    // Detect affected orthos and update frontier
    let affected = detect_affected_orthos(&resume.frontier, &old_interner, &new_interner);
    println!("[fold_single] Detected {} affected orthos from vocabulary changes", affected.len());
    
    // Add affected orthos to frontier (they become new starting points)
    let mut frontier_set: HashSet<usize> = resume.frontier.iter().map(|o| o.id()).collect();
    for ortho in affected {
        if !frontier_set.contains(&ortho.id()) {
            frontier_set.insert(ortho.id());
            resume.frontier.push(ortho);
        }
    }
    
    resume.interner = new_interner;
    
    println!("[fold_single] Updated frontier size: {}", resume.frontier.len());
    save_resume(resume_path, &resume)?;
    println!("[fold_single] Saved updated resume file with merged interner and expanded frontier");
    
    Ok(())
}

fn process_input_folder(state_dir: &str) -> Result<(), FoldError> {
    use std::path::PathBuf;
    
    // Setup state folder structure
    let state_path = PathBuf::from(state_dir);
    let input_dir = state_path.join("input");
    let resume_path = state_path.join("fold_resume.bin");
    
    // Create directories if they don't exist
    fs::create_dir_all(&input_dir)
        .map_err(|e| FoldError::Io(e))?;
    fs::create_dir_all(&state_path)
        .map_err(|e| FoldError::Io(e))?;
    
    println!("[fold_single] State directory: {}", state_path.display());
    println!("[fold_single] Input directory: {}", input_dir.display());
    
    // Ensure resume file exists
    if !resume_path.exists() {
        println!("[fold_single] No resume file found, creating blank state");
        let blank = create_blank_resume()?;
        save_resume(resume_path.to_str().unwrap(), &blank)?;
        println!("[fold_single] Blank resume file created at {}", resume_path.display());
    }
    
    // Get list of input files
    let mut input_files: Vec<PathBuf> = Vec::new();
    if input_dir.exists() {
        for entry in fs::read_dir(&input_dir).map_err(|e| FoldError::Io(e))? {
            let entry = entry.map_err(|e| FoldError::Io(e))?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("txt") {
                input_files.push(path);
            }
        }
    }
    
    input_files.sort();
    let total_files = input_files.len();
    
    if total_files == 0 {
        println!("[fold_single] No input files found in {}", input_dir.display());
        return Ok(());
    }
    
    println!("[fold_single] Found {} input files to process", total_files);
    
    // Process each file
    for (index, file_path) in input_files.iter().enumerate() {
        let file_name = file_path.file_name().unwrap().to_str().unwrap();
        let progress = ((index + 1) as f64 / total_files as f64) * 100.0;
        
        println!("\n[fold_single] ===== Processing file {}/{} ({:.1}%) =====", 
                 index + 1, total_files, progress);
        println!("[fold_single] File: {}", file_name);
        
        // Ingest the file
        println!("[fold_single] Step 1/3: Ingesting...");
        ingest_text(resume_path.to_str().unwrap(), file_path.to_str().unwrap())?;
        
        // Run the worker
        println!("[fold_single] Step 2/3: Running worker...");
        run_worker(resume_path.to_str().unwrap())?;
        
        // Delete the file
        println!("[fold_single] Step 3/3: Deleting processed file...");
        fs::remove_file(file_path)
            .map_err(|e| FoldError::Io(e))?;
        println!("[fold_single] Deleted: {}", file_name);
    }
    
    println!("\n[fold_single] ===== Processing complete! =====");
    println!("[fold_single] Processed {} files (100.0%)", total_files);
    
    Ok(())
}

fn run_worker(resume_path: &str) -> Result<(), FoldError> {
    // Load resume file
    let mut resume = load_resume(resume_path)?;
    
    println!("[fold_single] Loaded frontier with {} orthos, interner version {}, vocab size: {}", 
             resume.frontier.len(), resume.interner.version(), resume.interner.vocabulary().len());
    
    // Initialize work queue from frontier
    let mut work_queue: VecDeque<Ortho> = resume.frontier.iter().cloned().collect();
    
    // Add seed ortho to work queue
    let seed = Ortho::new(resume.interner.version());
    println!("[fold_single] Adding seed ortho to work queue");
    work_queue.push_back(seed);
    
    // Process work queue
    let mut frontier_set: HashSet<usize> = resume.frontier.iter().map(|o| o.id()).collect();
    let mut new_frontier: Vec<Ortho> = resume.frontier.clone();
    let mut processed = 0;
    
    println!("[fold_single] Starting worker loop with {} items in queue", work_queue.len());
    
    while let Some(ortho) = work_queue.pop_front() {
        processed += 1;
        
        if processed % 100 == 0 {
            println!("[fold_single] Processed {} orthos, queue size: {}, frontier size: {}", 
                     processed, work_queue.len(), new_frontier.len());
        }
        
        // Get requirements for this ortho
        let (forbidden, required) = ortho.get_requirements();
        let completions = resume.interner.intersect(&required, &forbidden);
        let version = resume.interner.version();
        
        // Generate children
        for completion in completions {
            let children = ortho.add(completion, version);
            for child in children {
                let child_id = child.id();
                if !frontier_set.contains(&child_id) {
                    frontier_set.insert(child_id);
                    new_frontier.push(child.clone());
                    work_queue.push_back(child);
                }
            }
        }
    }
    
    println!("[fold_single] Worker loop complete. Processed {} orthos", processed);
    println!("[fold_single] Final frontier size before deduplication: {}", new_frontier.len());
    
    // Deduplicate frontier using prefix rule
    new_frontier = deduplicate_frontier(new_frontier);
    println!("[fold_single] Final frontier size after deduplication: {}", new_frontier.len());
    
    // Save frontier and interner
    resume.frontier = new_frontier;
    save_resume(resume_path, &resume)?;
    println!("[fold_single] Saved resume file to {}", resume_path);
    
    Ok(())
}

fn detect_affected_orthos(
    frontier: &[Ortho],
    old_interner: &Interner,
    new_interner: &Interner
) -> Vec<Ortho> {
    let old_vocab_len = old_interner.vocabulary().len();
    let new_vocab_len = new_interner.vocabulary().len();
    
    // No changes? Return empty
    if old_vocab_len == new_vocab_len {
        return Vec::new();
    }
    
    let mut affected = Vec::new();
    
    for ortho in frontier {
        let (forbidden, required) = ortho.get_requirements();
        let forbidden_set: HashSet<usize> = forbidden.iter().copied().collect();
        
        // Check for new vocabulary additions
        let mut has_new_completions = false;
        
        if required.is_empty() {
            // New vocab always creates completions for empty requirements
            for i in old_vocab_len..new_vocab_len {
                if !forbidden_set.contains(&i) {
                    has_new_completions = true;
                    break;
                }
            }
        } else {
            // Check each required prefix
            for prefix in &required {
                // Use existing interner method to detect differences
                let diffs = old_interner.differing_completions_indices_up_to_vocab(new_interner, prefix);
                
                if !diffs.is_empty() {
                    has_new_completions = true;
                    break;
                }
                
                // Check new vocabulary indices
                if let Some(high_bs) = new_interner.completions_for_prefix(prefix) {
                    for idx in old_vocab_len..new_vocab_len {
                        if !forbidden_set.contains(&idx) && high_bs.contains(idx) {
                            has_new_completions = true;
                            break;
                        }
                    }
                }
            }
        }
        
        if has_new_completions {
            affected.push(ortho.clone());
        }
    }
    
    affected
}

fn create_blank_resume() -> Result<ResumeFile, FoldError> {
    // Create an interner from empty text
    let interner = Interner::from_text("");
    Ok(ResumeFile {
        frontier: vec![],
        interner,
    })
}

fn load_resume(path: &str) -> Result<ResumeFile, FoldError> {
    let bytes = fs::read(path)
        .map_err(|e| FoldError::Interner(format!("Failed to read resume file: {}", e)))?;
    let (resume, _len): (ResumeFile, _) = decode_from_slice(&bytes, standard())
        .map_err(|e| FoldError::Interner(format!("Failed to decode resume file: {}", e)))?;
    Ok(resume)
}

fn save_resume(path: &str, resume: &ResumeFile) -> Result<(), FoldError> {
    let bytes = encode_to_vec(resume, standard())
        .map_err(|e| FoldError::Interner(format!("Failed to encode resume file: {}", e)))?;
    fs::write(path, bytes)
        .map_err(|e| FoldError::Interner(format!("Failed to write resume file: {}", e)))?;
    Ok(())
}

fn deduplicate_frontier(frontier: Vec<Ortho>) -> Vec<Ortho> {
    // Group orthos by shape (dims)
    use std::collections::HashMap;
    let mut by_shape: HashMap<Vec<usize>, Vec<Ortho>> = HashMap::new();
    
    for ortho in frontier {
        by_shape.entry(ortho.dims().clone()).or_insert_with(Vec::new).push(ortho);
    }
    
    let mut result = Vec::new();
    
    for (_shape, orthos) in by_shape {
        // For each ortho, check if it's a prefix of another (non-lead node detection)
        let mut to_keep = Vec::new();
        
        for (i, ortho) in orthos.iter().enumerate() {
            let mut is_prefix = false;
            
            // Check if this ortho is a prefix of any other ortho with same shape
            for (j, other) in orthos.iter().enumerate() {
                if i != j && is_canonicalized_prefix(ortho, other) {
                    is_prefix = true;
                    break;
                }
            }
            
            if !is_prefix {
                to_keep.push(ortho.clone());
            }
        }
        
        result.extend(to_keep);
    }
    
    result
}

fn is_canonicalized_prefix(candidate: &Ortho, other: &Ortho) -> bool {
    // Check if candidate's payload is a prefix of other's payload
    // Both are already canonicalized on construction
    
    // Shape must match for prefix check
    if candidate.dims() != other.dims() {
        return false;
    }
    
    let candidate_payload = candidate.payload();
    let other_payload = other.payload();
    
    if candidate_payload.len() > other_payload.len() {
        return false;
    }
    
    // Check if all filled positions in candidate match other
    for (i, val) in candidate_payload.iter().enumerate() {
        if val.is_some() && val != &other_payload[i] {
            return false;
        }
    }
    
    // Make sure candidate has fewer filled positions than other
    let candidate_filled = candidate_payload.iter().filter(|v| v.is_some()).count();
    let other_filled = other_payload.iter().filter(|v| v.is_some()).count();
    
    candidate_filled < other_filled
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_create_blank_resume() {
        let resume = create_blank_resume().expect("Should create blank resume");
        assert_eq!(resume.frontier.len(), 0);
        assert_eq!(resume.interner.version(), 1);
        assert_eq!(resume.interner.vocabulary().len(), 0);
    }
    
    #[test]
    fn test_is_canonicalized_prefix_different_filled_counts() {
        // Create two orthos where one is a prefix of the other
        let interner = Interner::from_text("a b c");
        let version = interner.version();
        
        let ortho1 = Ortho::new(version);
        let ortho1 = ortho1.add(0, version).pop().unwrap(); // Add 'a'
        
        let ortho2 = ortho1.clone();
        let ortho2 = ortho2.add(1, version).pop().unwrap(); // Add 'b'
        
        // ortho1 should be a prefix of ortho2
        assert!(is_canonicalized_prefix(&ortho1, &ortho2));
        assert!(!is_canonicalized_prefix(&ortho2, &ortho1));
    }
    
    #[test]
    fn test_is_canonicalized_prefix_same_filled_counts() {
        // Two orthos with same number of filled positions are not prefixes
        let interner = Interner::from_text("a b c");
        let version = interner.version();
        
        let ortho1 = Ortho::new(version);
        let ortho1 = ortho1.add(0, version).pop().unwrap(); // Add 'a'
        
        let ortho2 = Ortho::new(version);
        let ortho2 = ortho2.add(1, version).pop().unwrap(); // Add 'b'
        
        assert!(!is_canonicalized_prefix(&ortho1, &ortho2));
        assert!(!is_canonicalized_prefix(&ortho2, &ortho1));
    }
    
    #[test]
    fn test_deduplicate_frontier_removes_prefixes() {
        let interner = Interner::from_text("a b c");
        let version = interner.version();
        
        let ortho1 = Ortho::new(version);
        let ortho1 = ortho1.add(0, version).pop().unwrap(); // Add 'a'
        
        let ortho2 = ortho1.clone();
        let ortho2 = ortho2.add(1, version).pop().unwrap(); // Add 'b' (prefix of ortho1)
        
        let ortho3 = Ortho::new(version);
        let ortho3 = ortho3.add(2, version).pop().unwrap(); // Add 'c' (different)
        
        let frontier = vec![ortho1, ortho2, ortho3];
        let deduplicated = deduplicate_frontier(frontier);
        
        // Should keep ortho2 and ortho3, remove ortho1 (prefix of ortho2)
        assert_eq!(deduplicated.len(), 2);
    }
    
    #[test]
    fn test_save_and_load_resume() {
        use std::fs;
        let test_path = "/tmp/test_resume.bin";
        
        // Clean up any existing file
        let _ = fs::remove_file(test_path);
        
        let interner = Interner::from_text("hello world");
        let version = interner.version();
        let ortho = Ortho::new(version);
        let ortho = ortho.add(0, version).pop().unwrap();
        
        let resume = ResumeFile {
            frontier: vec![ortho.clone()],
            interner: interner.clone(),
        };
        
        save_resume(test_path, &resume).expect("Should save resume");
        let loaded = load_resume(test_path).expect("Should load resume");
        
        assert_eq!(loaded.frontier.len(), 1);
        assert_eq!(loaded.interner.version(), interner.version());
        assert_eq!(loaded.interner.vocabulary().len(), interner.vocabulary().len());
        
        // Clean up
        let _ = fs::remove_file(test_path);
    }
}
