use fold::ortho::Ortho;
use std::collections::HashSet;

#[test]
fn test_simple_worker_loop() {
    // Arrange
    let text = "the quick brown fox";
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act
    let (interner, _changed_keys_count, frontier_size) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert
    assert_eq!(interner.version(), 1, "Should create version 1");
    assert!(seen_ids.len() > 0, "Should have generated at least one ortho");
    assert!(optimal_ortho.is_some(), "Should have an optimal ortho");
    assert!(frontier_size > 0, "Should have orthos in the frontier");
    assert_eq!(frontier_size, frontier.len(), "Frontier size should match frontier set size");
}

#[test]
fn test_multiple_file_processing() {
    // Arrange
    let texts = vec!["the cat sat", "the dog ran"];
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        let (new_interner, _changed_keys_count, _frontier_size) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier);
        interner = Some(new_interner);
    }
    
    // Assert
    let final_interner = interner.unwrap();
    assert_eq!(final_interner.version(), 2, "Should have created 2 versions");
    assert!(final_interner.vocabulary().len() > 3, "Should have accumulated vocabulary");
    assert!(seen_ids.len() > 0, "Should have generated orthos from both texts");
}

#[test]
fn test_optimal_ortho_tracking() {
    // Arrange
    let text = "a b c d e";
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act
    let (_interner, _changed_keys_count, _frontier_size) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert
    let optimal = optimal_ortho.expect("Should have an optimal ortho");
    let volume: usize = optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
    assert!(volume > 0, "Optimal ortho should have positive volume");
}

#[test]
fn test_end_to_end_run_pattern() {
    // Arrange
    let texts = vec![
        "the quick brown fox jumps over the lazy dog",
        "the cat sat on the mat"
    ];
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        let (new_interner, _changed_keys_count, _frontier_size) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier);
        interner = Some(new_interner);
    }
    
    // Assert
    let final_interner = interner.expect("Should have built interner");
    assert_eq!(final_interner.version(), 2, "Should have 2 versions from 2 texts");
    assert!(final_interner.vocabulary().len() > 8, "Should have accumulated vocabulary from both texts");
    
    let optimal = optimal_ortho.expect("Should have found optimal ortho");
    let volume: usize = optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
    assert!(volume > 0, "Optimal should have positive volume");
    assert!(seen_ids.len() > 0, "Should have generated orthos");
}

#[test]
fn test_interner_version_increments() {
    // Arrange
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act
    let (interner1, _, _) = fold::process_text("first text", None, &mut seen_ids, &mut optimal_ortho, &mut frontier);
    let (interner2, _, _) = fold::process_text("second text", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier);
    let (interner3, _, _) = fold::process_text("third text", Some(interner2), &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert
    assert_eq!(interner3.version(), 3, "Should have version 3 after processing 3 texts");
}

#[test]
fn test_seen_ids_accumulate() {
    // Arrange
    let texts = vec!["a b", "c d"];
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        let (new_interner, _changed_keys_count, _frontier_size) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier);
        interner = Some(new_interner);
    }
    
    // Assert
    // Seen IDs should accumulate across all files
    assert!(seen_ids.len() > 0, "Should track orthos across all files");
}

#[test]
fn test_changed_keys_tracking() {
    // Arrange
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act - first text (baseline)
    let (interner1, changed_count1, _) = fold::process_text("a b c", None, &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert - first text should have 0 changed keys (no previous interner)
    assert_eq!(changed_count1, 0, "First text should have 0 changed keys");
    
    // Act - second text adds new phrase structure
    let (interner2, changed_count2, _) = fold::process_text("a c", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert - second text should have some changed keys
    assert!(changed_count2 > 0, "Second text should have changed keys: {}", changed_count2);
    
    // Act - third text with no new vocabulary or patterns within existing vocab
    let (_interner3, changed_count3, _) = fold::process_text("x y z", Some(interner2), &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert - third text adds new vocabulary, so should have changed keys
    assert!(changed_count3 > 0, "Third text should have changed keys for new vocabulary");
}

#[test]
fn test_frontier_tracks_leaf_orthos() {
    // Arrange
    let text = "a b c";
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act
    let (_interner, _changed_keys_count, frontier_size) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert
    assert!(frontier_size > 0, "Frontier should not be empty");
    assert_eq!(frontier_size, frontier.len(), "Frontier size should match set size");
    assert!(frontier_size <= seen_ids.len(), "Frontier should be subset of all seen orthos");
}

#[test]
fn test_frontier_only_contains_leaf_orthos() {
    // Arrange
    let text = "a b";
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act
    let (_interner, _changed_keys_count, frontier_size) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert
    // With simple text "a b", we expect some orthos to be in frontier
    assert!(frontier_size > 0, "Should have orthos in frontier");
    // The frontier should be smaller than total seen orthos (some orthos produced children)
    assert!(frontier_size < seen_ids.len(), "Frontier should be smaller than total orthos");
}

#[test]
fn test_frontier_accumulates_across_files() {
    // Arrange
    let texts = vec!["a b", "c d"];
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    let mut frontier_sizes = Vec::new();
    for text in texts {
        let (new_interner, _changed_keys_count, frontier_size) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier);
        frontier_sizes.push(frontier_size);
        interner = Some(new_interner);
    }
    
    // Assert
    // Frontier should accumulate across files (or at least stay consistent)
    assert!(frontier_sizes.iter().all(|&size| size > 0), "All frontier sizes should be positive");
    // Final frontier size should equal the frontier set size
    assert_eq!(frontier_sizes.last().unwrap(), &frontier.len(), "Final frontier size should match set size");
}

#[test]
fn test_frontier_is_subset_of_seen_ids() {
    // Arrange
    let text = "the quick brown fox jumps over";
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act
    let (_interner, _changed_keys_count, _frontier_size) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier);
    
    // Assert
    // Every ID in frontier should be in seen_ids
    for frontier_id in &frontier {
        assert!(seen_ids.contains(frontier_id), "Frontier ID {} should be in seen_ids", frontier_id);
    }
    // Frontier should be a proper subset (some orthos produced children)
    assert!(frontier.len() < seen_ids.len(), "Frontier should be smaller than total seen orthos");
}
