use fold::ortho::Ortho;
use fold::FoldError;
use std::collections::HashSet;
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
    let mut frontier = HashSet::new();
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Process each file
    for (file_idx, file_path) in input_files.iter().enumerate() {
        println!("\n[main] Processing file {}/{}: {:?}", 
                 file_idx + 1, input_files.len(), file_path);
        
        // Read file content
        let text = fs::read_to_string(file_path)
            .map_err(|e| FoldError::Io(e))?;
        
        println!("[main] File size: {} bytes", text.len());
        
        // Process text through worker loop and track changed keys
        let (new_interner, changed_keys_count, frontier_size, impacted_frontier_count) = fold::process_text(&text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier);
        interner = Some(new_interner);
        
        let current_interner = interner.as_ref().unwrap();
        println!("[main] Created interner version {}, vocabulary size: {}", 
                 current_interner.version(), current_interner.vocabulary().len());
        println!("[main] Number of impacted keys: {}", changed_keys_count);
        println!("[main] Impacted frontier orthos: {}", impacted_frontier_count);
        
        // Print optimal ortho so far
        if let Some(ref optimal) = optimal_ortho {
            println!("[main] Optimal ortho so far:");
            print_ortho_details(optimal, current_interner);
            println!("  Frontier size: {}", frontier_size);
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
        println!("  Frontier size: {}", frontier.len());
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
