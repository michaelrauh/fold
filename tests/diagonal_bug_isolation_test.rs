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
    // We need to find all positions < 6 with distance = 3
    // - Position 3: [0,2] distance = 2 (NO)
    // - Position 4: [1,1] distance = 2 (NO)
    // - Position 5: [2,0] distance = 2 (NO)
    //
    // So position 6 should have NO diagonal positions!
    
    let (_, diagonals) = spatial::get_requirements(6, &dims);
    println!("Position 6 diagonals: {:?}", diagonals);
    
    // But in the test, 'and' is at position 1 (Some(1))
    // and should NOT be in the forbidden list for position 6
    // since position 1 [0,1] has distance 1, not 3
    
    assert_eq!(diagonals, vec![], "Position 6 should have no diagonal positions");
    
    // Now let's check position 4 [1,1], which is where 'and' gets added the second time
    println!("\n=== Checking position 4 ===");
    
    // In the payload at step 2: [Some(5), Some(1), Some(4), Some(4), Some(3), Some(0), None, Some(2), None]
    // Position 1 has token index 1 ('and')
    // Position 4 is [1,1] with distance = 2
    
    // Positions with distance = 2 and position < 4:
    // - Position 3: [0,2] distance = 2 (YES!)
    // But position 3 has token index 4 ('south'), not 'and'
    
    let (_prefixes_4, diagonals_4) = spatial::get_requirements(4, &dims);
    println!("Position 4 diagonals (position indices): {:?}", diagonals_4);
    
    // Let me manually build the ortho to match the test scenario
    let interner = Interner::from_text("a and of shoulders south the");
    let vocab = interner.vocabulary();
    println!("\nVocabulary: {:?}", vocab);
    
    // Payload from test: [Some(5), Some(1), Some(4), Some(4), Some(3), Some(0), None, Some(2), None]
    // This is after adding 'south' at position 3
    // Token indices: 0=a, 1=and, 2=of, 3=shoulders, 4=south, 5=the
    
    // So the payload means:
    // Pos 0: token 5 = 'the'
    // Pos 1: token 1 = 'and'  
    // Pos 2: token 4 = 'south'
    // Pos 3: token 4 = 'south' (duplicate!)
    // Pos 4: token 3 = 'shoulders'
    // Pos 5: token 0 = 'a'
    // Pos 6: None (to be filled)
    // Pos 7: token 2 = 'of'
    // Pos 8: None
    
    // Ah! Position 3 has 'south' (token 4)
    // Position 2 also has 'south' (token 4)
    // They're both at distance 1+0=1 and 0+2=2, respectively
    // But position 2 is [1,0] with distance 1, and position 3 is [0,2] with distance 2
    // So they're NOT on the same diagonal!
    
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
