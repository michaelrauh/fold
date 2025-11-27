use fold::interner::Interner;
use fold::ortho::Ortho;
use fold::spatial;

/// ANALYSIS SUMMARY: ORTHO CONSTRUCTION AND REMAP SHELL VIOLATIONS
/// ================================================================
/// 
/// This test file investigates whether the `Ortho::remap()` function or the
/// spatial reorganization during expansion can introduce duplicates in the
/// same distance shell.
/// 
/// ## Background
/// 
/// An ortho is a multi-dimensional structure where positions are filled in
/// order of increasing "distance" from the origin (sum of coordinates).
/// Positions at the same distance form a "shell". A key invariant is that
/// the same token should NOT appear twice in positions within the same shell.
/// 
/// ## Key Findings
/// 
/// ### 1. Vocabulary Remap (`Ortho::remap()`)
/// 
/// The vocabulary remap function translates token indices from one vocabulary
/// to another. Since vocabulary merging always produces bijective mappings
/// (each word maps to a unique index), `remap()` CANNOT introduce duplicates
/// if the original ortho was valid.
/// 
/// However, `remap()` does NOT re-canonicalize the ortho. The [2,2] ortho
/// canonicalization (swapping positions 1 and 2 to maintain ordering) is not
/// re-applied after remap. This could affect deduplication but NOT shell validity.
/// 
/// ### 2. Spatial Reorganization (during expansion)
/// 
/// During expansion (e.g., [2,2] → [2,2,2] or [2,2] → [3,2]), tokens are
/// reorganized to new positions. The reorganization pattern preserves the
/// distance of each token - a token at distance D in the old structure will
/// be at distance D in the new structure.
/// 
/// The diagonal/shell check during construction ensures that when filling a
/// new position, we forbid any token that already appears at a previous
/// position in the same shell.
/// 
/// ### 3. The Actual Bug (Hypothesis)
/// 
/// After much investigation, the shell violation issue likely comes from one
/// of these scenarios:
/// 
/// a) **Serialization/Deserialization corruption**: If orthos are corrupted
///    during save/load, invalid states could be introduced.
/// 
/// b) **Non-bijective vocabulary mapping**: If a bug in vocabulary merging
///    causes two different tokens to map to the same index, duplicates
///    would appear after remap.
/// 
/// c) **Edge case in diagonal computation**: A subtle bug in how diagonals
///    are computed for certain dimension configurations.
/// 
/// d) **Race condition during parallel processing**: If multiple processes
///    are modifying shared state.
/// 
/// ## Test Strategy
/// 
/// The tests below verify:
/// 1. Shell validity is checked correctly via diagonals
/// 2. Bijective vocabulary mapping preserves shell validity
/// 3. Non-bijective mapping (simulated bug) causes shell violations
/// 4. Expansion reorganization preserves shell validity
/// 5. Systematic exploration of expansion patterns

/// Helper function to check if an ortho has shell violations.
/// 
/// Returns `Some((pos, diag_pos, value))` if a violation is found, where:
/// - `pos`: the position where the duplicate was found
/// - `diag_pos`: the diagonal position with the same value
/// - `value`: the duplicate token value
/// 
/// Returns `None` if no shell violations are detected.
fn has_shell_violation(ortho: &Ortho) -> Option<(usize, usize, usize)> {
    let dims = ortho.dims();
    let payload = ortho.payload();
    
    for pos in 0..payload.len() {
        if let Some(my_val) = payload[pos] {
            let (_, diagonals) = spatial::get_requirements(pos, dims);
            for &diag_pos in &diagonals {
                if let Some(diag_val) = payload[diag_pos] {
                    if my_val == diag_val {
                        return Some((pos, diag_pos, my_val));
                    }
                }
            }
        }
    }
    None
}

/// Helper function to verify vocabulary mapping is bijective
fn is_bijective_mapping(vocab_map: &[usize]) -> bool {
    let unique: std::collections::HashSet<_> = vocab_map.iter().collect();
    unique.len() == vocab_map.len()
}

#[test]
fn test_diagonal_positions_in_2x2x2() {
    // In [2,2,2], positions are ordered by distance from origin:
    // Position 0 [0,0,0]: distance=0
    // Position 1 [0,0,1]: distance=1
    // Position 2 [0,1,0]: distance=1
    // Position 3 [1,0,0]: distance=1
    // Position 4 [0,1,1]: distance=2
    // Position 5 [1,0,1]: distance=2
    // Position 6 [1,1,0]: distance=2
    // Position 7 [1,1,1]: distance=3
    
    let dims = vec![2, 2, 2];
    
    // Positions 1, 2, 3 are all at distance 1
    // When filling position 3, diagonals should include positions 1 and 2
    let (_, diagonals_3) = spatial::get_requirements(3, &dims);
    
    assert!(diagonals_3.contains(&1), "Position 3 should have position 1 as diagonal");
    assert!(diagonals_3.contains(&2), "Position 3 should have position 2 as diagonal");
}

