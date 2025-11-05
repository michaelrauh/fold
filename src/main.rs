use fold::{
    interner::{InMemoryInternerHolder, InternerHolderLike},
    ortho::Ortho,
    ortho_database::{InMemoryOrthoDatabase, OrthoDatabaseLike},
    FoldError,
};
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

/// Simple in-memory queue implementation for the worker loop
struct SimpleQueue {
    items: VecDeque<Ortho>,
}

impl SimpleQueue {
    fn new() -> Self {
        SimpleQueue {
            items: VecDeque::new(),
        }
    }
    
    fn into_inner(self) -> VecDeque<Ortho> {
        self.items
    }
}

impl fold::queue::QueueProducerLike for SimpleQueue {
    fn push_many(&mut self, items: Vec<Ortho>) -> Result<(), FoldError> {
        self.items.extend(items);
        Ok(())
    }
}

impl fold::queue::QueueLenLike for SimpleQueue {
    fn len(&mut self) -> Result<usize, FoldError> {
        Ok(self.items.len())
    }
}

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

    // Initialize interner holder and database
    let mut interner_holder = InMemoryInternerHolder::new()?;
    let mut db = InMemoryOrthoDatabase::new();
    let mut seen_ids = std::collections::HashSet::new();
    
    // Process each file
    for (file_idx, file_path) in input_files.iter().enumerate() {
        println!("\n[main] Processing file {}/{}: {:?}", 
                 file_idx + 1, input_files.len(), file_path);
        
        // Read file content
        let text = fs::read_to_string(file_path)
            .map_err(|e| FoldError::Io(e))?;
        
        println!("[main] File size: {} bytes", text.len());
        
        // Add text to interner - this creates new version and seeds the queue
        let mut seed_queue = SimpleQueue::new();
        interner_holder.add_text_with_seed(&text, &mut seed_queue)?;
        
        let version = interner_holder.latest_version();
        let interner = interner_holder.get_latest()
            .expect("interner should exist after add_text_with_seed");
        
        println!("[main] Created interner version {}, vocabulary size: {}", 
                 version, interner.vocabulary().len());
        
        // Get the seed ortho from the queue
        let mut work_queue: VecDeque<Ortho> = seed_queue.into_inner();
        
        println!("[main] Work queue initialized with {} ortho(s)", work_queue.len());
        
        // Worker loop: process until queue is empty
        let mut processed = 0;
        let mut generated = 0;
        
        while let Some(ortho) = work_queue.pop_front() {
            processed += 1;
            
            // Get requirements for this ortho
            let (forbidden, required) = ortho.get_requirements();
            
            // Get completions from interner
            let completions = interner.intersect(&required, &forbidden);
            
            // Generate children
            for completion in completions {
                let children = ortho.add(completion, version);
                for child in children {
                    let child_id = child.id();
                    // Only add to queue if never seen before
                    if !seen_ids.contains(&child_id) {
                        seen_ids.insert(child_id);
                        // Store in database
                        db.insert_or_update(child.clone())?;
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
        
        // Print optimal ortho for this file
        if let Some(optimal) = db.get_optimal()? {
            println!("[main] Optimal ortho for this file:");
            print_ortho_details(&optimal, &interner);
        } else {
            println!("[main] No optimal ortho found");
        }
    }
    
    // Print final optimal ortho
    println!("\n[main] === Final Results ===");
    if let Some(optimal) = db.get_optimal()? {
        println!("[main] Final optimal ortho:");
        let interner = interner_holder.get_latest().unwrap();
        print_ortho_details(&optimal, &interner);
    } else {
        println!("[main] No optimal ortho found");
    }
    
    let total_orthos = db.len()?;
    println!("[main] Total orthos in database: {}", total_orthos);
    
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
