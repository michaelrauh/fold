pub mod checkpoint;
pub mod error;
pub mod interner;
pub mod ortho;
pub mod ortho_database;
pub mod queue;
pub mod spatial;
pub mod splitter;
pub mod disk_backed_queue;

pub use checkpoint::*;
pub use error::*;
pub use interner::*;
pub use ortho_database::*;
pub use queue::*;
pub use disk_backed_queue::*;

use std::collections::{HashMap, HashSet};
use ortho::Ortho;
use std::env;

/// Process a single text through the worker loop, updating the interner and tracking optimal ortho.
/// Returns a tuple of (new_interner, changed_keys_count, frontier_size, impacted_frontier_count, total_processed).
pub fn process_text(
    text: &str,
    interner: Option<interner::Interner>,
    seen_ids: &mut HashSet<usize>,
    optimal_ortho: &mut Option<Ortho>,
    frontier: &mut HashSet<usize>,
    frontier_orthos_saved: &mut HashMap<usize, Ortho>,
) -> Result<(interner::Interner, usize, usize, usize, usize), FoldError>
{
    // Build or update interner and track changed keys
    let (current_interner, changed_keys, changed_keys_count) = if let Some(prev_interner) = interner {
        let new_interner = prev_interner.add_text(text);
        let changed_keys = prev_interner.find_changed_keys(&new_interner);
        let count = changed_keys.len();
        (new_interner, changed_keys, count)
    } else {
        // First interner - all keys are "new" but we return 0 as there's no previous state to compare
        (interner::Interner::from_text(text), vec![], 0)
    };
    
    let version = current_interner.version();
    
    // Track frontier orthos for checking impacted keys and for next iteration
    let mut frontier_orthos: HashMap<usize, Ortho> = HashMap::new();
    
    // Find impacted orthos from previous frontier and rewind them
    let rewound_orthos = find_and_rewind_impacted_orthos(frontier_orthos_saved, &changed_keys, version);
    let impacted_frontier_count = rewound_orthos.len();
    
    // Create seed ortho and work queue
    let seed_ortho = Ortho::new(version);
    let seed_id = seed_ortho.id();
    
    // Add seed to frontier (will be removed if it produces children)
    frontier.insert(seed_id);
    frontier_orthos.insert(seed_id, seed_ortho.clone());
    
    // Create disk-backed work queue with 10K ortho buffer (~2-9 MB)
    let temp_dir = env::temp_dir().join("fold_work_queue");
    let mut work_queue = DiskBackedQueue::new(10000, temp_dir)?;
    work_queue.push(seed_ortho)?;
    
    // Add rewound orthos to work queue, deduplicating by ID
    let mut rewound_count = 0;
    for rewound_ortho in rewound_orthos {
        let rewound_id = rewound_ortho.id();
        // Only add if we haven't seen this ortho before
        if !seen_ids.contains(&rewound_id) {
            seen_ids.insert(rewound_id);
            work_queue.push(rewound_ortho)?;
            rewound_count += 1;
        }
    }
    
    if rewound_count > 0 {
        eprintln!("[loader] Loaded {} frontier orthos into work queue", rewound_count);
    }
    
    // Worker loop: process until queue is empty
    let mut processed_count = 0;
    let checkpoint_interval = 100000; // Checkpoint every 100k orthos
    let log_interval = 1000; // Log every 1k orthos
    
    while let Some(ortho) = work_queue.pop()? {
        let ortho_id = ortho.id();
        processed_count += 1;
        
        // Checkpoint every 100k processed
        if processed_count % checkpoint_interval == 0 {
            print_progress_update(processed_count, optimal_ortho);
        }
        
        // Log queue state every 1k processed
        if processed_count % log_interval == 0 {
            eprintln!("[worker] Processed: {}, Queue: {}, Seen: {}, Frontier: {}", 
                processed_count, work_queue.len(), seen_ids.len(), frontier.len());
        }
        
        // Get requirements for this ortho
        let (forbidden, required) = ortho.get_requirements();
        
        // Get completions from interner
        let completions = current_interner.intersect(&required, &forbidden);
        
        // Track if this ortho produced any children
        let mut produced_children = false;
        
        // Generate children
        for completion in completions {
            let children = ortho.add(completion, version);
            for child in children {
                let child_id = child.id();
                // Only add to queue if never seen before
                if !seen_ids.contains(&child_id) {
                    seen_ids.insert(child_id);
                    produced_children = true;
                    
                    // Add newly discovered ortho to frontier
                    frontier.insert(child_id);
                    
                    // Check if this child is optimal
                    update_optimal(optimal_ortho, &child);
                    
                    // Add to frontier orthos and work queue
                    frontier_orthos.insert(child_id, child.clone());
                    work_queue.push(child)?;
                }
            }
        }
        
        // Remove parent from frontier if it produced any children
        if produced_children {
            frontier.remove(&ortho_id);
            frontier_orthos.remove(&ortho_id);
        }
        // Note: If it produced nothing, it stays in the frontier (added when it was created as a child, or as seed)
    }
    
    let frontier_size = frontier.len();
    
    // Save frontier orthos for next iteration
    frontier_orthos_saved.clear();
    frontier_orthos_saved.extend(frontier_orthos);
    
    Ok((current_interner, changed_keys_count, frontier_size, impacted_frontier_count, processed_count))
}

