use fold::ortho::Ortho;
use std::collections::{HashSet, VecDeque};

const MAX_TEST_ITERATIONS: usize = 1000;

#[test]
fn test_simple_worker_loop() {
    // Test the basic worker loop with a simple text
    let text = "the quick brown fox";
    
    let interner = fold::interner::Interner::from_text(text);
    let version = interner.version();
    
    // Create seed ortho and work queue
    let seed_ortho = Ortho::new(version);
    let mut work_queue: VecDeque<Ortho> = VecDeque::new();
    work_queue.push_back(seed_ortho);
    
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    
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
                    
                    // Check if this child is optimal
                    let child_volume: usize = child.dims().iter().map(|d| d.saturating_sub(1)).product();
                    let is_optimal = if let Some(ref current_optimal) = optimal_ortho {
                        let current_volume: usize = current_optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
                        child_volume > current_volume
                    } else {
                        true
                    };
                    
                    if is_optimal {
                        optimal_ortho = Some(child.clone());
                    }
                    
                    work_queue.push_back(child);
                }
            }
        }
        
        // Prevent infinite loop in test
        if processed > MAX_TEST_ITERATIONS {
            break;
        }
    }
    
    // Check that we processed some orthos
    assert!(processed > 0, "Should have processed at least one ortho");
    
    // Check that we generated orthos
    assert!(seen_ids.len() > 0, "Should have generated at least one ortho");
    
    // Check that we have an optimal
    assert!(optimal_ortho.is_some(), "Should have an optimal ortho");
}

#[test]
fn test_multiple_file_processing() {
    // Test processing multiple text inputs (simulating multiple files)
    let texts = vec!["the cat sat", "the dog ran"];
    
    let mut interner: Option<fold::interner::Interner> = None;
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    
    for text in texts {
        // Build or update interner
        interner = Some(if let Some(prev_interner) = interner {
            prev_interner.add_text(text)
        } else {
            fold::interner::Interner::from_text(text)
        });
        
        let current_interner = interner.as_ref().unwrap();
        let version = current_interner.version();
        
        // Create seed ortho and work queue
        let seed_ortho = Ortho::new(version);
        let mut work_queue: VecDeque<Ortho> = VecDeque::new();
        work_queue.push_back(seed_ortho);
        
        let mut processed = 0;
        while let Some(ortho) = work_queue.pop_front() {
            processed += 1;
            
            let (forbidden, required) = ortho.get_requirements();
            let completions = current_interner.intersect(&required, &forbidden);
            
            for completion in completions {
                let children = ortho.add(completion, version);
                for child in children {
                    let child_id = child.id();
                    if !seen_ids.contains(&child_id) {
                        seen_ids.insert(child_id);
                        
                        // Check if this child is optimal
                        let child_volume: usize = child.dims().iter().map(|d| d.saturating_sub(1)).product();
                        let is_optimal = if let Some(ref current_optimal) = optimal_ortho {
                            let current_volume: usize = current_optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
                            child_volume > current_volume
                        } else {
                            true
                        };
                        
                        if is_optimal {
                            optimal_ortho = Some(child.clone());
                        }
                        
                        work_queue.push_back(child);
                    }
                }
            }
            
            if processed > MAX_TEST_ITERATIONS {
                break;
            }
        }
    }
    
    // Check that we processed orthos from both texts
    assert!(seen_ids.len() > 0, "Should have generated orthos from both texts");
    
    // Check that vocabulary grew across versions
    let final_interner = interner.as_ref().unwrap();
    assert_eq!(final_interner.version(), 2, "Should have created 2 versions");
    assert!(final_interner.vocabulary().len() > 3, "Should have accumulated vocabulary");
}

