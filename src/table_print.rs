use crate::ortho::Ortho;
use crate::interner::Interner;
use crate::spatial;

/// Print ortho as a nice table
pub fn print_ortho_table(ortho: &Ortho, interner: &Interner) {
    let dims = ortho.dims();
    let payload = ortho.payload();
    
    // Convert payload indices to strings
    let tokens: Vec<String> = payload
        .iter()
        .map(|&opt_idx| {
            opt_idx
                .map(|idx| interner.string_for_index(idx).to_string())
                .unwrap_or_else(|| "·".to_string()) // Use middle dot for empty cells
        })
        .collect();
    
    if dims.len() < 2 {
        // Should not happen according to requirements, but handle gracefully
        println!("  Tokens: {}", tokens.join(" "));
        return;
    }
    
    // Find maximum token width for proper alignment
    let max_width = tokens.iter().map(|s| s.len()).max().unwrap_or(1).max(3);
    
    if dims.len() == 2 {
        // 2D ortho: print as a simple table
        print_2d_table_spatial(&tokens, dims, max_width, "  ");
    } else {
        // 3D and higher: tile 2D slices
        print_nd_table_spatial(&tokens, dims, max_width);
    }
}

/// Get the spatial coordinate for a linear index in the payload
fn get_spatial_coords(index: usize, dims: &[usize]) -> Vec<usize> {
    // We need to figure out which coordinate this index corresponds to
    // The payload is stored in "distance order" - we need to reverse this
    // For now, let's use spatial module to get the mapping
    spatial::index_to_coords(index, dims)
}

fn print_2d_table_spatial(tokens: &[String], dims: &[usize], max_width: usize, indent: &str) {
    let rows = dims[0];
    let cols = dims[1];
    
    // Build a 2D grid from the spatial layout
    let mut grid: Vec<Vec<String>> = vec![vec!["·".to_string(); cols]; rows];
    
    for (linear_idx, token) in tokens.iter().enumerate() {
        let coords = get_spatial_coords(linear_idx, dims);
        if coords.len() == 2 {
            let row = coords[0];
            let col = coords[1];
            if row < rows && col < cols {
                grid[row][col] = token.clone();
            }
        }
    }
    
    // Print the grid
    for row in 0..rows {
        print!("{}", indent);
        for col in 0..cols {
            print!("{:width$}", grid[row][col], width = max_width);
            if col < cols - 1 {
                print!(" │ ");
            }
        }
        println!();
        
        // Print separator line between rows (except after last row)
        if row < rows - 1 {
            print!("{}", indent);
            for col in 0..cols {
                print!("{}", "─".repeat(max_width));
                if col < cols - 1 {
                    print!("─┼─");
                }
            }
            println!();
        }
    }
}

fn print_nd_table_spatial(tokens: &[String], dims: &[usize], max_width: usize) {
    if dims.len() == 2 {
        print_2d_table_spatial(tokens, dims, max_width, "  ");
        return;
    }
    
    // For N-dimensional (N >= 3), we treat it as a collection of 2D slices
    let rows = dims[dims.len() - 2];
    let cols = dims[dims.len() - 1];
    let slice_size = rows * cols;
    
    // Calculate number of slices (product of all dimensions except last two)
    let num_slices: usize = dims[..dims.len() - 2].iter().product();
    
    // Build grids for each slice
    for slice_idx in 0..num_slices {
        let coords = linear_to_coords(slice_idx, &dims[..dims.len() - 2]);
        
        // Print slice header
        print!("  [");
        for (i, &coord) in coords.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("{}", coord);
        }
        println!(", :, :]");
        
        // Build this 2D slice from spatial coordinates
        let mut grid: Vec<Vec<String>> = vec![vec!["·".to_string(); cols]; rows];
        
        for (linear_idx, token) in tokens.iter().enumerate() {
            let full_coords = get_spatial_coords(linear_idx, dims);
            if full_coords.len() == dims.len() {
                // Check if this belongs to the current slice
                let slice_matches = coords.iter().enumerate().all(|(i, &c)| full_coords[i] == c);
                if slice_matches {
                    let row = full_coords[full_coords.len() - 2];
                    let col = full_coords[full_coords.len() - 1];
                    if row < rows && col < cols {
                        grid[row][col] = token.clone();
                    }
                }
            }
        }
        
        // Print the grid
        for row in 0..rows {
            print!("    ");
            for col in 0..cols {
                print!("{:width$}", grid[row][col], width = max_width);
                if col < cols - 1 {
                    print!(" │ ");
                }
            }
            println!();
            
            if row < rows - 1 {
                print!("    ");
                for col in 0..cols {
                    print!("{}", "─".repeat(max_width));
                    if col < cols - 1 {
                        print!("─┼─");
                    }
                }
                println!();
            }
        }
        
        if slice_idx < num_slices - 1 {
            println!();
        }
    }
}

// Convert linear index to multi-dimensional coordinates
fn linear_to_coords(mut linear_idx: usize, dims: &[usize]) -> Vec<usize> {
    let mut coords = vec![0; dims.len()];
    for i in (0..dims.len()).rev() {
        coords[i] = linear_idx % dims[i];
        linear_idx /= dims[i];
    }
    coords
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interner::Interner;
    
    #[test]
    fn test_2d_ortho_spatial_layout() {
        // Create an ortho with known structure
        // For a 3x3 grid, if we fill it in order: a, b, c, d, e, f, g, h, i
        // Reading left-to-right: "a b c", "d e f", "g h i"
        // Reading top-to-bottom: "a d g", "b e h", "c f i"
        
        // First create an interner with the words we need
        let text = "do i not believe know that but";
        let interner = Interner::from_text(text);
        
        // Create a 3x3 ortho manually to test
        let ortho = Ortho::new(1);
        
        // We need to understand: if we have indices [0, 1, 2, 3, 4, 5, 6, 7, 8]
        // what spatial coordinates do they map to in a 3x3 grid?
        
        // Let's use the spatial module to figure this out
        let dims = vec![3, 3];
        for i in 0..9 {
            let coords = get_spatial_coords(i, &dims);
            println!("Index {} -> coords {:?}", i, coords);
        }
        
        // This test will fail but help us understand the mapping
        assert!(false, "Intentional fail to see output");
    }
}