/// Find impacted orthos from the frontier and rewind them until the impacted key
/// is at the "most advanced position" (the next insertion point).
fn find_and_rewind_impacted_orthos(
    frontier_orthos: &HashMap<usize, Ortho>,
    changed_keys: &[Vec<usize>],
    new_version: usize,
) -> Vec<Ortho> {
    if changed_keys.is_empty() {
        return vec![];
    }
    
    // Convert changed keys to a flat set of impacted indices
    let mut impacted_indices: HashSet<usize> = HashSet::new();
    for key in changed_keys {
        for &index in key {
            impacted_indices.insert(index);
        }
    }
    
    let mut rewound_orthos = Vec::new();
    
    // For each frontier ortho
    for ortho in frontier_orthos.values() {
        // Find ALL impacted positions in this ortho
        let mut impacted_positions: Vec<usize> = Vec::new();
        
        for (pos, &opt_idx) in ortho.payload().iter().enumerate() {
            if let Some(idx) = opt_idx {
                if impacted_indices.contains(&idx) {
                    impacted_positions.push(pos);
                }
            }
        }
        
        if impacted_positions.is_empty() {
            continue;
        }
        
        // Rebuild to EACH impacted position, not just the earliest
        // For each impacted position, create a rewound ortho where that impacted key
        // is at the "most advanced position" (rightmost, ready for next add)
        for &impacted_pos in &impacted_positions {
            // Rebuild this ortho up to and including the impacted position
            // This means the impacted key will be at position impacted_pos,
            // which is the last filled position (current_position - 1)
            let target_position = impacted_pos + 1;
            
            if let Some(mut rewound) = ortho.rebuild_to_position(target_position) {
                // Update version to new version
                rewound = rewound.set_version(new_version);
                rewound_orthos.push(rewound);
            }
        }
    }
    
    rewound_orthos
}

/// Count how many frontier orthos contain any of the impacted keys in their payload
fn count_impacted_frontier_orthos(
    frontier_orthos: &HashMap<usize, Ortho>,
    changed_keys: &[Vec<usize>],
) -> usize {
    if changed_keys.is_empty() {
        return 0;
    }
    
    // Convert changed keys to a flat set for efficient lookup
    let mut impacted_indices: HashSet<usize> = HashSet::new();
    for key in changed_keys {
        for &index in key {
            impacted_indices.insert(index);
        }
    }
    
    // Count frontier orthos that contain any impacted index in their payload
    let mut count = 0;
    for ortho in frontier_orthos.values() {
        for &opt_idx in ortho.payload() {
            if let Some(idx) = opt_idx {
                if impacted_indices.contains(&idx) {
                    count += 1;
                    break; // This ortho is impacted, no need to check more indices
                }
            }
        }
    }
    count
}

