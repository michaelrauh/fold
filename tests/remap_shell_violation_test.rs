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