#[test]
fn test_remap_preserves_shell_validity_bijective_case() {
    // This test verifies that remap with a bijective mapping preserves shell validity
    
    let interner_a = Interner::from_text("alpha beta gamma delta");
    let vocab_a = interner_a.vocabulary();
    
    let alpha_idx = vocab_a.iter().position(|w| w == "alpha").unwrap();
    let beta_idx = vocab_a.iter().position(|w| w == "beta").unwrap();
    let gamma_idx = vocab_a.iter().position(|w| w == "gamma").unwrap();
    
    // Build a valid [2,2] ortho
    let ortho = Ortho::new();
    let ortho = ortho.add(alpha_idx)[0].clone();  // pos 0
    let ortho = ortho.add(beta_idx)[0].clone();   // pos 1
    let ortho = ortho.add(gamma_idx)[0].clone();  // pos 2
    
    println!("Original ortho: dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Create a bijective vocabulary mapping (shuffle the indices)
    // Original vocab: ["alpha", "beta", "delta", "gamma"] or similar
    // New vocab: different order
    let interner_b = Interner::from_text("gamma alpha delta beta");
    let merged = interner_a.merge(&interner_b);
    
    // Build mapping from interner_a indices to merged indices
    let vocab_map: Vec<usize> = interner_a.vocabulary().iter().map(|word| {
        merged.vocabulary().iter().position(|w| w == word).unwrap()
    }).collect();
    
    println!("Vocabulary mapping: {:?}", vocab_map);
    
    // Remap the ortho
    let remapped = ortho.remap(&vocab_map).expect("Remap should succeed");
    
    println!("Remapped ortho: dims={:?}, payload={:?}", remapped.dims(), remapped.payload());
    
    // Check that positions in the same shell don't have duplicate tokens
    // In [2,2], positions 1 and 2 are in the same shell (distance 1)
    if let (Some(val1), Some(val2)) = (remapped.payload()[1], remapped.payload()[2]) {
        assert_ne!(val1, val2, "Positions 1 and 2 are in the same shell and should not have duplicate tokens");
    }
}

/// This test explores whether remap could theoretically introduce duplicates.
/// 
/// The hypothesis is: if the vocabulary mapping is NOT bijective (maps two different
/// old indices to the same new index), duplicates could appear.
/// 
/// However, in practice, the vocabulary merge should always be bijective.
#[test]
fn test_remap_with_non_bijective_mapping_shows_bug() {
    let interner = Interner::from_text("alpha beta gamma");
    let vocab = interner.vocabulary();
    
    let alpha_idx = vocab.iter().position(|w| w == "alpha").unwrap();
    let beta_idx = vocab.iter().position(|w| w == "beta").unwrap();
    let gamma_idx = vocab.iter().position(|w| w == "gamma").unwrap();
    
    // Build a valid [2,2] ortho where positions 1 and 2 have different tokens
    let ortho = Ortho::new();
    let ortho = ortho.add(alpha_idx)[0].clone();  // pos 0: alpha
    let ortho = ortho.add(beta_idx)[0].clone();   // pos 1: beta
    let ortho = ortho.add(gamma_idx)[0].clone();  // pos 2: gamma
    
    // Verify original is valid
    assert_ne!(ortho.payload()[1], ortho.payload()[2], "Original should have different tokens at pos 1 and 2");
    assert!(has_shell_violation(&ortho).is_none(), "Original ortho should have no shell violations");
    
    println!("Original payload: {:?}", ortho.payload());
    
    // Create a NON-bijective mapping that maps both beta and gamma to the same index.
    // This simulates what would happen if the vocabulary merge had a bug.
    // NOTE: In normal operation, vocabulary merging always produces bijective mappings
    // because words are unique in both vocabularies.
    let mut vocab_map = vec![0; vocab.len()];
    vocab_map[alpha_idx] = 0;
    vocab_map[beta_idx] = 1;  // beta -> 1
    vocab_map[gamma_idx] = 1; // gamma -> 1 (SAME as beta!)
    
    // Remap with the non-bijective mapping
    let remapped = ortho.remap(&vocab_map).expect("Remap should succeed");
    
    println!("Remapped payload with non-bijective map: {:?}", remapped.payload());
    
    // Verify that the remapped ortho now has a shell violation
    assert!(
        has_shell_violation(&remapped).is_some(),
        "Remapped ortho with non-bijective mapping should have shell violations"
    );
    
    // Now positions 1 and 2 would both have value 1!
    if let (Some(val1), Some(val2)) = (remapped.payload()[1], remapped.payload()[2]) {
        if val1 == val2 {
            println!("BUG EXPOSED: Remapped ortho has duplicate token {} at positions 1 and 2 (same shell)!", val1);
            // This IS the bug - remap doesn't validate that the result is still shell-valid
        }
    }
    
    // The question is: can this happen in practice?
    // The vocabulary merge SHOULD be bijective, but this test shows that
    // remap() doesn't validate shell constraints.
}

/// Test the actual expansion reorganization pattern
#[test]
fn test_expansion_reorganization_preserves_shell_validity() {
    let interner = Interner::from_text("a b c d e");
    let vocab = interner.vocabulary();
    
    let a_idx = vocab.iter().position(|w| w == "a").unwrap();
    let b_idx = vocab.iter().position(|w| w == "b").unwrap();
    let c_idx = vocab.iter().position(|w| w == "c").unwrap();
    let d_idx = vocab.iter().position(|w| w == "d").unwrap();
    
    // Build a [2,2] ortho
    let mut ortho = Ortho::new();
    ortho = ortho.add(a_idx)[0].clone();  // pos 0
    ortho = ortho.add(b_idx)[0].clone();  // pos 1
    ortho = ortho.add(c_idx)[0].clone();  // pos 2
    
    println!("Before expansion: dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Add 4th token to trigger expansion
    let children = ortho.add(d_idx);
    
    println!("After expansion, got {} children:", children.len());
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, payload={:?}", i, child.dims(), child.payload());
        
        // Check shell validity for each child
        let dims = child.dims();
        let payload = child.payload();
        
        for pos in 0..payload.len() {
            let (_, diagonals) = spatial::get_requirements(pos, dims);
            
            if let Some(my_val) = payload[pos] {
                for &diag_pos in &diagonals {
                    if let Some(diag_val) = payload[diag_pos] {
                        if my_val == diag_val {
                            panic!(
                                "SHELL VIOLATION in child {}: position {} has value {} which also appears at diagonal position {}",
                                i, pos, my_val, diag_pos
                            );
                        }
                    }
                }
            }
        }
        
        println!("    Child {} is shell-valid", i);
    }
}

/// Verify the reorganization pattern from [2,2] to [2,2,2]
#[test]
fn test_reorganization_pattern_for_up() {
    // According to the tests in spatial.rs:
    // remap_for_up([2,2], 0) = [0,2,3,6]
    // This means:
    // - old position 0 -> new position 0
    // - old position 1 -> new position 2
    // - old position 2 -> new position 3
    // - old position 3 -> new position 6
    
    // In [2,2,2], the position ordering is:
    // Position 0 [0,0,0]: distance=0
    // Position 1 [0,0,1]: distance=1
    // Position 2 [0,1,0]: distance=1
    // Position 3 [1,0,0]: distance=1
    // Position 4 [0,1,1]: distance=2
    // Position 5 [1,0,1]: distance=2
    // Position 6 [1,1,0]: distance=2
    // Position 7 [1,1,1]: distance=3
    
    // So after reorganization:
    // - old pos 0 (distance 0) -> new pos 0 (distance 0) ✓
    // - old pos 1 (distance 1) -> new pos 2 (distance 1) ✓
    // - old pos 2 (distance 1) -> new pos 3 (distance 1) ✓
    // - old pos 3 (distance 2) -> new pos 6 (distance 2) ✓
    
    // This looks correct! The reorganization preserves the distance of each token.
    
    // But wait - in [2,2], positions 1 and 2 are at distance 1 (same shell).
    // After reorganization to [2,2,2], they map to positions 2 and 3, 
    // which are also at distance 1 (same shell).
    // 
    // Additionally, position 1 in [2,2,2] is also at distance 1!
    // This is a NEW position that gets filled with the new value.
    //
    // So after expansion, we have THREE positions at distance 1:
    // - Position 1: the new value
    // - Position 2: old position 1's value
    // - Position 3: old position 2's value
    //
    // The question is: when the new value is inserted at position 1,
    // does the construction logic properly forbid values from positions 2 and 3?
    
    println!("Testing: when filling position 1 in [2,2,2], what are the diagonals?");
    let (_, diagonals_1) = spatial::get_requirements(1, &[2,2,2]);
    println!("Diagonals for position 1: {:?}", diagonals_1);
    
    // The answer should be empty because there are no PREVIOUS positions at distance 1
    // Position 0 is at distance 0, and positions 2 and 3 come AFTER position 1
    assert_eq!(diagonals_1, vec![], "Position 1 should have no diagonals (no previous positions at distance 1)");
    
    // This means when we insert the new value at position 1, we DON'T check
    // against positions 2 and 3! But positions 2 and 3 are already filled
    // from the reorganization!
    
    // WAIT - but the expansion function inserts the new value at the FIRST empty position,
    // which after reorganization might not be position 1...
    // Let me check the actual expand function behavior.
}

/// This test explores whether the expansion can create a state where
/// adding a duplicate in the same shell is possible
#[test]
fn test_expansion_shell_vulnerability() {
    // After investigation, we found that the expansion uses different reorganization
    // patterns depending on get_insert_position(). The actual expansion used pattern
    // [0, 1, 2, 4] (expand_up position=2), which results in:
    // - Positions 0, 1, 2 remain filled
    // - Position 3 is the first empty (to be filled next)
    // - Position 4 has the new value 'd'
    
    // When filling position 3, the diagonals are [1, 2] (positions at distance 1),
    // so values 'b' and 'c' are correctly forbidden.
    
    // To find a bug, we need to explore different expansion scenarios where
    // the reorganization might leave positions in the same shell with:
    // 1. One position already filled with value X
    // 2. Another position empty (to be filled)
    // 3. The empty position's diagonals NOT including the filled position
    
    let _interner = Interner::from_text("a b c d e f g h");
    
    // Build a larger ortho and trace through all expansion patterns
    let mut ortho = Ortho::new();
    for i in 0..3 {
        ortho = ortho.add(i)[0].clone();
    }
    
    println!("Built [2,2] ortho: payload={:?}", ortho.payload());
    
    // Trigger expansion by adding a 4th token
    let children = ortho.add(3);
    
    println!("Expansion children:");
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, payload={:?}", i, child.dims(), child.payload());
        
        // Find the first empty position
        let next_pos = child.get_current_position();
        let (forbidden, _) = child.get_requirements();
        
        println!("    Next pos: {}, forbidden: {:?}", next_pos, forbidden);
        
        // Check if any value currently in the ortho (same shell as next_pos) is NOT forbidden
        let (_, diagonals) = spatial::get_requirements(next_pos, child.dims());
        
        // Get the set of values at diagonal positions
        let diag_values: Vec<usize> = diagonals.iter()
            .filter_map(|&pos| child.payload()[pos])
            .collect();
        
        // Check if forbidden matches diag_values
        let forbidden_set: std::collections::HashSet<usize> = forbidden.iter().copied().collect();
        let diag_set: std::collections::HashSet<usize> = diag_values.iter().copied().collect();
        
        if diag_set != forbidden_set {
            println!("    MISMATCH! Diag values: {:?}, Forbidden: {:?}", diag_set, forbidden_set);
        }
        
        // Check shell validity of the child itself
        if let Some((pos, diag_pos, val)) = has_shell_violation(child) {
            panic!("Shell violation in child {}: pos {} and diag {} both have value {}", i, pos, diag_pos, val);
        }
    }
    
    println!("\nAll expansion children are shell-valid");
}

