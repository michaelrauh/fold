use fold::{disk_backed_queue::DiskBackedQueue, file_handler::{self, StateConfig}, interner::Interner, memory_config::MemoryConfig, metrics::Metrics, ortho::Ortho, seen_tracker::SeenTracker, tui::Tui, FoldError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

fn calculate_score(ortho: &Ortho) -> (usize, usize) {
    let volume = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    (volume, fullness)
}

fn main() -> Result<(), FoldError> {
    // Check for test environment variable
    let config = if let Ok(test_dir) = std::env::var("FOLD_STATE_DIR") {
        StateConfig::custom(PathBuf::from(test_dir))
    } else {
        StateConfig::default()
    };
    
    // Initialize: setup directories and recover abandoned files
    file_handler::initialize_with_config(&config)?;
    
    // Initialize metrics and TUI
    let metrics = Metrics::new();
    let should_quit = Arc::new(AtomicBool::new(false));
    
    // Spawn TUI thread
    let metrics_clone = metrics.clone_handle();
    let should_quit_clone = Arc::clone(&should_quit);
    let tui_handle = thread::spawn(move || {
        let mut tui = Tui::new(metrics_clone, should_quit_clone);
        if let Err(e) = tui.run() {
            eprintln!("TUI error: {}", e);
        }
    });
    
    // Count initial chunks
    let total_chunks = file_handler::count_txt_files_remaining_with_config(&config)?;
    metrics.update_global(|g| {
        g.total_chunks = total_chunks;
        g.remaining_chunks = total_chunks;
    });
    
    // Initialize largest archive metric from existing archives
    if let Ok(Some(largest)) = file_handler::find_largest_archive_with_config(&config) {
        metrics.update_largest_archive(|la| {
            la.filename = largest.path.clone();
            la.ortho_count = largest.ortho_count;
            la.lineage = largest.lineage;
        });
        
        // Load and restore the optimal ortho from the largest archive
        let optimal_ortho = file_handler::load_optimal_ortho(&largest.path)?;
        let interner = file_handler::load_interner(&largest.path)?;
        
        let (volume, fullness) = calculate_score(&optimal_ortho);
        metrics.update_optimal_ortho(|opt| {
            opt.volume = volume;
            opt.dims = optimal_ortho.dims().clone();
            opt.fullness = fullness;
            opt.capacity = optimal_ortho.payload().len();
            opt.payload = optimal_ortho.payload().clone();
            opt.vocab = interner.vocabulary().to_vec();
        });
        metrics.add_log(format!("Restored optimal ortho from archive: volume={}", volume));
    }
    
    // Main processing loop - two modes:
    // Mode 1: If there are 2+ result archives, merge smallest with largest
    // Mode 2: If there are not 2 results, process txt into result
    loop {
        // Check for existing archives
        let archive_pair = file_handler::get_smallest_and_largest_archives_with_config(&config)?;
        
        if let Some((smallest, largest)) = archive_pair {
            // Mode 1: Merge archives
            metrics.update_global(|g| g.mode = "Merging Archives".to_string());
            metrics.add_log("MODE 1: Merging archives".to_string());
            metrics.add_log(format!("Merging: {} + {}", smallest, largest));
            
            merge_archives(&smallest, &largest, &config, &metrics)?;
            
        } else {
            // Mode 2: Process txt file
            let txt_file = file_handler::find_txt_file_with_config(&config)?;
            
            if txt_file.is_none() {
                metrics.add_log("No more files to process".to_string());
                metrics.add_log("Processing completed".to_string());
                break;
            }
            
            metrics.update_global(|g| g.mode = "Processing Text".to_string());
            metrics.add_log("MODE 2: Processing text file".to_string());
            
            process_txt_file(txt_file.unwrap(), &config, &metrics)?;
        }
    }
    
    // Signal TUI to quit and wait for it
    should_quit.store(true, Ordering::Relaxed);
    let _ = tui_handle.join();
    
    Ok(())
}

fn process_txt_file(file_path: String, config: &StateConfig, metrics: &Metrics) -> Result<(), FoldError> {
    // Ingest the text file (now includes reading the text)
    let ingestion = file_handler::ingest_txt_file_with_config(&file_path, config)?;
    let remaining = file_handler::count_txt_files_remaining_with_config(config)?;
    
    metrics.update_operation(|op| {
        op.current_file = ingestion.filename.clone();
        op.status = "Building interner".to_string();
    });
    metrics.update_global(|g| {
        g.remaining_chunks = remaining;
        g.current_lineage = format!("\"{}\"", ingestion.filename);
    });
    metrics.add_log(format!("Ingested: {} ({} remaining)", ingestion.filename, remaining));
    
    // Build interner from the text
    let interner = Interner::from_text(&ingestion.text);
    let version = interner.version();
    
    metrics.update_global(|g| {
        g.interner_version = version;
        g.vocab_size = interner.vocabulary().len();
    });
    metrics.add_log(format!("Interner built: v{}, vocab={}", version, interner.vocabulary().len()));
    
    // Calculate memory config for this file
    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())?.len();
    let memory_config = MemoryConfig::calculate(interner_bytes, 0);
    
    metrics.update_global(|g| {
        g.queue_buffer_size = memory_config.queue_buffer_size;
        g.bloom_capacity = memory_config.bloom_capacity;
        g.num_shards = memory_config.num_shards;
        g.max_shards_in_memory = memory_config.max_shards_in_memory;
    });
    
    // Initialize work queue for this file (isolated to work folder)
    let work_queue_path = ingestion.work_queue_path();
    let mut work_queue = DiskBackedQueue::new_from_path(&work_queue_path, memory_config.queue_buffer_size)?;
    
    // Initialize results queue for this file (disk-backed to avoid OOM)
    let results_path = ingestion.results_path();
    let mut results = DiskBackedQueue::new_from_path(&results_path, memory_config.queue_buffer_size)?;
    
    // Initialize tracker for this file (isolated to work folder)
    let seen_shards_path = ingestion.seen_shards_path();
    let mut tracker = SeenTracker::with_path(
        &seen_shards_path,
        memory_config.bloom_capacity,
        memory_config.num_shards,
        memory_config.max_shards_in_memory,
    );
    
    // Seed with empty ortho
    let seed_ortho = Ortho::new(version);
    let seed_id = seed_ortho.id();
    
    tracker.insert(seed_id);
    let mut best_ortho = seed_ortho.clone();
    let mut best_score = calculate_score(&best_ortho);
    
    work_queue.push(seed_ortho)?;
    
    metrics.update_operation(|op| op.status = "Processing orthos".to_string());
    
    // Process work queue until empty
    let mut processed_count = 0;
    while let Some(ortho) = work_queue.pop()? {
        processed_count += 1;
        
        if processed_count % 1000 == 0 {
            // Update metrics
            metrics.record_queue_depth(work_queue.len());
            metrics.record_seen_size(tracker.len());
            metrics.record_results_count(results.len());
            let (volume, _) = calculate_score(&best_ortho);
            metrics.record_optimal_volume(volume);
            metrics.update_operation(|op| {
                op.progress_current = processed_count;
            });
            
            // Update RAM usage
            let mut sys = sysinfo::System::new();
            sys.refresh_memory();
            let used_mb = sys.used_memory() / 1_048_576;
            metrics.update_global(|g| g.ram_mb = used_mb as usize);
        }
        
        if processed_count % 50000 == 0 {
            metrics.add_log(format!("Progress: {} orthos processed", processed_count));
        }
        
        if processed_count % 100000 == 0 {
            print_optimal(&best_ortho, &interner);
            // Update heartbeat every 100k orthos (zero-arity)
            ingestion.touch_heartbeat()?;
        }
        
        // Get requirements from ortho
        let (forbidden, required) = ortho.get_requirements();
        
        // Get completions from interner
        let completions = interner.intersect(&required, &forbidden);
        
        // Generate child orthos
        for completion in completions {
            let children = ortho.add(completion, version);
            
            for child in children {
                let child_id = child.id();
                
                // Use tracker for bloom-filtered deduplication check
                if !tracker.contains(&child_id) {
                    tracker.insert(child_id);
                    
                    let candidate_score = calculate_score(&child);
                    
                    // Update local best for this operation
                    if candidate_score > best_score {
                        best_ortho = child.clone();
                        best_score = candidate_score;
                    }
                    
                    // Check against global optimal
                    let snapshot = metrics.snapshot();
                    let global_score = (snapshot.optimal_ortho.volume, snapshot.optimal_ortho.fullness);
                    if candidate_score > global_score {
                        let (volume, fullness) = candidate_score;
                        metrics.update_optimal_ortho(|opt| {
                            opt.volume = volume;
                            opt.dims = child.dims().clone();
                            opt.fullness = fullness;
                            opt.capacity = child.payload().len();
                            opt.payload = child.payload().clone();
                            opt.vocab = interner.vocabulary().to_vec();
                        });
                    }
                    
                    results.push(child.clone())?;
                    work_queue.push(child)?;
                }
            }
        }
    }
    
    let total_orthos = results.len();
    metrics.add_log(format!("Completed: {} orthos generated", total_orthos));
    
    print_optimal(&best_ortho, &interner);
    
    // Save the result (using method on ingestion)
    let (archive_path, lineage) = ingestion.save_result(&interner, results, Some(&best_ortho), total_orthos)?;
    
    metrics.add_log(format!("Archive saved: {}", archive_path));
    
    // Update largest archive if this is bigger
    metrics.update_largest_archive(|la| {
        if total_orthos > la.ortho_count {
            la.filename = archive_path.clone();
            la.ortho_count = total_orthos;
            la.lineage = lineage;
        }
    });
    
    metrics.update_global(|g| g.processed_chunks += 1);
    
    // Delete the work folder (using consuming cleanup method)
    ingestion.cleanup()?;
    
    Ok(())
}

