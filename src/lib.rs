pub mod error;
pub mod interner;
pub mod ortho;
pub mod ortho_database;
pub mod queue;
pub mod spatial;
pub mod splitter;
pub mod disk_queue;
pub mod seen_tracker;
pub mod tui;

pub use error::*;
pub use interner::*;
pub use ortho_database::*;
pub use queue::*;
pub use seen_tracker::SeenTracker;

use ortho::Ortho;
use disk_queue::DiskQueue;
use std::collections::HashSet;
use std::sync::Arc;

/// Process a single text through the worker loop, updating the interner and tracking optimal ortho
/// Expects the interner to already be updated with the current text
/// If previous_interner is provided, will seed the queue with orthos that need revisiting
pub fn process_text<F>(
    current_interner: Arc<interner::Interner>,
    previous_interner: Option<Arc<interner::Interner>>,
    seen_ids: &mut SeenTracker,
    optimal_ortho: &mut Option<Ortho>,
    ortho_storage: &mut DiskQueue,
    mut metrics_callback: F,
) -> Result<usize, FoldError>
where
    F: FnMut(usize, usize, usize, usize, usize, usize, usize, usize, usize, f64, f64, f64, u64, usize, f64, u64, f64, u64, usize, f64, u64, f64, &Option<Ortho>),  // (queue_length, total_seen, bloom_hits, bloom_misses, bloom_false_positives, shard_cache_hits, disk_checks, queue_mem, queue_disk, work_disk_write_rate, work_disk_read_rate, results_disk_write_rate, work_spillover, work_peak, work_spillover_time, work_loads, work_load_time, results_spillover, results_peak, results_spillover_time, results_loads, results_load_time, optimal_ortho)
{
    let version = current_interner.version();
    
    // Create work queue with disk spillover
    let mut work_queue = DiskQueue::new();
    
    // Track how many orthos we seeded
    let mut seeded_count = 0;
    
    // Always seed the queue with a blank ortho
    let seed_ortho = Ortho::new(version);
    work_queue.push_back(seed_ortho)?;
    seeded_count += 1;
    
    // Also add revisit points if we have a previous interner
    // Stream through ortho_storage, checking each ortho and seeding impacted ones
    if let Some(prev_interner) = previous_interner {
        // Find tokens that changed between versions
        let changed_tokens = find_changed_tokens(&prev_interner, &current_interner);
        
        // Stream through ortho storage: pop each, check if impacted, optionally seed to work queue,
        // and push to new storage (no RAM spike - disk naturally backs it)
        let revisit_count = stream_revisit_orthos(&changed_tokens, ortho_storage, &mut work_queue)?;
        seeded_count += revisit_count;
    }
    
    let mut processed = 0;
    // Worker loop: process until queue is empty
    while let Ok(Some(ortho)) = work_queue.pop_front() {
        processed += 1;
        if processed % 1_000 == 0 {
            let (bloom_hits, bloom_misses, bloom_false_positives, shard_cache_hits, disk_checks) = seen_ids.get_stats();
            let (queue_mem, queue_disk) = work_queue.get_stats();
            let (work_disk_write_rate, work_disk_read_rate) = work_queue.get_rates();
            let (work_spillover, work_peak, _, work_spillover_time, work_loads, work_load_time) = work_queue.get_buffer_stats();
            let (results_disk_write_rate, _) = ortho_storage.get_rates();
            let (results_spillover, results_peak, _, results_spillover_time, results_loads, results_load_time) = ortho_storage.get_buffer_stats();
            metrics_callback(work_queue.len(), seen_ids.len(), bloom_hits, bloom_misses, bloom_false_positives, shard_cache_hits, disk_checks, queue_mem, queue_disk, work_disk_write_rate, work_disk_read_rate, results_disk_write_rate, work_spillover, work_peak, work_spillover_time, work_loads, work_load_time, results_spillover, results_peak, results_spillover_time, results_loads, results_load_time, optimal_ortho);
        }
        
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
                if !seen_ids.contains(child_id)? {
                    seen_ids.insert(child_id)?;
                    
                    // Save to persistent storage (for later revisiting)
                    ortho_storage.push_back(child.clone())?;
                    
                    // Check if this child is optimal
                    update_optimal(optimal_ortho, &child);
                    
                    work_queue.push_back(child)?;
                }
            }
        }
    }
    
    // Final metrics update
    let (bloom_hits, bloom_misses, bloom_false_positives, shard_cache_hits, disk_checks) = seen_ids.get_stats();
    let (queue_mem, queue_disk) = work_queue.get_stats();
    let (work_disk_write_rate, work_disk_read_rate) = work_queue.get_rates();
    let (work_spillover, work_peak, _, work_spillover_time, work_loads, work_load_time) = work_queue.get_buffer_stats();
    let (results_disk_write_rate, _) = ortho_storage.get_rates();
    let (results_spillover, results_peak, _, results_spillover_time, results_loads, results_load_time) = ortho_storage.get_buffer_stats();
    metrics_callback(work_queue.len(), seen_ids.len(), bloom_hits, bloom_misses, bloom_false_positives, shard_cache_hits, disk_checks, queue_mem, queue_disk, work_disk_write_rate, work_disk_read_rate, results_disk_write_rate, work_spillover, work_peak, work_spillover_time, work_loads, work_load_time, results_spillover, results_peak, results_spillover_time, results_loads, results_load_time, optimal_ortho);
    
    Ok(seeded_count)
}

