use fold::interner::Interner;
use fold::ortho::Ortho;
use fold::error::FoldError;
use std::collections::{VecDeque, HashSet};
use std::fs;
use bincode::{encode_to_vec, decode_from_slice, config::standard};

// Hardcoded state directory location
const STATE_DIR: &str = "./fold_state";

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
    // Simply process the input folder at the hardcoded location
    process_input_folder(STATE_DIR)?;
    
    Ok(())
}

fn ingest_text(resume_path: &str, text_path: &str) -> Result<(), FoldError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let start_time = SystemTime::now();
    let timestamp = start_time.duration_since(UNIX_EPOCH).unwrap().as_secs();
    
    println!("[fold_single][ingest] Starting ingest at timestamp {}", timestamp);
    println!("[fold_single][ingest] Reading text from {}", text_path);
    
    let text = fs::read_to_string(text_path)
        .map_err(|e| FoldError::Interner(format!("Failed to read text file: {}", e)))?;
    
    let text_len = text.len();
    println!("[fold_single][ingest] Read {} characters from file", text_len);
    
    let mut resume = load_resume(resume_path)?;
    let old_interner = resume.interner.clone();
    
    println!("[fold_single][ingest] Current state: interner v{}, vocab size: {}, frontier size: {}", 
             old_interner.version(), old_interner.vocabulary().len(), resume.frontier.len());
    
    // Add text to interner
    println!("[fold_single][ingest] Merging text into interner...");
    let new_interner = old_interner.add_text(&text);
    let vocab_added = new_interner.vocabulary().len() - old_interner.vocabulary().len();
    println!("[fold_single][ingest] New interner v{}, vocab size: {} (+{} new words)", 
             new_interner.version(), new_interner.vocabulary().len(), vocab_added);
    
    // Detect affected orthos and update frontier
    println!("[fold_single][ingest] Detecting affected orthos...");
    let affected = detect_affected_orthos(&resume.frontier, &old_interner, &new_interner);
    println!("[fold_single][ingest] Detected {} affected orthos from vocabulary changes", affected.len());
    
    // Add affected orthos to frontier (they become new starting points)
    let mut frontier_set: HashSet<usize> = resume.frontier.iter().map(|o| o.id()).collect();
    let mut added_count = 0;
    for ortho in affected {
        if !frontier_set.contains(&ortho.id()) {
            frontier_set.insert(ortho.id());
            resume.frontier.push(ortho);
            added_count += 1;
        }
    }
    
    resume.interner = new_interner;
    
    println!("[fold_single][ingest] Added {} new orthos to frontier (total: {})", added_count, resume.frontier.len());
    println!("[fold_single][ingest] Saving checkpoint...");
    save_resume(resume_path, &resume)?;
    
    let end_time = SystemTime::now();
    let end_timestamp = end_time.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let duration = end_time.duration_since(start_time).unwrap().as_secs();
    println!("[fold_single][ingest] CHECKPOINT SAVED at timestamp {} (duration: {}s)", end_timestamp, duration);
    println!("[fold_single][ingest] Ingest complete - safe to stop before next stage");
    
    Ok(())
}

