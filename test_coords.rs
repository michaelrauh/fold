fn main() {
    // Simulate [4,2] spatial layout
    let dims = vec![4, 2];
    println!("For dims {:?}:", dims);
    println!("Position 7 would be the 8th element (capacity = {})", dims.iter().product::<usize>());
    
    // This is the "next" position after 7 filled
    println!("After filling positions 0-6, position 7 is next");
    println!("In distance order for [4,2], position 7 might be coord [3,1] or similar");
}
