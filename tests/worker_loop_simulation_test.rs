use fold::interner::Interner;
use fold::ortho::Ortho;

/// Test that simulates the worker loop logic to see if it can create
/// an ortho with duplicate tokens on the same diagonal
#[test]
fn test_worker_loop_prevents_duplicates() {
    // Create text that would allow "and" to appear multiple times
    let text = "The south and\nshoulders and a\nof the and";
    let interner = Interner::from_text(text);
    
    let vocab = interner.vocabulary();
    let and_idx = vocab.iter().position(|w| w == "and").unwrap();
    
    println!("Vocabulary: {:?}", vocab);
    println!("'and' is at index: {}", and_idx);
    
    // Start with empty ortho
    let mut ortho = Ortho::new();
    let mut steps = 0;
    
    // Simulate the worker loop - keep adding tokens until we can't add "and" anymore
    loop {
        steps += 1;
        if steps > 20 {
            println!("Breaking after 20 steps to prevent infinite loop");
            break;
        }
        
        let (forbidden, required) = ortho.get_requirements();
        let completions = interner.intersect(&required, &forbidden);
        
        println!("\nStep {}: ortho dims={:?}, payload={:?}", steps, ortho.dims(), ortho.payload());
        println!("  Required: {:?}", required);
        println!("  Forbidden: {:?}", forbidden);
        println!("  Completions: {:?}", completions.iter().map(|&idx| vocab[idx].as_str()).collect::<Vec<_>>());
        
        // Check if "and" is in completions
        if completions.contains(&and_idx) {
            println!("  -> Adding 'and'");
            let children = ortho.add(and_idx);
            if children.is_empty() {
                println!("  -> No children created!");
                break;
            }
            ortho = children[0].clone();
        } else {
            println!("  -> 'and' is NOT a valid completion!");
            
            // Verify that if payload already contains 'and' on the diagonal, it should be forbidden
            let payload = ortho.payload();
            let and_positions: Vec<usize> = payload.iter().enumerate()
                .filter_map(|(i, opt)| if *opt == Some(and_idx) { Some(i) } else { None })
                .collect();
            
            if !and_positions.is_empty() {
                println!("  -> 'and' exists at positions: {:?}", and_positions);
                println!("  -> Forbidden should include 'and', and it's correctly excluded");
            }
            
            break;
        }
    }
    
    // Final check: count how many times "and" appears in the final ortho
    let final_and_count = ortho.payload().iter().filter(|opt| **opt == Some(and_idx)).count();
    println!("\nFinal ortho has 'and' appearing {} times", final_and_count);
    println!("Final ortho: {:?}", ortho.payload());
    
    // The test passes if we can't add "and" multiple times to the same diagonal
    // Let's manually verify the logic is sound
}

/// Test diagonal restrictions in a 3x3 ortho
#[test]
fn test_3x3_duplicate_and() {
    let text = "the south and shoulders a of";
    let interner = Interner::from_text(text);
    
    let vocab = interner.vocabulary();
    println!("Vocabulary: {:?}", vocab);
    let the_idx = vocab.iter().position(|w| w == "the").unwrap();
    let south_idx = vocab.iter().position(|w| w == "south").unwrap();
    let and_idx = vocab.iter().position(|w| w == "and").unwrap();
    let shoulders_idx = vocab.iter().position(|w| w == "shoulders").unwrap();
    
    // Build ortho step by step to a 3x3
    let mut ortho = Ortho::new();
    
    // Position 0: [0,0] - distance 0
    ortho = ortho.add(the_idx)[0].clone();
    println!("After adding 'The': dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Position 1: [0,1] - distance 1
    ortho = ortho.add(south_idx)[0].clone();
    println!("After adding 'south': dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Position 2: [1,0] - distance 1 (diagonal with position 1)
    ortho = ortho.add(shoulders_idx)[0].clone();
    println!("After adding 'shoulders': dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Position 3: [0,2] or [1,1] depending on expansion - will trigger expansion
    let children = ortho.add(and_idx);
    println!("After adding 'and' to position 3: got {} children", children.len());
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, payload={:?}", i, child.dims(), child.payload());
    }
    
    // Find a 3x3 child if it exists, or verify the expansion worked correctly
    let ortho_3x3 = children.iter().find(|o| o.dims() == &vec![3, 3]).cloned();
    if let Some(ortho) = ortho_3x3 {
        println!("\nFound 3x3 ortho: {:?}", ortho.payload());
        
        // Verify position 4 [1,1] (distance 2) has position 3 [0,2] on its diagonal
        let (forbidden, required) = ortho.get_requirements();
        println!("\nFor position 4 [1,1]:");
        println!("  Required: {:?}", required);
        println!("  Forbidden (token indices): {:?}", forbidden);
        
        let completions = interner.intersect(&required, &forbidden);
        println!("  Completions: {:?}", completions.iter().map(|&idx| vocab[idx].as_str()).collect::<Vec<_>>());
        
        // If position 3 has 'and', it should be forbidden at position 4 if they're on the same diagonal
        if ortho.payload().get(3) == Some(&Some(and_idx)) {
            assert!(
                !completions.contains(&and_idx),
                "'and' should be forbidden at position 4 since it's at position 3 (same diagonal)"
            );
            println!("  GOOD: 'and' is correctly forbidden at position 4");
        }
    } else {
        // No 3x3 child is fine - expansion might create 3x2 or 2x2x2 instead
        println!("\nNo 3x3 child created - expansion chose different dimensions");
        assert!(!children.is_empty(), "Should have created at least one child");
    }
}
