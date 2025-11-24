use fold::ortho::Ortho;
use fold::interner::Interner;

/// Trace through the expansion path to reach [3,3]
#[test]
fn test_path_to_3x3() {
    let interner = Interner::from_text("a b c d e f g h i j k l m n");
    let vocab = interner.vocabulary();
    
    let mut ortho = Ortho::new(1);
    let mut step = 0;
    
    // Fill until we get various expansions
    for &token_idx in &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
        step += 1;
        println!("\n=== Step {}: Adding token '{}' ===", step, vocab[token_idx]);
        println!("Current ortho: dims={:?}, filled={}", ortho.dims(), ortho.get_current_position());
        
        let children = ortho.add(token_idx, 1);
        println!("Generated {} children:", children.len());
        for (i, child) in children.iter().enumerate() {
            println!("  Child {}: dims={:?}, capacity={}, filled={}", 
                     i, child.dims(), child.payload().len(), child.get_current_position());
        }
        
        // Pick the first child and continue
        if !children.is_empty() {
            ortho = children[0].clone();
            
            if ortho.dims() == &vec![3, 3] {
                println!("\n*** Reached [3, 3]! ***");
                println!("Payload: {:?}", ortho.payload());
                break;
            }
        } else {
            println!("No children generated!");
            break;
        }
    }
}
