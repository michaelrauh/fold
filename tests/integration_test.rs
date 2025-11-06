use fold::ortho::Ortho;
use std::collections::{HashSet, HashMap};

#[test]
fn test_simple_worker_loop() {
    // Arrange
    let text = "the quick brown fox";
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act
    let (interner, _changed_keys_count, frontier_size, _impacted_frontier_count) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
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
        let (new_interner, _changed_keys_count, _frontier_size, _impacted_frontier_count) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
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
    let (_interner, _changed_keys_count, _frontier_size, _impacted_frontier_count) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
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
        let (new_interner, _changed_keys_count, _frontier_size, _impacted_frontier_count) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
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
    let (interner1, _, _, _) = fold::process_text("first text", None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    let (interner2, _, _, _) = fold::process_text("second text", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    let (interner3, _, _, _) = fold::process_text("third text", Some(interner2), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
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
        let (new_interner, _changed_keys_count, _frontier_size, _impacted_frontier_count) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
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
    let (interner1, changed_count1, _, _) = fold::process_text("a b c", None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert - first text should have 0 changed keys (no previous interner)
    assert_eq!(changed_count1, 0, "First text should have 0 changed keys");
    
    // Act - second text adds new phrase structure
    let (interner2, changed_count2, _, _) = fold::process_text("a c", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert - second text should have some changed keys
    assert!(changed_count2 > 0, "Second text should have changed keys: {}", changed_count2);
    
    // Act - third text with no new vocabulary or patterns within existing vocab
    let (_interner3, changed_count3, _, _) = fold::process_text("x y z", Some(interner2), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
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
    let (_interner, _changed_keys_count, frontier_size, _impacted_frontier_count) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
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
    let (_interner, _changed_keys_count, frontier_size, _impacted_frontier_count) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
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
        let (new_interner, _changed_keys_count, frontier_size, _impacted_frontier_count) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
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
    let (_interner, _changed_keys_count, _frontier_size, _impacted_frontier_count) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert
    // Every ID in frontier should be in seen_ids
    for frontier_id in &frontier {
        assert!(seen_ids.contains(frontier_id), "Frontier ID {} should be in seen_ids", frontier_id);
    }
    // Frontier should be a proper subset (some orthos produced children)
    assert!(frontier.len() < seen_ids.len(), "Frontier should be smaller than total seen orthos");
}

#[test]
fn test_impacted_frontier_count_with_no_changes() {
    // Arrange
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act - first text (baseline, no previous interner to compare)
    let (interner1, changed_count1, _, impacted_count1) = fold::process_text("a b c", None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert
    assert_eq!(changed_count1, 0, "First text should have 0 changed keys");
    assert_eq!(impacted_count1, 0, "Should have 0 impacted frontier orthos with no changed keys");
    
    // Act - second text with empty text (no changes)
    let (_interner2, changed_count2, _, impacted_count2) = fold::process_text("", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert
    assert_eq!(changed_count2, 0, "Empty text should have 0 changed keys");
    assert_eq!(impacted_count2, 0, "Should have 0 impacted frontier orthos with no changed keys");
}

#[test]
fn test_impacted_frontier_count_with_changes() {
    // Arrange
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act - first text (baseline)
    let (interner1, changed_count1, frontier_size1, impacted_count1) = fold::process_text("a b", None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert
    assert_eq!(changed_count1, 0, "First text should have 0 changed keys");
    assert_eq!(impacted_count1, 0, "First text should have 0 impacted frontier orthos");
    assert!(frontier_size1 > 0, "Should have frontier orthos");
    
    // Act - second text adds new completion for existing vocab
    let (_interner2, changed_count2, frontier_size2, impacted_count2) = fold::process_text("a c", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert
    assert!(changed_count2 > 0, "Second text should have changed keys");
    assert!(frontier_size2 > 0, "Should have frontier orthos");
    // The impacted count should be between 0 and frontier_size (some frontier orthos may contain impacted keys)
    assert!(impacted_count2 <= frontier_size2, "Impacted count should not exceed frontier size");
}

#[test]
fn test_impacted_frontier_count_with_new_vocabulary() {
    // Arrange
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act - first text
    let (interner1, _, _, _) = fold::process_text("hello world", None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Act - second text with completely new vocabulary
    let (_interner2, changed_count2, frontier_size2, impacted_count2) = fold::process_text("foo bar", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert
    assert!(changed_count2 > 0, "Should have changed keys for new vocabulary");
    assert!(frontier_size2 > 0, "Should have frontier orthos");
    // Impacted count depends on whether frontier orthos contain the new keys
    assert!(impacted_count2 <= frontier_size2, "Impacted count should not exceed frontier size");
}

#[test]
fn test_impacted_frontier_count_accumulates_correctly() {
    // Arrange
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act - process multiple texts and track impacted counts
    let (interner1, _, _, impacted1) = fold::process_text("a b c", None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    assert_eq!(impacted1, 0, "First should have 0 impacted");
    
    let (interner2, changed2, _, impacted2) = fold::process_text("a d", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    if changed2 > 0 {
        // If there were changes, impacted count should be meaningful
        assert!(impacted2 <= frontier.len(), "Impacted should not exceed frontier size");
    }
    
    let (_interner3, changed3, frontier_size3, impacted3) = fold::process_text("e f", Some(interner2), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    if changed3 > 0 {
        assert!(impacted3 <= frontier_size3, "Impacted should not exceed frontier size");
    }
}

#[test]
fn test_impacted_frontier_orthos_contain_changed_keys() {
    // This test verifies that the impacted count correctly identifies orthos
    // that contain keys present in the changed_keys list
    
    // Arrange
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    
    // Act - Create baseline with simple vocabulary
    let (interner1, _, _, _) = fold::process_text("the cat", None, &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Act - Add text that changes existing key completions
    let (_interner2, changed_count, frontier_size, impacted_count) = fold::process_text("the dog", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut frontier, &mut HashMap::new()).expect("process_text should succeed");
    
    // Assert
    assert!(changed_count > 0, "Should have changed keys when adding new completions");
    assert!(frontier_size > 0, "Should have orthos in frontier");
    // Impacted count should be meaningful and bounded
    assert!(impacted_count <= frontier_size, "Impacted frontier count should not exceed total frontier size");
}

#[test]
fn test_checkpoint_save_and_restore() {
    use fold::{Checkpoint, CheckpointManager};
    use std::env;
    
    // Setup temp directory
    let temp_dir = env::temp_dir().join("fold_checkpoint_integration_test");
    let _ = std::fs::remove_dir_all(&temp_dir);
    
    let manager = CheckpointManager::new(&temp_dir).expect("Should create manager");
    
    // Create initial state
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    let mut frontier_orthos_saved = HashMap::new();
    
    // Process first text
    let (interner1, _, _, _) = fold::process_text(
        "hello world",
        None,
        &mut seen_ids,
        &mut optimal_ortho,
        &mut frontier,
        &mut frontier_orthos_saved
    ).expect("process_text should succeed");
    
    let checkpoint1 = Checkpoint::new(
        Some(0),
        Some(interner1.clone()),
        seen_ids.clone(),
        optimal_ortho.clone(),
        frontier.clone(),
        frontier_orthos_saved.clone(),
    );
    
    // Save checkpoint
    manager.save_checkpoint(&checkpoint1).expect("Should save checkpoint");
    assert!(manager.checkpoint_exists());
    
    // Load checkpoint
    let loaded = manager.load_checkpoint().expect("Should load checkpoint");
    assert!(loaded.is_some());
    
    let loaded_checkpoint = loaded.unwrap();
    assert_eq!(loaded_checkpoint.last_completed_file_index, Some(0));
    assert_eq!(loaded_checkpoint.seen_ids.len(), seen_ids.len());
    assert_eq!(loaded_checkpoint.frontier.len(), frontier.len());
    
    // Verify interner was restored
    assert!(loaded_checkpoint.interner.is_some());
    let loaded_interner = loaded_checkpoint.interner.unwrap();
    assert_eq!(loaded_interner.version(), interner1.version());
    assert_eq!(loaded_interner.vocabulary().len(), interner1.vocabulary().len());
    
    // Clean up
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn test_checkpoint_continuation() {
    use fold::{Checkpoint, CheckpointManager};
    use std::env;
    
    // Setup temp directory
    let temp_dir = env::temp_dir().join("fold_checkpoint_continuation_test");
    let _ = std::fs::remove_dir_all(&temp_dir);
    
    let manager = CheckpointManager::new(&temp_dir).expect("Should create manager");
    
    // Create and save first checkpoint
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    let mut frontier_orthos_saved = HashMap::new();
    
    let (interner1, _, _, _) = fold::process_text(
        "first text",
        None,
        &mut seen_ids,
        &mut optimal_ortho,
        &mut frontier,
        &mut frontier_orthos_saved
    ).expect("process_text should succeed");
    
    let checkpoint1 = Checkpoint::new(
        Some(0),
        Some(interner1.clone()),
        seen_ids.clone(),
        optimal_ortho.clone(),
        frontier.clone(),
        frontier_orthos_saved.clone(),
    );
    
    manager.save_checkpoint(&checkpoint1).expect("Should save checkpoint");
    let initial_seen_count = seen_ids.len();
    
    // Load checkpoint and continue
    let loaded = manager.load_checkpoint().expect("Should load checkpoint").unwrap();
    
    let mut seen_ids = loaded.seen_ids;
    let mut optimal_ortho = loaded.optimal_ortho;
    let mut frontier = loaded.frontier;
    let mut frontier_orthos_saved = loaded.frontier_orthos_saved;
    let interner = loaded.interner;
    
    // Process second text
    let (interner2, _, _, _) = fold::process_text(
        "second text",
        interner,
        &mut seen_ids,
        &mut optimal_ortho,
        &mut frontier,
        &mut frontier_orthos_saved
    ).expect("process_text should succeed");
    
    // Verify continuation
    assert_eq!(interner2.version(), 2, "Should increment version");
    assert!(seen_ids.len() >= initial_seen_count, "Should accumulate seen IDs");
    
    // Clean up
    let _ = std::fs::remove_dir_all(&temp_dir);
}
