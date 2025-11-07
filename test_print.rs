use fold::ortho::Ortho;
use fold::interner::Interner;
use fold::spatial;

fn main() {
    // Test with a 4x3 ortho to match the output we're seeing
    let text = "do i not believe know that but";
    let interner = Interner::from_text(text);
    
    println!("Token indices:");
    for (idx, token) in interner.vocabulary().iter().enumerate() {
        println!("  {} -> {}", idx, token);
    }
    
    println!("\n=== Testing 4x3 spatial layout ===");
    let dims = vec![4, 3];
    println!("For a 4x3 grid, showing payload index -> coords mapping:");
    for idx in 0..12 {
        let coords = spatial::index_to_coords(idx, &dims);
        let distance: usize = coords.iter().sum();
        println!("  payload[{:2}] -> coords {:?} (distance {})", idx, coords, distance);
    }
    
    println!("\n=== Axis positions for 4x3 ===");
    let axis_positions = spatial::get_axis_positions(&dims);
    println!("Axis positions (payload indices): {:?}", axis_positions);
    
    println!("\n=== How the table SHOULD be constructed ===");
    println!("For each row and column, show which payload index goes there:");
    for row in 0..4 {
        print!("Row {}: ", row);
        for col in 0..3 {
            // Find which payload index maps to [row, col]
            for idx in 0..12 {
                let coords = spatial::index_to_coords(idx, &dims);
                if coords == vec![row, col] {
                    print!("payload[{:2}] ", idx);
                    break;
                }
            }
        }
        println!();
    }
}