fn process_input_folder(state_dir: &str) -> Result<(), FoldError> {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let batch_start_time = SystemTime::now();
    let batch_start_timestamp = batch_start_time.duration_since(UNIX_EPOCH).unwrap().as_secs();
    
    println!("[fold_single][process] ========================================");
    println!("[fold_single][process] BATCH PROCESSING START");
    println!("[fold_single][process] Timestamp: {}", batch_start_timestamp);
    println!("[fold_single][process] ========================================");
    
    // Setup state folder structure
    let state_path = PathBuf::from(state_dir);
    let input_dir = state_path.join("input");
    let resume_path = state_path.join("fold_resume.bin");
    
    // Create directories if they don't exist
    fs::create_dir_all(&input_dir)
        .map_err(|e| FoldError::Io(e))?;
    fs::create_dir_all(&state_path)
        .map_err(|e| FoldError::Io(e))?;
    
    println!("[fold_single][process] State directory: {}", state_path.display());
    println!("[fold_single][process] Input directory: {}", input_dir.display());
    
    // Ensure resume file exists
    if !resume_path.exists() {
        println!("[fold_single][process] No resume file found, creating blank state");
        let blank = create_blank_resume()?;
        save_resume(resume_path.to_str().unwrap(), &blank)?;
        println!("[fold_single][process] Blank resume file created at {}", resume_path.display());
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
        println!("[fold_single][process] No input files found in {}", input_dir.display());
        println!("[fold_single][process] Batch processing complete (0 files)");
        return Ok(());
    }
    
    println!("[fold_single][process] Found {} input files to process", total_files);
    println!("[fold_single][process] ========================================\n");
    
    // Process each file
    for (index, file_path) in input_files.iter().enumerate() {
        let file_name = file_path.file_name().unwrap().to_str().unwrap();
        let file_num = index + 1;
        let overall_progress = (file_num as f64 / total_files as f64) * 100.0;
        
        let file_start_time = SystemTime::now();
        let file_start_timestamp = file_start_time.duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        println!("[fold_single][process] ========================================");
        println!("[fold_single][process] FILE {}/{} ({:.1}% complete)", file_num, total_files, overall_progress);
        println!("[fold_single][process] Name: {}", file_name);
        println!("[fold_single][process] Timestamp: {}", file_start_timestamp);
        println!("[fold_single][process] ========================================\n");
        
        // Ingest the file
        println!("[fold_single][process] >>> Stage 1/3: INGEST <<<");
        ingest_text(resume_path.to_str().unwrap(), file_path.to_str().unwrap())?;
        println!("");
        
        // Run the worker with file context
        println!("[fold_single][process] >>> Stage 2/3: RUN WORKER <<<");
        run_worker(resume_path.to_str().unwrap(), file_name, file_num, total_files)?;
        println!("");
        
        // Delete the file
        println!("[fold_single][process] >>> Stage 3/3: CLEANUP <<<");
        let cleanup_timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        println!("[fold_single][process] Deleting processed file at timestamp {}...", cleanup_timestamp);
        fs::remove_file(file_path)
            .map_err(|e| FoldError::Io(e))?;
        println!("[fold_single][process] Deleted: {}", file_name);
        
        let file_end_time = SystemTime::now();
        let file_duration = file_end_time.duration_since(file_start_time).unwrap().as_secs();
        println!("[fold_single][process] File processing time: {}s\n", file_duration);
    }
    
    let batch_end_time = SystemTime::now();
    let batch_end_timestamp = batch_end_time.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let batch_duration = batch_end_time.duration_since(batch_start_time).unwrap().as_secs();
    
    println!("[fold_single][process] ========================================");
    println!("[fold_single][process] BATCH PROCESSING COMPLETE");
    println!("[fold_single][process] Files processed: {}", total_files);
    println!("[fold_single][process] Total time: {}s", batch_duration);
    println!("[fold_single][process] End timestamp: {}", batch_end_timestamp);
    println!("[fold_single][process] ========================================");
    
    Ok(())
}