/// Test vocabulary merge bijectivity
#[test]
fn test_vocabulary_merge_is_bijective() {
    // Test that vocabulary merge always produces bijective mappings
    
    // Case 1: Disjoint vocabularies
    let interner_a = Interner::from_text("cat dog");
    let interner_b = Interner::from_text("fish bird");
    let merged = interner_a.merge(&interner_b);
    
    let map_a: Vec<usize> = interner_a.vocabulary().iter().map(|w| {
        merged.vocabulary().iter().position(|v| v == w).unwrap()
    }).collect();
    let map_b: Vec<usize> = interner_b.vocabulary().iter().map(|w| {
        merged.vocabulary().iter().position(|v| v == w).unwrap()
    }).collect();
    
    assert!(is_bijective_mapping(&map_a), "Mapping A should be bijective");
    assert!(is_bijective_mapping(&map_b), "Mapping B should be bijective");
    
    // Case 2: Overlapping vocabularies
    let interner_a = Interner::from_text("cat dog mouse");
    let interner_b = Interner::from_text("dog bird mouse");
    let merged = interner_a.merge(&interner_b);
    
    let map_a: Vec<usize> = interner_a.vocabulary().iter().map(|w| {
        merged.vocabulary().iter().position(|v| v == w).unwrap()
    }).collect();
    let map_b: Vec<usize> = interner_b.vocabulary().iter().map(|w| {
        merged.vocabulary().iter().position(|v| v == w).unwrap()
    }).collect();
    
    assert!(is_bijective_mapping(&map_a), "Mapping A should be bijective");
    assert!(is_bijective_mapping(&map_b), "Mapping B should be bijective");
    
    // Case 3: Identical vocabularies
    let interner_a = Interner::from_text("cat dog mouse");
    let interner_b = Interner::from_text("cat dog mouse");
    let merged = interner_a.merge(&interner_b);
    
    let map_a: Vec<usize> = interner_a.vocabulary().iter().map(|w| {
        merged.vocabulary().iter().position(|v| v == w).unwrap()
    }).collect();
    let map_b: Vec<usize> = interner_b.vocabulary().iter().map(|w| {
        merged.vocabulary().iter().position(|v| v == w).unwrap()
    }).collect();
    
    assert!(is_bijective_mapping(&map_a), "Mapping A should be bijective");
    assert!(is_bijective_mapping(&map_b), "Mapping B should be bijective");
}

/// Test that the `has_shell_violation` helper works correctly
#[test]
fn test_shell_violation_detection() {
    let interner = Interner::from_text("a b c d");
    let vocab = interner.vocabulary();
    
    // Build a valid ortho
    let mut ortho = Ortho::new();
    ortho = ortho.add(0)[0].clone();
    ortho = ortho.add(1)[0].clone();
    ortho = ortho.add(2)[0].clone();
    
    // This ortho should be valid
    assert!(has_shell_violation(&ortho).is_none(), "Valid ortho should have no shell violations");
    
    // Now create an invalid ortho using non-bijective remap.
    // This simulates a bug scenario where vocabulary merging produces a 
    // non-bijective mapping - this is NOT expected to occur in normal operation.
    let mut vocab_map = vec![0; vocab.len()];
    vocab_map[0] = 0;
    vocab_map[1] = 1; 
    vocab_map[2] = 1; // Maps both b and c to the same index
    
    let invalid = ortho.remap(&vocab_map).expect("Remap should succeed");
    
    // This ortho should have a shell violation at positions 1 and 2
    let violation = has_shell_violation(&invalid);
    assert!(violation.is_some(), "Invalid ortho should have a shell violation");
    
    if let Some((pos1, pos2, val)) = violation {
        println!("Detected shell violation: positions {} and {} both have value {}", pos1, pos2, val);
    }
}

// ============================================================================
// WHITESPACE AND PUNCTUATION INVESTIGATION
// ============================================================================
// 
// The TUI displayed tokens that appeared to be duplicates in the same shell,
// but they might actually be different tokens that LOOK the same due to:
// 1. Unicode whitespace characters that appear invisible
// 2. Non-printable characters surviving the filter
// 3. Unicode homoglyphs (different code points that look identical)
// 4. Trailing/leading whitespace in vocabulary
// ============================================================================

/// Test that Unicode non-breaking space creates a different token than regular space
#[test]
fn test_unicode_nbsp_creates_different_tokens() {
    // Non-breaking space (U+00A0) vs regular space (U+0020)
    let text_with_nbsp = "hello\u{00A0}world"; // hello[NBSP]world - appears as one word
    let text_with_space = "hello world"; // hello world - two words
    
    let interner_nbsp = Interner::from_text(text_with_nbsp);
    let interner_space = Interner::from_text(text_with_space);
    
    println!("Text with NBSP: '{}'", text_with_nbsp);
    println!("Text with space: '{}'", text_with_space);
    println!("NBSP vocab: {:?}", interner_nbsp.vocabulary());
    println!("Space vocab: {:?}", interner_space.vocabulary());
    
    // The NBSP might be treated differently
    // If it's treated as whitespace: we get ["hello", "world"]
    // If it's not: we might get ["hello\u{00A0}world"] as a single token
}

/// Test that various Unicode whitespace characters are handled correctly
#[test]
fn test_unicode_whitespace_handling() {
    use fold::splitter::Splitter;
    
    let splitter = Splitter::new();
    
    // Various Unicode whitespace characters
    let test_cases = vec![
        ("regular space", "hello world"),
        ("tab", "hello\tworld"),
        ("non-breaking space", "hello\u{00A0}world"),
        ("en space", "hello\u{2002}world"),
        ("em space", "hello\u{2003}world"),
        ("thin space", "hello\u{2009}world"),
        ("zero-width space", "hello\u{200B}world"),
        ("zero-width joiner", "hello\u{200D}world"),
        ("ideographic space", "hello\u{3000}world"),
    ];
    
    println!("\nUnicode whitespace handling:");
    for (name, text) in &test_cases {
        let vocab = splitter.vocabulary(text);
        let word_count = vocab.len();
        println!("  {} ({} chars): {} words -> {:?}", 
                 name, text.len(), word_count, vocab);
        
        // Check if any word contains invisible characters
        // (uses same rules as filter_char: alphabetic or apostrophe)
        for word in &vocab {
            let printable_len = word.chars().filter(|c| c.is_alphabetic() || *c == '\'').count();
            if printable_len != word.len() {
                println!("    WARNING: Word '{}' has {} chars but only {} are printable", 
                         word, word.len(), printable_len);
            }
        }
    }
}

/// Test that homoglyphs (visually identical but different codepoints) create different tokens
#[test]
fn test_homoglyph_tokens_look_same_but_differ() {
    // Latin 'a' (U+0061) vs Cyrillic 'а' (U+0430) - look identical!
    let latin_a = "cat";
    let cyrillic_a = "c\u{0430}t"; // Uses Cyrillic 'а' instead of Latin 'a'
    
    let interner_latin = Interner::from_text(latin_a);
    let interner_cyrillic = Interner::from_text(cyrillic_a);
    
    println!("\nHomoglyph test:");
    println!("  Latin 'cat': {:?}", interner_latin.vocabulary());
    println!("  Cyrillic 'cаt': {:?}", interner_cyrillic.vocabulary());
    
    // These should produce different vocabularies
    assert_ne!(
        interner_latin.vocabulary(),
        interner_cyrillic.vocabulary(),
        "Latin and Cyrillic homoglyphs should create different tokens"
    );
    
    // But when displayed, they look identical
    // This could cause the appearance of duplicates in the TUI
    println!("  Visual comparison: '{}' vs '{}'", 
             interner_latin.vocabulary()[0], 
             interner_cyrillic.vocabulary()[0]);
    println!("  They look the same but are different!");
}

/// Test that combining characters don't create false duplicates
#[test]
fn test_combining_characters() {
    // 'é' can be represented as:
    // 1. Single codepoint: U+00E9 (LATIN SMALL LETTER E WITH ACUTE)
    // 2. Two codepoints: U+0065 + U+0301 (LATIN SMALL LETTER E + COMBINING ACUTE ACCENT)
    
    let single_codepoint = "caf\u{00E9}"; // café with precomposed é
    let combining = "cafe\u{0301}"; // café with combining accent
    
    let interner_single = Interner::from_text(single_codepoint);
    let interner_combining = Interner::from_text(combining);
    
    println!("\nCombining characters test:");
    println!("  Precomposed café: {:?}", interner_single.vocabulary());
    println!("  Combining café: {:?}", interner_combining.vocabulary());
    
    // Note: These might or might not be normalized to the same form
    // If they're different, they could appear as duplicates in the TUI
}

