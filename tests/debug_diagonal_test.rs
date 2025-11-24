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
    
    // Position 3 should have diagonals [1, 2]
    let (_prefixes, diagonals) = spatial::get_requirements(3, &dims);
    assert!(
        diagonals.contains(&1),
        "Position 3 should have position 1 in its diagonal, but got: {:?}",
        diagonals
    );
    assert!(
        diagonals.contains(&2),
        "Position 3 should have position 2 in its diagonal, but got: {:?}",
        diagonals
    );
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
    
    // Position 4 [1,1] should have diagonals at distance 2: positions 3 [0,2] and 5 [2,0]
    let (_prefixes, diagonals) = spatial::get_requirements(4, &dims);
    assert!(
        diagonals.contains(&3),
        "Position 4 should have position 3 in its diagonal (both at distance 2), but got: {:?}",
        diagonals
    );
}
