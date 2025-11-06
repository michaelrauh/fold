use fold::ortho::Ortho;
use fold::{CheckpointManager, FoldError};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

fn main() -> Result<(), FoldError> {
    let state_dir = std::env::var("FOLD_STATE_DIR").unwrap_or_else(|_| "./fold_state".to_string());
    let state_dir_path = PathBuf::from(&state_dir);
    let input_dir = state_dir_path.join("input");

    if !input_dir.exists() {
        eprintln!("Error: Input directory does not exist: {:?}", input_dir);
        eprintln!("Run stage.sh to create input files first.");
        return Ok(());
    }

    // Initialize checkpoint manager
    let checkpoint_manager = CheckpointManager::new(&state_dir_path)?;

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

    log_info(&format!("Found {} input files", input_files.len()));

    // Track optimal ortho and seen IDs across all files
    let mut optimal_ortho: Option<Ortho> = None;
    let mut seen_ids = HashSet::new();
    let mut frontier = HashSet::new();
    let mut frontier_orthos_saved = std::collections::HashMap::new();
    let mut interner: Option<fold::interner::Interner> = None;
    let mut start_file_idx = 0;

    // Try to load checkpoint
    if let Some(checkpoint) = checkpoint_manager.load_checkpoint()? {
        log_info(&format!("📦 Checkpoint found from {}", checkpoint.timestamp));
        log_info(&format!("   Resuming from file index: {}", 
            checkpoint.last_completed_file_index.map(|i| i + 1).unwrap_or(0)));
        
        interner = checkpoint.interner;
        seen_ids = checkpoint.seen_ids;
        optimal_ortho = checkpoint.optimal_ortho;
        frontier = checkpoint.frontier;
        frontier_orthos_saved = checkpoint.frontier_orthos_saved;
        
        // Start from the next file after the last completed one
        start_file_idx = checkpoint.last_completed_file_index.map(|i| i + 1).unwrap_or(0);
        
        if let Some(ref int) = interner {
            log_info(&format!("   Interner version: {}", int.version()));
            log_info(&format!("   Vocabulary size: {}", int.vocabulary().len()));
        }
        log_info(&format!("   Seen orthos: {}", seen_ids.len()));
        log_info(&format!("   Frontier size: {}", frontier.len()));
        
        if start_file_idx >= input_files.len() {
            log_info("All files already processed. Nothing to do.");
            return Ok(());
        }
    } else {
        log_info("No checkpoint found. Starting from scratch.");
    }
    
    // Process each file
    for file_idx in start_file_idx..input_files.len() {
        let file_path = &input_files[file_idx];
        
        clear_screen();
        log_header(&format!("Processing file {}/{}: {:?}", 
                 file_idx + 1, input_files.len(), file_path.file_name().unwrap()));
        
        // Read file content
        let text = fs::read_to_string(file_path)
            .map_err(|e| FoldError::Io(e))?;
        
        log_info(&format!("File size: {} bytes ({:.2} KB)", 
            text.len(), text.len() as f64 / 1024.0));
        
        // Process text through worker loop and track changed keys
        let (new_interner, changed_keys_count, frontier_size, impacted_frontier_count) = 
            fold::process_text(&text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut frontier_orthos_saved)?;
        interner = Some(new_interner);
        
        let current_interner = interner.as_ref().unwrap();
        log_info(&format!("Interner version: {}", current_interner.version()));
        log_info(&format!("Vocabulary size: {}", current_interner.vocabulary().len()));
        log_info(&format!("Impacted keys: {}", changed_keys_count));
        log_info(&format!("Impacted frontier orthos: {}", impacted_frontier_count));
        log_info(&format!("Total orthos generated: {}", seen_ids.len()));
        log_info(&format!("Frontier size: {}", frontier_size));
        
        // Print optimal ortho so far
        if let Some(ref optimal) = optimal_ortho {
            log_section("Optimal Ortho");
            print_ortho_details(optimal, current_interner);
        } else {
            log_info("No optimal ortho found yet");
        }
        
        // Save checkpoint after each file
        let checkpoint = fold::Checkpoint::new(
            Some(file_idx),
            interner.clone(),
            seen_ids.clone(),
            optimal_ortho.clone(),
            frontier.clone(),
            frontier_orthos_saved.clone(),
        );
        
        checkpoint_manager.save_checkpoint(&checkpoint)?;
        log_checkpoint(&format!("✓ Checkpoint saved at {}", checkpoint.timestamp));
        
        // Pause briefly to allow user to see output
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    
    // Print final results
    clear_screen();
    log_header("=== Final Results ===");
    
    if let Some(ref optimal) = optimal_ortho {
        log_section("Final Optimal Ortho");
        let final_interner = interner.as_ref().unwrap();
        print_ortho_details(optimal, final_interner);
        log_info(&format!("Frontier size: {}", frontier.len()));
    } else {
        log_info("No optimal ortho found");
    }
    
    log_info(&format!("Total unique orthos generated: {}", seen_ids.len()));
    
    // Clear checkpoint after successful completion
    checkpoint_manager.clear_checkpoint()?;
    log_info("✓ Processing complete. Checkpoint cleared.");
    
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

// Logging utilities
fn clear_screen() {
    // ANSI escape code to clear screen and move cursor to top-left
    print!("\x1B[2J\x1B[1;1H");
    let _ = io::stdout().flush();
}

fn log_header(msg: &str) {
    println!("\n╔════════════════════════════════════════════════════════════════╗");
    println!("║ {:^62} ║", msg);
    println!("╚════════════════════════════════════════════════════════════════╝\n");
}

fn log_section(msg: &str) {
    println!("\n┌─ {} ─", msg);
}

fn log_info(msg: &str) {
    println!("│ {}", msg);
}

fn log_checkpoint(msg: &str) {
    println!("\n{}", msg);
}