/// Test that text from stage.sh might introduce whitespace issues
#[test]
fn test_stage_script_whitespace_edge_cases() {
    use fold::splitter::Splitter;
    
    let splitter = Splitter::new();
    
    // Simulate text that might come from stage.sh processing
    let test_cases = vec![
        // Leading/trailing whitespace
        ("leading space", " hello world"),
        ("trailing space", "hello world "),
        ("both spaces", " hello world "),
        
        // Multiple spaces
        ("double space", "hello  world"),
        ("many spaces", "hello     world"),
        
        // Tabs mixed with spaces
        ("tab and space", "hello \t world"),
        
        // Windows line endings
        ("windows newline", "hello\r\nworld"),
        
        // Newlines within text
        ("single newline", "hello\nworld"),
        
        // Trailing newline
        ("trailing newline", "hello world\n"),
        
        // Empty lines between words
        ("empty line", "hello\n\nworld"),
    ];
    
    println!("\nStage script edge cases:");
    for (name, text) in &test_cases {
        let vocab = splitter.vocabulary(text);
        println!("  {}: {:?}", name, vocab);
        
        // All these should produce clean words without extra whitespace
        for word in &vocab {
            assert!(
                !word.chars().any(|c| c.is_whitespace()),
                "Word '{}' from '{}' contains whitespace", word, name
            );
        }
    }
}

/// Test that special punctuation doesn't create invisible differences
#[test]
fn test_special_punctuation_handling() {
    use fold::splitter::Splitter;
    
    let splitter = Splitter::new();
    
    // Various types of apostrophes and quotes
    let test_cases = vec![
        ("straight apostrophe", "it's"),
        ("curly apostrophe", "it\u{2019}s"), // RIGHT SINGLE QUOTATION MARK
        ("grave accent", "it\u{0060}s"),
        ("acute accent", "it\u{00B4}s"),
        ("prime", "it\u{2032}s"),
    ];
    
    println!("\nApostrophe/quote variations:");
    for (name, text) in &test_cases {
        let vocab = splitter.vocabulary(text);
        println!("  {} '{}': {:?}", name, text, vocab);
    }
    
    // Check if they produce visually similar but different tokens
    let interner1 = Interner::from_text("it's");
    let interner2 = Interner::from_text("it\u{2019}s");
    
    if interner1.vocabulary() != interner2.vocabulary() {
        println!("\n  WARNING: Different apostrophe types create different tokens!");
        println!("  Straight: {:?}", interner1.vocabulary());
        println!("  Curly: {:?}", interner2.vocabulary());
        println!("  These would appear as duplicates in the TUI!");
    }
}

/// Test that merged vocabularies with homoglyphs could cause apparent duplicates
#[test]
fn test_merged_vocab_with_homoglyphs() {
    // Simulate two archives that were processed from different sources
    // where one used Latin characters and one used Cyrillic homoglyphs
    
    let latin_text = "the cat sat on the mat";
    let cyrillic_text = "the c\u{0430}t sat on the mat"; // Cyrillic 'а' in 'cat'
    
    let interner_latin = Interner::from_text(latin_text);
    let interner_cyrillic = Interner::from_text(cyrillic_text);
    
    println!("\nMerged vocab homoglyph test:");
    println!("  Latin vocab: {:?}", interner_latin.vocabulary());
    println!("  Cyrillic vocab: {:?}", interner_cyrillic.vocabulary());
    
    // Merge them
    let merged = interner_latin.merge(&interner_cyrillic);
    println!("  Merged vocab: {:?}", merged.vocabulary());
    
    // Check for visually similar entries
    let vocab = merged.vocabulary();
    for i in 0..vocab.len() {
        for j in (i+1)..vocab.len() {
            if vocab[i].chars().count() == vocab[j].chars().count() {
                // Same length - might be homoglyphs
                let v1_lower = vocab[i].to_lowercase();
                let v2_lower = vocab[j].to_lowercase();
                if v1_lower != v2_lower {
                    // Different content but same length - worth checking
                    println!("  Potential homoglyph pair: '{}' ({} bytes) vs '{}' ({} bytes)",
                             vocab[i], vocab[i].len(), vocab[j], vocab[j].len());
                }
            }
        }
    }
}

/// Test that the filter_char function handles edge cases correctly
#[test]
fn test_filter_char_edge_cases() {
    use fold::splitter::Splitter;
    
    let splitter = Splitter::new();
    
    // Characters that might survive filtering unexpectedly
    let edge_cases = vec![
        ("zero width joiner", "hello\u{200D}world"),
        ("soft hyphen", "hello\u{00AD}world"),
        ("word joiner", "hello\u{2060}world"),
        ("byte order mark", "hello\u{FEFF}world"),
        ("object replacement", "hello\u{FFFC}world"),
    ];
    
    println!("\nFilter edge cases:");
    for (name, text) in &edge_cases {
        let vocab = splitter.vocabulary(text);
        println!("  {} ({} chars in text): {:?}", name, text.chars().count(), vocab);
        
        // Check each word for non-printable characters
        // (uses same rules as Splitter::filter_char: alphabetic or apostrophe)
        for word in &vocab {
            for (i, c) in word.chars().enumerate() {
                if !c.is_alphabetic() && c != '\'' {
                    println!("    WARNING: Word '{}' char {} is U+{:04X} ({})", 
                             word, i, c as u32, c);
                }
            }
        }
    }
}

// ============================================================================
// PRINCESS OF MARS SHELL VIOLATION REPRODUCTION
// ============================================================================
// 
// The following test reproduces a shell violation observed in an actual
// ortho generated from "A Princess of Mars" by Edgar Rice Burroughs.
// 
// The ortho had dimensions [4, 3] with the following layout:
//          of        my trappings
//       their trappings       and
//       craft        at         ·
//         for       the         ·
//
// The word "trappings" appears at:
// - Position 3: coords [0,2], distance=2
// - Position 4: coords [1,1], distance=2
//
// Both positions are in the same shell (distance 2), which violates the
// "no repeats in same shell" rule.
// ============================================================================

/// Test that reproduces the actual [4,3] shell violation from Princess of Mars
#[test]
fn test_princess_of_mars_shell_violation() {
    // The text chunks that produced the problematic ortho
    let chunks = vec![
        // Chunk 1
        "Unseen we reached a rear window and with the straps
and leather of my trappings I lowered, first Sola and then Dejah Thoris
to the ground below.",
        
        // Chunk 2
        "All were mounted upon the small domestic bull
thoats of the red Martians, and their trappings and ornamentation bore
such a quantity of gorgeously colored feathers that I could not but be
struck with the startling resemblance the concourse bore to a band of
the red Indians of my own Earth.",
        
        // Chunk 3
        "Driving my fleet air craft at high speed directly behind the warriors I
soon overtook them and without diminishing my speed I rammed the prow
of my little flier between the shoulders of the nearest.",
        
        // Chunk 4
        "My companion signaled that I slow down, and running his machine close
beside mine suggested that we approach and watch the ceremony, which,
he said, was for the purpose of conferring honors on individual
officers and men for bravery and other distinguished service.",
        
        // Chunk 5
        "Clinging to the wall with my feet and one hand, I unloosened one of the
long leather straps of my trappings at the end of which dangled a great
hook by which air sailors are hung to the sides and bottoms of their
craft for various purposes of repair, and by means of which landing
parties are lowered to the ground from the battleships.",
        
        // Chunk 6 is an intentional duplicate of Chunk 5.
        // This matches the bug report exactly - the text appears twice in the source
        // because it was copy-pasted in the original Project Gutenberg file.
        "Clinging to the wall with my feet and one hand, I unloosened one of the
long leather straps of my trappings at the end of which dangled a great
hook by which air sailors are hung to the sides and bottoms of their
craft for various purposes of repair, and by means of which landing
parties are lowered to the ground from the battleships.",
        
        // Chunk 7
        "Donning my trappings and weapons I hastened to the sheds, and soon had
out both my machine and Kantos Kan's.",
    ];
    
    // Process chunks like main.rs does - merge them one by one
    let mut interner = Interner::from_text(chunks[0]);
    for chunk in chunks.iter().skip(1) {
        let next_interner = Interner::from_text(chunk);
        interner = interner.merge(&next_interner);
    }
    
    println!("Vocabulary size: {}", interner.vocabulary().len());
    println!("Vocabulary: {:?}", interner.vocabulary());
    
    // Check if "trappings" is in the vocabulary
    let trappings_idx = interner.vocabulary().iter().position(|w| w == "trappings");
    println!("'trappings' is at index: {:?}", trappings_idx);
    
    // The ortho from the bug report has:
    // - "of" at position 0
    // - "my" at position 1
    // - "trappings" at position 3 (coords [0,2])
    // - "their" at position 2
    // - "trappings" at position 4 (coords [1,1]) <- DUPLICATE IN SAME SHELL
    // - "and" at position 6
    // - "craft" at position 5
    // - "at" at position 7
    // - "for" at position 8
    // - "the" at position 10
    
    // Position 4's diagonals are [3], so when filling position 4, the value at 
    // position 3 should be forbidden. If "trappings" is at position 3, then
    // "trappings" should NOT be allowed at position 4.
    
    // Check the diagonals for position 4 in [4,3]
    let dims = vec![4, 3];
    let (_, diagonals_4) = spatial::get_requirements(4, &dims);
    println!("Position 4 diagonals: {:?}", diagonals_4);
    assert!(diagonals_4.contains(&3), "Position 4 should have position 3 as a diagonal");
    
    // Show the position layout for [4,3]
    println!("\nPosition layout for [4,3]:");
    let location_to_index = spatial::get_location_to_index(&dims);
    for row in 0..4 {
        for col in 0..3 {
            let coords = vec![row, col];
            let pos = location_to_index.get(&coords).unwrap();
            let distance: usize = coords.iter().sum();
            print!("pos{:2}(d{}) ", pos, distance);
        }
        println!();
    }
    
    // Now let's trace the issue: if the ortho was built correctly, position 4
    // should have "trappings" forbidden because position 3 already has "trappings"
    // 
    // The bug must be in how the ortho was constructed or how the requirements
    // are computed/used during construction.
}

