use fold::ortho::Ortho;
use fold::interner::Interner;

/// Test filling a [3,3] ortho to see when diagonal conflicts should occur
#[test]
fn test_filling_3x3_with_diagonal_check() {
    // Create text with "and" appearing multiple times so it's available as a completion
    let text = "the and south and shoulders and a and of";
    let interner = Interner::from_text(text);
    let vocab = interner.vocabulary();
    
    println!("Vocabulary: {:?}\n", vocab);
    
    let and_idx = vocab.iter().position(|w| w == "and").unwrap();
    let the_idx = vocab.iter().position(|w| w == "the").unwrap();
    let south_idx = vocab.iter().position(|w| w == "south").unwrap();
    let shoulders_idx = vocab.iter().position(|w| w == "shoulders").unwrap();
    let a_idx = vocab.iter().position(|w| w == "a").unwrap();
    let of_idx = vocab.iter().position(|w| w == "of").unwrap();
    
    // Build up to a [3,2] ortho first
    let mut ortho = Ortho::new(1);
    for &idx in &[the_idx, south_idx, and_idx, shoulders_idx, a_idx] {
        let children = ortho.add(idx, 1);
        ortho = children[0].clone();
    }
    
    println!("Built ortho: dims={:?}, filled={}", ortho.dims(), ortho.get_current_position());
    println!("Payload: {:?}\n", ortho.payload());
    
    // Now add the 6th token to trigger expansion - one child should be [3,3]
    let children = ortho.add(of_idx, 1);
    println!("After adding 'of', got {} children:", children.len());
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, filled={}, payload={:?}", 
                 i, child.dims(), child.get_current_position(), child.payload());
    }
    
    // Find the [3,3] child
    let ortho_3x3 = children.iter().find(|o| o.dims() == &vec![3, 3]).cloned().expect("Should have [3,3] child");
    
    println!("\n=== Now filling the [3,3] ortho ===");
    println!("Starting [3,3] ortho: dims={:?}, filled={}, payload={:?}\n", 
             ortho_3x3.dims(), ortho_3x3.get_current_position(), ortho_3x3.payload());
    
    let mut current = ortho_3x3;
    let mut step = 0;
    
    // Try to keep adding tokens, checking for 'and' conflicts
    loop {
        step += 1;
        if step > 10 {
            break;
        }
        
        let pos = current.get_current_position();
        if pos >= current.payload().len() {
            println!("Ortho is full!");
            break;
        }
        
        let (forbidden, required) = current.get_requirements();
        let completions = interner.intersect(&required, &forbidden);
        
        println!("=== Step {} (position {}) ===", step, pos);
        println!("Forbidden indices: {:?}", forbidden);
        println!("Forbidden tokens: {:?}", forbidden.iter().map(|&i| vocab[i].as_str()).collect::<Vec<_>>());
        println!("Required: {:?}", required);
        println!("Completions: {:?}", completions.iter().map(|&i| vocab[i].as_str()).collect::<Vec<_>>());
        
        // Check if 'and' is in forbidden list
        let and_in_payload = current.payload().iter().any(|opt| *opt == Some(and_idx));
        let and_is_forbidden = forbidden.contains(&and_idx);
        let and_is_completion = completions.contains(&and_idx);
        
        println!("'and' in payload: {}, 'and' in forbidden: {}, 'and' in completions: {}", 
                 and_in_payload, and_is_forbidden, and_is_completion);
        
        if and_in_payload && and_is_completion {
            println!("\n*** BUG FOUND! ***");
            println!("'and' is already in the payload but is still a valid completion!");
            println!("Current payload: {:?}", current.payload());
            println!("Display:\n{}", current.display(&interner));
            panic!("Bug: 'and' allowed on same diagonal!");
        }
        
        // If no completions, try to force 'and' if not in forbidden
        if completions.is_empty() {
            if and_in_payload && !and_is_forbidden {
                println!("\n*** FORCING 'and' since it's not forbidden but also not a completion ***");
                println!("This will demonstrate if the bug is in intersect() or get_requirements()");
                let children = current.add(and_idx, 1);
                if !children.is_empty() {
                    current = children[0].clone();
                    println!("Successfully added 'and'! New payload: {:?}\n", current.payload());
                    continue;
                }
            }
            println!("No completions available, stopping");
            break;
        }
        
        // Prefer to add 'and' if it's a valid completion
        let token_to_add = if and_is_completion { and_idx } else { completions[0] };
        println!("Adding token: '{}'", vocab[token_to_add]);
        
        let children = current.add(token_to_add, 1);
        if children.is_empty() {
            break;
        }
        current = children[0].clone();
        println!("New payload: {:?}\n", current.payload());
    }
    
    println!("\nFinal ortho:");
    println!("{}", current.display(&interner));
}
