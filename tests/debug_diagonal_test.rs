use fold::spatial;

#[test]
fn test_diagonal_calculation_2x2() {
    // For a 2x2 grid, let's manually verify the diagonal positions
    // diagonals returns positions in the same shell that are < current
    let dims = vec![2, 2];
    
    // Position 0: [0,0] - distance 0
    // Position 1: [0,1] - distance 1
    // Position 2: [1,0] - distance 1
    // Position 3: [1,1] - distance 2
    
    for pos in 0..4 {
        let (prefixes, diagonals) = spatial::get_requirements(pos, &dims);
        println!("Position {}: prefixes={:?}, diagonals={:?}", pos, prefixes, diagonals);
    }
    
    // Position 0 [0,0]: distance 0 - no other positions at distance 0
    let (_prefixes, diagonals) = spatial::get_requirements(0, &dims);
    assert_eq!(diagonals, vec![], "Position 0 should have no diagonal positions");
    
    // Position 1 [0,1]: distance 1 - no positions < 1 at distance 1
    let (_prefixes, diagonals) = spatial::get_requirements(1, &dims);
    assert_eq!(diagonals, vec![], "Position 1 should have no diagonal positions (no predecessors in same shell)");
    
    // Position 2 [1,0]: distance 1 - position 1 [0,1] is < 2 and at distance 1
    let (_prefixes, diagonals) = spatial::get_requirements(2, &dims);
    assert_eq!(diagonals, vec![1], "Position 2 should have position 1 in its diagonals");
    
    // Position 3 [1,1]: distance 2 - no positions < 3 at distance 2
    let (_prefixes, diagonals) = spatial::get_requirements(3, &dims);
    assert_eq!(diagonals, vec![], "Position 3 should have no diagonal positions");
}

#[test]
fn test_diagonal_calculation_3x3() {
    // For a 3x3 grid
    // diagonals returns positions in the same shell that are < current
    let dims = vec![3, 3];
    
    println!("\n3x3 Grid diagonal calculations:");
    for pos in 0..9 {
        let (prefixes, diagonals) = spatial::get_requirements(pos, &dims);
        println!("Position {}: prefixes={:?}, diagonals={:?}", pos, prefixes, diagonals);
    }
    
    // Position 4 [1,1]: distance 2 - position 3 [0,2] is < 4 and at distance 2
    let (_prefixes, diagonals) = spatial::get_requirements(4, &dims);
    assert_eq!(diagonals, vec![3], "Position 4 should have position 3 in its diagonals");
    
    // Position 5 [2,0]: distance 2 - positions 3 [0,2] and 4 [1,1] are < 5 and at distance 2
    let (_prefixes, diagonals) = spatial::get_requirements(5, &dims);
    assert_eq!(diagonals, vec![3, 4], "Position 5 should have positions 3 and 4 in its diagonals");
}