fn merge_archives(archive_a_path: &str, archive_b_path: &str, config: &StateConfig, metrics: &Metrics) -> Result<(), FoldError> {
    // Get archive ortho counts for display BEFORE ingest moves them
    let orthos_a = file_handler::load_archive_metadata(archive_a_path).unwrap_or(0);
    let orthos_b = file_handler::load_archive_metadata(archive_b_path).unwrap_or(0);
    
    // Ingest archives for merging
    let ingestion = file_handler::ingest_archives_with_config(archive_a_path, archive_b_path, config)?;
    
    metrics.update_operation(|op| op.status = "Loading interners".to_string());
    
    // Load both interners using method
    let (interner_a, interner_b) = ingestion.load_interners()?;
    
    // Load lineages early to display provenance tree during merge
    let (lineage_a_early, lineage_b_early) = ingestion.load_lineages()?;
    let merged_lineage_preview = format!("({} {})", lineage_a_early, lineage_b_early);
    metrics.update_global(|g| g.current_lineage = merged_lineage_preview);
    
    // Find differences from a to b and from b to a
    let impacted_a = interner_a.impacted_keys(&interner_b);
    let impacted_b = interner_b.impacted_keys(&interner_a);
    
    metrics.update_merge(|m| {
        m.current_merge = format!("merge_{}", std::process::id());
        m.archive_a_orthos = orthos_a;
        m.archive_b_orthos = orthos_b;
        m.impacted_a = impacted_a.len();
        m.impacted_b = impacted_b.len();
        m.seed_orthos_a = orthos_a;
        m.seed_orthos_b = orthos_b;
    });
    
    // Create merged interner using proper merge method
    let merged_interner = interner_a.merge(&interner_b);
    let new_version = merged_interner.version();
    
    metrics.update_global(|g| {
        g.interner_version = new_version;
        g.vocab_size = merged_interner.vocabulary().len();
    });
    metrics.add_log(format!("Merged interner: v{}, vocab={}", new_version, merged_interner.vocabulary().len()));
    
    // Build vocabulary mapping for A and B
    let vocab_map_a = build_vocab_mapping(interner_a.vocabulary(), merged_interner.vocabulary());
    let vocab_map_b = build_vocab_mapping(interner_b.vocabulary(), merged_interner.vocabulary());
    
    // Calculate memory config
    let interner_bytes = bincode::encode_to_vec(&merged_interner, bincode::config::standard())?.len();
    let memory_config = MemoryConfig::calculate(interner_bytes, 0);
    
    metrics.update_global(|g| {
        g.queue_buffer_size = memory_config.queue_buffer_size;
        g.bloom_capacity = memory_config.bloom_capacity;
        g.num_shards = memory_config.num_shards;
        g.max_shards_in_memory = memory_config.max_shards_in_memory;
    });
    
    // Initialize work queue and results for merge (isolated to merge work folder)
    let work_queue_path = ingestion.work_queue_path();
    let mut work_queue = DiskBackedQueue::new_from_path(&work_queue_path, memory_config.queue_buffer_size)?;
    let results_path = config.results_dir(&format!("merged_{}", std::process::id()));
    let mut merged_results = DiskBackedQueue::new_from_path(results_path.to_str().unwrap(), memory_config.queue_buffer_size)?;
    
    // Initialize tracker (isolated to merge work folder)
    let seen_shards_path = ingestion.seen_shards_path();
    let mut tracker = SeenTracker::with_path(
        &seen_shards_path,
        memory_config.bloom_capacity,
        memory_config.num_shards,
        memory_config.max_shards_in_memory,
    );
    
    // Seed with empty ortho
    let seed_ortho = Ortho::new(new_version);
    let seed_id = seed_ortho.id();
    tracker.insert(seed_id);
    let mut best_ortho = seed_ortho.clone();
    let mut best_score = calculate_score(&best_ortho);
    work_queue.push(seed_ortho)?;
    
    metrics.update_operation(|op| {
        op.status = "Remapping Archive A".to_string();
        op.progress_current = 0;
    });
    
    // Get results paths using method
    let (results_a_path, results_b_path) = ingestion.get_results_paths();
    
    // Process archive A results: remap ALL orthos to results
    // Add impacted orthos to work queue for further processing
    let mut results_a = DiskBackedQueue::new_from_path(&results_a_path, memory_config.queue_buffer_size)?;
    
    let total_a_count = results_a.len();
    let mut total_from_a = 0;
    let mut impacted_from_a = 0;
    
    metrics.update_operation(|op| op.progress_total = total_a_count);
    
    while let Some(ortho) = results_a.pop()? {
        // Remap the ortho to new vocabulary
        if let Some(remapped) = ortho.remap(&vocab_map_a, new_version) {
            let remapped_id = remapped.id();
            if !tracker.contains(&remapped_id) {
                tracker.insert(remapped_id);
                // Add ALL remapped orthos to merged results
                merged_results.push(remapped.clone())?;
                total_from_a += 1;
                
                // Log progress every 10k orthos and keep heartbeat fresh (zero-arity)
                if total_from_a % 10000 == 0 {
                    ingestion.touch_heartbeat()?;
                    
                    metrics.update_operation(|op| op.progress_current = total_from_a);
                    metrics.record_seen_size(tracker.len());
                    metrics.record_results_count(merged_results.len());
                }
                
                // Check if this ortho is impacted - if so, add to work queue
                if is_ortho_impacted(&ortho, &impacted_a) {
                    work_queue.push(remapped)?;
                    impacted_from_a += 1;
                }
            }
        }
    }
    
    metrics.update_merge(|m| m.impacted_queued_a = impacted_from_a);
    
    metrics.update_operation(|op| {
        op.status = "Remapping Archive B".to_string();
        op.progress_current = 0;
    });
    
    // Process archive B results: remap ALL orthos to results
    // Add impacted orthos to work queue for further processing
    let mut results_b = DiskBackedQueue::new_from_path(&results_b_path, memory_config.queue_buffer_size)?;
    
    let total_b_count = results_b.len();
    let mut total_from_b = 0;
    let mut impacted_from_b = 0;
    
    metrics.update_operation(|op| op.progress_total = total_b_count);
    
    while let Some(ortho) = results_b.pop()? {
        // Remap the ortho to new vocabulary
        if let Some(remapped) = ortho.remap(&vocab_map_b, new_version) {
            let remapped_id = remapped.id();
            if !tracker.contains(&remapped_id) {
                tracker.insert(remapped_id);
                // Add ALL remapped orthos to merged results
                merged_results.push(remapped.clone())?;
                total_from_b += 1;
                
                // Log progress every 10k orthos and keep heartbeat fresh (zero-arity)
                if total_from_b % 10000 == 0 {
                    ingestion.touch_heartbeat()?;
                    
                    metrics.update_operation(|op| op.progress_current = total_from_b);
                    metrics.record_seen_size(tracker.len());
                    metrics.record_results_count(merged_results.len());
                }
                
                // Check if this ortho is impacted - if so, add to work queue
                if is_ortho_impacted(&ortho, &impacted_b) {
                    work_queue.push(remapped)?;
                    impacted_from_b += 1;
                }
            }
        }
    }
    
    metrics.update_merge(|m| m.impacted_queued_b = impacted_from_b);
    
    metrics.add_log(format!("Remapping complete: A={}, B={}", total_from_a, total_from_b));
    
    metrics.update_operation(|op| {
        op.status = "Processing merged space".to_string();
        op.progress_current = 0;
        op.progress_total = work_queue.len();
    });
    
    // Process work queue
    let mut processed_count = 0;
    while let Some(ortho) = work_queue.pop()? {
        processed_count += 1;
        
        if processed_count % 1000 == 0 {
            let queue_depth = work_queue.len();
            metrics.record_queue_depth(queue_depth);
            metrics.record_seen_size(tracker.len());
            metrics.record_results_count(merged_results.len());
            let (volume, _) = calculate_score(&best_ortho);
            metrics.record_optimal_volume(volume);
            metrics.update_operation(|op| {
                op.progress_current = processed_count;
                op.progress_total = processed_count + queue_depth;
            });
            
            // Update RAM usage
            let mut sys = sysinfo::System::new();
            sys.refresh_memory();
            let used_mb = sys.used_memory() / 1_048_576;
            metrics.update_global(|g| g.ram_mb = used_mb as usize);
        }
        
        if processed_count % 50000 == 0 {
            metrics.add_log(format!("Merge progress: {} orthos", processed_count));
        }
        
        if processed_count % 100000 == 0 {
            print_optimal(&best_ortho, &merged_interner);
            // Touch heartbeat (zero-arity)
            ingestion.touch_heartbeat()?;
        }
        
        let (forbidden, required) = ortho.get_requirements();
        let completions = merged_interner.intersect(&required, &forbidden);
        
        for completion in completions {
            let children = ortho.add(completion, new_version);
            
            for child in children {
                let child_id = child.id();
                
                if !tracker.contains(&child_id) {
                    tracker.insert(child_id);
                    
                    let candidate_score = calculate_score(&child);
                    
                    // Update local best for this operation
                    if candidate_score > best_score {
                        best_ortho = child.clone();
                        best_score = candidate_score;
                    }
                    
                    // Check against global optimal
                    let snapshot = metrics.snapshot();
                    let global_score = (snapshot.optimal_ortho.volume, snapshot.optimal_ortho.fullness);
                    if candidate_score > global_score {
                        let (volume, fullness) = candidate_score;
                        metrics.update_optimal_ortho(|opt| {
                            opt.volume = volume;
                            opt.dims = child.dims().clone();
                            opt.fullness = fullness;
                            opt.capacity = child.payload().len();
                            opt.payload = child.payload().clone();
                            opt.vocab = merged_interner.vocabulary().to_vec();
                        });
                    }
                    
                    merged_results.push(child.clone())?;
                    work_queue.push(child)?;
                }
            }
        }
    }
    
    let total_merged = merged_results.len();
    print_optimal(&best_ortho, &merged_interner);
    
    let new_orthos_from_merge = total_merged.saturating_sub(total_from_a + total_from_b);
    
    metrics.add_log(format!("Merge complete: {} orthos ({} new)", total_merged, new_orthos_from_merge));
    metrics.update_merge(|m| {
        m.completed_merges += 1;
        m.new_orthos_from_merge = new_orthos_from_merge;
    });
    metrics.update_global(|g| g.processed_chunks += 1);
    
    // Save the merged result using method (lineages already loaded earlier)
    let (merged_archive_path, merged_lineage) = ingestion.save_result(
        &merged_interner, 
        merged_results, 
        results_path.to_str().unwrap(), 
        Some(&best_ortho), 
        &lineage_a_early, 
        &lineage_b_early,
        total_merged
    )?;
    
    metrics.add_log(format!("Archive saved: {}", merged_archive_path));
    
    // Update largest archive metrics
    metrics.update_largest_archive(|la| {
        if total_merged > la.ortho_count {
            la.filename = merged_archive_path.clone();
            la.ortho_count = total_merged;
            la.lineage = merged_lineage;
        }
    });
    
    // Delete the original archives using consuming cleanup method
    ingestion.cleanup()?;
    
    Ok(())
}

