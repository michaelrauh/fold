use fold::interner::Interner;
use fold::ortho::Ortho;

/// This test reproduces the EXACT bug from the user's report:
/// "and" appears at [0,2] (position 3) and [1,1] (position 4), both in shell 2
/// Note: With sorted canonical dims, [3,2] becomes [2,3]
#[test]
fn test_and_duplicate_in_shell_2() {
    let text = "the south and shoulders a of";
    let interner = Interner::from_text(text);
    let vocab = interner.vocabulary();
    
    println!("Vocabulary: {:?}\n", vocab);
    
    let the_idx = vocab.iter().position(|w| w == "the").unwrap();
    let south_idx = vocab.iter().position(|w| w == "south").unwrap();
    let and_idx = vocab.iter().position(|w| w == "and").unwrap();
    let shoulders_idx = vocab.iter().position(|w| w == "shoulders").unwrap();
    
    // First build a [2,2] with 'the' and 'south'
    let mut ortho = Ortho::new();
    ortho = ortho.add(the_idx)[0].clone();
    ortho = ortho.add(south_idx)[0].clone();
    ortho = ortho.add(shoulders_idx)[0].clone();
    
    println!("After 3 additions: dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Add 'and' at position 3 - this will trigger expansion
    let children = ortho.add(and_idx);
    println!("\nAfter adding 'and', got {} children:", children.len());
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, payload={:?}", i, child.dims(), child.payload());
    }
    
    // Pick the [2,3] child (sorted canonical form, was [3,2])
    let mut ortho = children.iter()
        .find(|o| o.dims() == &vec![2, 3])
        .expect("Should have [2,3] child")
        .clone();
    println!("\nChosen ortho: dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Continue filling until we have a [3,3] with 'and' at position 4
    // (With [2,3] layout, 'and' is at position 4 after remap)
    let mut steps = 0;
    while ortho.get_current_position() < ortho.payload().len() && steps < 20 {
        steps += 1;
        
        let _pos = ortho.get_current_position();
        let (forbidden, required) = ortho.get_requirements();
        let completions = interner.intersect(&required, &forbidden);
        
        if completions.is_empty() {
            break;
        }
        
        // Add first completion
        let children = ortho.add(completions[0]);
        if children.is_empty() {
            break;
        }
        
        // If we got a [3,3] child, switch to it
        let next_ortho = children.iter()
            .find(|o| o.dims() == &vec![3, 3])
            .or(Some(&children[0]))
            .unwrap()
            .clone();
        
        ortho = next_ortho;
        
        if ortho.dims() == &vec![3, 3] {
            println!("\nReached [3,3] at step {}: payload={:?}", steps, ortho.payload());
            break;
        }
    }
    
    // Now we should have a [3,3] ortho
    // Find where 'and' is in the payload
    let and_positions: Vec<usize> = ortho.payload().iter().enumerate()
        .filter_map(|(i, opt)| if *opt == Some(and_idx) { Some(i) } else { None })
        .collect();
    
    println!("\n'and' is at positions: {:?}", and_positions);
    
    // With sorted [2,3] layout, 'and' ends up at position 4 after remap
    // Check the diagonal behavior from the current layout
    println!("\nFinal payload: {:?}", ortho.payload());
    println!("Display:\n{}", ortho.display(&interner));
}
