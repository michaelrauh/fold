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
    }
    
    Ok(())
}

fn ingest_text(resume_path: &str, text_path: &str) -> Result<(), FoldError> {
    println!("[fold_single] Ingesting text from {}", text_path);
    
    let text = fs::read_to_string(text_path)
        .map_err(|e| FoldError::Interner(format!("Failed to read text file: {}", e)))?;
    
    let mut resume = load_resume(resume_path)?;
    println!("[fold_single] Current interner version: {}, vocab size: {}", 
             resume.interner.version(), resume.interner.vocabulary().len());
    
    // Add text to interner
    resume.interner = resume.interner.add_text(&text);
    println!("[fold_single] New interner version: {}, vocab size: {}", 
             resume.interner.version(), resume.interner.vocabulary().len());
    
    save_resume(resume_path, &resume)?;
    println!("[fold_single] Saved updated resume file");
    
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
    let (resume, _): (ResumeFile, _) = decode_from_slice(&bytes, standard())
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