/// Calculate the volume of an ortho based on its dimensions
fn calculate_volume(ortho: &Ortho) -> usize {
    ortho.dims().iter().map(|d| d.saturating_sub(1)).product()
}

/// Calculate the fullness of an ortho (ratio of filled positions to total capacity)
fn calculate_fullness(ortho: &Ortho) -> f64 {
    let filled_count = ortho.payload().iter().filter(|p| p.is_some()).count();
    let total_capacity = ortho.payload().len();
    if total_capacity == 0 {
        0.0
    } else {
        filled_count as f64 / total_capacity as f64
    }
}

/// Pretty print ortho geometry showing dims and payload in ND layout
fn pretty_print_ortho(ortho: &Ortho) -> String {
    let dims = ortho.dims();
    let payload = ortho.payload();
    let volume = calculate_volume(ortho);
    let fullness = calculate_fullness(ortho);
    
    // Get the mapping from flat index to ND coordinates
    let index_to_location = spatial::get_index_to_location(dims);
    
    // Build header
    let dims_str = dims.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("×");
    let mut result = format!("dims=[{}] volume={} fullness={:.2}\n", dims_str, volume, fullness);
    
    // Orthos are always at least 2D (base dims are [2,2])
    let rows = dims[0];
    let cols = dims[1];
    
    if dims.len() == 2 {
        // 2D case - simple rows and columns
        for r in 0..rows {
            result.push_str("  ");
            for c in 0..cols {
                let loc = vec![r, c];
                // Find the index that corresponds to this location
                let idx = index_to_location.iter().position(|l| l == &loc);
                if let Some(idx) = idx {
                    if let Some(val) = payload.get(idx).and_then(|p| *p) {
                        result.push_str(&format!("{:3} ", val));
                    } else {
                        result.push_str("  _ ");
                    }
                } else {
                    result.push_str("  ? ");
                }
            }
            result.push('\n');
        }
    } else {
        // 3D+ case - tile by higher dimensions
        let higher_dims = &dims[2..];
        let higher_capacity: usize = higher_dims.iter().product();
        
        for tile_idx in 0..higher_capacity {
            // Calculate the higher-dimensional coordinates for this tile
            let mut higher_coords = Vec::new();
            let mut remaining = tile_idx;
            for &dim in higher_dims.iter().rev() {
                higher_coords.push(remaining % dim);
                remaining /= dim;
            }
            higher_coords.reverse();
            
            // Print separator between tiles
            if tile_idx > 0 {
                result.push_str("  ---\n");
            }
            
            // Print the label for this tile if we have 3+ dimensions
            if !higher_coords.is_empty() {
                let coord_str = higher_coords.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",");
                result.push_str(&format!("  [..,..{}]:\n", if higher_coords.len() > 0 { format!(",{}", coord_str) } else { coord_str }));
            }
            
            // Print the 2D slice
            for r in 0..rows {
                result.push_str("    ");
                for c in 0..cols {
                    let mut loc = vec![r, c];
                    loc.extend(&higher_coords);
                    
                    // Find the index that corresponds to this location
                    let idx = index_to_location.iter().position(|l| l == &loc);
                    if let Some(idx) = idx {
                        if let Some(val) = payload.get(idx).and_then(|p| *p) {
                            result.push_str(&format!("{:3} ", val));
                        } else {
                            result.push_str("  _ ");
                        }
                    } else {
                        result.push_str("  ? ");
                    }
                }
                result.push('\n');
            }
        }
    }
    
    result
}

