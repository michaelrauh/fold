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

// Global cache for forbidden coordinate indices keyed by (dimensions, storage_length)
static FORBIDDEN_INDICES_CACHE: OnceLock<Mutex<HashMap<(Vec<u16>, usize), Vec<usize>>>> = OnceLock::new();

// Global cache for required coordinate indices keyed by (dimensions, current_logical_coordinate)
static REQUIRED_INDICES_CACHE: OnceLock<Mutex<HashMap<(Vec<u16>, Vec<u16>), Vec<Vec<usize>>>>> = OnceLock::new();

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

/// Get cached forbidden indices or compute and cache them
fn get_forbidden_indices(dimensions: &[u16], storage_length: usize) -> Vec<usize> {
    let cache = FORBIDDEN_INDICES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache_key = (dimensions.to_vec(), storage_length);
    let mut cache_guard = cache.lock().unwrap();
    
    if let Some(indices) = cache_guard.get(&cache_key) {
        indices.clone()
    } else {
        let logical_coords = get_logical_coordinates(dimensions);
        let current_shell: u16 = logical_coords[storage_length].iter().sum();
        
        let forbidden_indices: Vec<usize> = (0..storage_length)
            .filter(|&index| {
                if index < logical_coords.len() {
                    let coords = &logical_coords[index];
                    let shell: u16 = coords.iter().sum();
                    shell == current_shell
                } else {
                    false
                }
            })
            .collect();
        
        cache_guard.insert(cache_key, forbidden_indices.clone());
        forbidden_indices
    }
}
/// Get cached required coordinate indices or compute and cache them
fn get_required_coordinate_indices(dimensions: &[u16], current_logical: &[u16]) -> Vec<Vec<usize>> {
    let cache = REQUIRED_INDICES_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let cache_key = (dimensions.to_vec(), current_logical.to_vec());
    let mut cache_guard = cache.lock().unwrap();
    
    if let Some(indices) = cache_guard.get(&cache_key) {
        indices.clone()
    } else {
        let logical_coords = get_logical_coordinates(dimensions);
        
        // Stage 1: Generate the list of list of indices for coordinates satisfying the property 
        // that each list of coordinates traverses one axis from the edge to the given position (not inclusive)
        let required_indices: Vec<Vec<usize>> = (0..dimensions.len())
            .map(|axis| {
                (0..current_logical[axis])
                    .filter_map(|coord_value| {
                        let mut coords = current_logical.to_vec();
                        coords[axis] = coord_value;
                        // Find the index of these coordinates in our logical coordinate system
                        logical_coords.iter().position(|c| c == &coords)
                    })
                    .collect()
            })
            .collect();
        
        cache_guard.insert(cache_key, required_indices.clone());
        required_indices
    }
}

