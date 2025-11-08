use fold::ortho::Ortho;
use fold::interner::Interner;
use fold::spatial;

#[test]
fn test_spatial_index_to_coords_2x2() {
    // For a 2x2 grid, indices should map as follows:
    // Index 0 -> [0,0]  (distance 0)
    // Index 1 -> [0,1]  (distance 1)
    // Index 2 -> [1,0]  (distance 1)
    // Index 3 -> [1,1]  (distance 2)
    
    let dims = vec![2, 2];
    assert_eq!(spatial::index_to_coords(0, &dims), vec![0, 0]);
    assert_eq!(spatial::index_to_coords(1, &dims), vec![0, 1]);
    assert_eq!(spatial::index_to_coords(2, &dims), vec![1, 0]);
    assert_eq!(spatial::index_to_coords(3, &dims), vec![1, 1]);
}

#[test]
fn test_spatial_index_to_coords_3x2() {
    // For a 3x2 grid, indices should be ordered by distance (sum of coords):
    // [0,0] -> distance 0
    // [0,1], [1,0] -> distance 1
    // [1,1], [2,0] -> distance 2
    // [2,1] -> distance 3
    
    let dims = vec![3, 2];
    println!("3x2 grid spatial layout:");
    for i in 0..6 {
        let coords = spatial::index_to_coords(i, &dims);
        println!("  Index {} -> {:?}", i, coords);
    }
    
    // Verify the first few
    assert_eq!(spatial::index_to_coords(0, &dims), vec![0, 0]);
    // One of indices 1 or 2 should be [0,1], the other [1,0]
    let coords1 = spatial::index_to_coords(1, &dims);
    let coords2 = spatial::index_to_coords(2, &dims);
    assert!(coords1 == vec![0, 1] || coords1 == vec![1, 0]);
    assert!(coords2 == vec![0, 1] || coords2 == vec![1, 0]);
    assert_ne!(coords1, coords2);
}

#[test]
fn test_ortho_spatial_layout_with_known_phrases() {
    // Create ortho with tokens that form known phrases
    // Start with base ortho and add tokens
    
    let text = "do i not know";
    let interner = Interner::from_text(text);
    
    // Token indices: do=0, i=1, not=2, know=3
    
    // Create an ortho and fill it
    let mut ortho = Ortho::new();
    ortho = ortho.add(0)[0].clone(); // add "do"
    ortho = ortho.add(1)[0].clone(); // add "i"
    ortho = ortho.add(1)[0].clone(); // add "i" again
    
    // Now check the spatial coords
    let dims = ortho.dims();
    println!("\nOrtho payload layout for {:?}:", dims);
    for (idx, token) in ortho.payload().iter().enumerate() {
        let coords = spatial::index_to_coords(idx, dims);
        let token_str = token.map(|t| interner.string_for_index(t)).unwrap_or("·");
        println!("  Payload[{}] at coords {:?} = {}", idx, coords, token_str);
    }
    
    // Verify: payload[0] should be at [0,0], which is "do"
    assert_eq!(spatial::index_to_coords(0, dims), vec![0, 0]);
    assert_eq!(ortho.payload()[0], Some(0)); // "do"
    
    // payload[1] should be at [0,1]
    assert_eq!(spatial::index_to_coords(1, dims), vec![0, 1]);
    
    // payload[2] should be at [1,0]
    assert_eq!(spatial::index_to_coords(2, dims), vec![1, 0]);
    
    // payload[3] should be at [1,1]
    assert_eq!(spatial::index_to_coords(3, dims), vec![1, 1]);
}

#[test]
fn test_3x3_ortho_reading() {
    // For a 3x3 ortho - just explore the spatial layout
    
    let text = "do i not know believe that but";
    let _interner = Interner::from_text(text);
    
    let dims = vec![3, 3];
    
    // Print out the distance-ordered indices
    println!("\n3x3 grid spatial layout:");
    for i in 0..9 {
        let coords = spatial::index_to_coords(i, &dims);
        let distance: usize = coords.iter().sum();
        println!("  Index {} -> {:?} (distance {})", i, coords, distance);
    }
    
    // Manually check a few key positions
    assert_eq!(spatial::index_to_coords(0, &dims), vec![0, 0]); // distance 0
}