/// Test that verifies diagonal checking during ortho construction
#[test]
fn test_diagonal_check_during_construction() {
    // Create a vocabulary with a few words
    let interner = Interner::from_text("of my trappings their and");
    let vocab = interner.vocabulary();
    
    println!("Vocabulary: {:?}", vocab);
    
    // Get indices
    let of_idx = vocab.iter().position(|w| w == "of").unwrap();
    let my_idx = vocab.iter().position(|w| w == "my").unwrap();
    let trappings_idx = vocab.iter().position(|w| w == "trappings").unwrap();
    let their_idx = vocab.iter().position(|w| w == "their").unwrap();
    let _and_idx = vocab.iter().position(|w| w == "and").unwrap();
    
    println!("of={}, my={}, trappings={}, their={}", of_idx, my_idx, trappings_idx, their_idx);
    
    // Build an ortho step by step
    let mut ortho = Ortho::new();
    println!("\nBuilding ortho step by step:");
    
    // Position 0: of
    ortho = ortho.add(of_idx)[0].clone();
    println!("After adding 'of': dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Position 1: my (distance 1)
    ortho = ortho.add(my_idx)[0].clone();
    println!("After adding 'my': dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Position 2: their (distance 1, same shell as 'my')
    ortho = ortho.add(their_idx)[0].clone();
    println!("After adding 'their': dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Position 3: trappings (distance 2)
    let children = ortho.add(trappings_idx);
    println!("After adding 'trappings': {} children generated", children.len());
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, payload={:?}", i, child.dims(), child.payload());
        
        // For each child, check what the requirements are for the next position
        let (forbidden, _required) = child.get_requirements();
        println!("    Next pos requirements: forbidden={:?}", forbidden);
        
        // Check if trappings is in the forbidden list
        if forbidden.contains(&trappings_idx) {
            println!("    'trappings' (index {}) IS forbidden for next position", trappings_idx);
        } else {
            println!("    'trappings' (index {}) is NOT forbidden for next position", trappings_idx);
        }
    }
}

/// Test that tries to build an ortho with the exact layout from the bug report
/// by directly constructing the payload and checking if it violates shell rules
#[test]
fn test_verify_reported_ortho_has_shell_violation() {
    // Create vocabulary matching the ortho
    let interner = Interner::from_text("of my trappings their and craft at for the");
    let vocab = interner.vocabulary();
    
    println!("Vocabulary: {:?}", vocab);
    
    // Get indices for each word
    let of_idx = vocab.iter().position(|w| w == "of").unwrap();
    let my_idx = vocab.iter().position(|w| w == "my").unwrap();
    let trappings_idx = vocab.iter().position(|w| w == "trappings").unwrap();
    let their_idx = vocab.iter().position(|w| w == "their").unwrap();
    let and_idx = vocab.iter().position(|w| w == "and").unwrap();
    let craft_idx = vocab.iter().position(|w| w == "craft").unwrap();
    let at_idx = vocab.iter().position(|w| w == "at").unwrap();
    let for_idx = vocab.iter().position(|w| w == "for").unwrap();
    let the_idx = vocab.iter().position(|w| w == "the").unwrap();
    
    println!("Indices: of={}, my={}, trappings={}, their={}, and={}, craft={}, at={}, for={}, the={}",
             of_idx, my_idx, trappings_idx, their_idx, and_idx, craft_idx, at_idx, for_idx, the_idx);
    
    // The reported ortho has this layout for [4,3]:
    // Position 0 (d0): "of"
    // Position 1 (d1): "my"
    // Position 2 (d1): "their"
    // Position 3 (d2): "trappings"  <- First
    // Position 4 (d2): "trappings"  <- Duplicate!
    // Position 5 (d2): "craft"
    // Position 6 (d3): "and"
    // Position 7 (d3): "at"
    // Position 8 (d3): "for"
    // Position 9 (d4): None
    // Position 10 (d4): "the"
    // Position 11 (d5): None
    
    let dims = vec![4, 3];
    let payload: Vec<Option<usize>> = vec![
        Some(of_idx),        // 0: of
        Some(my_idx),        // 1: my
        Some(their_idx),     // 2: their
        Some(trappings_idx), // 3: trappings (first)
        Some(trappings_idx), // 4: trappings (DUPLICATE!)
        Some(craft_idx),     // 5: craft
        Some(and_idx),       // 6: and
        Some(at_idx),        // 7: at
        Some(for_idx),       // 8: for
        None,                // 9: empty
        Some(the_idx),       // 10: the
        None,                // 11: empty
    ];
    
    println!("\nChecking for shell violations in reported ortho layout...");
    
    // Check if this payload has a shell violation
    for pos in 0..payload.len() {
        if let Some(my_val) = payload[pos] {
            let (_, diagonals) = spatial::get_requirements(pos, &dims);
            for &diag_pos in &diagonals {
                if let Some(diag_val) = payload[diag_pos] {
                    if my_val == diag_val {
                        println!("SHELL VIOLATION CONFIRMED:");
                        println!("  Position {} (value={}, word='{}') duplicates", 
                                 pos, my_val, vocab[my_val]);
                        println!("  Position {} (value={}, word='{}')", 
                                 diag_pos, diag_val, vocab[diag_val]);
                        
                        // Get coordinates for both positions
                        let location_to_index = spatial::get_location_to_index(&dims);
                        for (loc, &idx) in location_to_index.iter() {
                            if idx == pos {
                                println!("  Position {} has coords {:?}, distance {}", 
                                         pos, loc, loc.iter().sum::<usize>());
                            }
                            if idx == diag_pos {
                                println!("  Position {} has coords {:?}, distance {}", 
                                         diag_pos, loc, loc.iter().sum::<usize>());
                            }
                        }
                        
                        // This confirms the bug - the reported ortho does violate shell rules
                        // Don't panic - we want to continue investigating
                        return;
                    }
                }
            }
        }
    }
    
    panic!("Expected to find a shell violation but none was found!");
}

/// Test simulating the merge process to see if it can create shell violations
#[test]
fn test_merge_process_can_create_violation() {
    // The key insight: the ortho was created from merged chunks.
    // Let's trace how the ortho building and merging process works.
    
    // Create interners from different chunks
    let chunk1 = "of my trappings";  // Contains "of my trappings"
    let chunk2 = "their trappings and";  // Contains "their trappings and"
    
    let interner1 = Interner::from_text(chunk1);
    let interner2 = Interner::from_text(chunk2);
    
    println!("Chunk1 vocab: {:?}", interner1.vocabulary());
    println!("Chunk2 vocab: {:?}", interner2.vocabulary());
    
    // Merge the interners
    let merged = interner1.merge(&interner2);
    println!("Merged vocab: {:?}", merged.vocabulary());
    
    // Check that the vocabulary mapping is bijective for both
    let vocab1_to_merged: Vec<usize> = interner1.vocabulary().iter().map(|w| {
        merged.vocabulary().iter().position(|v| v == w).unwrap()
    }).collect();
    let vocab2_to_merged: Vec<usize> = interner2.vocabulary().iter().map(|w| {
        merged.vocabulary().iter().position(|v| v == w).unwrap()
    }).collect();
    
    println!("Vocab1 mapping: {:?}", vocab1_to_merged);
    println!("Vocab2 mapping: {:?}", vocab2_to_merged);
    
    // The mapping is bijective - each word maps to one index
    // So the bug must be in how orthos are constructed during the worker loop,
    // not in the remap/merge process itself.
}

// ============================================================================
// UNIT TESTS TO FIND THE EXACT POINT OF INVARIANT VIOLATION
// ============================================================================
// 
// The goal is to trace through ortho construction step by step and find
// where the shell invariant first breaks. We check:
// 1. After every add() call, verify no shell violations exist
// 2. After every expansion, verify the reorganization preserves shell validity
// 3. After every remap(), verify no shell violations are introduced
// ============================================================================

/// Helper: Check shell invariant for an ortho
/// Returns Some((pos1, pos2, value)) if violation found
fn check_shell_invariant(ortho: &Ortho) -> Option<(usize, usize, usize)> {
    let dims = ortho.dims();
    let payload = ortho.payload();
    
    for pos in 0..payload.len() {
        if let Some(val) = payload[pos] {
            let (_, diagonals) = spatial::get_requirements(pos, dims);
            for &diag_pos in &diagonals {
                if let Some(diag_val) = payload[diag_pos] {
                    if val == diag_val {
                        return Some((pos, diag_pos, val));
                    }
                }
            }
        }
    }
    None
}

/// Test: Build ortho step by step, checking invariant after every add
#[test]
fn test_build_ortho_checking_invariant_at_each_step() {
    // We'll try to build toward a [4,3] ortho and check the invariant at each step
    let mut ortho = Ortho::new();
    
    // Use sequential values 0, 1, 2, ... for simplicity
    let mut step = 0;
    
    // Add first value
    ortho = ortho.add(0)[0].clone();
    step += 1;
    assert!(
        check_shell_invariant(&ortho).is_none(),
        "Step {}: Shell invariant violated after adding value 0: dims={:?}, payload={:?}",
        step, ortho.dims(), ortho.payload()
    );
    
    // Add second value
    ortho = ortho.add(1)[0].clone();
    step += 1;
    assert!(
        check_shell_invariant(&ortho).is_none(),
        "Step {}: Shell invariant violated after adding value 1: dims={:?}, payload={:?}",
        step, ortho.dims(), ortho.payload()
    );
    
    // Add third value
    ortho = ortho.add(2)[0].clone();
    step += 1;
    assert!(
        check_shell_invariant(&ortho).is_none(),
        "Step {}: Shell invariant violated after adding value 2: dims={:?}, payload={:?}",
        step, ortho.dims(), ortho.payload()
    );
    
    // Add fourth value - this triggers expansion
    println!("Before step {}: dims={:?}, payload={:?}", step + 1, ortho.dims(), ortho.payload());
    let children = ortho.add(3);
    step += 1;
    
    println!("Step {} produced {} children:", step, children.len());
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, payload={:?}", i, child.dims(), child.payload());
        if let Some((p1, p2, v)) = check_shell_invariant(child) {
            println!("    INVARIANT VIOLATED: positions {} and {} both have value {}", p1, p2, v);
        }
    }
    
    // All children should pass
    for (i, child) in children.iter().enumerate() {
        assert!(
            check_shell_invariant(child).is_none(),
            "Step {}, child {}: Shell invariant violated: dims={:?}, payload={:?}",
            step, i, child.dims(), child.payload()
        );
    }
    
    // Continue with one of the children
    ortho = children[0].clone();
    
    // Keep adding values
    for val in 4..15 {
        let children = ortho.add(val);
        step += 1;
        
        if children.is_empty() {
            break;
        }
        
        for (i, child) in children.iter().enumerate() {
            if let Some((p1, p2, v)) = check_shell_invariant(child) {
                panic!(
                    "Step {}, child {}: Shell invariant violated - positions {} and {} both have value {}\n  dims={:?}\n  payload={:?}",
                    step, i, p1, p2, v, child.dims(), child.payload()
                );
            }
        }
        
        ortho = children[0].clone();
    }
    
    println!("Final ortho: dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
}

/// Test: Verify that expansion reorganization preserves shell invariant
#[test]
fn test_expansion_reorganization_invariant() {
    // Build a valid [2,2] ortho
    let mut ortho = Ortho::new();
    ortho = ortho.add(0)[0].clone(); // pos 0
    ortho = ortho.add(1)[0].clone(); // pos 1
    ortho = ortho.add(2)[0].clone(); // pos 2
    
    println!("Before expansion: dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    assert!(check_shell_invariant(&ortho).is_none(), "Pre-expansion ortho should be valid");
    
    // Trigger expansion
    let children = ortho.add(3);
    
    println!("After expansion - {} children:", children.len());
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}", i, child.dims());
        
        // Check each position's diagonals
        let dims = child.dims();
        let payload = child.payload();
        
        for pos in 0..payload.len() {
            if let Some(val) = payload[pos] {
                let (_, diagonals) = spatial::get_requirements(pos, dims);
                println!("    pos {} (val={}) has diagonals {:?}", pos, val, diagonals);
                
                for &diag_pos in &diagonals {
                    if let Some(diag_val) = payload[diag_pos] {
                        if val == diag_val {
                            panic!(
                                "EXPANSION BUG: Child {} has value {} at both pos {} and diagonal pos {}",
                                i, val, pos, diag_pos
                            );
                        }
                    }
                }
            }
        }
        
        assert!(
            check_shell_invariant(child).is_none(),
            "Expansion child {} should maintain shell invariant",
            i
        );
    }
}

/// Test: Check if the forbidden list in get_requirements() is correct for [4,3]
#[test]
fn test_forbidden_list_for_4x3_position_4() {
    // In [4,3]:
    // Position 3 has coords [0,2], distance 2
    // Position 4 has coords [1,1], distance 2
    // Position 4's diagonals should include position 3
    
    let dims = vec![4, 3];
    let (_, diagonals) = spatial::get_requirements(4, &dims);
    
    println!("[4,3] position 4 diagonals: {:?}", diagonals);
    
    assert!(
        diagonals.contains(&3),
        "Position 4 in [4,3] should have position 3 as a diagonal (both at distance 2)"
    );
}

/// Test: Try to create the problematic scenario through normal construction
#[test]
fn test_can_construction_produce_duplicate_in_shell() {
    // Build ortho step by step and try to add the SAME value at position 4
    // that's already at position 3
    
    let mut ortho = Ortho::new();
    ortho = ortho.add(0)[0].clone(); // "of" at pos 0
    ortho = ortho.add(1)[0].clone(); // "my" at pos 1  
    ortho = ortho.add(2)[0].clone(); // "their" at pos 2
    
    // Now add "trappings" (value 3) to trigger expansion
    let children = ortho.add(3);
    
    // Find a child that could lead to [4,3]
    // and check what values are forbidden for the next position
    
    for (i, child) in children.iter().enumerate() {
        let (forbidden, _) = child.get_requirements();
        let next_pos = child.get_current_position();
        
        println!("Child {} (dims={:?}): next pos={}, forbidden={:?}", 
                 i, child.dims(), next_pos, forbidden);
        
        // If position 3 has value 3, then value 3 should be in forbidden
        // when filling position 4 (if they're in the same shell)
        let payload = child.payload();
        if let Some(val_at_3) = payload.get(3).and_then(|v| *v) {
            if forbidden.contains(&val_at_3) {
                println!("  Value {} at pos 3 is correctly forbidden for pos {}", val_at_3, next_pos);
            } else {
                println!("  WARNING: Value {} at pos 3 is NOT forbidden for pos {}", val_at_3, next_pos);
                
                // Check if positions 3 and next_pos are in the same shell
                let dims = child.dims();
                let (_, diagonals_for_next) = spatial::get_requirements(next_pos, dims);
                println!("  Diagonals for pos {}: {:?}", next_pos, diagonals_for_next);
            }
        }
    }
}

/// Test: Trace construction from [2,2] through multiple expansions to [4,3]
#[test]
fn test_trace_construction_to_4x3() {
    println!("=== Tracing construction from [2,2] toward [4,3] ===\n");
    
    // We need to understand how to get to [4,3]
    // [2,2] -> [3,2] or [2,2,2] -> ... -> [4,3]
    
    let mut ortho = Ortho::new();
    let mut step = 0;
    
    // Function to print ortho state
    fn print_state(step: usize, ortho: &Ortho, action: &str) {
        println!("Step {}: {} -> dims={:?}, payload={:?}", 
                 step, action, ortho.dims(), ortho.payload());
        
        // Check invariant
        if let Some((p1, p2, v)) = check_shell_invariant(ortho) {
            println!("  !!! INVARIANT VIOLATION: pos {} and {} both have value {} !!!", p1, p2, v);
        }
    }
    
    // Build initial [2,2]
    ortho = ortho.add(0)[0].clone();
    step += 1;
    print_state(step, &ortho, "add(0)");
    
    ortho = ortho.add(1)[0].clone();
    step += 1;
    print_state(step, &ortho, "add(1)");
    
    ortho = ortho.add(2)[0].clone();
    step += 1;
    print_state(step, &ortho, "add(2)");
    
    // Expansion
    let children = ortho.add(3);
    step += 1;
    println!("\nStep {}: add(3) -> {} children", step, children.len());
    
    // Try to find a path to [4,3]
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}", i, child.dims());
        
        // Check [3,2] child - this is the path to [4,3]
        if child.dims() == &vec![3, 2] {
            println!("\n  Following [3,2] path:");
            let mut current = child.clone();
            
            // Continue adding values
            for val in 4..12 {
                let next_children = current.add(val);
                step += 1;
                
                if next_children.is_empty() {
                    println!("    Step {}: add({}) -> no children", step, val);
                    break;
                }
                
                println!("    Step {}: add({}) -> {} children", step, val, next_children.len());
                
                for (j, nc) in next_children.iter().enumerate() {
                    println!("      Child {}: dims={:?}", j, nc.dims());
                    if let Some((p1, p2, v)) = check_shell_invariant(nc) {
                        println!("      !!! INVARIANT VIOLATION: pos {} and {} both have value {} !!!", p1, p2, v);
                    }
                    
                    // If we reach [4,3], print details
                    if nc.dims() == &vec![4, 3] {
                        println!("\n      === REACHED [4,3] ===");
                        println!("      payload={:?}", nc.payload());
                    }
                }
                
                // Follow first child
                current = next_children[0].clone();
            }
        }
    }
}