/// Get required values by computing coordinate indices and mapping to storage
fn get_required_values(dimensions: &[u16], storage: &[u16]) -> Vec<Vec<u16>> {
    let logical_coords = get_logical_coordinates(dimensions);
    let current_logical = logical_coords[storage.len()].clone();
    
    // Get cached coordinate indices
    let required_indices = get_required_coordinate_indices(dimensions, &current_logical);
    
    // Stage 2: Turn those indices into values contained by the storage 
    // by looking them up directly
    let required: Vec<Vec<u16>> = required_indices.into_iter()
        .map(|index_list| {
            index_list.into_iter()
                .filter_map(|index| {
                    // Look up the stored value at that index
                    if index < storage.len() {
                        Some(storage[index])
                    } else {
                        None
                    }
                })
                .collect()
        })
        .filter(|axis_values: &Vec<u16>| !axis_values.is_empty())
        .collect();
    
    required
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
    
    /// Get the current logical coordinate based on storage length (for testing)
    #[cfg(test)]
    fn get_current_logical_coordinate(&self) -> Vec<u16> {
        let logical_coords = get_logical_coordinates(&self.dimensions);
        logical_coords[self.storage.len()].clone()
    }

    pub(crate) fn get_required_and_forbidden(&self) -> (Vec<Vec<u16>>, Vec<u16>) {
        let required = self.get_required();
        let forbidden = self.get_forbidden();
        (required, forbidden)
    }
    
    fn get_forbidden(&self) -> Vec<u16> {
        let forbidden_indices = get_forbidden_indices(&self.dimensions, self.storage.len());
        forbidden_indices.into_iter()
            .map(|index| self.storage[index])
            .collect()
    }
    
    fn get_required(&self) -> Vec<Vec<u16>> {
        get_required_values(&self.dimensions, &self.storage)
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
    fn test_get_current_logical_coordinate_shells() {
        let mut ortho = Ortho::new(1);
        let current = ortho.get_current_logical_coordinate();
        assert_eq!(current, vec![0, 0]); // shell = 0
        assert_eq!(current.iter().sum::<u16>(), 0);
        
        ortho.storage.push(10);
        let current = ortho.get_current_logical_coordinate();
        assert_eq!(current, vec![0, 1]); // shell = 1
        assert_eq!(current.iter().sum::<u16>(), 1);
        
        ortho.storage.push(20);
        let current = ortho.get_current_logical_coordinate();
        assert_eq!(current, vec![1, 0]); // shell = 1
        assert_eq!(current.iter().sum::<u16>(), 1);
        
        ortho.storage.push(30);
        let current = ortho.get_current_logical_coordinate();
        assert_eq!(current, vec![1, 1]); // shell = 2
        assert_eq!(current.iter().sum::<u16>(), 2);
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
    fn test_forbidden_caching() {
        // Test that forbidden calculations are cached
        let mut ortho = Ortho::with_dimensions(1, vec![3, 2]);
        
        // Multiple calls should return same values (from cache)
        let forbidden1 = ortho.get_forbidden(); 
        let forbidden2 = ortho.get_forbidden(); 
        assert_eq!(forbidden1, forbidden2);
        assert!(forbidden1.is_empty()); // Empty storage means no forbidden values
        
        ortho.storage.push(100); // [0,0] shell 0
        ortho.storage.push(200); // [0,1] shell 1
        // Current position is [1,0] shell 1
        
        // Should cache forbidden values for this state
        let forbidden3 = ortho.get_forbidden(); 
        let forbidden4 = ortho.get_forbidden();
        assert_eq!(forbidden3, forbidden4);
        assert_eq!(forbidden3, vec![200]); // Value at [0,1] has same shell as current [1,0]
        
        // Test different dimensions cache separately
        let ortho_diff = Ortho::with_dimensions(1, vec![2, 2]);
        let forbidden_diff = ortho_diff.get_forbidden();
        assert!(forbidden_diff.is_empty()); // Should work with different dimensions
    }

    #[test]
    fn test_required_values_caching() {
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

    #[test]
    fn test_forbidden_cache_bug_different_storage_content() {
        // This test demonstrates the bug where forbidden values are incorrectly cached
        // based only on dimensions and storage length, not storage content
        
        // Create first ortho with specific values
        let mut ortho1 = Ortho::with_dimensions(1, vec![2, 2]);
        ortho1.storage.push(100); // [0,0] shell 0
        ortho1.storage.push(200); // [0,1] shell 1
        // Current position is [1,0] shell 1
        // Forbidden should be [200] (value at [0,1] which has same shell)
        
        // Create second ortho with DIFFERENT values but same dimensions and length
        let mut ortho2 = Ortho::with_dimensions(2, vec![2, 2]);
        ortho2.storage.push(999); // [0,0] shell 0  
        ortho2.storage.push(888); // [0,1] shell 1
        // Current position is [1,0] shell 1
        // Forbidden should be [888] (value at [0,1] which has same shell)
        
        let forbidden1 = ortho1.get_forbidden();
        let forbidden2 = ortho2.get_forbidden();
        
        // These should be different because the storage contents are different
        // But with the current buggy caching, they will be the same
        assert_eq!(forbidden1, vec![200]);
        assert_eq!(forbidden2, vec![888]); // This will fail with current buggy cache
        assert_ne!(forbidden1, forbidden2);
    }
}
