use fold::spatial;

#[test]
fn test_diagonal_calculation_2x2() {
    // For a 2x2 grid, let's manually verify the diagonals
    let dims = vec![2, 2];
    
    // Position 0: [0,0] - distance 0
    // Position 1: [0,1] - distance 1
    // Position 2: [1,0] - distance 1
    // Position 3: [1,1] - distance 2
    
    for pos in 0..4 {
        let (prefixes, diagonals) = spatial::get_requirements(pos, &dims);
        println!("Position {}: prefixes={:?}, diagonals={:?}", pos, prefixes, diagonals);
    }
    
    // Position 0 [0,0]: distance 0 - no earlier positions at distance 0
    let (_prefixes, diagonals) = spatial::get_requirements(0, &dims);
    assert_eq!(diagonals, vec![], "Position 0 should have no diagonals");
    
    // Position 1 [0,1]: distance 1 - no earlier positions at distance 1
    let (_prefixes, diagonals) = spatial::get_requirements(1, &dims);
    assert_eq!(diagonals, vec![], "Position 1 should have no diagonals");
    
    // Position 2 [1,0]: distance 1 - position 1 [0,1] is also at distance 1 and comes before
    let (_prefixes, diagonals) = spatial::get_requirements(2, &dims);
    assert_eq!(diagonals, vec![1], "Position 2 should have position 1 in its diagonal");
    
    // Position 3 [1,1]: distance 2 - no earlier positions at distance 2
    let (_prefixes, diagonals) = spatial::get_requirements(3, &dims);
    assert_eq!(diagonals, vec![], "Position 3 should have no diagonals");
}

#[test]
fn test_diagonal_calculation_3x3() {
    // For a 3x3 grid
    let dims = vec![3, 3];
    
    println!("\n3x3 Grid diagonal calculations:");
    for pos in 0..9 {
        let (prefixes, diagonals) = spatial::get_requirements(pos, &dims);
        println!("Position {}: prefixes={:?}, diagonals={:?}", pos, prefixes, diagonals);
    }
    
    // Position 4 [1,1]: distance 2 - positions 3 [0,2] is at distance 2 and comes before
    let (_prefixes, diagonals) = spatial::get_requirements(4, &dims);
    assert_eq!(diagonals, vec![3], "Position 4 should have position 3 in its diagonal (both at distance 2)");
    
    // Position 5 [2,0]: distance 2 - positions 3 [0,2] and 4 [1,1] are at distance 2 and come before
    let (_prefixes, diagonals) = spatial::get_requirements(5, &dims);
    assert_eq!(diagonals, vec![3, 4], "Position 5 should have positions 3 and 4 in its diagonal");
}
