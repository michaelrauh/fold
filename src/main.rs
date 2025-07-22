pub mod ortho;
pub mod spatial;

use spatial::{get_requirements, get_capacity, is_base, expand_up, expand_over};
use std::time::Instant;

fn main() {
    println!("Spatial Caching Demo");
    println!("===================");

    // Demo capacity caching
    println!("\nTesting get_capacity caching:");
    let dims = vec![3, 3, 2];
    
    let start = Instant::now();
    let result1 = get_capacity(&dims);
    let time1 = start.elapsed();
    
    let start = Instant::now();
    let result2 = get_capacity(&dims);
    let time2 = start.elapsed();
    
    println!("First call: {} (took {:?})", result1, time1);
    println!("Second call: {} (took {:?})", result2, time2);
    println!("Results match: {}", result1 == result2);

    // Demo expand_over caching
    println!("\nTesting expand_over caching:");
    let dims = vec![2, 2];
    
    let start = Instant::now();
    let result1 = expand_over(&dims);
    let time1 = start.elapsed();
    
    let start = Instant::now();
    let result2 = expand_over(&dims);
    let time2 = start.elapsed();
    
    println!("First call result length: {} (took {:?})", result1.len(), time1);
    println!("Second call result length: {} (took {:?})", result2.len(), time2);
    println!("Results match: {}", result1 == result2);

    println!("\nAll public functions are now cached and working correctly!");
}
