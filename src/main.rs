use fold::ortho::Ortho;
use fold::FoldError;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), FoldError> {
    let state_dir = std::env::var("FOLD_STATE_DIR").unwrap_or_else(|_| "./fold_state".to_string());
    let input_dir = PathBuf::from(&state_dir).join("input");

    if !input_dir.exists() {
        eprintln!("Error: Input directory does not exist: {:?}", input_dir);
        eprintln!("Run stage.sh to create input files first.");
        return Ok(());
    }

    // Collect and sort input files
    let mut input_files: Vec<PathBuf> = fs::read_dir(&input_dir)
        .map_err(|e| FoldError::Io(e))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("txt") {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    input_files.sort();

    if input_files.is_empty() {
        println!("No input files found in {:?}", input_dir);
        return Ok(());
    }

    println!("Found {} input files", input_files.len());

    // Track optimal ortho and seen IDs across all files
    let mut optimal_ortho: Option<Ortho> = None;
    let mut seen_ids = HashSet::new();
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Process each file
    for (file_idx, file_path) in input_files.iter().enumerate() {
        println!("\n[main] Processing file {}/{}: {:?}", 
                 file_idx + 1, input_files.len(), file_path);
        
        // Read file content
        let text = fs::read_to_string(file_path)
            .map_err(|e| FoldError::Io(e))?;
        
        println!("[main] File size: {} bytes", text.len());
        
        // Build or update interner
        interner = Some(if let Some(prev_interner) = interner {
            prev_interner.add_text(&text)
        } else {
            fold::interner::Interner::from_text(&text)
        });
        
        let current_interner = interner.as_ref().unwrap();
        let version = current_interner.version();
        
        println!("[main] Created interner version {}, vocabulary size: {}", 
                 version, current_interner.vocabulary().len());
        
        // Create seed ortho and work queue
        let seed_ortho = Ortho::new(version);
        let mut work_queue: VecDeque<Ortho> = VecDeque::new();
        work_queue.push_back(seed_ortho);
        
        println!("[main] Work queue initialized with {} ortho(s)", work_queue.len());
        
        // Worker loop: process until queue is empty
        let mut processed = 0;
        let mut generated = 0;
        
        while let Some(ortho) = work_queue.pop_front() {
            processed += 1;
            
            // Get requirements for this ortho
            let (forbidden, required) = ortho.get_requirements();
            
            // Get completions from interner
            let completions = current_interner.intersect(&required, &forbidden);
            
            // Generate children
            for completion in completions {
                let children = ortho.add(completion, version);
                for child in children {
                    let child_id = child.id();
                    // Only add to queue if never seen before
                    if !seen_ids.contains(&child_id) {
                        seen_ids.insert(child_id);
                        
                        // Check if this child is optimal
                        let child_volume: usize = child.dims().iter().map(|d| d.saturating_sub(1)).product();
                        let is_optimal = if let Some(ref current_optimal) = optimal_ortho {
                            let current_volume: usize = current_optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
                            child_volume > current_volume
                        } else {
                            true
                        };
                        
                        if is_optimal {
                            optimal_ortho = Some(child.clone());
                        }
                        
                        work_queue.push_back(child);
                        generated += 1;
                    }
                }
            }
            
            if processed % 1000 == 0 {
                println!("[main] Processed: {}, Generated: {}, Queue size: {}", 
                         processed, generated, work_queue.len());
            }
        }
        
        println!("[main] File complete. Processed: {}, Generated: {}", 
                 processed, generated);
        
        // Print optimal ortho so far
        if let Some(ref optimal) = optimal_ortho {
            println!("[main] Optimal ortho so far:");
            print_ortho_details(optimal, current_interner);
        } else {
            println!("[main] No optimal ortho found yet");
        }
    }
    
    // Print final optimal ortho
    println!("\n[main] === Final Results ===");
    if let Some(ref optimal) = optimal_ortho {
        println!("[main] Final optimal ortho:");
        let final_interner = interner.as_ref().unwrap();
        print_ortho_details(optimal, final_interner);
    } else {
        println!("[main] No optimal ortho found");
    }
    
    println!("[main] Total unique orthos generated: {}", seen_ids.len());
    
    Ok(())
}

fn print_ortho_details(ortho: &Ortho, interner: &fold::interner::Interner) {
    let dims = ortho.dims();
    let volume: usize = dims.iter().map(|d| d.saturating_sub(1)).product();
    println!("  ID: {}", ortho.id());
    println!("  Version: {}", ortho.version());
    println!("  Dims: {:?}", dims);
    println!("  Volume: {}", volume);
    
    // Print tokens in the ortho
    let payload = ortho.payload();
    let tokens: Vec<String> = payload
        .iter()
        .filter_map(|&opt_idx| {
            opt_idx.map(|idx| interner.string_for_index(idx).to_string())
        })
        .collect();
    
    if !tokens.is_empty() {
        println!("  Tokens: {}", tokens.join(" "));
    }
}
