use fold::{interner::Interner, ortho::Ortho, FoldError};
use std::collections::{HashMap, VecDeque};
use std::fs;

fn main() -> Result<(), FoldError> {
    let input_dir = "./fold_state/input";
    
    println!("[fold] Starting fold processing");
    println!("[fold] Input directory: {}", input_dir);
    
    // Get all files from input directory, sorted
    let mut files = get_input_files(&input_dir)?;
    files.sort();
    
    if files.is_empty() {
        println!("[fold] No input files found in {}", input_dir);
        return Ok(());
    }
    
    println!("[fold] Found {} file(s) to process", files.len());
    
    let mut interner: Option<Interner> = None;
    let mut global_best: Option<Ortho> = None;
    let mut global_best_score: usize = 0;
    
    for file_path in files {
        println!("\n[fold] Processing file: {}", file_path);
        
        let text = fs::read_to_string(&file_path)
            .map_err(|e| FoldError::Io(e))?;
        
        // Build or extend interner
        interner = Some(if let Some(prev) = interner {
            prev.add_text(&text)
        } else {
            Interner::from_text(&text)
        });
        
        let current_interner = interner.as_ref().unwrap();
        let version = current_interner.version();
        
        println!("[fold] Interner version: {}", version);
        println!("[fold] Vocabulary size: {}", current_interner.vocabulary().len());
        
        // Initialize work queue and seen orthos set
        let mut work_queue: VecDeque<Ortho> = VecDeque::new();
        let mut seen_orthos: HashMap<usize, ()> = HashMap::new();
        
        // Seed with empty ortho
        let seed_ortho = Ortho::new(version);
        let seed_id = seed_ortho.id();
        println!("[fold] Seeding with ortho id={}, version={}", seed_id, version);
        
        seen_orthos.insert(seed_id, ());
        
        let (new_best, new_score) = update_best(global_best, global_best_score, seed_ortho.clone());
        global_best = new_best;
        global_best_score = new_score;
        
        work_queue.push_back(seed_ortho);
        
        // Process work queue until empty
        let mut processed_count = 0;
        while let Some(ortho) = work_queue.pop_front() {
            processed_count += 1;
            
            if processed_count % 1000 == 0 {
                println!("[fold] Processed {} orthos, queue size: {}, seen: {}", 
                         processed_count, work_queue.len(), seen_orthos.len());
            }
            
            // Get requirements from ortho
            let (forbidden, required) = ortho.get_requirements();
            
            // Get completions from interner
            let completions = current_interner.intersect(&required, &forbidden);
            
            // Generate child orthos
            for completion in completions {
                let children = ortho.add(completion, version);
                
                for child in children {
                    let child_id = child.id();
                    
                    if !seen_orthos.contains_key(&child_id) {
                        seen_orthos.insert(child_id, ());
                        
                        let (new_best, new_score) = update_best(global_best, global_best_score, child.clone());
                        global_best = new_best;
                        global_best_score = new_score;
                        
                        work_queue.push_back(child);
                    }
                }
            }
        }
        
        println!("[fold] Finished processing file");
        println!("[fold] Total orthos generated: {}", seen_orthos.len());
        
        print_optimal(&global_best, current_interner);
    }
    
    println!("\n[fold] All files processed");
    
    if let Some(final_interner) = interner {
        println!("\n[fold] ===== FINAL OPTIMAL ORTHO =====");
        print_optimal(&global_best, &final_interner);
    }
    
    Ok(())
}

fn get_input_files(input_dir: &str) -> Result<Vec<String>, FoldError> {
    let path = std::path::Path::new(input_dir);
    
    if !path.exists() {
        return Ok(Vec::new());
    }
    
    let mut files = Vec::new();
    
    for entry in fs::read_dir(path).map_err(|e| FoldError::Io(e))? {
        let entry = entry.map_err(|e| FoldError::Io(e))?;
        let path = entry.path();
        
        if path.is_file() {
            if let Some(path_str) = path.to_str() {
                files.push(path_str.to_string());
            }
        }
    }
    
    Ok(files)
}

fn update_best(current_best: Option<Ortho>, current_best_score: usize, candidate: Ortho) -> (Option<Ortho>, usize) {
    let candidate_score = candidate.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    
    match current_best {
        None => (Some(candidate), candidate_score),
        Some(best) => {
            if candidate_score > current_best_score {
                (Some(candidate), candidate_score)
            } else {
                (Some(best), current_best_score)
            }
        }
    }
}

fn print_optimal(optimal: &Option<Ortho>, interner: &Interner) {
    if let Some(ortho) = optimal {
        println!("\n[fold] ===== OPTIMAL ORTHO =====");
        println!("[fold] Ortho ID: {}", ortho.id());
        println!("[fold] Version: {}", ortho.version());
        println!("[fold] Dimensions: {:?}", ortho.dims());
        println!("[fold] Score: {}", ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>());
        
        println!("[fold] Payload (indices): {:?}", ortho.payload());
        
        // Print the payload strings for reference
        let payload_strings: Vec<String> = ortho.payload()
            .iter()
            .map(|opt_idx| {
                opt_idx
                    .map(|idx| interner.string_for_index(idx).to_string())
                    .unwrap_or_else(|| "·".to_string())
            })
            .collect();
        println!("[fold] Payload (strings): {:?}", payload_strings);
        
        // Pretty print the ortho geometry
        println!("[fold] Geometry:");
        for line in format!("{}", ortho).lines() {
            println!("[fold]   {}", line);
        }
        
        println!("[fold] ========================\n");
    } else {
        println!("\n[fold] No optimal ortho found\n");
    }
}
