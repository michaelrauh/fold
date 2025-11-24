use fold::interner::Interner;
use fold::ortho::Ortho;

/// This test verifies the diagonal/shell logic.
/// In a 2x2 grid being filled:
/// Position 0: [0,0] - shell 0
/// Position 1: [0,1] - shell 1
/// Position 2: [1,0] - shell 1
/// Position 3: [1,1] - shell 2
///
/// When filling position 3, positions 1 and 2 are on the diagonal (same shell distance)
/// and should be in the forbidden list.
#[test]
fn test_duplicate_token_in_same_shell_forbidden() {
    let interner = Interner::from_text("a b c d");
    let vocab = interner.vocabulary();
    let a_idx = vocab.iter().position(|w| w == "a").unwrap();
    let b_idx = vocab.iter().position(|w| w == "b").unwrap();
    let c_idx = vocab.iter().position(|w| w == "c").unwrap();
    
    // Build a 2x2 ortho
    let ortho = Ortho::new(1);
    
    // Fill position 0: [0,0]
    let ortho = ortho.add(a_idx, 1);
    let ortho = &ortho[0];
    
    // Fill position 1: [0,1]
    let ortho = ortho.add(b_idx, 1);
    let ortho = &ortho[0];
    
    // Fill position 2: [1,0]
    let ortho = ortho.add(c_idx, 1);
    let ortho = &ortho[0];
    
    // Now check requirements for position 3: [1,1]
    // The diagonal positions should be 1 and 2 (both at distance 1 from origin)
    let (forbidden, _required) = ortho.get_requirements();
    
    println!("Forbidden list for position 3: {:?}", forbidden);
    println!("Payload at this point: {:?}", ortho.payload());
    
    // Position 1 has 'b' (index b_idx)
    // Position 2 has 'c' (index c_idx)
    // Both should be in the forbidden list
    assert!(
        forbidden.contains(&b_idx),
        "Position 1 (containing 'b') should be in forbidden list for position 3, but forbidden list is: {:?}",
        forbidden
    );
    assert!(
        forbidden.contains(&c_idx),
        "Position 2 (containing 'c') should be in forbidden list for position 3, but forbidden list is: {:?}",
        forbidden
    );
    
    // Now try to add 'b' again - it should fail the intersect test
    let candidates = interner.intersect(&_required, &forbidden);
    assert!(
        !candidates.contains(&b_idx),
        "Token 'b' should not be a valid candidate for position 3 since it's already on the diagonal"
    );
}

/// Test the actual bug case from the display: "and" appearing twice in the same shell
#[test] 
fn test_display_shows_correct_structure() {
    // When we have an ortho with duplicate tokens on the same diagonal,
    // the display should show them accurately
    let interner = Interner::from_text("a b c d");
    let vocab = interner.vocabulary();
    let a_idx = vocab.iter().position(|w| w == "a").unwrap();
    let b_idx = vocab.iter().position(|w| w == "b").unwrap();
    
    // Manually create an ortho that violates the diagonal rule
    // This shouldn't be possible through normal add() operations,
    // but let's verify what the display would show
    let ortho = Ortho::new(1);
    let ortho = ortho.add(a_idx, 1);
    let ortho = &ortho[0];
    let ortho = ortho.add(b_idx, 1);
    let ortho = &ortho[0];
    
    // Position 1 has 'b', now try to add 'b' at position 2
    // This should be caught by the forbidden logic
    let ortho = ortho.add(a_idx, 1);
    let ortho = &ortho[0];
    
    // Now at position 3, check if 'b' would be allowed
    let (forbidden, _) = ortho.get_requirements();
    
    // 'b' is at position 1, which is on the diagonal for position 3
    assert!(
        forbidden.contains(&b_idx),
        "Token 'b' at position 1 should forbid 'b' at position 3 (same diagonal)"
    );
}
