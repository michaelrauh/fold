use fold::ortho::Ortho;
use fold::SeenTracker;
use fold::disk_queue::DiskQueue;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// Use atomic counter to generate unique test directories
static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn create_test_tracker() -> SeenTracker {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let temp_dir = std::env::temp_dir().join(format!("fold_test_{}", id));
    SeenTracker::new_with_dir(temp_dir).unwrap()
}

// Helper for test callbacks - accepts all 23 parameters but does nothing
fn noop_callback(_q: usize, _s: usize, _bh: usize, _bm: usize, _bfp: usize, _sch: usize, _dc: usize, _qm: usize, _qd: usize, 
                  _wwr: f64, _wrr: f64, _rwr: f64, _ws: u64, _wp: usize, _wst: f64, 
                  _wl: u64, _wlt: f64, _rs: u64, _rp: usize, _rst: f64, _rl: u64, _rlt: f64, _o: &Option<Ortho>) {}

#[test]
fn test_simple_worker_loop() {
    // Arrange
    let text = "the quick brown fox";
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    
    // Act
    let interner = Arc::new(fold::interner::Interner::from_text(text));
    let _seeded = fold::process_text(interner.clone(), None, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
    
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
    let mut interner: Option<Arc<fold::interner::Interner>> = None;
    
    // Act
    for text in texts {
        let prev_interner = interner.clone();
        let current_interner = if let Some(prev) = interner {
            Arc::new(Arc::unwrap_or_clone(prev).add_text(text))
        } else {
            Arc::new(fold::interner::Interner::from_text(text))
        };
        let _seeded = fold::process_text(current_interner.clone(), prev_interner, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
        interner = Some(current_interner);
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
    let text = "hello world test example";
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    
    // Act
    let interner = Arc::new(fold::interner::Interner::from_text(text));
    let _seeded = fold::process_text(interner, None, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
    
    // Assert
    // Just verify that we tracked some optimal ortho
    assert!(optimal_ortho.is_some(), "Should have found at least one ortho");
}

#[test]
fn test_multiple_texts_accumulate_vocabulary() {
    // Arrange
    let texts = vec![
        "the quick brown fox jumps over the lazy dog",
        "the cat sat on the mat"
    ];
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new(); // Use in-memory queue for tests
    let mut interner: Option<Arc<fold::interner::Interner>> = None;
    
    // Act
    for text in texts {
        let prev_interner = interner.clone();
        let current_interner = if let Some(prev) = interner {
            Arc::new(Arc::unwrap_or_clone(prev).add_text(text))
        } else {
            Arc::new(fold::interner::Interner::from_text(text))
        };
        let _seeded = fold::process_text(current_interner.clone(), prev_interner, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
        interner = Some(current_interner);
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
    let interner1 = Arc::new(fold::interner::Interner::from_text("first text"));
    let _ = fold::process_text(interner1.clone(), None, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
    
    let interner2 = Arc::new(Arc::unwrap_or_clone(interner1.clone()).add_text("second text"));
    let _ = fold::process_text(interner2.clone(), Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
    
    let interner3 = Arc::new(Arc::unwrap_or_clone(interner2.clone()).add_text("third text"));
    let _ = fold::process_text(interner3.clone(), Some(interner2), &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
    
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
    let mut interner: Option<Arc<fold::interner::Interner>> = None;
    
    // Act
    for text in texts {
        let prev_interner = interner.clone();
        let current_interner = if let Some(prev) = interner {
            Arc::new(Arc::unwrap_or_clone(prev).add_text(text))
        } else {
            Arc::new(fold::interner::Interner::from_text(text))
        };
        let _seeded = fold::process_text(current_interner.clone(), prev_interner, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
        interner = Some(current_interner);
    }
    
    // Assert
    // Seen IDs should accumulate across all files
    assert!(seen_ids.len() > 0, "Should track orthos across all files");
}

#[test]
fn test_revisit_seeding_count() {
    // This test verifies that when processing a second file with new vocabulary,
    // the seeded count includes orthos from the first file that need revisiting
    
    // Arrange
    let mut seen_ids = create_test_tracker();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut ortho_storage = DiskQueue::new_persistent().unwrap();
    
    // First file: "cat dog"
    let text1 = "cat dog";
    let interner1 = Arc::new(fold::interner::Interner::from_text(text1));
    let seeded1 = fold::process_text(interner1.clone(), None, &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
    
    println!("File 1 seeded: {}", seeded1);
    println!("File 1 ortho_storage len after: {}", ortho_storage.len());
    assert_eq!(seeded1, 1, "First file should seed just the blank ortho");
    
    // Second file: "cab dab" (shares prefixes 'c' and 'd' with first file words)
    let text2 = "cab dab";
    let interner2 = Arc::new(Arc::unwrap_or_clone(interner1.clone()).add_text(text2));
    let seeded2 = fold::process_text(interner2.clone(), Some(interner1), &mut seen_ids, &mut optimal_ortho, &mut ortho_storage, noop_callback).unwrap();
    
    println!("File 2 seeded: {}", seeded2);
    println!("Interner1 version: {}, Interner2 version: {}", 1, interner2.version());
    
    // Assert: Second file should seed the blank ortho (1) PLUS orthos from first file that could use new tokens
    // "cab" and "dab" are new tokens that share prefixes with "cat" and "dog"
    assert!(seeded2 > 1, "Second file should seed blank ortho plus revisit orthos from first file, got {}", seeded2);
}

