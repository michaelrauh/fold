pub mod error;
pub mod interner;
pub mod ortho;
pub mod ortho_database;
pub mod queue;
pub mod spatial;
pub mod splitter;

pub use error::*;
pub use interner::*;
pub use ortho_database::*;
pub use queue::*;

use std::collections::{HashMap, HashSet, VecDeque};
use ortho::Ortho;

/// Process a single text through the worker loop, updating the interner and tracking optimal ortho.
/// Returns a tuple of (new_interner, changed_keys_count, frontier_size, impacted_frontier_count).
pub fn process_text(
    text: &str,
    interner: Option<interner::Interner>,
    seen_ids: &mut HashSet<usize>,
    optimal_ortho: &mut Option<Ortho>,
    frontier: &mut HashSet<usize>,
) -> (interner::Interner, usize, usize, usize) {
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
    
    // Track frontier orthos for checking impacted keys
    let mut frontier_orthos: HashMap<usize, Ortho> = HashMap::new();
    
    // Create seed ortho and work queue
    let seed_ortho = Ortho::new(version);
    let seed_id = seed_ortho.id();
    let mut work_queue: VecDeque<Ortho> = VecDeque::new();
    work_queue.push_back(seed_ortho.clone());
    
    // Add seed to frontier (will be removed if it produces children)
    frontier.insert(seed_id);
    frontier_orthos.insert(seed_id, seed_ortho);
    
    // Worker loop: process until queue is empty
    while let Some(ortho) = work_queue.pop_front() {
        let ortho_id = ortho.id();
        
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
                    frontier_orthos.insert(child_id, child.clone());
                    
                    // Check if this child is optimal
                    update_optimal(optimal_ortho, &child);
                    
                    work_queue.push_back(child);
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
    
    // Count frontier orthos that contain impacted keys in their payload
    let impacted_frontier_count = count_impacted_frontier_orthos(&frontier_orthos, &changed_keys);
    
    (current_interner, changed_keys_count, frontier_size, impacted_frontier_count)
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
    frontier_orthos.values().filter(|ortho| {
        ortho.payload().iter().any(|&opt_idx| {
            opt_idx.map_or(false, |idx| impacted_indices.contains(&idx))
        })
    }).count()
}

/// Update the optimal ortho if the new candidate is better
fn update_optimal(optimal_ortho: &mut Option<Ortho>, candidate: &Ortho) {
    let candidate_volume: usize = candidate.dims().iter().map(|d| d.saturating_sub(1)).product();
    let is_optimal = if let Some(current_optimal) = optimal_ortho.as_ref() {
        let current_volume: usize = current_optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
        candidate_volume > current_volume
    } else {
        true
    };
    
    if is_optimal {
        *optimal_ortho = Some(candidate.clone());
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
}