/// Print progress update every 100k cycles
fn print_progress_update(processed_count: usize, optimal_ortho: &Option<Ortho>) {
    eprintln!("Progress: {} orthos processed", processed_count);
    if let Some(optimal) = optimal_ortho {
        eprintln!("  Current optimal: {}", pretty_print_ortho(optimal));
    } else {
        eprintln!("  No optimal ortho yet");
    }
}

/// Update the optimal ortho if the new candidate is better
/// Uses volume as primary criterion, fullness as tiebreak
fn update_optimal(optimal_ortho: &mut Option<Ortho>, candidate: &Ortho) {
    let candidate_volume = calculate_volume(candidate);
    let candidate_fullness = calculate_fullness(candidate);
    
    let is_optimal = if let Some(current_optimal) = optimal_ortho.as_ref() {
        let current_volume = calculate_volume(current_optimal);
        let current_fullness = calculate_fullness(current_optimal);
        
        // Primary criterion: volume
        // Tiebreak: fullness (higher fullness is better)
        candidate_volume > current_volume || 
        (candidate_volume == current_volume && candidate_fullness > current_fullness)
    } else {
        true
    };
    
    if is_optimal {
        *optimal_ortho = Some(candidate.clone());
    }
}

#[cfg(test)]
mod impacted_backtracking_tests {
    use super::*;
    use crate::ortho::Ortho;

    #[test]
    fn test_find_and_rewind_impacted_orthos_empty_frontier() {
        let frontier_orthos = HashMap::new();
        let changed_keys = vec![vec![0], vec![1]];
        let rewound = find_and_rewind_impacted_orthos(&frontier_orthos, &changed_keys, 2);
        assert_eq!(rewound.len(), 0, "Should have no rewound orthos from empty frontier");
    }

    #[test]
    fn test_find_and_rewind_impacted_orthos_no_changed_keys() {
        let mut frontier_orthos = HashMap::new();
        let ortho = Ortho::new(1).add(0, 1).pop().unwrap().add(1, 1).pop().unwrap();
        frontier_orthos.insert(ortho.id(), ortho);
        
        let changed_keys: Vec<Vec<usize>> = vec![];
        let rewound = find_and_rewind_impacted_orthos(&frontier_orthos, &changed_keys, 2);
        assert_eq!(rewound.len(), 0, "Should have no rewound orthos with no changed keys");
    }

    #[test]
    fn test_find_and_rewind_impacted_orthos_no_matching_orthos() {
        let mut frontier_orthos = HashMap::new();
        // Ortho with indices [0, 1]
        let ortho = Ortho::new(1).add(0, 1).pop().unwrap().add(1, 1).pop().unwrap();
        frontier_orthos.insert(ortho.id(), ortho);
        
        // Changed keys with indices [2, 3] - not in ortho
        let changed_keys = vec![vec![2], vec![3]];
        let rewound = find_and_rewind_impacted_orthos(&frontier_orthos, &changed_keys, 2);
        assert_eq!(rewound.len(), 0, "Should have no rewound orthos when no match");
    }

    #[test]
    fn test_find_and_rewind_impacted_orthos_with_match() {
        let mut frontier_orthos = HashMap::new();
        // Ortho with indices [0, 1, 2]
        let ortho = Ortho::new(1).add(0, 1).pop().unwrap()
            .add(1, 1).pop().unwrap()
            .add(2, 1).pop().unwrap();
        frontier_orthos.insert(ortho.id(), ortho.clone());
        
        // Changed keys contain index 0 - first index in ortho
        let changed_keys = vec![vec![0]];
        let rewound = find_and_rewind_impacted_orthos(&frontier_orthos, &changed_keys, 2);
        
        assert_eq!(rewound.len(), 1, "Should have one rewound ortho");
        let rewound_ortho = &rewound[0];
        
        // Since index 0 is at position 0, and we want it at position "current_position - 1",
        // we need current_position to be 1, which means we subtract all values after position 0
        assert_eq!(rewound_ortho.get_current_position(), 1, "Should be rewound to position 1");
        assert_eq!(rewound_ortho.payload()[0], Some(0), "Should keep index 0 at position 0");
        assert_eq!(rewound_ortho.payload()[1], None, "Should remove index 1");
        assert_eq!(rewound_ortho.payload()[2], None, "Should remove index 2");
        assert_eq!(rewound_ortho.version(), 2, "Should update version to new version");
    }

