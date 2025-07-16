use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
pub struct Ortho {
    version: u64,
    storage: Vec<u16>,
    dimensions: Vec<u16>,
}

// Global cache for logical coordinates keyed by dimensions
static LOGICAL_COORDINATES_CACHE: OnceLock<Mutex<HashMap<Vec<u16>, Vec<Vec<u16>>>>> = OnceLock::new();

// Global cache for shell calculations keyed by (dimensions, storage_length)
static SHELL_CACHE: OnceLock<Mutex<HashMap<(Vec<u16>, usize), u16>>> = OnceLock::new();

// Global cache for required coordinate lists keyed by (dimensions, current_logical_coordinate)
static REQUIRED_COORDS_CACHE: OnceLock<Mutex<HashMap<(Vec<u16>, Vec<u16>), Vec<Vec<Vec<u16>>>>>> = OnceLock::new();

/// Get cached logical coordinates or compute and cache them
fn get_logical_coordinates(dimensions: &[u16]) -> Vec<Vec<u16>> {
    let cache = LOGICAL_COORDINATES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache_guard = cache.lock().unwrap();
    
    if let Some(coords) = cache_guard.get(dimensions) {
        coords.clone()
    } else {
        let coords = generate_logical_coordinates(dimensions);
        cache_guard.insert(dimensions.to_vec(), coords.clone());
        coords
    }
}

/// Get cached shell value or compute and cache it
fn get_shell_for_position(dimensions: &[u16], storage_length: usize) -> u16 {
    let cache = SHELL_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache_key = (dimensions.to_vec(), storage_length);
    let mut cache_guard = cache.lock().unwrap();
    
    if let Some(&shell) = cache_guard.get(&cache_key) {
        shell
    } else {
        let logical_coords = get_logical_coordinates(dimensions);
        let shell = logical_coords[storage_length].iter().sum();
        cache_guard.insert(cache_key, shell);
        shell
    }
}

