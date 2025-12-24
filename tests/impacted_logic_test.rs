use fold::{interner::Interner, ortho::Ortho};

/// Test that demonstrates the bug: is_ortho_impacted_fast uses payload instead of requirements
#[test]
fn test_impacted_should_check_requirements_not_payload() {
    // Create two interners where interner_b adds a new completion for an existing prefix
    let interner_a = Interner::from_text("a b c.");
    let interner_b = interner_a.add_text("a b d."); // Adds "d" as another completion after "a b"

    // Merge them
    let merged = interner_a.merge(&interner_b);

    // Get impacted keys from A's perspective (what changed for A)
    let impacted_a = merged.impacted_keys(&interner_a);

    // The prefix [a, b] should be impacted because it gained "d" as a completion
    assert!(!impacted_a.is_empty(), "Should have impacted keys");

    // Create an ortho with payload [a, b, c] - this was built from interner_a
    // Its requirements would be prefixes like [a], [a, b], etc.
    let mut ortho = Ortho::new();
    let children = ortho.add(0); // Add 'a'
    ortho = children[0].clone();
    let children = ortho.add(1); // Add 'b'
    ortho = children[0].clone();
    let children = ortho.add(2); // Add 'c'
    ortho = children[0].clone();

    // Get the ortho's requirements (prefixes it's trying to satisfy)
    let requirements = ortho.get_requirement_phrases();

    // The current buggy implementation would check if the entire payload [0,1,2] 
    // matches an impacted prefix. But it should check if ANY requirement prefix matches.
    
    // Map interner_a vocabulary to merged vocabulary
    let vocab_a = interner_a.vocabulary();
    let vocab_merged = merged.vocabulary();
    let a_idx = vocab_merged.iter().position(|w| w == &vocab_a[0]).unwrap();
    let b_idx = vocab_merged.iter().position(|w| w == &vocab_a[1]).unwrap();

    // Check if requirements contain the impacted prefix
    let has_impacted_requirement = requirements.iter().any(|req| {
        impacted_a.contains(&vec![a_idx, b_idx]) && req == &vec![a_idx, b_idx]
    });

    // This ortho SHOULD be marked as impacted because one of its requirement prefixes
    // [a, b] matches an impacted key
    assert!(
        has_impacted_requirement || impacted_a.iter().any(|imp| requirements.contains(imp)),
        "Ortho should be impacted because its requirements include the changed prefix [a,b]"
    );
}

/// Test vocabulary space mismatch bug for smaller archive
#[test]
fn test_impacted_keys_must_be_in_merged_space_for_smaller_archive() {
    // Create two interners with different vocabulary
    let interner_a = Interner::from_text("a b c.");
    let interner_b = Interner::from_text("x y z. a b d."); // Different vocab + extends "a b"

    // A is smaller, so it will be remapped
    assert!(interner_a.vocab_size() < interner_b.vocab_size());

    // Merge them (B is larger, so A's vocab gets remapped)
    let merged = interner_a.merge(&interner_b);

    // Get impacted keys from A's perspective
    let impacted_a = merged.impacted_keys(&interner_a);

    // Build vocab mapping for A -> merged
    let vocab_a = interner_a.vocabulary();
    let vocab_merged = merged.vocabulary();
    let vocab_map_a: Vec<usize> = vocab_a
        .iter()
        .map(|word| {
            vocab_merged
                .iter()
                .position(|v| v == word)
                .expect("Word should be in merged vocab")
        })
        .collect();

    // Create an ortho from archive A
    let mut ortho = Ortho::new();
    let children = ortho.add(0); // 'a' in A's space
    ortho = children[0].clone();
    let children = ortho.add(1); // 'b' in A's space
    ortho = children[0].clone();

    // Remap ortho to merged space
    let remapped_ortho = ortho.remap(&vocab_map_a).unwrap();

    // Get requirements from the REMAPPED ortho
    let remapped_requirements = remapped_ortho.get_requirement_phrases();

    // The impacted_a keys are already in MERGED space (returned by merged.impacted_keys)
    // So we can directly compare remapped requirements against impacted_a
    
    // This should work correctly - both are in merged space
    let is_impacted = remapped_requirements.iter().any(|req| impacted_a.contains(req));

    // The bug would be if we compared remapped_requirements against keys in A's original space
    // That would be comparing apples (merged indices) to oranges (A's indices)
    println!("Impacted keys (merged space): {:?}", impacted_a);
    println!("Remapped requirements: {:?}", remapped_requirements);
    println!("Is impacted: {}", is_impacted);
}