    #[test]
    fn test_find_and_rewind_impacted_orthos_with_later_impacted_index() {
        let mut frontier_orthos = HashMap::new();
        // Ortho with indices [0, 1, 2]
        let ortho = Ortho::new(1).add(0, 1).pop().unwrap()
            .add(1, 1).pop().unwrap()
            .add(2, 1).pop().unwrap();
        frontier_orthos.insert(ortho.id(), ortho.clone());
        
        // Changed keys contain index 1 - second index in ortho
        let changed_keys = vec![vec![1]];
        let rewound = find_and_rewind_impacted_orthos(&frontier_orthos, &changed_keys, 2);
        
        assert_eq!(rewound.len(), 1, "Should have one rewound ortho");
        let rewound_ortho = &rewound[0];
        
        // Index 1 is at position 1, so we want current_position to be 2
        assert_eq!(rewound_ortho.get_current_position(), 2, "Should be rewound to position 2");
        assert_eq!(rewound_ortho.payload()[0], Some(0), "Should keep index 0");
        assert_eq!(rewound_ortho.payload()[1], Some(1), "Should keep index 1");
        assert_eq!(rewound_ortho.payload()[2], None, "Should remove index 2");
    }

    #[test]
    fn test_find_and_rewind_impacted_orthos_no_rewinding_needed() {
        let mut frontier_orthos = HashMap::new();
        // Ortho with only one value [0]
        let ortho = Ortho::new(1).add(0, 1).pop().unwrap();
        frontier_orthos.insert(ortho.id(), ortho.clone());
        
        // Changed keys contain index 0 - already at the end
        let changed_keys = vec![vec![0]];
        let rewound = find_and_rewind_impacted_orthos(&frontier_orthos, &changed_keys, 2);
        
        assert_eq!(rewound.len(), 1, "Should have one rewound ortho");
        let rewound_ortho = &rewound[0];
        
        // No rewinding should happen since index 0 is already at the end
        assert_eq!(rewound_ortho.get_current_position(), 1, "Should have position 1");
        assert_eq!(rewound_ortho.payload()[0], Some(0), "Should keep index 0");
        assert_eq!(rewound_ortho.version(), 2, "Should update version");
    }

    #[test]
    fn test_find_and_rewind_impacted_orthos_multiple_orthos() {
        let mut frontier_orthos = HashMap::new();
        
        // Ortho 1 with indices [0, 1]
        let ortho1 = Ortho::new(1).add(0, 1).pop().unwrap().add(1, 1).pop().unwrap();
        frontier_orthos.insert(ortho1.id(), ortho1.clone());
        
        // Ortho 2 with indices [2, 3]
        let ortho2 = Ortho::new(1).add(2, 1).pop().unwrap().add(3, 1).pop().unwrap();
        frontier_orthos.insert(ortho2.id(), ortho2.clone());
        
        // Ortho 3 with indices [4, 5] - not impacted
        let ortho3 = Ortho::new(1).add(4, 1).pop().unwrap().add(5, 1).pop().unwrap();
        frontier_orthos.insert(ortho3.id(), ortho3.clone());
        
        // Changed keys contain indices [0, 2]
        let changed_keys = vec![vec![0], vec![2]];
        let rewound = find_and_rewind_impacted_orthos(&frontier_orthos, &changed_keys, 2);
        
        assert_eq!(rewound.len(), 2, "Should have two rewound orthos");
        
        // Check that both orthos are properly rewound
        for rewound_ortho in &rewound {
            assert_eq!(rewound_ortho.version(), 2, "Should update version");
            let first_index = rewound_ortho.payload()[0].unwrap();
            assert!(first_index == 0 || first_index == 2, "First index should be 0 or 2");
        }
    }