fn run_worker(resume_path: &str, file_name: &str, file_num: usize, total_files: usize) -> Result<(), FoldError> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let start_time = SystemTime::now();
    let timestamp = start_time.duration_since(UNIX_EPOCH).unwrap().as_secs();
    
    println!("[fold_single][run] Starting worker at timestamp {}", timestamp);
    
    // Load resume file
    let mut resume = load_resume(resume_path)?;
    
    println!("[fold_single][run] Loaded state: frontier {} orthos, interner v{}, vocab size: {}", 
             resume.frontier.len(), resume.interner.version(), resume.interner.vocabulary().len());
    
    // Initialize work queue from frontier
    let mut work_queue: VecDeque<Ortho> = resume.frontier.iter().cloned().collect();
    
    // Add seed ortho to work queue
    let seed = Ortho::new(resume.interner.version());
    println!("[fold_single][run] Adding seed ortho to explore vocabulary at origin");
    work_queue.push_back(seed);
    
    // Process work queue
    let mut frontier_set: HashSet<usize> = resume.frontier.iter().map(|o| o.id()).collect();
    let mut new_frontier: Vec<Ortho> = resume.frontier.clone();
    let mut processed = 0;
    let total_initial = work_queue.len();
    
    println!("[fold_single][run] Starting worker loop with {} items in initial queue", total_initial);
    println!("[fold_single][run] IMPORTANT: Adding {} orthos to frontier (pausing 3 seconds before continuing)...", total_initial);
    std::thread::sleep(std::time::Duration::from_secs(3));
    
    const FRONTIER_COMPACTION_THRESHOLD: usize = 10_000_000; // 10 million orthos
    const COMPACTION_CHECK_INTERVAL: usize = 100; // Check every 100 iterations
    
    while let Some(ortho) = work_queue.pop_front() {
        processed += 1;
        
        // Check if frontier is getting too large and compact if needed
        if processed % COMPACTION_CHECK_INTERVAL == 0 && new_frontier.len() > FRONTIER_COMPACTION_THRESHOLD {
            println!("[fold_single][run] File {}/{} ({}): Frontier size {} exceeds threshold {}, compacting...", 
                     file_num, total_files, file_name, new_frontier.len(), FRONTIER_COMPACTION_THRESHOLD);
            let pre_compact_size = new_frontier.len();
            new_frontier = deduplicate_frontier(new_frontier, file_name, file_num, total_files);
            let post_compact_size = new_frontier.len();
            frontier_set.clear();
            frontier_set = new_frontier.iter().map(|o| o.id()).collect();
            println!("[fold_single][run] File {}/{} ({}): Compaction complete: {} -> {} orthos (removed {})", 
                     file_num, total_files, file_name, pre_compact_size, post_compact_size, pre_compact_size - post_compact_size);
        }
        
        // Report progress every 1000 orthos (less chatty)
        if processed % 1000 == 0 {
            let progress_pct = if total_initial > 0 { 
                (processed as f64 / total_initial as f64 * 100.0).min(100.0)
            } else { 0.0 };
            println!("[fold_single][run] File {}/{} ({}): processed {}/{} orthos ({:.1}%), queue: {}, frontier: {}", 
                     file_num, total_files, file_name, processed, total_initial, progress_pct, work_queue.len(), new_frontier.len());
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
    
    println!("[fold_single][run] File {}/{} ({}): Worker loop complete. Processed {} orthos total", 
             file_num, total_files, file_name, processed);
    println!("[fold_single][run] File {}/{} ({}): Frontier before deduplication: {} orthos", 
             file_num, total_files, file_name, new_frontier.len());
    
    // Deduplicate frontier using prefix rule
    println!("[fold_single][run] File {}/{} ({}): Deduplicating frontier...", 
             file_num, total_files, file_name);
    new_frontier = deduplicate_frontier(new_frontier, file_name, file_num, total_files);
    println!("[fold_single][run] File {}/{} ({}): Frontier after deduplication: {} orthos", 
             file_num, total_files, file_name, new_frontier.len());
    
    // Save frontier and interner
    println!("[fold_single][run] Saving checkpoint...");
    resume.frontier = new_frontier;
    save_resume(resume_path, &resume)?;
    
    let end_time = SystemTime::now();
    let end_timestamp = end_time.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let duration = end_time.duration_since(start_time).unwrap().as_secs();
    println!("[fold_single][run] CHECKPOINT SAVED at timestamp {} (duration: {}s)", end_timestamp, duration);
    println!("[fold_single][run] Run complete - safe to stop before next stage");
    
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

// Trie node for fast prefix detection
#[derive(Default)]
struct TrieNode {
    children: std::collections::HashMap<Option<usize>, TrieNode>,
    is_terminal: bool, // Marks if a complete ortho ends at this node
}

struct PayloadTrie {
    root: TrieNode,
}

impl PayloadTrie {
    fn new() -> Self {
        PayloadTrie {
            root: TrieNode { children: std::collections::HashMap::new(), is_terminal: false },
        }
    }
    
    // Insert a payload into the trie, marking the end as terminal
    fn insert(&mut self, payload: &[Option<usize>]) {
        let mut node = &mut self.root;
        // Only traverse filled positions
        for &value in payload.iter().filter(|v| v.is_some()) {
            node = node.children.entry(value).or_insert_with(|| TrieNode { 
                children: std::collections::HashMap::new(), 
                is_terminal: false 
            });
        }
        // Mark this node as terminal (an ortho ends here)
        node.is_terminal = true;
    }
    
    // Check if payload is a proper prefix (continues in trie beyond this point to another terminal)
    fn is_prefix_of_longer(&self, payload: &[Option<usize>]) -> bool {
        let mut node = &self.root;
        
        // Only traverse filled positions
        for &value in payload.iter().filter(|v| v.is_some()) {
            match node.children.get(&value) {
                Some(child) => node = child,
                None => return false, // No matching path
            }
        }
        
        // After traversing payload, check if there are any children
        // AND that we haven't reached a terminal (this would mean exact match, not a prefix)
        // We're looking for cases where the path continues to another terminal node
        !node.children.is_empty()
    }
}

fn deduplicate_frontier(frontier: Vec<Ortho>, file_name: &str, file_num: usize, total_files: usize) -> Vec<Ortho> {
    // OPTIMIZATION: Group orthos by origin (first payload item) first, then by shape within each origin
    // This dramatically reduces the comparison pool since prefixes must share the same origin
    use std::collections::HashMap;
    
    let total_orthos = frontier.len();
    println!("[fold_single][run] File {}/{} ({}): Grouping {} orthos by origin...", 
             file_num, total_files, file_name, total_orthos);
    
    // Group by origin (first filled payload item)
    let mut by_origin: HashMap<Option<usize>, Vec<Ortho>> = HashMap::new();
    
    for ortho in frontier {
        let origin = ortho.payload().get(0).and_then(|v| *v);
        by_origin.entry(origin).or_insert_with(Vec::new).push(ortho);
    }
    
    let num_origins = by_origin.len();
    println!("[fold_single][run] File {}/{} ({}): Found {} unique origins", 
             file_num, total_files, file_name, num_origins);
    
    let mut result = Vec::new();
    let mut processed_origins = 0;
    
    // Process each origin group
    for (_origin, orthos_in_origin) in by_origin {
        processed_origins += 1;
        
        if processed_origins % 10 == 0 || processed_origins == num_origins {
            let progress_pct = (processed_origins as f64 / num_origins as f64) * 100.0;
            println!("[fold_single][run] File {}/{} ({}): Processing origin {}/{} ({:.1}%)", 
                     file_num, total_files, file_name, processed_origins, num_origins, progress_pct);
        }
        
        // Now group by shape within this origin
        let mut by_shape: HashMap<Vec<usize>, Vec<Ortho>> = HashMap::new();
        
        for ortho in orthos_in_origin {
            by_shape.entry(ortho.dims().clone()).or_insert_with(Vec::new).push(ortho);
        }
        
        // Deduplicate within each shape group using TRIE
        for (_shape, mut orthos) in by_shape {
            // Sort by number of filled positions (ascending)
            orthos.sort_by_key(|o| {
                o.payload().iter().filter(|v| v.is_some()).count()
            });
            
            // Build trie with ALL orthos
            let mut trie = PayloadTrie::new();
            for ortho in &orthos {
                trie.insert(ortho.payload());
            }
            
            // Check each ortho to see if it's a prefix of a fuller ortho
            for ortho in orthos {
                // Use trie to check if this payload is a proper prefix
                // (i.e., the trie path continues beyond this payload)
                if trie.is_prefix_of_longer(ortho.payload()) {
                    // This ortho's payload is a prefix of a longer one, skip it
                    continue;
                }
                
                result.push(ortho);
            }
        }
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

// Optimized version that accepts pre-calculated filled counts
fn is_canonicalized_prefix_fast(candidate: &Ortho, other: &Ortho, _candidate_filled: usize, _other_filled: usize) -> bool {
    // Shape already guaranteed to match by grouping
    // Filled count comparison already done by caller (candidate_filled < other_filled guaranteed)
    
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
    
    true
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
        let deduplicated = deduplicate_frontier(frontier, "test_file.txt", 1, 1);
        
        // Should keep ortho2 and ortho3, remove ortho1 (prefix of ortho2)
        assert_eq!(deduplicated.len(), 2);
    }
    
    #[test]
    fn test_trie_prefix_detection() {
        // Test that trie correctly identifies prefixes
        let interner = Interner::from_text("a b c");
        let version = interner.version();
        
        let ortho1 = Ortho::new(version).add(0, version).pop().unwrap(); // [a]
        let ortho2 = ortho1.clone().add(1, version).pop().unwrap(); // [a, b]
        let ortho3 = ortho2.clone().add(2, version).pop().unwrap(); // [a, b, c]
        
        let frontier = vec![ortho1.clone(), ortho2.clone(), ortho3.clone()];
        let deduplicated = deduplicate_frontier(frontier, "test", 1, 1);
        
        // Should only keep ortho3 (the longest), removing ortho1 and ortho2
        assert_eq!(deduplicated.len(), 1);
        assert_eq!(deduplicated[0].payload(), ortho3.payload());
    }
    
    #[test]
    fn test_trie_divergent_payloads_not_prefixes() {
        // Test that divergent payloads are NOT treated as prefixes
        let interner = Interner::from_text("a b c d");
        let version = interner.version();
        
        let ortho1 = Ortho::new(version).add(0, version).pop().unwrap(); // [a]
        let ortho2 = ortho1.clone().add(1, version).pop().unwrap(); // [a, b]
        let ortho3 = ortho1.clone().add(2, version).pop().unwrap(); // [a, c] - diverges!
        
        let frontier = vec![ortho1.clone(), ortho2.clone(), ortho3.clone()];
        let deduplicated = deduplicate_frontier(frontier, "test", 1, 1);
        
        // Should remove ortho1 (prefix of both ortho2 and ortho3)
        // But keep both ortho2 and ortho3 (they diverge, neither is prefix of other)
        assert_eq!(deduplicated.len(), 2);
        
        // Verify both ortho2 and ortho3 remain
        let payloads: Vec<_> = deduplicated.iter().map(|o| o.payload()).collect();
        assert!(payloads.contains(&ortho2.payload()));
        assert!(payloads.contains(&ortho3.payload()));
    }
    
    #[test]
    fn test_trie_same_start_different_length_not_all_prefixes() {
        // Test edge case: [a] and [a,b] where both start with 'a'
        // but only [a] should be marked as prefix
        let interner = Interner::from_text("a b");
        let version = interner.version();
        
        let ortho_short = Ortho::new(version).add(0, version).pop().unwrap(); // [a]
        let ortho_long = ortho_short.clone().add(1, version).pop().unwrap(); // [a, b]
        
        let frontier = vec![ortho_short.clone(), ortho_long.clone()];
        let deduplicated = deduplicate_frontier(frontier, "test", 1, 1);
        
        // Should only keep ortho_long, removing ortho_short (prefix)
        assert_eq!(deduplicated.len(), 1);
        assert_eq!(deduplicated[0].payload(), ortho_long.payload());
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