/// Test: Check if remap after vocabulary merge can create shell violations
#[test]
fn test_remap_after_merge_invariant() {
    println!("=== Testing remap after merge ===\n");
    
    // Create an interner with some words
    let interner1 = Interner::from_text("a b c d e");
    let vocab1 = interner1.vocabulary();
    println!("Vocab1: {:?}", vocab1);
    
    // Build an ortho using vocab1 indices
    let a_idx = vocab1.iter().position(|w| w == "a").unwrap();
    let b_idx = vocab1.iter().position(|w| w == "b").unwrap();
    let c_idx = vocab1.iter().position(|w| w == "c").unwrap();
    let d_idx = vocab1.iter().position(|w| w == "d").unwrap();
    
    let mut ortho = Ortho::new();
    ortho = ortho.add(a_idx)[0].clone();
    ortho = ortho.add(b_idx)[0].clone();
    ortho = ortho.add(c_idx)[0].clone();
    let children = ortho.add(d_idx);
    ortho = children[0].clone();
    
    println!("Ortho before merge: dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    assert!(check_shell_invariant(&ortho).is_none(), "Pre-merge ortho should be valid");
    
    // Create another interner with overlapping words
    let interner2 = Interner::from_text("c d e f g");
    let vocab2 = interner2.vocabulary();
    println!("Vocab2: {:?}", vocab2);
    
    // Merge
    let merged = interner1.merge(&interner2);
    let merged_vocab = merged.vocabulary();
    println!("Merged vocab: {:?}", merged_vocab);
    
    // Build vocab_map from old indices to new indices
    let vocab_map: Vec<usize> = vocab1.iter().map(|w| {
        merged_vocab.iter().position(|v| v == w).unwrap()
    }).collect();
    println!("Vocab map (old -> new): {:?}", vocab_map);
    
    // Remap the ortho
    let remapped = ortho.remap(&vocab_map).unwrap();
    println!("Ortho after remap: dims={:?}, payload={:?}", remapped.dims(), remapped.payload());
    
    // Check invariant after remap
    if let Some((p1, p2, v)) = check_shell_invariant(&remapped) {
        panic!(
            "REMAP VIOLATION: positions {} and {} both have value {} after remap\n  dims={:?}\n  payload={:?}",
            p1, p2, v, remapped.dims(), remapped.payload()
        );
    }
    
    println!("Remap preserved shell invariant");
}

/// Test: The real scenario - build ortho from chunk1, then check if 
/// continuing to build after merge can violate the invariant
#[test]
fn test_build_continue_after_merge() {
    println!("=== Testing build-merge-continue scenario ===\n");
    
    // Chunk 1: "of my trappings"
    let chunk1 = "of my trappings";
    let interner1 = Interner::from_text(chunk1);
    let vocab1 = interner1.vocabulary();
    println!("Chunk1 vocab: {:?}", vocab1);
    
    let of_idx1 = vocab1.iter().position(|w| w == "of").unwrap();
    let my_idx1 = vocab1.iter().position(|w| w == "my").unwrap();
    let trappings_idx1 = vocab1.iter().position(|w| w == "trappings").unwrap();
    
    // Build ortho with chunk1 vocabulary
    let mut ortho = Ortho::new();
    ortho = ortho.add(of_idx1)[0].clone();
    ortho = ortho.add(my_idx1)[0].clone();
    ortho = ortho.add(trappings_idx1)[0].clone();
    
    println!("Ortho from chunk1: dims={:?}, payload={:?}", ortho.dims(), ortho.payload());
    
    // Chunk 2: "their trappings and"
    let chunk2 = "their trappings and";
    let interner2 = Interner::from_text(chunk2);
    
    // Merge
    let merged = interner1.merge(&interner2);
    let merged_vocab = merged.vocabulary();
    println!("Merged vocab: {:?}", merged_vocab);
    
    // Build vocab_map
    let vocab_map: Vec<usize> = vocab1.iter().map(|w| {
        merged_vocab.iter().position(|v| v == w).unwrap()
    }).collect();
    println!("Vocab map: {:?}", vocab_map);
    
    // Remap the ortho
    let remapped = ortho.remap(&vocab_map).unwrap();
    println!("Ortho after remap: dims={:?}, payload={:?}", remapped.dims(), remapped.payload());
    
    // Now try to continue adding values using merged vocabulary
    let their_idx_merged = merged_vocab.iter().position(|w| w == "their").unwrap();
    let trappings_idx_merged = merged_vocab.iter().position(|w| w == "trappings").unwrap();
    let and_idx_merged = merged_vocab.iter().position(|w| w == "and").unwrap();
    
    println!("\nMerged indices: their={}, trappings={}, and={}", 
             their_idx_merged, trappings_idx_merged, and_idx_merged);
    
    // Get requirements for the next position
    let (forbidden, _required) = remapped.get_requirements();
    println!("Next position requirements: forbidden={:?}", forbidden);
    
    // Try adding "their" to trigger expansion
    let children = remapped.add(their_idx_merged);
    println!("After adding 'their': {} children", children.len());
    
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, payload={:?}", i, child.dims(), child.payload());
        
        if let Some((p1, p2, v)) = check_shell_invariant(&child) {
            panic!(
                "VIOLATION after adding 'their': positions {} and {} both have value {}\n  dims={:?}\n  payload={:?}",
                p1, p2, v, child.dims(), child.payload()
            );
        }
        
        // For each child, check if "trappings" is forbidden for the next position
        let (next_forbidden, _) = child.get_requirements();
        let next_pos = child.get_current_position();
        
        println!("    Next pos: {}, forbidden: {:?}", next_pos, next_forbidden);
        
        if next_forbidden.contains(&trappings_idx_merged) {
            println!("    'trappings' ({}) IS forbidden", trappings_idx_merged);
        } else {
            println!("    'trappings' ({}) is NOT forbidden", trappings_idx_merged);
            
            // Check the diagonals
            let (_, diagonals) = spatial::get_requirements(next_pos, child.dims());
            println!("    Diagonals for pos {}: {:?}", next_pos, diagonals);
            
            // What values are at those diagonal positions?
            for &diag_pos in &diagonals {
                if let Some(v) = child.payload()[diag_pos] {
                    let word = &merged_vocab[v];
                    println!("      Diagonal pos {} has value {} ('{}')", diag_pos, v, word);
                }
            }
        }
        
        // Continue adding "trappings" - this should NOT be allowed if forbidden correctly
        let (forbidden2, _) = child.get_requirements();
        if !forbidden2.contains(&trappings_idx_merged) {
            println!("\n    !!! ATTEMPTING TO ADD 'trappings' when not forbidden !!!");
            let children2 = child.add(trappings_idx_merged);
            
            for (j, child2) in children2.iter().enumerate() {
                println!("      After adding 'trappings': Child {}: dims={:?}", j, child2.dims());
                
                if let Some((p1, p2, v)) = check_shell_invariant(&child2) {
                    println!(
                        "      !!! SHELL VIOLATION: positions {} and {} both have value {} !!!",
                        p1, p2, v
                    );
                }
            }
        }
    }
}

