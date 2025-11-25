use fold::interner::Interner;
use fold::ortho::Ortho;

/// This test verifies the diagonal/shell logic.
/// In a 2x2 grid being filled:
/// Position 0: [0,0] - distance 0
/// Position 1: [0,1] - distance 1
/// Position 2: [1,0] - distance 1
/// Position 3: [1,1] - distance 2
///
/// When filling position 2 [1,0], position 1 [0,1] is on the diagonal (same distance 1)
/// and should forbid duplicate tokens.
#[test]
fn test_duplicate_token_in_same_shell_forbidden() {
    let interner = Interner::from_text("a b c d");
    let vocab = interner.vocabulary();
    let a_idx = vocab.iter().position(|w| w == "a").unwrap();
    let b_idx = vocab.iter().position(|w| w == "b").unwrap();
    
    // Build a 2x2 ortho
    let ortho = Ortho::new();
    
    // Fill position 0: [0,0] - distance 0
    let ortho = ortho.add(a_idx);
    let ortho = &ortho[0];
    
    // Fill position 1: [0,1] - distance 1
    let ortho = ortho.add(b_idx);
    let ortho = &ortho[0];
    
    // Now check requirements for position 2: [1,0] - distance 1
    // Position 1 [0,1] is on the diagonal (both at distance 1)
    let (forbidden, _required) = ortho.get_requirements();
    
    println!("Forbidden list for position 2: {:?}", forbidden);
    println!("Payload at this point: {:?}", ortho.payload());
    
    // Position 1 has 'b' (index b_idx)
    // It should be in the forbidden list since it's on the same diagonal
    assert!(
        forbidden.contains(&b_idx),
        "Position 1 (containing 'b') should be in forbidden list for position 2, but forbidden list is: {:?}",
        forbidden
    );
    
    // Now try to add 'b' again - it should fail the intersect test
    let candidates = interner.intersect(&_required, &forbidden);
    assert!(
        !candidates.contains(&b_idx),
        "Token 'b' should not be a valid candidate for position 2 since it's already on the diagonal"
    );
}

/// Test that diagonal restrictions work correctly in a 3x3 grid
#[test] 
fn test_display_shows_correct_structure() {
    let interner = Interner::from_text("a b c d e f");
    let vocab = interner.vocabulary();
    let a_idx = vocab.iter().position(|w| w == "a").unwrap();
    let b_idx = vocab.iter().position(|w| w == "b").unwrap();
    let c_idx = vocab.iter().position(|w| w == "c").unwrap();
    let d_idx = vocab.iter().position(|w| w == "d").unwrap();
    
    // Build to a 3x3 ortho to test diagonal at distance 2
    let ortho = Ortho::new();
    let ortho = ortho.add(a_idx)[0].clone();
    let ortho = ortho.add(b_idx)[0].clone();
    let ortho = ortho.add(c_idx)[0].clone();
    
    // Position 3 [0,2] with 'd' - distance 2
    let ortho = ortho.add(d_idx)[0].clone();
    
    // Now at position 4 [1,1] - also distance 2
    // Position 3 should be on the diagonal and 'd' should be forbidden
    let (forbidden, _) = ortho.get_requirements();
    
    assert!(
        forbidden.contains(&d_idx),
        "Token 'd' at position 3 [0,2] should forbid 'd' at position 4 [1,1] (same diagonal, distance 2)"
    );
}