/// Get cached required coordinate lists or compute and cache them (stage one logic)
fn get_required_coordinate_lists(dimensions: &[u16], current_logical: &[u16]) -> Vec<Vec<Vec<u16>>> {
    let cache = REQUIRED_COORDS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache_key = (dimensions.to_vec(), current_logical.to_vec());
    let mut cache_guard = cache.lock().unwrap();
    
    if let Some(coord_lists) = cache_guard.get(&cache_key) {
        coord_lists.clone()
    } else {
        // Stage 1: Generate the list of list of logical coordinates satisfying the property 
        // that each list of logical coordinates traverses one axis from the edge to the given position (not inclusive)
        let required_coordinate_lists: Vec<Vec<Vec<u16>>> = (0..dimensions.len())
            .map(|axis| {
                (0..current_logical[axis])
                    .map(|coord_value| {
                        let mut coords = current_logical.to_vec();
                        coords[axis] = coord_value;
                        coords
                    })
                    .collect()
            })
            .collect();
        
        cache_guard.insert(cache_key, required_coordinate_lists.clone());
        required_coordinate_lists
    }
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
        Ortho {
            version,
            storage: Vec::new(),
            dimensions,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }
    
    /// Get the current logical coordinate based on storage length
    fn get_current_logical_coordinate(&self) -> Vec<u16> {
        let logical_coords = get_logical_coordinates(&self.dimensions);
        let index = self.storage.len();
        logical_coords[index].clone()
    }
    
    /// Get the current shell (sum of logical coordinates)
    fn get_current_shell(&self) -> u16 {
        get_shell_for_position(&self.dimensions, self.storage.len())
    }

    pub(crate) fn get_required_and_forbidden(&self) -> (Vec<Vec<u16>>, Vec<u16>) {
        let required = self.get_required();
        let forbidden = self.get_forbidden();
        (required, forbidden)
    }
    
    fn get_forbidden(&self) -> Vec<u16> {
        let current_shell = self.get_current_shell();
        let logical_coords = get_logical_coordinates(&self.dimensions);
        
        self.storage.iter().enumerate()
            .filter(|(index, _)| {
                if *index < logical_coords.len() {
                    let coords = &logical_coords[*index];
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
        let logical_coords = get_logical_coordinates(&self.dimensions);
        
        // Stage 1: Use cached required coordinate lists
        let required_coordinate_lists = get_required_coordinate_lists(&self.dimensions, &current_logical);
        
        // Stage 2: Turn those coordinates into numbers contained by the storage 
        // by mapping them back to flat and looking them up
        required_coordinate_lists.into_iter()
            .map(|coord_list| {
                coord_list.into_iter()
                    .filter_map(|coords| {
                        // Find the index of these coordinates in our logical coordinate system
                        logical_coords.iter().position(|c| c == &coords)
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
        let dimensions = vec![2, 2];
        let coords = get_logical_coordinates(&dimensions);
        
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
        let coords = get_logical_coordinates(&ortho.dimensions);
        let expected = vec![
            vec![0, 0],  // shell 0
            vec![0, 1],  // shell 1
            vec![1, 0],  // shell 1
            vec![1, 1],  // shell 2  
            vec![2, 0],  // shell 2
            vec![2, 1],  // shell 3
        ];
        assert_eq!(coords, expected);
        
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
    fn test_shared_cache_across_instances() {
        // Create two different ortho instances with same dimensions
        let ortho1 = Ortho::with_dimensions(1, vec![2, 3]);
        let ortho2 = Ortho::with_dimensions(2, vec![2, 3]);
        
        // Both should get the same logical coordinates from the shared cache
        let coords1 = get_logical_coordinates(&ortho1.dimensions);
        let coords2 = get_logical_coordinates(&ortho2.dimensions);
        assert_eq!(coords1, coords2);
        
        // Test different dimensions get different coordinates
        let ortho3 = Ortho::with_dimensions(3, vec![3, 2]);
        let coords3 = get_logical_coordinates(&ortho3.dimensions);
        assert_ne!(coords1, coords3);
    }

    #[test]
    fn test_logical_coordinates_cached() {
        let ortho = Ortho::with_dimensions(1, vec![3, 2]);
        
        // Verify that the logical coordinates are generated correctly
        let logical_coords = get_logical_coordinates(&ortho.dimensions);
        let expected = vec![
            vec![0, 0],  // shell 0
            vec![0, 1],  // shell 1
            vec![1, 0],  // shell 1
            vec![1, 1],  // shell 2  
            vec![2, 0],  // shell 2
            vec![2, 1],  // shell 3
        ];
        assert_eq!(logical_coords, expected);
        
        // Verify that methods use the cached coordinates consistently
        assert_eq!(ortho.get_current_logical_coordinate(), vec![0, 0]);
        
        // Add some items and verify cache is still used correctly
        let ortho2 = ortho.add(100, 2);
        assert_eq!(ortho2.get_current_logical_coordinate(), vec![0, 1]);
        
        // Verify that calling get_logical_coordinates multiple times returns consistent results
        let coords1 = get_logical_coordinates(&ortho.dimensions);
        let coords2 = get_logical_coordinates(&ortho.dimensions);
        assert_eq!(coords1, coords2);
    }

    #[test]
    fn test_shell_caching() {
        // Test that shell calculations are cached
        let ortho = Ortho::with_dimensions(1, vec![3, 2]);
        
        // Multiple calls should return same values (from cache)
        assert_eq!(ortho.get_current_shell(), 0); // Position 0 -> [0,0] -> shell 0
        assert_eq!(ortho.get_current_shell(), 0); // Should use cached value
        
        let ortho2 = ortho.add(100, 2);
        assert_eq!(ortho2.get_current_shell(), 1); // Position 1 -> [0,1] -> shell 1
        assert_eq!(ortho2.get_current_shell(), 1); // Should use cached value
        
        let ortho3 = ortho2.add(200, 3);  
        assert_eq!(ortho3.get_current_shell(), 1); // Position 2 -> [1,0] -> shell 1
        
        // Test different dimensions cache separately
        let ortho_diff = Ortho::with_dimensions(1, vec![2, 2]);
        assert_eq!(ortho_diff.get_current_shell(), 0); // Should work with different dimensions
    }

    #[test]
    fn test_required_coordinate_lists_caching() {
        let mut ortho = Ortho::with_dimensions(1, vec![3, 2]);
        ortho.storage.push(100); // [0,0]
        ortho.storage.push(200); // [0,1] 
        ortho.storage.push(300); // [1,0]
        // Current position is [1,1]
        
        // Get required values multiple times - should use cache after first call
        let required1 = ortho.get_required();
        let required2 = ortho.get_required();
        assert_eq!(required1, required2);
        
        // The required logic should be:
        // At position [1,1]:
        // - Axis 0: need values from coord 0 with same axis 1 coord (1) -> that's [0,1]=200
        // - Axis 1: need values from coord 0 with same axis 0 coord (1) -> that's [1,0]=300
        assert_eq!(required1.len(), 2);
        assert_eq!(required1[0], vec![200]); // axis 0 requirement
        assert_eq!(required1[1], vec![300]); // axis 1 requirement
    }
}
