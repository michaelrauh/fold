use fold::spatial;
use fold::interner::Interner;

#[test]
fn test_3x3_position_6_diagonals() {
    let dims = vec![3, 3];
    
    // According to the test output, when we're at position 6,
    // the payload is: [Some(5), Some(1), Some(4), Some(4), Some(3), Some(0), None, Some(2), None]
    //
    // In a 3x3 grid, the coordinates are:
    // Pos 0: [0,0], Pos 1: [0,1], Pos 2: [1,0]
    // Pos 3: [0,2], Pos 4: [1,1], Pos 5: [2,0]
    // Pos 6: [1,2], Pos 7: [2,1], Pos 8: [2,2]
    //
    // Position 6 is [1,2] with distance = 3
    // Position 7 is [2,1] with distance = 3 (same distance as position 6)
    // diagonals only returns positions < current in the same shell
    // Position 7 > 6, so 7 is NOT in diagonals
    
    let (_, diagonals) = spatial::get_requirements(6, &dims);
    println!("Position 6 diagonal positions: {:?}", diagonals);
    
    // Position 6 [1,2] at distance 3 should have no diagonals (no position < 6 at distance 3)
    assert_eq!(diagonals, vec![], "Position 6 should have no diagonal positions (no predecessors in same shell)");
    
    // But position 7 is in same shell and > 6, so it's in diagonals_after
    let diagonals_after = spatial::get_diagonals_after(6, &dims);
    assert_eq!(diagonals_after, vec![7], "Position 6 should have position 7 in diagonals_after");
    
    // Now let's check position 4 [1,1], which is where 'and' gets added the second time
    println!("\n=== Checking position 4 ===");
    
    // Position 4 is [1,1] with distance = 2
    // Positions with distance = 2:
    // - Position 3: [0,2] distance = 2 (< 4, so in diagonals)
    // - Position 5: [2,0] distance = 2 (> 4, so in diagonals_after)
    
    let (_prefixes_4, diagonals_4) = spatial::get_requirements(4, &dims);
    println!("Position 4 diagonal positions: {:?}", diagonals_4);
    assert_eq!(diagonals_4, vec![3], "Position 4 should have position 3 in its diagonals");
    
    let diagonals_after_4 = spatial::get_diagonals_after(4, &dims);
    assert_eq!(diagonals_after_4, vec![5], "Position 4 should have position 5 in diagonals_after");
    
    // Let me manually build the ortho to match the test scenario
    let interner = Interner::from_text("a and of shoulders south the");
    let vocab = interner.vocabulary();
    println!("\nVocabulary: {:?}", vocab);
    
    // Payload from test: [Some(5), Some(1), Some(4), Some(4), Some(3), Some(0), None, Some(2), None]
    // Token indices: 0=a, 1=and, 2=of, 3=shoulders, 4=south, 5=the
    
    println!("\nLet me trace through what's really happening...");
}

#[test]
fn test_exact_bug_scenario() {
    // From the test output, after reorganization to [3,3]:
    // Payload: [Some(5), Some(1), Some(4), None, Some(3), Some(0), None, Some(2), None]
    //
    // Then at position 3, it adds 'south' (token 4):
    // Payload: [Some(5), Some(1), Some(4), Some(4), Some(3), Some(0), None, Some(2), None]
    //
    // Then at position 6, it's trying to add something, but 'and' (token 1) is at position 1
    
    let text = "a and of shoulders south the";
    let interner = Interner::from_text(text);
    let vocab = interner.vocabulary();
    
    println!("Vocabulary: {:?}", vocab);
    let and_idx = vocab.iter().position(|w| w == "and").unwrap();
    println!("'and' token index: {}", and_idx);
    
    // Manually create an ortho with the problematic payload
    // We can't directly set payload, so let's trace through the actual sequence
    
    // Starting from [3,2] with payload: [Some(5), Some(1), Some(4), Some(3), Some(0), None]
    // Positions in [3,2]:
    // Pos 0: [0,0] = 'the' (5)
    // Pos 1: [0,1] = 'and' (1)  
    // Pos 2: [1,0] = 'south' (4)
    // Pos 3: [1,1] = 'shoulders' (3)
    // Pos 4: [2,0] = 'a' (0)
    // Pos 5: [2,1] = None (to be filled)
    
    // When we add a token at position 5 and expand to [3,3], the reorganization happens
    // The expansion includes a reorganization pattern that remaps the positions
    
    println!("\nThe bug is likely in the reorganization - tokens are moved but their");
    println!("diagonal relationships might not be preserved correctly during remapping!");
}