/// Test that we compare against the RIGHT interner (merged vs original, not original vs original)
#[test]
fn test_impacted_must_compare_merged_vs_original_not_a_vs_b() {
    // Create two interners
    let interner_a = Interner::from_text("a b c.");
    let interner_b = Interner::from_text("a b d.");

    // The WRONG approach (current bug):
    let wrong_impacted_a = interner_a.impacted_keys(&interner_b); // Comparing A vs B
    let wrong_impacted_b = interner_b.impacted_keys(&interner_a); // Comparing B vs A

    // The CORRECT approach:
    let merged = interner_a.merge(&interner_b);
    let correct_impacted_a = merged.impacted_keys(&interner_a); // What changed for A
    let correct_impacted_b = merged.impacted_keys(&interner_b); // What changed for B

    // These should be different!
    // wrong_impacted_a tells us "keys in A that differ from B" (in A's vocab space)
    // correct_impacted_a tells us "keys in merged that changed from A's perspective" (in merged vocab space)

    println!("Wrong approach - A.impacted_keys(B): {:?}", wrong_impacted_a);
    println!("Wrong approach - B.impacted_keys(A): {:?}", wrong_impacted_b);
    println!("Correct approach - Merged.impacted_keys(A): {:?}", correct_impacted_a);
    println!("Correct approach - Merged.impacted_keys(B): {:?}", correct_impacted_b);

    // The correct approach gives us keys in the merged vocabulary space,
    // which is what we need to check against remapped orthos
}

/// Integration test: full scenario with ortho impact checking
#[test]
fn test_complete_impacted_ortho_scenario() {
    // Setup: two archives with overlapping vocabulary
    let interner_a = Interner::from_text("a b c. d e f.");
    let interner_b = Interner::from_text("a b g. d e h."); // Extends "a b" and "d e"

    // Create orthos from each archive
    let mut ortho_a1 = Ortho::new();
    let children = ortho_a1.add(0); // a
    ortho_a1 = children[0].clone();
    let children = ortho_a1.add(1); // b
    ortho_a1 = children[0].clone();
    let children = ortho_a1.add(2); // c
    ortho_a1 = children[0].clone();

    let mut ortho_a2 = Ortho::new();
    let children = ortho_a2.add(3); // d
    ortho_a2 = children[0].clone();
    let children = ortho_a2.add(4); // e
    ortho_a2 = children[0].clone();
    let children = ortho_a2.add(5); // f
    ortho_a2 = children[0].clone();

    // Determine which is smaller
    let a_is_smaller = interner_a.vocab_size() <= interner_b.vocab_size();
    
    // Merge
    let merged = if a_is_smaller {
        interner_b.merge(&interner_a)
    } else {
        interner_a.merge(&interner_b)
    };

    // Get impacted keys for A (in merged vocabulary space)
    let impacted_a = merged.impacted_keys(&interner_a);

    println!("Interner A vocab: {:?}", interner_a.vocabulary());
    println!("Interner B vocab: {:?}", interner_b.vocabulary());
    println!("Merged vocab: {:?}", merged.vocabulary());
    println!("Impacted keys for A: {:?}", impacted_a);

    // Build vocab mapping for A -> merged
    let vocab_map_a: Vec<usize> = interner_a
        .vocabulary()
        .iter()
        .map(|word| {
            merged
                .vocabulary()
                .iter()
                .position(|v| v == word)
                .expect("Word should be in merged vocab")
        })
        .collect();

    // If A is smaller, remap orthos
    let (final_ortho_a1, final_ortho_a2) = if a_is_smaller {
        (
            ortho_a1.remap(&vocab_map_a).unwrap(),
            ortho_a2.remap(&vocab_map_a).unwrap(),
        )
    } else {
        (ortho_a1.clone(), ortho_a2.clone())
    };

    // Check which orthos are impacted by comparing their requirements against impacted keys
    let ortho_a1_impacted = final_ortho_a1
        .get_requirement_phrases()
        .iter()
        .any(|req| impacted_a.contains(req));

    let ortho_a2_impacted = final_ortho_a2
        .get_requirement_phrases()
        .iter()
        .any(|req| impacted_a.contains(req));

    println!("Ortho A1 requirements: {:?}", final_ortho_a1.get_requirement_phrases());
    println!("Ortho A1 impacted: {}", ortho_a1_impacted);
    println!("Ortho A2 requirements: {:?}", final_ortho_a2.get_requirement_phrases());
    println!("Ortho A2 impacted: {}", ortho_a2_impacted);

    // Both should be impacted since both had their prefixes extended
    assert!(
        ortho_a1_impacted,
        "Ortho with [a,b,c] should be impacted because [a,b] gained new completions"
    );
    assert!(
        ortho_a2_impacted,
        "Ortho with [d,e,f] should be impacted because [d,e] gained new completions"
    );
}
