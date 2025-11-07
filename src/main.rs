use fold::ortho::Ortho;
use fold::FoldError;
use fold::SeenTracker;
use fold::tui::{AppState, TuiApp};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
        eprintln!("No input files found in {:?}", input_dir);
        return Ok(());
    }

    // Initialize TUI
    let state = Arc::new(Mutex::new(AppState::new(input_files.len())));
    let quit_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    
    // Spawn TUI update thread
    let state_clone = Arc::clone(&state);
    let quit_flag_clone = Arc::clone(&quit_flag);
    let tui_handle = thread::spawn(move || {
        let mut tui_thread = TuiApp::new().unwrap();
        loop {
            if quit_flag_clone.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            
            let state_lock = state_clone.lock().unwrap();
            if let Ok(should_quit) = tui_thread.draw(&state_lock) {
                if should_quit {
                    quit_flag_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
            }
            drop(state_lock);
            
            thread::sleep(Duration::from_millis(50));
        }
    });
    
    // Track optimal ortho and seen IDs across all files
    let mut optimal_ortho: Option<Ortho> = None;
    let mut seen_ids = SeenTracker::load()?;
    let mut ortho_storage = fold::disk_queue::DiskQueue::new_persistent()?;
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Process each file
    for (file_idx, file_path) in input_files.iter().enumerate() {
        // Check if user quit
        if quit_flag.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        
        // Read file content
        let text = fs::read_to_string(file_path)
            .map_err(|e| FoldError::Io(e))?;
        
        let word_count = text.split_whitespace().count();
        
        // Update state for new file
        {
            let mut state_lock = state.lock().unwrap();
            state_lock.start_file(file_idx + 1, word_count, 0);
        }
        
        // Process text through worker loop with metrics callback
        let state_clone = Arc::clone(&state);
        let quit_check = Arc::clone(&quit_flag);
        
        interner = Some(fold::process_text(
            &text, 
            interner, 
            &mut seen_ids, 
            &mut optimal_ortho, 
            &mut ortho_storage,
            move |queue_len, total_found| {
                if quit_check.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let mut state_lock = state_clone.lock().unwrap();
                state_lock.update_metrics(queue_len, total_found);
            }
        )?);
        
        // Update optimal ortho display
        if let Some(ref optimal) = optimal_ortho {
            let current_interner = interner.as_ref().unwrap();
            let ortho_display = format_ortho_display(optimal, current_interner);
            let mut state_lock = state.lock().unwrap();
            state_lock.set_optimal(ortho_display);
        }
    }
    
    // Signal TUI thread to quit
    quit_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    
    // Wait for TUI thread to finish
    let _ = tui_handle.join();
    
    // Save seen_ids state
    seen_ids.save()?;
    
    // Flush ortho storage to disk
    ortho_storage.flush()?;
    
    Ok(())
}

fn format_ortho_display(ortho: &Ortho, interner: &fold::interner::Interner) -> String {
    let dims = ortho.dims();
    let volume: usize = dims.iter().map(|d| d.saturating_sub(1)).product();
    let filled: usize = ortho.payload().iter().filter(|x| x.is_some()).count();
    
    let mut result = format!(
        "V:{} Dims:{:?} Vol:{} Fill:{}/{}\n",
        ortho.version(), dims, volume, filled, ortho.payload().len()
    );
    
    // Convert payload to tokens
    let tokens: Vec<String> = ortho.payload()
        .iter()
        .map(|&opt_idx| {
            opt_idx
                .map(|idx| interner.string_for_index(idx).to_string())
                .unwrap_or_else(|| "·".to_string())
        })
        .collect();
    
    let max_width = tokens.iter().map(|s| s.len()).max().unwrap_or(1).max(3);
    
    if dims.len() == 2 {
        // 2D table
        result.push_str(&format_2d_table(&tokens, dims, max_width));
    } else if dims.len() >= 3 {
        // Show first slice of 3D+
        result.push_str(&format_nd_table(&tokens, dims, max_width));
    } else {
        // 1D fallback
        result.push_str(&tokens.join(" "));
    }
    
    result
}

fn format_2d_table(tokens: &[String], dims: &[usize], max_width: usize) -> String {
    let rows = dims[0];
    let cols = dims[1];
    
    let mut grid: Vec<Vec<String>> = vec![vec!["·".to_string(); cols]; rows];
    
    for (linear_idx, token) in tokens.iter().enumerate() {
        let coords = fold::spatial::index_to_coords(linear_idx, dims);
        if coords.len() == 2 {
            let row = coords[0];
            let col = coords[1];
            if row < rows && col < cols {
                grid[row][col] = token.clone();
            }
        }
    }
    
    let mut result = String::new();
    for row in 0..rows {
        for col in 0..cols {
            result.push_str(&format!("{:width$}", grid[row][col], width = max_width));
            if col < cols - 1 {
                result.push_str(" │ ");
            }
        }
        if row < rows - 1 {
            result.push('\n');
            for col in 0..cols {
                result.push_str(&"─".repeat(max_width));
                if col < cols - 1 {
                    result.push_str("─┼─");
                }
            }
            result.push('\n');
        }
    }
    result
}

fn format_nd_table(tokens: &[String], dims: &[usize], max_width: usize) -> String {
    if dims.len() == 2 {
        return format_2d_table(tokens, dims, max_width);
    }
    
    let rows = dims[dims.len() - 2];
    let cols = dims[dims.len() - 1];
    
    let mut result = String::new();
    
    // Just show first slice to fit in TUI
    let slice_idx = 0;
    let coords = linear_to_coords(slice_idx, &dims[..dims.len() - 2]);
    
    result.push_str("[");
    for (i, &coord) in coords.iter().enumerate() {
        if i > 0 {
            result.push_str(", ");
        }
        result.push_str(&coord.to_string());
    }
    result.push_str(", :, :]\n");
    
    let mut grid: Vec<Vec<String>> = vec![vec!["·".to_string(); cols]; rows];
    
    for (linear_idx, token) in tokens.iter().enumerate() {
        let full_coords = fold::spatial::index_to_coords(linear_idx, dims);
        if full_coords.len() == dims.len() {
            let slice_matches = coords.iter().enumerate().all(|(i, &c)| full_coords[i] == c);
            if slice_matches {
                let row = full_coords[full_coords.len() - 2];
                let col = full_coords[full_coords.len() - 1];
                if row < rows && col < cols {
                    grid[row][col] = token.clone();
                }
            }
        }
    }
    
    for row in 0..rows {
        for col in 0..cols {
            result.push_str(&format!("{:width$}", grid[row][col], width = max_width));
            if col < cols - 1 {
                result.push_str(" │ ");
            }
        }
        if row < rows - 1 {
            result.push('\n');
            for col in 0..cols {
                result.push_str(&"─".repeat(max_width));
                if col < cols - 1 {
                    result.push_str("─┼─");
                }
            }
            result.push('\n');
        }
    }
    
    result
}

fn linear_to_coords(mut linear_idx: usize, dims: &[usize]) -> Vec<usize> {
    let mut coords = vec![0; dims.len()];
    for i in (0..dims.len()).rev() {
        coords[i] = linear_idx % dims[i];
        linear_idx /= dims[i];
    }
    coords
}

