use fold::ortho::Ortho;
use fold::{CheckpointManager, Checkpoint, FoldError};
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

    // Track optimal ortho and seen IDs across all files
    let mut optimal_ortho: Option<Ortho> = None;
    let mut seen_ids = HashSet::new();
    let mut frontier = HashSet::new();
    let mut frontier_orthos_saved = std::collections::HashMap::new();
    let mut interner: Option<fold::interner::Interner> = None;
    let mut start_file_idx = 0;
    let mut total_processed = 0;

    // Try to load checkpoint
    if let Some(checkpoint) = checkpoint_manager.load_checkpoint()? {
        interner = checkpoint.interner;
        seen_ids = checkpoint.seen_ids;
        optimal_ortho = checkpoint.optimal_ortho;
        frontier = checkpoint.frontier;
        frontier_orthos_saved = checkpoint.frontier_orthos_saved;
        total_processed = checkpoint.processed_count;
        
        // Start from the next file after the last completed one
        start_file_idx = checkpoint.last_completed_file_index.map(|i| i + 1).unwrap_or(0);
        
        if start_file_idx >= input_files.len() {
            println!("All files already processed. Nothing to do.");
            return Ok(());
        }
    }
    
    // Process each file
    for file_idx in start_file_idx..input_files.len() {
        let file_path = &input_files[file_idx];
        
        // Read file content
        let text = fs::read_to_string(file_path)
            .map_err(|e| FoldError::Io(e))?;
        
        // Shared state for logging
        let file_info = format!("File {}/{}: {}", 
            file_idx + 1, input_files.len(), 
            file_path.file_name().unwrap().to_str().unwrap());
        
        // Process text through worker loop with checkpoint callback
        let checkpoint_fn = move |_processed: usize| -> Result<(), FoldError> {
            // Checkpoints are saved after each file instead of during processing
            // to avoid complex state management
            Ok(())
        };
        
        let (new_interner, _changed_keys_count, _frontier_size, _impacted_frontier_count, file_processed) = 
            fold::process_text(&text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut frontier_orthos_saved, checkpoint_fn)?;
        
        interner = Some(new_interner);
        total_processed += file_processed;
        
        // Save checkpoint after each file
        let checkpoint = Checkpoint::new(
            Some(file_idx),
            interner.clone(),
            seen_ids.clone(),
            optimal_ortho.clone(),
            frontier.clone(),
            frontier_orthos_saved.clone(),
            total_processed,
        );
        
        checkpoint_manager.save_checkpoint(&checkpoint)?;
        
        // Final display for this file
        let current_interner = interner.as_ref().unwrap();
        display_status(
            &file_info,
            current_interner,
            &seen_ids,
            &frontier,
            total_processed,
            &checkpoint.timestamp,
        );
    }
    
    // Clear checkpoint after successful completion
    checkpoint_manager.clear_checkpoint()?;
    
    // Print final summary
    println!("\n");
    println!("=== Processing Complete ===");
    println!("Total files processed: {}", input_files.len());
    println!("Total orthos generated: {}", seen_ids.len());
    println!("Final frontier size: {}", frontier.len());
    if let Some(ref optimal) = optimal_ortho {
        println!("Optimal ortho volume: {}", 
            optimal.dims().iter().map(|d| d.saturating_sub(1)).product::<usize>());
    }
    
    Ok(())
}

fn display_status(
    file_info: &str,
    interner: &fold::interner::Interner,
    seen_ids: &HashSet<usize>,
    frontier: &HashSet<usize>,
    processed: usize,
    checkpoint_time: &str,
) {
    // Clear screen and redraw
    print!("\x1B[2J\x1B[1;1H");
    
    // File info at top
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║ {:<64} ║", file_info);
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();
    
    // Key metrics
    println!("Interner version: {}", interner.version());
    println!("Vocabulary size: {}", interner.vocabulary().len());
    println!("Total orthos generated: {}", seen_ids.len());
    println!("Frontier size: {}", frontier.len());
    println!();
    
    // Worker progress
    println!("─── Worker Progress ───");
    println!("Processed: {}", processed);
    println!();
    
    // Checkpoint info
    println!("Last checkpoint: {}", checkpoint_time);
    
    let _ = io::stdout().flush();
}