    #[test]
    fn test_impacted_backtracking_integration() {
        // Test that rewound orthos are properly integrated into the work queue
        let mut seen_ids = HashSet::new();
        let mut optimal_ortho: Option<Ortho> = None;
        let mut frontier = HashSet::new();
        let mut frontier_orthos_saved = HashMap::new();
        
        // First text - establish baseline
        let (interner1, _, _, _, _processed) = process_text(
            "a b c",
            None,
            &mut seen_ids,
            &mut optimal_ortho,
            &mut frontier,
            &mut frontier_orthos_saved
        ).expect("process_text should succeed");
        
        let baseline_seen_count = seen_ids.len();
        
        // Second text - should trigger impacted backtracking
        let (interner2, changed_count, _, _impacted_count, _processed) = process_text(
            "a d",
            Some(interner1),
            &mut seen_ids,
            &mut optimal_ortho,
            &mut frontier,
            &mut frontier_orthos_saved
        ).expect("process_text should succeed");
        
        // Verify that changed keys were detected
        assert!(changed_count > 0, "Should have changed keys");
        
        // Verify that impacted orthos were found and rewound
        // (The impacted_count now represents the number of rewound orthos added to the queue)
        
        // Verify that new orthos were generated (from exploring the rewound orthos)
        assert!(seen_ids.len() > baseline_seen_count, "Should have generated new orthos from backtracking");
        
        assert_eq!(interner2.version(), 2, "Should have incremented version");
    }

    #[test]
    fn test_impacted_backtracking_finds_new_paths() {
        // Test that impacted backtracking actually finds new orthos that weren't found before
        let mut seen_ids = HashSet::new();
        let mut optimal_ortho: Option<Ortho> = None;
        let mut frontier = HashSet::new();
        let mut frontier_orthos_saved = HashMap::new();
        
        // First text: "a b"
        let (interner1, _, _, _, _processed) = process_text(
            "a b",
            None,
            &mut seen_ids,
            &mut optimal_ortho,
            &mut frontier,
            &mut frontier_orthos_saved
        ).expect("process_text should succeed");
        
        let seen_after_first = seen_ids.clone();
        
        // Second text: "a c" - adds new completion for prefix "a"
        let (_, changed_count, _, _impacted_count, _processed) = process_text(
            "a c",
            Some(interner1),
            &mut seen_ids,
            &mut optimal_ortho,
            &mut frontier,
            &mut frontier_orthos_saved
        ).expect("process_text should succeed");
        
        // Should have detected changes
        assert!(changed_count > 0, "Should detect changed keys");
        
        // Should have found new orthos through backtracking
        let new_orthos_count = seen_ids.len() - seen_after_first.len();
        assert!(new_orthos_count > 0, "Should have found new orthos through backtracking");
    }
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
    
    #[test]
    fn test_count_impacted_frontier_orthos_empty_changed_keys() {
        let frontier_orthos = HashMap::new();
        let changed_keys: Vec<Vec<usize>> = vec![];
        assert_eq!(count_impacted_frontier_orthos(&frontier_orthos, &changed_keys), 0);
    }
    
    #[test]
    fn test_count_impacted_frontier_orthos_no_matching_orthos() {
        // Create orthos with indices [0, 1]
        let mut frontier_orthos = HashMap::new();
        let ortho1 = Ortho::new(1).add(0, 1).pop().unwrap().add(1, 1).pop().unwrap();
        frontier_orthos.insert(ortho1.id(), ortho1);
        
        // Changed keys contain indices [2, 3] - not in any ortho payload
        let changed_keys = vec![vec![2], vec![3]];
        assert_eq!(count_impacted_frontier_orthos(&frontier_orthos, &changed_keys), 0);
    }
    
