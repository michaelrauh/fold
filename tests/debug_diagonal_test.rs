use fold::spatial;

#[test]
fn test_diagonal_calculation_2x2() {
    // For a 2x2 grid, let's manually verify the same-shell positions
    let dims = vec![2, 2];
    
    // Position 0: [0,0] - distance 0
    // Position 1: [0,1] - distance 1
    // Position 2: [1,0] - distance 1
    // Position 3: [1,1] - distance 2
    
    for pos in 0..4 {
        let (prefixes, same_shell) = spatial::get_requirements(pos, &dims);
        println!("Position {}: prefixes={:?}, same_shell={:?}", pos, prefixes, same_shell);
    }
    
    // Position 0 [0,0]: distance 0 - no other positions at distance 0
    let (_prefixes, same_shell) = spatial::get_requirements(0, &dims);
    assert_eq!(same_shell, vec![], "Position 0 should have no same-shell positions");
    
    // Position 1 [0,1]: distance 1 - position 2 [1,0] is also at distance 1
    let (_prefixes, same_shell) = spatial::get_requirements(1, &dims);
    assert_eq!(same_shell, vec![2], "Position 1 should have position 2 in its same-shell");
    
    // Position 2 [1,0]: distance 1 - position 1 [0,1] is also at distance 1
    let (_prefixes, same_shell) = spatial::get_requirements(2, &dims);
    assert_eq!(same_shell, vec![1], "Position 2 should have position 1 in its same-shell");
    
    // Position 3 [1,1]: distance 2 - no other positions at distance 2
    let (_prefixes, same_shell) = spatial::get_requirements(3, &dims);
    assert_eq!(same_shell, vec![], "Position 3 should have no same-shell positions");
}

#[test]
fn test_diagonal_calculation_3x3() {
    // For a 3x3 grid
    let dims = vec![3, 3];
    
    println!("\n3x3 Grid same-shell calculations:");
    for pos in 0..9 {
        let (prefixes, same_shell) = spatial::get_requirements(pos, &dims);
        println!("Position {}: prefixes={:?}, same_shell={:?}", pos, prefixes, same_shell);
    }
    
    // Position 4 [1,1]: distance 2 - positions 3 [0,2] and 5 [2,0] are at distance 2
    let (_prefixes, same_shell) = spatial::get_requirements(4, &dims);
    assert_eq!(same_shell, vec![3, 5], "Position 4 should have positions 3 and 5 in its same-shell (all at distance 2)");
    
    // Position 5 [2,0]: distance 2 - positions 3 [0,2] and 4 [1,1] are at distance 2
    let (_prefixes, same_shell) = spatial::get_requirements(5, &dims);
    assert_eq!(same_shell, vec![3, 4], "Position 5 should have positions 3 and 4 in its same-shell");
}