/// Update the optimal ortho if the new candidate is better
fn update_optimal(optimal_ortho: &mut Option<Ortho>, candidate: &Ortho) {
    let candidate_volume: usize = candidate.dims().iter().map(|d| d.saturating_sub(1)).product();
    let candidate_filled: usize = candidate.payload().iter().filter(|x| x.is_some()).count();
    
    let is_optimal = if let Some(current_optimal) = optimal_ortho.as_ref() {
        let current_volume: usize = current_optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
        let current_filled: usize = current_optimal.payload().iter().filter(|x| x.is_some()).count();
        
        // First compare by volume, then by how filled they are
        candidate_volume > current_volume || 
        (candidate_volume == current_volume && candidate_filled > current_filled)
    } else {
        true
    };
    
    if is_optimal {
        *optimal_ortho = Some(candidate.clone());
    }
}

/// Find tokens whose bitsets have changed between two interner versions
fn find_changed_tokens(old_interner: &interner::Interner, new_interner: &interner::Interner) -> HashSet<usize> {
    let mut changed = HashSet::new();
    
    let old_vocab_len = old_interner.vocabulary().len();
    let new_vocab_len = new_interner.vocabulary().len();
    
    // ALL new tokens are considered "changed" - they didn't exist before
    // This allows existing orthos to be revisited with new vocabulary
    for token_idx in old_vocab_len..new_vocab_len {
        changed.insert(token_idx);
    }
    
    // Also check existing tokens whose completion bitsets have changed
    for token_idx in 0..old_vocab_len {
        // Check if this token's completion bitsets have changed
        // For simplicity, check single-token prefixes
        for prefix_idx in 0..old_vocab_len {
            let prefix = vec![prefix_idx];
            let old_completions = old_interner.completions_for_prefix(&prefix);
            let new_completions = new_interner.completions_for_prefix(&prefix);
            
            match (old_completions, new_completions) {
                (Some(old_bits), Some(new_bits)) => {
                    // Compare bitsets (ignoring padding differences)
                    let old_has = token_idx < old_bits.len() && old_bits.contains(token_idx);
                    let new_has = token_idx < new_bits.len() && new_bits.contains(token_idx);
                    
                    if old_has != new_has {
                        changed.insert(token_idx);
                    }
                }
                (Some(_), None) | (None, Some(_)) => {
                    // Prefix exists in one but not the other
                    changed.insert(token_idx);
                }
                (None, None) => {}
            }
        }
    }
    
    changed
}

