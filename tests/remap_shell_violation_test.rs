//! Tests verifying that the shell violation fix works correctly.
//!
//! ## THE BUG (FIXED)
//!
//! After expansion (e.g., [2,2] → [2,2,2]), orthos have **sparse layouts** where
//! later positions can be filled while earlier positions are empty. The old
//! `get_requirements()` function only checked positions **BEFORE** the current
//! position for forbidden values, but later positions that are pre-filled from
//! reorganization should also be checked.
//!
//! ## THE FIX
//!
//! Modified `spatial::get_diagonals_compute()` to include prefilled positions
//! from the predecessor dims. Now diagonals include both earlier positions
//! AND later positions that are prefilled from reorganization.

use fold::spatial;

/// Test that the fix correctly includes prefilled positions in diagonals.
///
/// After expansion to [2,2,2], positions 0,2,3,6 are prefilled from [2,2].
/// For position 1 (empty after expansion), diagonals should include
/// positions 2 and 3 (same distance = 1) since they are prefilled.
#[test]
fn test_fix_includes_prefilled_positions_in_diagonals() {
    // [2,2,2] comes from expand_up of [2,2]
    // remap_for_up([2,2], 0) gives prefilled positions: [0, 2, 3, 6]
    //
    // The sorted order by (distance, then lexicographic) for [2,2,2]:
    // idx=0: [0,0,0] distance 0
    // idx=1: [0,0,1] distance 1 (NOT prefilled)
    // idx=2: [0,1,0] distance 1 (prefilled)
    // idx=3: [1,0,0] distance 1 (prefilled)
    // idx=4: [0,1,1] distance 2 (NOT prefilled)
    // idx=5: [1,0,1] distance 2 (NOT prefilled)
    // idx=6: [1,1,0] distance 2 (prefilled)
    // idx=7: [1,1,1] distance 3 (NOT prefilled)
    
    let dims = vec![2, 2, 2];
    
    // Position 1 (NOT prefilled) should include positions 2 and 3 (prefilled, same distance)
    let (_, diag1) = spatial::get_requirements(1, &dims);
    println!("Position 1 diagonals: {:?}", diag1);
    assert!(diag1.contains(&2), "Position 1 should include prefilled position 2 in diagonals");
    assert!(diag1.contains(&3), "Position 1 should include prefilled position 3 in diagonals");
    
    // Position 4 (NOT prefilled, distance 2) should include position 6 (prefilled, same distance)
    let (_, diag4) = spatial::get_requirements(4, &dims);
    println!("Position 4 diagonals: {:?}", diag4);
    assert!(diag4.contains(&6), "Position 4 should include prefilled position 6 in diagonals");
    
    // Position 5 (NOT prefilled, distance 2) should include positions 4 and 6
    // (4 is earlier, 6 is prefilled)
    let (_, diag5) = spatial::get_requirements(5, &dims);
    println!("Position 5 diagonals: {:?}", diag5);
    assert!(diag5.contains(&4), "Position 5 should include position 4 in diagonals");
    assert!(diag5.contains(&6), "Position 5 should include prefilled position 6 in diagonals");
}

/// Test that [2,2] base case has no prefilled positions (no predecessor).
#[test]
fn test_base_dims_have_no_prefilled() {
    let dims = vec![2, 2];
    
    // In [2,2], there's no predecessor so no prefilled positions.
    // Diagonals should only include earlier positions.
    
    // Distance 1: positions 1, 2
    let (_, diag1) = spatial::get_requirements(1, &dims);
    let (_, diag2) = spatial::get_requirements(2, &dims);
    
    println!("[2,2] Position 1 diagonals: {:?}", diag1);
    println!("[2,2] Position 2 diagonals: {:?}", diag2);
    
    // Position 1 should have no diagonals (no earlier same-distance positions)
    assert!(diag1.is_empty(), "Position 1 in [2,2] should have no diagonals");
    
    // Position 2 should include position 1 (earlier, same distance)
    assert!(diag2.contains(&1), "Position 2 in [2,2] should include position 1");
}

/// Test that [3,2] (from expand_over) correctly handles prefilled positions.
#[test]
fn test_expand_over_prefilled() {
    // [3,2] comes from expand_over of [2,2]
    // remap([2,2], [3,2]) gives prefilled positions: [0, 1, 2, 3]
    //
    // Distance layout in [3,2]:
    // Distance 0: position 0
    // Distance 1: positions 1, 2
    // Distance 2: positions 3, 4
    // Distance 3: position 5
    
    let dims = vec![3, 2];
    
    // Position 4 (empty after expansion) should include position 3 (prefilled, same distance)
    let (_, diag4) = spatial::get_requirements(4, &dims);
    println!("[3,2] Position 4 diagonals: {:?}", diag4);
    assert!(diag4.contains(&3), "Position 4 should include prefilled position 3 in diagonals");
    
    // Position 5 (empty after expansion) has no same-distance prefilled positions
    let (_, diag5) = spatial::get_requirements(5, &dims);
    println!("[3,2] Position 5 diagonals: {:?}", diag5);
    // Position 5 is the only position at distance 3, so no diagonals
}

/// Test no regression: earlier positions at same distance are still included.
#[test]
fn test_earlier_positions_still_included() {
    let dims = vec![3, 3];
    
    // In [3,3], position 5 (coords [1,1]) has distance 2
    // Other distance-2 positions: 3, 4 (coords [0,2], [2,0])
    // Both 3 and 4 are earlier than 5, so should be in diagonals
    
    let (_, diag5) = spatial::get_requirements(5, &dims);
    println!("[3,3] Position 5 diagonals: {:?}", diag5);
    assert!(diag5.contains(&3), "Position 5 should include position 3");
    assert!(diag5.contains(&4), "Position 5 should include position 4");
}
