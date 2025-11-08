use fold::ortho::Ortho;
use fold::SeenTracker;
use fold::disk_queue::DiskQueue;
use std::sync::atomic::{AtomicUsize, Ordering};

// Use atomic counter to generate unique test directories
static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn create_test_tracker() -> SeenTracker {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let temp_dir = std::env::temp_dir().join(format!("fold_test_{}", id));
    SeenTracker::new_with_dir(temp_dir).unwrap()
}

#[test]
fn test_simple_worker_loop() {
    // Arrange
    let text = "the quick brown fox";
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    
    // Act
    let (interner, _seeded) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, |_,_,_,_,_,_,_| {}).unwrap();
    
    // Assert
    assert_eq!(interner.version(), 1, "Should create version 1");
    assert!(seen_ids.len() > 0, "Should have generated at least one ortho");
    assert!(optimal_ortho.is_some(), "Should have an optimal ortho");
}

#[test]
fn test_multiple_file_processing() {
    // Arrange
    let texts = vec!["the cat sat", "the dog ran"];
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        let (new_interner, _seeded) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, |_,_,_,_,_,_,_| {}).unwrap();
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
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    
    // Act
    let (_interner, _seeded) = fold::process_text(text, None, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, |_,_,_,_,_,_,_| {}).unwrap();
    
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
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        let (new_interner, _seeded) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, |_,_,_,_,_,_,_| {}).unwrap();
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
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    
    // Act
    let (interner1, _) = fold::process_text("first text", None, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, |_,_,_,_,_,_,_| {}).unwrap();
    let (interner2, _) = fold::process_text("second text", Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, |_,_,_,_,_,_,_| {}).unwrap();
    let (interner3, _) = fold::process_text("third text", Some(interner2), &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, |_,_,_,_,_,_,_| {}).unwrap();
    
    // Assert
    assert_eq!(interner3.version(), 3, "Should have version 3 after processing 3 texts");
}

#[test]
fn test_seen_ids_accumulate() {
    // Arrange
    let texts = vec!["a b", "c d"];
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    let mut interner: Option<fold::interner::Interner> = None;
    
    // Act
    for text in texts {
        let (new_interner, _seeded) = fold::process_text(text, interner, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, |_,_,_,_,_,_,_| {}).unwrap();
        interner = Some(new_interner);
    }
    
    // Assert
    // Seen IDs should accumulate across all files
    assert!(seen_ids.len() > 0, "Should track orthos across all files");
}
