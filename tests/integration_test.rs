use fold::ortho::Ortho;
use std::collections::HashSet;

#[test]
fn test_simple_worker_loop() {
    // Arrange
    let text = "the quick brown fox";
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    
    // Act
    let interner = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho);
    
    // Assert
    assert_eq!(interner.version(), 1, "Should create version 1");
    assert!(seen_ids.len() > 0, "Should have generated at least one ortho");
    assert!(optimal_ortho.is_some(), "Should have an optimal ortho");
}

#[test]
fn test_multiple_file_processing() {
    // Arrange
    let texts = vec!["the cat sat", "the dog ran"];
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        interner = Some(fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho));
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
    
    // Act
    let _interner = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho);
    
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
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        interner = Some(fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho));
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
    
    // Act
    let interner1 = fold::process_text("first text", None, &mut seen_ids, &mut optimal_ortho);
    let interner2 = fold::process_text("second text", Some(interner1), &mut seen_ids, &mut optimal_ortho);
    let interner3 = fold::process_text("third text", Some(interner2), &mut seen_ids, &mut optimal_ortho);
    
    // Assert
    assert_eq!(interner3.version(), 3, "Should have version 3 after processing 3 texts");
}

#[test]
fn test_seen_ids_accumulate() {
    // Arrange
    let texts = vec!["a b", "c d"];
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        interner = Some(fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho));
    }
    
    // Assert
    // Seen IDs should accumulate across all files
    assert!(seen_ids.len() > 0, "Should track orthos across all files");
}
