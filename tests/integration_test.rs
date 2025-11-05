use fold::{
    interner::{InMemoryInternerHolder, InternerHolderLike},
    ortho::Ortho,
    ortho_database::{InMemoryOrthoDatabase, OrthoDatabaseLike},
    queue::MockQueue,
};
use std::collections::VecDeque;

#[test]
fn test_simple_worker_loop() {
    // Test the basic worker loop with a simple text
    let text = "the quick brown fox";
    
    let mut interner_holder = InMemoryInternerHolder::new().unwrap();
    let mut seed_queue = MockQueue::new();
    interner_holder.add_text_with_seed(text, &mut seed_queue).unwrap();
    
    let interner = interner_holder.get_latest().unwrap();
    let version = interner.version();
    
    // Get seed ortho and set up work queue
    let mut work_queue: VecDeque<Ortho> = seed_queue.items.into();
    let mut db = InMemoryOrthoDatabase::new();
    let mut seen_ids = std::collections::HashSet::new();
    
    // Run worker loop
    let mut processed = 0;
    while let Some(ortho) = work_queue.pop_front() {
        processed += 1;
        
        let (forbidden, required) = ortho.get_requirements();
        let completions = interner.intersect(&required, &forbidden);
        
        for completion in completions {
            let children = ortho.add(completion, version);
            for child in children {
                let child_id = child.id();
                if !seen_ids.contains(&child_id) {
                    seen_ids.insert(child_id);
                    db.insert_or_update(child.clone()).unwrap();
                    work_queue.push_back(child);
                }
            }
        }
        
        // Prevent infinite loop in test
        if processed > 1000 {
            break;
        }
    }
    
    // Check that we processed some orthos
    assert!(processed > 0, "Should have processed at least one ortho");
    
    // Check that we have orthos in the database
    let total = db.len().unwrap();
    assert!(total > 0, "Should have generated at least one ortho");
    
    // Check that we can get an optimal
    let optimal = db.get_optimal().unwrap();
    assert!(optimal.is_some(), "Should have an optimal ortho");
}

#[test]
fn test_multiple_file_processing() {
    // Test processing multiple text inputs (simulating multiple files)
    let texts = vec!["the cat sat", "the dog ran"];
    
    let mut interner_holder = InMemoryInternerHolder::new().unwrap();
    let mut db = InMemoryOrthoDatabase::new();
    let mut seen_ids = std::collections::HashSet::new();
    
    for text in texts {
        let mut seed_queue = MockQueue::new();
        interner_holder.add_text_with_seed(text, &mut seed_queue).unwrap();
        
        let interner = interner_holder.get_latest().unwrap();
        let version = interner.version();
        
        let mut work_queue: VecDeque<Ortho> = seed_queue.items.into();
        
        let mut processed = 0;
        while let Some(ortho) = work_queue.pop_front() {
            processed += 1;
            
            let (forbidden, required) = ortho.get_requirements();
            let completions = interner.intersect(&required, &forbidden);
            
            for completion in completions {
                let children = ortho.add(completion, version);
                for child in children {
                    let child_id = child.id();
                    if !seen_ids.contains(&child_id) {
                        seen_ids.insert(child_id);
                        db.insert_or_update(child.clone()).unwrap();
                        work_queue.push_back(child);
                    }
                }
            }
            
            if processed > 1000 {
                break;
            }
        }
    }
    
    // Check that we processed orthos from both texts
    let total = db.len().unwrap();
    assert!(total > 0, "Should have generated orthos from both texts");
    
    // Check that vocabulary grew across versions
    let final_version = interner_holder.latest_version();
    assert_eq!(final_version, 2, "Should have created 2 versions");
    
    let final_interner = interner_holder.get_latest().unwrap();
    assert!(final_interner.vocabulary().len() > 3, "Should have accumulated vocabulary");
}

#[test]
fn test_optimal_ortho_tracking() {
    // Test that optimal ortho is correctly identified
    let text = "a b c d e";
    
    let mut interner_holder = InMemoryInternerHolder::new().unwrap();
    let mut seed_queue = MockQueue::new();
    interner_holder.add_text_with_seed(text, &mut seed_queue).unwrap();
    
    let interner = interner_holder.get_latest().unwrap();
    let version = interner.version();
    
    let mut work_queue: VecDeque<Ortho> = seed_queue.items.into();
    let mut db = InMemoryOrthoDatabase::new();
    let mut seen_ids = std::collections::HashSet::new();
    
    let mut processed = 0;
    while let Some(ortho) = work_queue.pop_front() {
        processed += 1;
        
        let (forbidden, required) = ortho.get_requirements();
        let completions = interner.intersect(&required, &forbidden);
        
        for completion in completions {
            let children = ortho.add(completion, version);
            for child in children {
                let child_id = child.id();
                if !seen_ids.contains(&child_id) {
                    seen_ids.insert(child_id);
                    db.insert_or_update(child.clone()).unwrap();
                    work_queue.push_back(child);
                }
            }
        }
        
        if processed > 1000 {
            break;
        }
    }
    
    let optimal = db.get_optimal().unwrap().unwrap();
    let volume: usize = optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
    
    // Optimal should have some volume
    assert!(volume > 0, "Optimal ortho should have positive volume");
    
    // All orthos in the DB should have volume <= optimal's volume
    let all_orthos = db.all_orthos().unwrap();
    for ortho in all_orthos {
        let ortho_volume: usize = ortho.dims().iter().map(|d| d.saturating_sub(1)).product();
        assert!(ortho_volume <= volume, "No ortho should exceed optimal volume");
    }
}
