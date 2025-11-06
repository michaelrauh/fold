use std::collections::{HashMap, HashSet};
use fold::ortho::Ortho;

fn main() {
    let mut seen_ids = HashSet::new();
    let mut optimal_ortho: Option<Ortho> = None;
    let mut frontier = HashSet::new();
    let mut frontier_orthos_saved = HashMap::new();
    
    println!("=== Demonstrating Impacted Backtracking ===\n");
    
    println!("Step 1: Processing first text 'a b c'");
    let (interner1, changed1, frontier1, impacted1) = fold::process_text(
        "a b c",
        None,
        &mut seen_ids,
        &mut optimal_ortho,
        &mut frontier,
        &mut frontier_orthos_saved
    ).expect("process_text should succeed");
    println!("  Version: {}", interner1.version());
    println!("  Changed keys: {}", changed1);
    println!("  Frontier size: {}", frontier1);
    println!("  Impacted orthos: {}", impacted1);
    println!("  Total orthos generated: {}", seen_ids.len());
    println!("  Frontier orthos saved for next iteration: {}\n", frontier_orthos_saved.len());
    
    let orthos_after_first = seen_ids.len();
    
    println!("Step 2: Processing second text 'a d' (adds new completion for prefix 'a')");
    let (interner2, changed2, frontier2, impacted2) = fold::process_text(
        "a d",
        Some(interner1),
        &mut seen_ids,
        &mut optimal_ortho,
        &mut frontier,
        &mut frontier_orthos_saved
    ).expect("process_text should succeed");
    println!("  Version: {}", interner2.version());
    println!("  Changed keys: {}", changed2);
    println!("  Frontier size: {}", frontier2);
    println!("  Rewound orthos added to queue: {}", impacted2);
    println!("  Total orthos generated: {}", seen_ids.len());
    println!("  New orthos from backtracking: {}", seen_ids.len() - orthos_after_first);
    println!("  Frontier orthos saved for next iteration: {}\n", frontier_orthos_saved.len());
    
    println!("=== Summary ===");
    println!("Impacted backtracking rewound {} frontier orthos and added them to the work queue.", impacted2);
    println!("This allowed the system to explore {} new ortho paths that were made possible", 
             seen_ids.len() - orthos_after_first);
    println!("by the changed completion sets in the interner.");
}