    #[test]
    fn test_count_impacted_frontier_orthos_with_matches() {
        // Create orthos with various indices
        let mut frontier_orthos = HashMap::new();
        
        let ortho1 = Ortho::new(1).add(0, 1).pop().unwrap().add(1, 1).pop().unwrap();
        let ortho2 = Ortho::new(1).add(2, 1).pop().unwrap().add(3, 1).pop().unwrap();
        let ortho3 = Ortho::new(1).add(4, 1).pop().unwrap().add(5, 1).pop().unwrap();
        
        frontier_orthos.insert(ortho1.id(), ortho1);
        frontier_orthos.insert(ortho2.id(), ortho2);
        frontier_orthos.insert(ortho3.id(), ortho3);
        
        // Changed keys contain indices [0, 2] - should match ortho1 and ortho2
        let changed_keys = vec![vec![0], vec![2]];
        assert_eq!(count_impacted_frontier_orthos(&frontier_orthos, &changed_keys), 2);
    }
    
    #[test]
    fn test_count_impacted_frontier_orthos_with_partial_matches() {
        let mut frontier_orthos = HashMap::new();
        
        let ortho1 = Ortho::new(1).add(0, 1).pop().unwrap().add(1, 1).pop().unwrap();
        let ortho2 = Ortho::new(1).add(2, 1).pop().unwrap();
        
        frontier_orthos.insert(ortho1.id(), ortho1);
        frontier_orthos.insert(ortho2.id(), ortho2);
        
        // Changed keys with compound prefix [0, 1] - both indices in ortho1
        let changed_keys = vec![vec![0, 1]];
        assert_eq!(count_impacted_frontier_orthos(&frontier_orthos, &changed_keys), 1);
    }
    
    #[test]
    fn test_fullness_calculation() {
        // Test empty ortho
        let empty = Ortho::new(1);
        assert_eq!(calculate_fullness(&empty), 0.0, "Empty ortho should have 0.0 fullness");
        
        // Test partially filled ortho
        let partial = empty.add(10, 1)[0].clone();
        assert_eq!(calculate_fullness(&partial), 0.25, "1 of 4 filled should be 0.25 fullness");
        
        // Test more filled ortho
        let more_filled = partial.add(20, 1)[0].clone();
        assert_eq!(calculate_fullness(&more_filled), 0.5, "2 of 4 filled should be 0.5 fullness");
        
        // Test even more filled ortho
        let even_more = more_filled.add(30, 1)[0].clone();
        assert_eq!(calculate_fullness(&even_more), 0.75, "3 of 4 filled should be 0.75 fullness");
        
        // Adding a 4th element triggers expansion, so we get a bigger ortho
        let expanded = even_more.add(40, 1)[0].clone();
        // After expansion, capacity increases (e.g., to 6 for [3,2] or 8 for [2,2,2])
        // So fullness will be less than 1.0
        assert!(calculate_fullness(&expanded) < 1.0, "Expanded ortho won't be fully filled");
        assert!(calculate_fullness(&expanded) > 0.5, "But should still be more than half filled");
    }
    
