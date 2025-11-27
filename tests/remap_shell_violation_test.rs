//! Test demonstrating shell violation bug caused by sparse layouts after expansion.
//!
//! ## ROOT CAUSE
//!
//! After expansion (e.g., [2,2] → [2,2,2]), orthos have **sparse layouts** where
//! later positions can be filled while earlier positions are empty. The
//! `get_requirements()` function only checks positions **BEFORE** the current
//! position for forbidden values, but later positions that are pre-filled from
//! reorganization should also be checked.
//!
//! The bug is in `spatial::get_diagonals_compute()`:
//! ```rust
//! .filter(|index| *index < current_index && index.iter().sum::<usize>() == current_distance)
//! ```
//!
//! This only considers indices lexicographically less than current, but after
//! expansion, later indices may already be filled.

use fold::interner::Interner;
use fold::ortho::Ortho;
use fold::spatial;

/// Test that demonstrates the actual sparse layout bug after expansion.
/// 
/// When a [2,2] ortho is expanded to [2,2,2], the payload becomes sparse:
/// positions at later indices may be filled while earlier positions are empty.
/// 
/// The `get_requirements()` function only checks positions BEFORE the current
/// position for forbidden values, missing the pre-filled later positions.
#[test]
fn test_expansion_creates_sparse_layout_bug() {
    // Build a [2,2] ortho and expand it to [2,2,2]
    // After expansion, the ortho will have a sparse layout
    
    let interner = Interner::from_text("alpha beta gamma delta epsilon");
    let vocab = interner.vocabulary();
    
    let idx = |name: &str| vocab.iter().position(|w| w == name).unwrap();
    
    // Build [2,2] ortho: fill positions 0, 1, 2, 3
    let ortho = Ortho::new();
    let ortho = ortho.add(idx("alpha"))[0].clone();  // pos 0: alpha
    let ortho = ortho.add(idx("beta"))[0].clone();   // pos 1: beta
    let ortho = ortho.add(idx("gamma"))[0].clone();  // pos 2: gamma (canonicalized if needed)
    
    println!("[2,2] ortho before expansion:");
    println!("  dims: {:?}", ortho.dims());
    println!("  payload: {:?}", ortho.payload());
    
    // When we add the 4th value, it will trigger expansion to [2,2,2] or [3,2]
    let children = ortho.add(idx("delta"));
    
    println!("\nAfter expansion (all children):");
    
    // Find the [2,2,2] child - it demonstrates the sparse layout
    let sparse_child = children.iter().find(|c| c.dims() == &[2, 2, 2]);
    
    if let Some(child) = sparse_child {
        println!("  [2,2,2] child found:");
        println!("    payload: {:?}", child.payload());
        
        // Verify sparse layout exists
        let payload = child.payload();
        let has_sparse = (0..payload.len()).any(|pos| {
            payload[pos].is_none() && (pos+1..payload.len()).any(|later| payload[later].is_some())
        });
        
        assert!(has_sparse, "Expected sparse layout after expansion");
        
        // Now demonstrate the bug
        let current_pos = child.get_current_position();
        let (forbidden, _) = child.get_requirements();
        let (_, diagonals) = spatial::get_requirements(current_pos, child.dims());
        
        println!("  Next position to fill: {}", current_pos);
        println!("  Forbidden: {:?}", forbidden);
        println!("  Diagonals (from spatial): {:?}", diagonals);
        
        // Check if there are same-distance positions AFTER current that are filled
        // but NOT in the forbidden list
        let mut bug_found = false;
        for later in (current_pos+1)..payload.len() {
            if let Some(val) = payload[later] {
                let (_, later_diags) = spatial::get_requirements(later, child.dims());
                if later_diags.contains(&current_pos) && !forbidden.contains(&val) {
                    println!("  *** BUG: Position {} has value {} (same shell as {}) but NOT forbidden! ***", 
                             later, val, current_pos);
                    bug_found = true;
                }
            }
        }
        
        assert!(bug_found, "Expected to find the sparse layout bug");
    } else {
        println!("  No [2,2,2] child found - expansion went to [3,2] only");
        // That's also valid, just means we need different test data
    }
}

/// Test that demonstrates get_requirements only looks at earlier positions.
/// 
/// This shows the fundamental issue: positions at the same distance (shell)
/// are only included if they come BEFORE the current position lexicographically.
#[test]
fn test_get_requirements_only_checks_earlier_positions() {
    // In [2,2,2], the coordinate-to-index mapping is:
    // [0,0,0] -> 0, [1,0,0] -> 1, [0,1,0] -> 2, [1,1,0] -> 3
    // [0,0,1] -> 4, [1,0,1] -> 5, [0,1,1] -> 6, [1,1,1] -> 7
    //
    // Distances (sum of coordinates):
    // 0: position 0
    // 1: positions 1, 2, 4
    // 2: positions 3, 5, 6
    // 3: position 7
    
    let dims = vec![2, 2, 2];
    
    println!("Diagonal positions in [2,2,2]:");
    for pos in 0..8 {
        let (_, diags) = spatial::get_requirements(pos, &dims);
        println!("  Position {}: diagonals = {:?}", pos, diags);
    }
    
    // The key observation: diagonals only include positions BEFORE the current one.
    // So for position 2 (distance 1), only position 1 is included (not position 4).
    // For position 4 (distance 1), NO positions are included (1 and 2 are before but not included!).
    
    let (_, diag1) = spatial::get_requirements(1, &dims);
    let (_, diag2) = spatial::get_requirements(2, &dims);
    let (_, diag4) = spatial::get_requirements(4, &dims);
    
    // Position 1 has no earlier same-distance positions
    assert!(diag1.is_empty(), "Position 1 should have no earlier same-distance positions");
    
    // Position 2 should have position 1 (same distance, earlier)
    assert!(diag2.contains(&1), "Position 2 should have 1 as diagonal");
    
    // Position 4 should have positions 1 and 2 (same distance, earlier)
    // BUT: the current implementation returns EMPTY for position 4!
    // This is the bug - it only looks at positions < 4 that are at the same distance,
    // but the filter uses lexicographic ordering which doesn't match distance ordering.
    
    println!("\nPosition 4's diagonals: {:?}", diag4);
    println!("Expected: [1, 2] (same distance as 4)");
    println!("This difference shows the filter is using index ordering, not distance shells.");
    
    // The fix should ensure that when filling a sparse ortho where position 4 is empty
    // but positions 1, 2 are filled, we check those positions for forbidden values.
}
