#[derive(Debug, Clone)]
pub struct Ortho {
    version: u64,
    storage: Vec<u16>,
    dimensions: Vec<u16>,
    logical_coordinates: Vec<Vec<u16>>,
}

impl Ortho {
    pub fn new(version: u64) -> Self {
        // Use minimum dimensions [2,2] for orthogonality
        let dimensions = vec![2, 2];
        let logical_coordinates = generate_logical_coordinates(&dimensions);
        Ortho { 
            version,
            storage: Vec::new(),
            dimensions,
            logical_coordinates,
        }
    }
    
    pub fn with_dimensions(version: u64, dimensions: Vec<u16>) -> Self {
        let logical_coordinates = generate_logical_coordinates(&dimensions);
        Ortho {
            version,
            storage: Vec::new(),
            dimensions,
            logical_coordinates,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }
    
    /// Get the current logical coordinate based on storage length
    fn get_current_logical_coordinate(&self) -> Vec<u16> {
        let index = self.storage.len();
        self.logical_coordinates[index].clone()
    }
    
    /// Get the current shell (sum of logical coordinates)
    fn get_current_shell(&self) -> u16 {
        let coords = self.get_current_logical_coordinate();
        coords.iter().sum()
    }

    pub(crate) fn get_required_and_forbidden(&self) -> (Vec<Vec<u16>>, Vec<u16>) {
        let required = self.get_required();
        let forbidden = self.get_forbidden();
        (required, forbidden)
    }
    
    fn get_forbidden(&self) -> Vec<u16> {
        let current_shell = self.get_current_shell();
        
        self.storage.iter().enumerate()
            .filter(|(index, _)| {
                if *index < self.logical_coordinates.len() {
                    let coords = &self.logical_coordinates[*index];
                    let shell: u16 = coords.iter().sum();
                    shell == current_shell
                } else {
                    false
                }
            })
            .map(|(_, value)| *value)
            .collect()
    }
    
    fn get_required(&self) -> Vec<Vec<u16>> {
        let current_logical = self.get_current_logical_coordinate();
        
        // Stage 1: Generate the list of list of logical coordinates satisfying the property 
        // that each list of logical coordinates traverses one axis from the edge to the given position (not inclusive)
        let required_coordinate_lists: Vec<Vec<Vec<u16>>> = (0..self.dimensions.len())
            .map(|axis| {
                (0..current_logical[axis])
                    .map(|coord_value| {
                        let mut coords = current_logical.clone();
                        coords[axis] = coord_value;
                        coords
                    })
                    .collect()
            })
            .collect();
        
        // Stage 2: Turn those coordinates into numbers contained by the storage 
        // by mapping them back to flat and looking them up
        required_coordinate_lists.into_iter()
            .map(|coord_list| {
                coord_list.into_iter()
                    .filter_map(|coords| {
                        // Find the index of these coordinates in our logical coordinate system
                        self.logical_coordinates.iter().position(|c| c == &coords)
                            .and_then(|index| {
                                // Look up the stored value at that index
                                if index < self.storage.len() {
                                    Some(self.storage[index])
                                } else {
                                    None
                                }
                            })
                    })
                    .collect()
            })
            .filter(|axis_values: &Vec<u16>| !axis_values.is_empty())
            .collect()
    }

    pub(crate) fn add(&self, to_add: u16, version: u64) -> Ortho {
        let mut new_storage = self.storage.clone();
        new_storage.push(to_add);
        
        Ortho {
            version,
            storage: new_storage,
            dimensions: self.dimensions.clone(),
            logical_coordinates: self.logical_coordinates.clone(),
        }
    }
}

/// Generate all logical coordinates sorted by shell (sum) then by components
fn generate_logical_coordinates(dimensions: &[u16]) -> Vec<Vec<u16>> {
    // Generate Cartesian product of all dimension ranges
    let mut coords = cartesian_product(dimensions);
    
    // Sort by shell (sum of coordinates) first, then by components
    coords.sort_by(|a, b| {
        let sum_a: u16 = a.iter().sum();
        let sum_b: u16 = b.iter().sum();
        sum_a.cmp(&sum_b).then_with(|| a.cmp(b))
    });
    
    coords
}

fn cartesian_product(dimensions: &[u16]) -> Vec<Vec<u16>> {
    if dimensions.is_empty() {
        return vec![vec![]];
    }
    
    let first_dim = dimensions[0];
    let rest = cartesian_product(&dimensions[1..]);
    
    (0..first_dim)
        .flat_map(|i| {
            rest.iter().map(move |suffix| {
                let mut result = vec![i];
                result.extend(suffix);
                result
            })
        })
        .collect()
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
    fn test_generate_logical_coordinates_2x2() {
        let ortho = Ortho::new(1);
        let coords = &ortho.logical_coordinates;
        
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
        
        assert_eq!(*coords, expected);
    }
    
    #[test]
    fn test_get_current_logical_coordinate_empty() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.get_current_logical_coordinate(), vec![0, 0]);
    }
    
    #[test]
    fn test_get_current_logical_coordinate_with_storage() {
        let mut ortho = Ortho::new(1);
        
        // With empty storage, current position should be [0,0]
        assert_eq!(ortho.get_current_logical_coordinate(), vec![0, 0]);
        
        ortho.storage.push(10);
        // With one item, current position should be [0,1] (next unfilled)
        assert_eq!(ortho.get_current_logical_coordinate(), vec![0, 1]);
        
        ortho.storage.push(20);
        // With two items, current position should be [1,0] (next unfilled)
        assert_eq!(ortho.get_current_logical_coordinate(), vec![1, 0]);
    }
    
    #[test]
    fn test_get_current_shell() {
        let mut ortho = Ortho::new(1);
        assert_eq!(ortho.get_current_shell(), 0); // [0,0] -> sum = 0
        
        ortho.storage.push(10);
        assert_eq!(ortho.get_current_shell(), 1); // [0,1] -> sum = 1
        
        ortho.storage.push(20);
        assert_eq!(ortho.get_current_shell(), 1); // [1,0] -> sum = 1
        
        ortho.storage.push(30);
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
        // Current position is [1,0] shell 1
        
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
        // Current position is [1,0]
        
        // For position [1,0]:
        // - Axis 0: need values from coord 0 (which is value 10 at [0,0])  
        // - Axis 1: current coord is 0, so no requirements
        let (required, _) = ortho.get_required_and_forbidden();
        assert_eq!(required, vec![vec![10]]);
    }
    
    #[test]
    fn test_complex_scenario_3x2() {
        let mut ortho = Ortho::with_dimensions(1, vec![3, 2]);
        
        // Generate coordinates for 3x2: [0,0], [0,1], [1,0], [1,1], [2,0], [2,1]
        let coords = &ortho.logical_coordinates;
        let expected = vec![
            vec![0, 0],  // shell 0
            vec![0, 1],  // shell 1
            vec![1, 0],  // shell 1
            vec![1, 1],  // shell 2  
            vec![2, 0],  // shell 2
            vec![2, 1],  // shell 3
        ];
        assert_eq!(*coords, expected);
        
        // Add values step by step to reach position [2,1] 
        ortho.storage.push(100); // [0,0]
        ortho.storage.push(200); // [0,1]  
        ortho.storage.push(300); // [1,0]
        ortho.storage.push(400); // [1,1]
        ortho.storage.push(500); // [2,0]
        // Current position is [2,1] (shell 3)
        
        // At position [2,1]:
        // For axis 0: current coord is 2, so we need values from coords 0,1 with same axis 1 coord (1)
        //   Looking for positions with axis 0 = 0,1 and axis 1 = 1 -> that's [0,1]=200, [1,1]=400
        // For axis 1: current coord is 1, so we need values from coord 0 with same axis 0 coord (2)  
        //   Looking for positions with axis 1 = 0 and axis 0 = 2 -> that's [2,0]=500
        let (required, forbidden) = ortho.get_required_and_forbidden();
        
        // required should have something of length two (axis 0 requirements) and something of length one (axis 1 requirements)
        assert_eq!(required.len(), 2);
        assert_eq!(required[0], vec![200, 400]); // axis 0: values at [0,1] and [1,1]
        assert_eq!(required[1], vec![500]);      // axis 1: value at [2,0]
        
        // forbidden should be nonempty - there are no other values in shell 3 yet, but let's add one more
        assert!(forbidden.is_empty()); // No other values in shell 3 yet
    }
    
    #[test]
    fn test_logical_coordinates_cached() {
        let ortho = Ortho::with_dimensions(1, vec![3, 2]);
        
        // Verify the cache is populated with correct coordinates
        let expected = vec![
            vec![0, 0],  // shell 0
            vec![0, 1],  // shell 1
            vec![1, 0],  // shell 1
            vec![1, 1],  // shell 2  
            vec![2, 0],  // shell 2
            vec![2, 1],  // shell 3
        ];
        assert_eq!(ortho.logical_coordinates, expected);
        
        // Verify that methods use the cached coordinates consistently
        assert_eq!(ortho.get_current_logical_coordinate(), vec![0, 0]);
        
        // Add some items and verify cache is still used correctly
        let ortho2 = ortho.add(100, 2);
        assert_eq!(ortho2.get_current_logical_coordinate(), vec![0, 1]);
        assert_eq!(ortho2.logical_coordinates, expected); // Cache should be same
    }
}
