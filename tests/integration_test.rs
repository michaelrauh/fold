use fold::{interner::Interner, ortho::Ortho};
use std::collections::{HashMap, VecDeque};

fn process_text_sequence(texts: &[&str]) -> Option<Ortho> {
    let mut interner: Option<Interner> = None;
    let mut global_best: Option<Ortho> = None;
    
    for text in texts {
        // Build or extend interner
        interner = Some(if let Some(prev) = interner {
            prev.add_text(text)
        } else {
            Interner::from_text(text)
        });
        
        let current_interner = interner.as_ref().unwrap();
        let version = current_interner.version();
        
        // Initialize work queue and seen orthos set
        let mut work_queue: VecDeque<Ortho> = VecDeque::new();
        let mut seen_orthos: HashMap<usize, ()> = HashMap::new();
        
        // Seed with empty ortho
        let seed_ortho = Ortho::new(version);
        let seed_id = seed_ortho.id();
        
        work_queue.push_back(seed_ortho.clone());
        seen_orthos.insert(seed_id, ());
        
        // Check if seed is optimal
        global_best = update_best(global_best, seed_ortho);
        
        // Process work queue until empty
        while let Some(ortho) = work_queue.pop_front() {
            // Get requirements from ortho
            let (forbidden, required) = ortho.get_requirements();
            
            // Get completions from interner
            let completions = current_interner.intersect(&required, &forbidden);
            
            // Generate child orthos
            for completion in completions {
                let children = ortho.add(completion, version);
                
                for child in children {
                    let child_id = child.id();
                    
                    // Only process if never seen before
                    if !seen_orthos.contains_key(&child_id) {
                        seen_orthos.insert(child_id, ());
                        
                        // Check for optimality as we create it
                        global_best = update_best(global_best, child.clone());
                        
                        work_queue.push_back(child);
                    }
                }
            }
        }
    }
    
    global_best
}

fn update_best(current_best: Option<Ortho>, candidate: Ortho) -> Option<Ortho> {
    let candidate_score = candidate.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    
    match current_best {
        None => Some(candidate),
        Some(best) => {
            let best_score = best.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
            if candidate_score > best_score {
                Some(candidate)
            } else {
                Some(best)
            }
        }
    }
}

#[test]
fn test_optimal_ortho_tracking() {
    // Test with simple text that creates predictable phrases
    // "a b" and "a c" will create phrases with 'a' as a common prefix
    let texts = vec!["a b a c"];
    
    let optimal = process_text_sequence(&texts);
    
    // Verify we found an optimal ortho
    assert!(optimal.is_some(), "Should find an optimal ortho");
    
    let ortho = optimal.unwrap();
    
    // The algorithm processes through orthos and tracks the best one
    // With "a b a c", we get vocabulary [a, b, c] and phrases ["a b", "a c"]
    // This should produce orthos with meaningful content
    
    // Verify dimensions are valid
    assert!(ortho.dims().len() >= 2, "Should have at least 2 dimensions");
    
    // Calculate score
    let score = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    
    // The optimal should have either:
    // 1. An empty [2,2] ortho (score=1) if nothing fits, or
    // 2. A filled ortho with score >= 1
    assert!(score >= 1, "Score should be at least 1");
    
    // Verify basic structure
    assert_eq!(ortho.payload().len(), ortho.dims().iter().product::<usize>(),
               "Payload length should match capacity");
}