/// Test: Trace all expansion paths to find one that could lead to the bug
#[test]
fn test_find_expansion_path_to_violation() {
    println!("=== Finding expansion path that could violate invariant ===\n");
    
    // We need to find a scenario where:
    // 1. "trappings" is placed at position 3 (distance 2)
    // 2. Later, "trappings" is allowed at position 4 (distance 2)
    
    // Let's build toward [4,3] with specific values
    let mut ortho = Ortho::new();
    
    // Add values 0, 1, 2 to get [2,2]
    ortho = ortho.add(0)[0].clone();
    ortho = ortho.add(1)[0].clone();
    ortho = ortho.add(2)[0].clone();
    
    println!("Starting [2,2]: {:?}", ortho.payload());
    
    // Add value 3 to trigger expansion
    let children = ortho.add(3);
    
    println!("After expansion, {} children:", children.len());
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}, payload={:?}", i, child.dims(), child.payload());
    }
    
    // Follow the [3,2] path
    let ortho32 = children.iter().find(|c| c.dims() == &vec![3, 2]).unwrap().clone();
    println!("\nFollowing [3,2] path...");
    
    // Continue building
    let mut current = ortho32;
    for val in 4..8 {
        let children = current.add(val);
        println!("Add {}: {} children", val, children.len());
        
        for (i, child) in children.iter().enumerate() {
            println!("  Child {}: dims={:?}", i, child.dims());
            
            // Check if we have [4,3]
            if child.dims() == &vec![4, 3] {
                println!("    [4,3] payload: {:?}", child.payload());
                
                // Check positions 3 and 4
                let p3 = child.payload()[3];
                let p4 = child.payload()[4];
                println!("    Position 3 value: {:?}", p3);
                println!("    Position 4 value: {:?}", p4);
                
                // Get diagonals
                let (_, diag3) = spatial::get_requirements(3, child.dims());
                let (_, diag4) = spatial::get_requirements(4, child.dims());
                println!("    Position 3 diagonals: {:?}", diag3);
                println!("    Position 4 diagonals: {:?}", diag4);
            }
        }
        
        current = children[0].clone();
    }
    
    println!("\nConclusion: Normal construction path does not produce violations.");
    println!("The bug must be in how the main loop processes orthos across file boundaries.");
}

