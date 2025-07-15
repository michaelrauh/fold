#[derive(Debug, Clone)]
pub struct Ortho {
    version: u64,
    storage: Vec<u16>,
    dimensions: Vec<u16>,
}

impl Ortho {
    pub fn new(version: u64) -> Self {
        // Use minimum dimensions [2,2] for orthogonality
        Ortho { 
            version,
            storage: Vec::new(),
            dimensions: vec![2, 2],
        }
    }
    
    pub fn with_dimensions(version: u64, dimensions: Vec<u16>) -> Self {
        // Ensure minimum dimensions are at least [2,2]
        let dims = if dimensions.len() < 2 || dimensions.iter().any(|&d| d < 2) {
            vec![2, 2]
        } else {
            dimensions
        };
        
        Ortho {
            version,
            storage: Vec::new(),
            dimensions: dims,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }
    
    /// Generate all logical coordinates sorted by shell (sum) then by components
    fn generate_logical_coordinates(&self) -> Vec<Vec<u16>> {
        let mut coords = Vec::new();
        
        // Generate Cartesian product of all dimension ranges
        self.cartesian_product(&mut vec![], 0, &mut coords);
        
        // Sort by shell (sum of coordinates) first, then by components
        coords.sort_by(|a, b| {
            let sum_a: u16 = a.iter().sum();
            let sum_b: u16 = b.iter().sum();
            sum_a.cmp(&sum_b).then_with(|| a.cmp(b))
        });
        
        coords
    }
    
    fn cartesian_product(&self, current: &mut Vec<u16>, dim_index: usize, result: &mut Vec<Vec<u16>>) {
        if dim_index == self.dimensions.len() {
            result.push(current.clone());
            return;
        }
        
        for i in 0..self.dimensions[dim_index] {
            current.push(i);
            self.cartesian_product(current, dim_index + 1, result);
            current.pop();
        }
    }
    
    /// Get the current logical coordinate based on storage length
    fn get_current_logical_coordinate(&self) -> Option<Vec<u16>> {
        if self.storage.is_empty() {
            return None;
        }
        
        let logical_coords = self.generate_logical_coordinates();
        let index = self.storage.len() - 1;
        
        if index < logical_coords.len() {
            Some(logical_coords[index].clone())
        } else {
            None
        }
    }
    
    /// Get the current shell (sum of logical coordinates)
    fn get_current_shell(&self) -> u16 {
        self.get_current_logical_coordinate()
            .map(|coords| coords.iter().sum())
            .unwrap_or(0)
    }

    pub(crate) fn get_required_and_forbidden(&self) -> (Vec<Vec<u16>>, Vec<u16>) {
        let required = self.get_required();
        let forbidden = self.get_forbidden();
        (required, forbidden)
    }
    
    fn get_forbidden(&self) -> Vec<u16> {
        if self.storage.is_empty() {
            return Vec::new();
        }
        
        let current_shell = self.get_current_shell();
        let logical_coords = self.generate_logical_coordinates();
        let current_index = self.storage.len() - 1;
        let mut forbidden = Vec::new();
        
        for (index, stored_value) in self.storage.iter().enumerate() {
            // Skip the current position
            if index == current_index {
                continue;
            }
            
            if index < logical_coords.len() {
                let coords = &logical_coords[index];
                let shell: u16 = coords.iter().sum();
                if shell == current_shell {
                    forbidden.push(*stored_value);
                }
            }
        }
        
        forbidden
    }
    
    fn get_required(&self) -> Vec<Vec<u16>> {
        let current_logical = match self.get_current_logical_coordinate() {
            Some(coords) => coords,
            None => return Vec::new(),
        };
        
        let logical_coords = self.generate_logical_coordinates();
        let mut required = Vec::new();
        
        // For each axis, get all values from edge (0) to current position (exclusive)
        for axis in 0..self.dimensions.len() {
            let mut axis_values = Vec::new();
            
            // Iterate from 0 to current coordinate on this axis (exclusive)
            for coord_value in 0..current_logical[axis] {
                // Find what values are stored at positions with this coordinate on this axis
                // and where all other coordinates match current position
                for (index, stored_value) in self.storage.iter().enumerate() {
                    if index < logical_coords.len() {
                        let coords = &logical_coords[index];
                        // Check if this position has the right coordinate on this axis
                        // and matches current position on all other axes
                        if coords[axis] == coord_value {
                            let mut matches_other_axes = true;
                            for other_axis in 0..self.dimensions.len() {
                                if other_axis != axis && coords[other_axis] != current_logical[other_axis] {
                                    matches_other_axes = false;
                                    break;
                                }
                            }
                            if matches_other_axes {
                                axis_values.push(*stored_value);
                            }
                        }
                    }
                }
            }
            
            if !axis_values.is_empty() {
                required.push(axis_values);
            }
        }
        
        required
    }

    pub(crate) fn add(&self, to_add: u16, version: u64) -> Ortho {
        let mut new_storage = self.storage.clone();
        new_storage.push(to_add);
        
        Ortho {
            version,
            storage: new_storage,
            dimensions: self.dimensions.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_stores_version() {
        let ortho = Ortho::new(42);
        assert_eq!(ortho.version(), 42);
    }

    #[test]
    fn test_version_returns_stored_value() {
        let ortho = Ortho::new(123);
        assert_eq!(ortho.version(), 123);
    }
    
    #[test]
    fn test_new_has_default_dimensions() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.dimensions, vec![2, 2]);
        assert!(ortho.storage.is_empty());
    }
    
    #[test]
    fn test_with_dimensions_custom() {
        let ortho = Ortho::with_dimensions(1, vec![3, 2]);
        assert_eq!(ortho.dimensions, vec![3, 2]);
    }
    
    #[test]
    fn test_with_dimensions_enforces_minimum() {
        let ortho = Ortho::with_dimensions(1, vec![1, 1]);
        assert_eq!(ortho.dimensions, vec![2, 2]);
    }
    
    #[test]
    fn test_generate_logical_coordinates_2x2() {
        let ortho = Ortho::new(1);
        let coords = ortho.generate_logical_coordinates();
        
        // Should be sorted by shell (sum) then by components
        // Shell 0: [0,0]
        // Shell 1: [0,1], [1,0]  
        // Shell 2: [1,1]
        let expected = vec![
            vec![0, 0],  // shell 0
            vec![0, 1],  // shell 1
            vec![1, 0],  // shell 1  
            vec![1, 1],  // shell 2
        ];
        
        assert_eq!(coords, expected);
    }
    
    #[test]
    fn test_get_current_logical_coordinate_empty() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.get_current_logical_coordinate(), None);
    }
    
    #[test]
    fn test_get_current_logical_coordinate_with_storage() {
        let mut ortho = Ortho::new(1);
        ortho.storage.push(10);
        
        // First item should be at coordinate [0,0]
        assert_eq!(ortho.get_current_logical_coordinate(), Some(vec![0, 0]));
        
        ortho.storage.push(20);
        // Second item should be at coordinate [0,1]
        assert_eq!(ortho.get_current_logical_coordinate(), Some(vec![0, 1]));
    }
    
    #[test]
    fn test_get_current_shell() {
        let mut ortho = Ortho::new(1);
        assert_eq!(ortho.get_current_shell(), 0);
        
        ortho.storage.push(10);
        assert_eq!(ortho.get_current_shell(), 0); // [0,0] -> sum = 0
        
        ortho.storage.push(20);
        assert_eq!(ortho.get_current_shell(), 1); // [0,1] -> sum = 1
        
        ortho.storage.push(30);
        assert_eq!(ortho.get_current_shell(), 1); // [1,0] -> sum = 1
        
        ortho.storage.push(40);
        assert_eq!(ortho.get_current_shell(), 2); // [1,1] -> sum = 2
    }
    
    #[test]
    fn test_get_forbidden_empty_storage() {
        let ortho = Ortho::new(1);
        let (_, forbidden) = ortho.get_required_and_forbidden();
        assert!(forbidden.is_empty());
    }
    
    #[test]
    fn test_get_forbidden_same_shell() {
        let mut ortho = Ortho::new(1);
        ortho.storage.push(10); // [0,0] shell 0
        ortho.storage.push(20); // [0,1] shell 1
        ortho.storage.push(30); // [1,0] shell 1 <- this is current
        
        // Current shell is 1, so forbidden should include value at [0,1] (also shell 1)
        let (_, forbidden) = ortho.get_required_and_forbidden();
        assert_eq!(forbidden, vec![20]);
    }
    
    #[test]
    fn test_get_required_empty_storage() {
        let ortho = Ortho::new(1);
        let (required, _) = ortho.get_required_and_forbidden();
        assert!(required.is_empty());
    }
    
    #[test]
    fn test_get_required_prefixes() {
        let mut ortho = Ortho::new(1);
        ortho.storage.push(10); // [0,0]
        ortho.storage.push(20); // [0,1]
        ortho.storage.push(30); // [1,0] <- current position
        
        // For position [1,0]:
        // - Axis 0: need values from coord 0 (which is value 10 at [0,0])  
        // - Axis 1: current coord is 0, so no requirements
        let (required, _) = ortho.get_required_and_forbidden();
        assert_eq!(required, vec![vec![10]]);
    }
    
    #[test]
    fn test_add_method() {
        let ortho = Ortho::new(1);
        let new_ortho = ortho.add(42, 2);
        
        assert_eq!(new_ortho.version(), 2);
        assert_eq!(new_ortho.storage, vec![42]);
        assert_eq!(new_ortho.dimensions, vec![2, 2]);
    }
    
    #[test]
    fn test_add_preserves_dimensions() {
        let ortho = Ortho::with_dimensions(1, vec![3, 2]);
        let new_ortho = ortho.add(42, 2);
        
        assert_eq!(new_ortho.dimensions, vec![3, 2]);
    }
    
    #[test]
    fn test_complex_scenario_3x2() {
        let mut ortho = Ortho::with_dimensions(1, vec![3, 2]);
        
        // Generate coordinates for 3x2: [0,0], [0,1], [1,0], [1,1], [2,0], [2,1]
        let coords = ortho.generate_logical_coordinates();
        let expected = vec![
            vec![0, 0],  // shell 0
            vec![0, 1],  // shell 1
            vec![1, 0],  // shell 1
            vec![1, 1],  // shell 2  
            vec![2, 0],  // shell 2
            vec![2, 1],  // shell 3
        ];
        assert_eq!(coords, expected);
        
        // Add values step by step
        ortho.storage.push(100); // [0,0]
        ortho.storage.push(200); // [0,1]  
        ortho.storage.push(300); // [1,0]
        ortho.storage.push(400); // [1,1] <- current
        
        // At position [1,1] (shell 2):
        // For axis 0: current coord is 1, so we need values from coord 0 with same other coords
        //   Looking for positions with axis 0 = 0 and axis 1 = 1 -> that's [0,1] = value 200
        // For axis 1: current coord is 1, so we need values from coord 0 with same other coords  
        //   Looking for positions with axis 1 = 0 and axis 0 = 1 -> that's [1,0] = value 300
        let (required, forbidden) = ortho.get_required_and_forbidden();
        assert_eq!(required, vec![vec![200], vec![300]]);
        assert!(forbidden.is_empty()); // No other values in shell 2 yet
    }
}