#[test]
fn test_optimal_ortho_tracking() {
    // Test that optimal ortho is correctly identified
    let text = "a b c d e";
    
    let interner = fold::interner::Interner::from_text(text);
    let version = interner.version();
    
    let seed_ortho = Ortho::new(version);
    let mut work_queue: VecDeque<Ortho> = VecDeque::new();
    work_queue.push_back(seed_ortho);
    
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut all_orthos = Vec::new();
    
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
                    
                    // Track all orthos for verification
                    all_orthos.push(child.clone());
                    
                    // Check if this child is optimal
                    let child_volume: usize = child.dims().iter().map(|d| d.saturating_sub(1)).product();
                    let is_optimal = if let Some(ref current_optimal) = optimal_ortho {
                        let current_volume: usize = current_optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
                        child_volume > current_volume
                    } else {
                        true
                    };
                    
                    if is_optimal {
                        optimal_ortho = Some(child.clone());
                    }
                    
                    work_queue.push_back(child);
                }
            }
        }
        
        if processed > MAX_TEST_ITERATIONS {
            break;
        }
    }
    
    let optimal = optimal_ortho.unwrap();
    let volume: usize = optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
    
    // Optimal should have some volume
    assert!(volume > 0, "Optimal ortho should have positive volume");
    
    // All orthos should have volume <= optimal's volume
    for ortho in all_orthos {
        let ortho_volume: usize = ortho.dims().iter().map(|d| d.saturating_sub(1)).product();
        assert!(ortho_volume <= volume, "No ortho should exceed optimal volume");
    }
}

#[test]
fn test_end_to_end_run_pattern() {
    // Test the complete end-to-end pattern as used in main
    let texts = vec![
        "the quick brown fox jumps over the lazy dog",
        "the cat sat on the mat"
    ];
    
    let mut interner: Option<fold::interner::Interner> = None;
    let mut optimal_ortho: Option<Ortho> = None;
    let mut seen_ids = HashSet::new();
    
    // Process each "file"
    for text in texts {
        // Build or update interner
        interner = Some(if let Some(prev_interner) = interner {
            prev_interner.add_text(text)
        } else {
            fold::interner::Interner::from_text(text)
        });
        
        let current_interner = interner.as_ref().unwrap();
        let version = current_interner.version();
        
        // Create seed ortho and work queue
        let seed_ortho = Ortho::new(version);
        let mut work_queue: VecDeque<Ortho> = VecDeque::new();
        work_queue.push_back(seed_ortho);
        
        // Worker loop
        let mut processed = 0;
        while let Some(ortho) = work_queue.pop_front() {
            processed += 1;
            
            let (forbidden, required) = ortho.get_requirements();
            let completions = current_interner.intersect(&required, &forbidden);
            
            for completion in completions {
                let children = ortho.add(completion, version);
                for child in children {
                    let child_id = child.id();
                    if !seen_ids.contains(&child_id) {
                        seen_ids.insert(child_id);
                        
                        // Check if this child is optimal
                        let child_volume: usize = child.dims().iter().map(|d| d.saturating_sub(1)).product();
                        let is_optimal = if let Some(ref current_optimal) = optimal_ortho {
                            let current_volume: usize = current_optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
                            child_volume > current_volume
                        } else {
                            true
                        };
                        
                        if is_optimal {
                            optimal_ortho = Some(child.clone());
                        }
                        
                        work_queue.push_back(child);
                    }
                }
            }
            
            if processed > MAX_TEST_ITERATIONS {
                break;
            }
        }
    }
    
    // Verify we have results
    assert!(interner.is_some(), "Should have built interner");
    assert!(optimal_ortho.is_some(), "Should have found optimal ortho");
    assert!(seen_ids.len() > 0, "Should have generated orthos");
    
    // Verify final state
    let final_interner = interner.unwrap();
    assert_eq!(final_interner.version(), 2, "Should have 2 versions from 2 texts");
    assert!(final_interner.vocabulary().len() > 8, "Should have accumulated vocabulary from both texts");
    
    let optimal = optimal_ortho.unwrap();
    let volume: usize = optimal.dims().iter().map(|d| d.saturating_sub(1)).product();
    assert!(volume > 0, "Optimal should have positive volume");
}