/// Stream through ortho storage, checking each for changed tokens
/// Pops from ortho_storage, optionally pushes to work_queue if impacted,
/// and always pushes to new_storage to preserve all orthos
fn stream_revisit_orthos(
    changed_tokens: &HashSet<usize>,
    ortho_storage: &mut DiskQueue,
    work_queue: &mut DiskQueue,
) -> Result<usize, FoldError> {
    let mut seeded_count = 0;
    let mut new_storage = DiskQueue::new_persistent()?;
    
    // If there are changed tokens, ALL non-full orthos could potentially benefit
    // from the new vocabulary, so we revisit them all
    let has_changes = !changed_tokens.is_empty();
    
    // Stream through all orthos: pop, check, optionally seed to work queue, push to new storage
    while let Ok(Some(ortho)) = ortho_storage.pop_front() {
        // Revisit non-full orthos when there are new tokens
        // Full orthos can't accept new tokens, so skip them
        let is_full = ortho.get_current_position() >= ortho.payload().len();
        let should_revisit = has_changes && !is_full;
        
        // If impacted, seed it into the work queue
        if should_revisit {
            work_queue.push_back(ortho.clone())?;
            seeded_count += 1;
        }
        
        // Always preserve the ortho in new storage
        new_storage.push_back(ortho)?;
    }
    
    // Replace old storage with new storage (swap the disk files)
    *ortho_storage = new_storage;
    
    Ok(seeded_count)
}

#[cfg(test)]
mod follower_diff_tests {
    use super::*;
    use crate::interner::InMemoryInternerHolder;
    use crate::queue::MockQueue;

    fn build_low_high(low_text: &str, high_text: &str) -> (crate::interner::Interner, crate::interner::Interner) {
        // low_text ingested first, then high_text appended to create new version
        let mut holder = InMemoryInternerHolder::new().unwrap();
        let mut q = MockQueue::new();
        holder.add_text_with_seed(low_text, &mut q).unwrap(); // version 1
        holder.add_text_with_seed(high_text, &mut q).unwrap(); // version 2
        let low = holder.get(1).unwrap();
        let high = holder.get(2).unwrap();
        (low, high)
    }

    #[test]
    fn test_delta_intersection_adds_only_new() {
        let (low, high) = build_low_high("a b", "a c"); // low has a b; high adds a c
        // Construct ortho with single token 'a' (index 0)
        let mut o = crate::ortho::Ortho::new(low.version());
        o = o.add(0, low.version()).pop().unwrap();
        let (forbidden, required) = o.get_requirements();
        assert!(forbidden.is_empty());
        assert_eq!(required, vec![vec![0]]);
        let low_set: std::collections::HashSet<usize> = low.intersect(&required, &forbidden).into_iter().collect();
        let high_set: std::collections::HashSet<usize> = high.intersect(&required, &forbidden).into_iter().collect();
        assert!(low_set.contains(&1)); // 'b'
        assert!(!low_set.contains(&2)); // 'c' absent in low
        assert!(high_set.contains(&1) && high_set.contains(&2));
        let delta: Vec<usize> = high_set.difference(&low_set).copied().collect();
        assert_eq!(delta, vec![2]);
    }

    #[test]
    fn test_delta_union_intersection_logic() {
        let (low, high) = build_low_high("a b", "a c");
        let mut o = crate::ortho::Ortho::new(low.version());
        o = o.add(0, low.version()).pop().unwrap();
        let (forbidden, required) = o.get_requirements();
        let low_set: std::collections::HashSet<usize> = low.intersect(&required, &forbidden).into_iter().collect();
        let high_set: std::collections::HashSet<usize> = high.intersect(&required, &forbidden).into_iter().collect();
        assert!(low_set.contains(&1));
        assert!(!low_set.contains(&2));
        assert!(high_set.contains(&2));
        let diff: Vec<usize> = high_set.difference(&low_set).copied().collect();
        assert_eq!(diff, vec![2]);
    }
}