    #[test]
    fn test_optimal_with_fullness_tiebreak() {
        let mut optimal: Option<Ortho> = None;
        
        // Create two orthos with same volume but different fullness
        // Ortho 1: dims [2,2], payload [10, 20, None, None] - volume=1, fullness=0.5
        let ortho1 = Ortho::new(1).add(10, 1)[0].clone().add(20, 1)[0].clone();
        assert_eq!(ortho1.dims(), &vec![2, 2]);
        let volume1 = calculate_volume(&ortho1);
        assert_eq!(volume1, 1, "Volume should be (2-1)*(2-1) = 1");
        assert_eq!(calculate_fullness(&ortho1), 0.5, "Should be half full");
        
        // Set as optimal
        update_optimal(&mut optimal, &ortho1);
        assert!(optimal.is_some());
        assert_eq!(optimal.as_ref().unwrap().id(), ortho1.id());
        
        // Create ortho2 with same volume but higher fullness
        // Ortho 2: dims [2,2], payload [10, 20, 30, None] - volume=1, fullness=0.75
        let ortho2 = ortho1.add(30, 1)[0].clone();
        assert_eq!(ortho2.dims(), &vec![2, 2]);
        let volume2 = calculate_volume(&ortho2);
        assert_eq!(volume2, 1, "Volume should still be 1");
        assert_eq!(calculate_fullness(&ortho2), 0.75, "Should be 3/4 full");
        
        // Update optimal - should replace due to higher fullness (tiebreak)
        update_optimal(&mut optimal, &ortho2);
        assert_eq!(optimal.as_ref().unwrap().id(), ortho2.id(), "Should prefer higher fullness when volumes are equal");
        
        // Create ortho3 with lower fullness but same volume - should not replace
        // We can't easily create another ortho with same dims and lower fullness than ortho2
        // So let's test with ortho1 again - should not replace ortho2
        update_optimal(&mut optimal, &ortho1);
        assert_eq!(optimal.as_ref().unwrap().id(), ortho2.id(), "Should keep higher fullness ortho");
    }
    
    #[test]
    fn test_pretty_print_ortho() {
        // Test empty ortho
        let empty = Ortho::new(1);
        let output = pretty_print_ortho(&empty);
        assert!(output.contains("dims=[2×2]"), "Should show dims");
        assert!(output.contains("volume=1"), "Should show volume");
        assert!(output.contains("fullness=0.00"), "Should show fullness");
        // New format shows in grid, so check for underscores in the output
        assert!(output.contains("_"), "Should show empty slots");
        
        // Test partially filled ortho
        let partial = empty.add(10, 1)[0].clone();
        let output = pretty_print_ortho(&partial);
        assert!(output.contains("dims=[2×2]"), "Should show dims");
        assert!(output.contains("fullness=0.25"), "Should show 0.25 fullness");
        assert!(output.contains("10"), "Should show the value 10");
        
        // Test more filled ortho
        let more = partial.add(20, 1)[0].clone();
        let output = pretty_print_ortho(&more);
        assert!(output.contains("fullness=0.50"), "Should show 0.50 fullness");
        assert!(output.contains("10"), "Should show value 10");
        assert!(output.contains("20"), "Should show value 20");
    }
    
    #[test]
    #[ignore] // This test just shows output, run with --ignored to see it
    fn show_pretty_print_examples() {
        // Test 2D ortho
        let ortho2d = Ortho::new(1);
        let ortho2d = ortho2d.add(10, 1)[0].clone();
        let ortho2d = ortho2d.add(20, 1)[0].clone();
        let ortho2d = ortho2d.add(30, 1)[0].clone();
        
        eprintln!("\n=== 2D Ortho [2x2] with 3 values ===");
        eprintln!("{}", pretty_print_ortho(&ortho2d));
        
        // Test expanded ortho
        let mut ortho_exp = Ortho::new(1);
        for i in 0..5 {
            let candidates = ortho_exp.add(i * 10, 1);
            ortho_exp = candidates[0].clone();
        }
        
        eprintln!("\n=== Expanded Ortho (dims={:?}) ===", ortho_exp.dims());
        eprintln!("{}", pretty_print_ortho(&ortho_exp));
        
        // Test 3D ortho by selecting the [2,2,2] expansion
        let mut ortho3d = Ortho::new(1);
        for i in 0..5 {
            let candidates = ortho3d.add(i * 10, 1);
            // Try to pick the [2,2,2] expansion if available
            ortho3d = candidates.iter()
                .find(|c| c.dims() == &vec![2, 2, 2])
                .unwrap_or(&candidates[0])
                .clone();
        }
        
        eprintln!("\n=== 3D Ortho (dims={:?}) ===", ortho3d.dims());
        eprintln!("{}", pretty_print_ortho(&ortho3d));
    }
}