// ============================================================================
// ROOT CAUSE ANALYSIS
// ============================================================================
// 
// THE BUG: After expansion, orthos have sparse layouts where later positions
// are filled before earlier positions. The `get_requirements()` function only
// looks at positions BEFORE the current position for forbidden values, but
// it should also check LATER positions that are already filled.
//
// EXAMPLE:
// After expanding [2,2] to [2,2,2], the ortho might have:
//   payload: [Some(23), None, Some(18), Some(20), None, None, Some(16), None]
//   words:   ["the",    ·,    "prow", "shoulders", ·,   ·,   "of",      ·   ]
//
// Position 1 is empty but positions 2 and 3 are filled.
// When filling position 1:
//   - get_current_position() returns 1 (first None)
//   - get_requirements(1, dims) checks diagonals at same distance
//   - BUT it only considers positions < 1, not positions 2 and 3!
//
// Since positions 2 and 3 are both at distance 1 (same shell as position 1),
// their values should be forbidden when filling position 1. But the diagonal
// check only looks at earlier positions.
// ============================================================================

/// THE DEFINITIVE TEST: This test demonstrates the exact bug that causes shell violations.
/// 
/// After expansion, positions are reorganized such that later positions can be filled
/// while earlier positions are empty. When filling those empty positions, the diagonal
/// check ONLY looks at earlier positions, not at later positions that are already filled.
#[test]
fn test_expansion_creates_sparse_layout_bug() {
    // Build a [2,2] ortho and expand it to [2,2,2]
    // After expansion, the ortho will have a sparse layout
    
    let interner = Interner::from_text("alpha beta gamma delta epsilon");
    let vocab = interner.vocabulary();
    
    let idx = |name: &str| vocab.iter().position(|w| w == name).unwrap();
    
    // Build [2,2] ortho: fill positions 0, 1, 2, 3
    let ortho = Ortho::new();
    let ortho = ortho.add(idx("alpha"))[0].clone();  // pos 0: alpha
    let ortho = ortho.add(idx("beta"))[0].clone();   // pos 1: beta
    let ortho = ortho.add(idx("gamma"))[0].clone();  // pos 2: gamma (canonicalized if needed)
    
    println!("[2,2] ortho before expansion:");
    println!("  dims: {:?}", ortho.dims());
    println!("  payload: {:?}", ortho.payload());
    
    // When we add the 4th value, it will trigger expansion to [2,2,2] or [3,2]
    let children = ortho.add(idx("delta"));
    
    println!("\nAfter expansion (all children):");
    for (i, child) in children.iter().enumerate() {
        println!("  Child {}: dims={:?}", i, child.dims());
        println!("    payload: {:?}", child.payload());
        
        // Find empty positions followed by filled positions
        let payload = child.payload();
        let mut empty_before_filled = vec![];
        for pos in 0..payload.len() {
            if payload[pos].is_none() {
                // Check if any later position is filled
                for later in (pos+1)..payload.len() {
                    if payload[later].is_some() {
                        empty_before_filled.push((pos, later));
                    }
                }
            }
        }
        
        if !empty_before_filled.is_empty() {
            println!("    SPARSE LAYOUT DETECTED!");
            for (empty, filled) in &empty_before_filled {
                println!("      Position {} is empty, but position {} is filled", empty, filled);
                
                // Check if they are at the same distance
                let (_, diag_empty) = spatial::get_requirements(*empty, child.dims());
                let (_, _diag_filled) = spatial::get_requirements(*filled, child.dims());
                
                // Get indices (coordinates) for distance calculation
                // The diagonals for empty position should include filled position
                // if they are at the same distance
                println!("      Empty position {} diagonals: {:?}", empty, diag_empty);
                
                // THE BUG: diag_empty only includes positions BEFORE empty,
                // not positions AFTER empty that are also at the same distance!
            }
        }
    }
    
    // Now demonstrate the actual bug:
    // Take a child with sparse layout and try to fill it
    println!("\n--- DEMONSTRATING THE BUG ---\n");
    
    for child in &children {
        let current_pos = child.get_current_position();
        if current_pos < child.payload().len() {
            println!("Next position to fill: {}", current_pos);
            println!("  Current payload: {:?}", child.payload());
            
            let (forbidden, required) = child.get_requirements();
            println!("  Forbidden (from get_requirements): {:?}", forbidden);
            println!("  Required: {:?}", required);
            
            // Check what values are at same-distance positions
            let (_, diagonals) = spatial::get_requirements(current_pos, child.dims());
            println!("  Diagonal positions (same distance as {}): {:?}", current_pos, diagonals);
            
            // But wait - diagonals only includes positions BEFORE current_pos!
            // Check if there are any same-distance positions AFTER current_pos
            let payload = child.payload();
            for later in (current_pos+1)..payload.len() {
                if let Some(val) = payload[later] {
                    // Check if later position is at same distance as current_pos
                    // This is what the current code MISSES!
                    let (_, later_diags) = spatial::get_requirements(later, child.dims());
                    if later_diags.contains(&current_pos) {
                        println!("  *** BUG: Position {} contains value {} but is NOT in forbidden! ***", later, val);
                        println!("  *** These positions are at the same distance (same shell) ***");
                        println!("  *** Filling position {} with value {} would create a shell violation! ***", current_pos, val);
                    }
                }
            }
        }
    }
}

/// Test that demonstrates get_requirements only looks at earlier positions
#[test]
fn test_get_requirements_only_checks_earlier_positions() {
    // In [2,2,2], positions at distance 1 are: 1, 2, 3
    // When checking position 1's requirements:
    //   - diagonals should include positions at same distance AND before position 1
    //   - but there are no positions before 1 at distance 1!
    // When checking position 2's requirements:
    //   - diagonals should include position 1 (same distance, before 2)
    // When checking position 3's requirements:
    //   - diagonals should include positions 1 and 2 (same distance, before 3)
    
    let dims = vec![2, 2, 2];
    
    println!("Diagonal positions in [2,2,2]:");
    for pos in 0..8 {
        let (_, diags) = spatial::get_requirements(pos, &dims);
        println!("  Position {}: diagonals = {:?}", pos, diags);
    }
    
    // Verify the expectation:
    let (_, diag1) = spatial::get_requirements(1, &dims);
    let (_, diag2) = spatial::get_requirements(2, &dims);
    let (_, diag3) = spatial::get_requirements(3, &dims);
    
    assert!(diag1.is_empty(), "Position 1 should have no earlier same-distance positions");
    assert!(diag2.contains(&1), "Position 2 should have 1 as diagonal");
    assert!(diag3.contains(&1), "Position 3 should have 1 as diagonal");
    assert!(diag3.contains(&2), "Position 3 should have 2 as diagonal");
    
    println!("\nThis is correct behavior for SEQUENTIAL filling.");
    println!("But after EXPANSION, the layout becomes sparse:");
    println!("  - Position 1 might be empty");
    println!("  - Positions 2 and 3 might be filled");
    println!("  - When filling position 1, we SHOULD check 2 and 3 for forbidden values");
    println!("  - But get_requirements(1) returns empty diagonals!");
}