// Build a mapping from old vocabulary indices to new vocabulary indices
fn build_vocab_mapping(old_vocab: &[String], new_vocab: &[String]) -> Vec<usize> {
    old_vocab.iter().map(|word| {
        new_vocab.iter().position(|w| w == word)
            .expect("Word from old vocab must exist in new vocab")
    }).collect()
}

// Check if an ortho uses any of the impacted keys
fn is_ortho_impacted(ortho: &Ortho, impacted_keys: &[Vec<usize>]) -> bool {
    if impacted_keys.is_empty() {
        return false;
    }
    
    // Get the requirement phrases from the ortho
    let requirement_phrases = ortho.get_requirement_phrases();
    
    // Check if any requirement phrase matches an impacted key
    for req_phrase in &requirement_phrases {
        for impacted_key in impacted_keys {
            if req_phrase == impacted_key {
                return true;
            }
        }
    }
    
    false
}

fn print_optimal(_ortho: &Ortho, _interner: &Interner) {
    // Optimal ortho info is now displayed in TUI metrics
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_calculate_score() {
        let ortho = Ortho::new(1);
        let (volume, fullness) = calculate_score(&ortho);
        // Empty ortho with dims [2,2] has volume (2-1)*(2-1) = 1
        assert_eq!(volume, 1);
        // All 4 slots are None
        assert_eq!(fullness, 0);
    }
}
