use crate::logical_coords::LogicalCoordinateCache;

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
    fn get_current_logical_coordinate(&self, cache: &mut LogicalCoordinateCache) -> Vec<u16> {
        let logical_coords = cache.get_logical_coordinates(&self.dimensions);
        logical_coords[self.storage.len()].clone()
    }

    pub(crate) fn get_required_and_forbidden(&self, cache: &mut LogicalCoordinateCache) -> (Vec<Vec<u16>>, Vec<u16>) {
        let required = self.get_required(cache);
        let forbidden = self.get_forbidden(cache);
        (required, forbidden)
    }
    
    fn get_forbidden(&self, cache: &mut LogicalCoordinateCache) -> Vec<u16> {
        let forbidden_indices = cache.get_forbidden_indices(&self.dimensions, self.storage.len());
        forbidden_indices.into_iter()
            .map(|index| self.storage[index])
            .collect()
    }
    
    fn get_required(&self, cache: &mut LogicalCoordinateCache) -> Vec<Vec<u16>> {
        cache.get_required_values(&self.dimensions, &self.storage)
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
    fn test_generate_logical_coordinates_2x2() {
        let dimensions = vec![2, 2];
        let mut cache = LogicalCoordinateCache::new();
        let coords = cache.get_logical_coordinates(&dimensions);
        
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
        let mut cache = LogicalCoordinateCache::new();
        assert_eq!(ortho.get_current_logical_coordinate(&mut cache), vec![0, 0]);
    }
    
    #[test]
    fn test_get_current_logical_coordinate_with_storage() {
        let mut ortho = Ortho::new(1);
        let mut cache = LogicalCoordinateCache::new();
        
        // With empty storage, current position should be [0,0]
        assert_eq!(ortho.get_current_logical_coordinate(&mut cache), vec![0, 0]);
        
        ortho.storage.push(10);
        // With one item, current position should be [0,1] (next unfilled)
        assert_eq!(ortho.get_current_logical_coordinate(&mut cache), vec![0, 1]);
        
        ortho.storage.push(20);
        // With two items, current position should be [1,0] (next unfilled)
        assert_eq!(ortho.get_current_logical_coordinate(&mut cache), vec![1, 0]);
    }
    
    #[test]
    fn test_get_current_logical_coordinate_shells() {
        let mut ortho = Ortho::new(1);
        let mut cache = LogicalCoordinateCache::new();
        
        let current = ortho.get_current_logical_coordinate(&mut cache);
        assert_eq!(current, vec![0, 0]); // shell = 0
        assert_eq!(current.iter().sum::<u16>(), 0);
        
        ortho.storage.push(10);
        let current = ortho.get_current_logical_coordinate(&mut cache);
        assert_eq!(current, vec![0, 1]); // shell = 1
        assert_eq!(current.iter().sum::<u16>(), 1);
        
        ortho.storage.push(20);
        let current = ortho.get_current_logical_coordinate(&mut cache);
        assert_eq!(current, vec![1, 0]); // shell = 1
        assert_eq!(current.iter().sum::<u16>(), 1);
        
        ortho.storage.push(30);
        let current = ortho.get_current_logical_coordinate(&mut cache);
        assert_eq!(current, vec![1, 1]); // shell = 2
        assert_eq!(current.iter().sum::<u16>(), 2);
    }
    
    #[test]
    fn test_get_forbidden_empty_storage() {
        let ortho = Ortho::new(1);
        let mut cache = LogicalCoordinateCache::new();
        let (_, forbidden) = ortho.get_required_and_forbidden(&mut cache);
        assert!(forbidden.is_empty());
    }
    
    #[test]
    fn test_get_forbidden_same_shell() {
        let mut ortho = Ortho::new(1);
        let mut cache = LogicalCoordinateCache::new();
        ortho.storage.push(10); // [0,0] shell 0
        ortho.storage.push(20); // [0,1] shell 1
        // Current position is [1,0] shell 1
        
        // Current shell is 1, so forbidden should include value at [0,1] (also shell 1)
        let (_, forbidden) = ortho.get_required_and_forbidden(&mut cache);
        assert_eq!(forbidden, vec![20]);
    }
    
    #[test]
    fn test_get_required_empty_storage() {
        let ortho = Ortho::new(1);
        let mut cache = LogicalCoordinateCache::new();
        let (required, _) = ortho.get_required_and_forbidden(&mut cache);
        assert!(required.is_empty());
    }
    
    #[test]
    fn test_get_required_prefixes() {
        let mut ortho = Ortho::new(1);
        let mut cache = LogicalCoordinateCache::new();
        ortho.storage.push(10); // [0,0]
        ortho.storage.push(20); // [0,1]
        // Current position is [1,0]
        
        // For position [1,0]:
        // - Axis 0: need values from coord 0 (which is value 10 at [0,0])  
        // - Axis 1: current coord is 0, so no requirements
        let (required, _) = ortho.get_required_and_forbidden(&mut cache);
        assert_eq!(required, vec![vec![10]]);
    }
    
    #[test]
    fn test_complex_scenario_3x2() {
        let mut ortho = Ortho::with_dimensions(1, vec![3, 2]);
        let mut cache = LogicalCoordinateCache::new();
        
        // Generate coordinates for 3x2: [0,0], [0,1], [1,0], [1,1], [2,0], [2,1]
        let coords = cache.get_logical_coordinates(&ortho.dimensions);
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
        let (required, forbidden) = ortho.get_required_and_forbidden(&mut cache);
        
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
        let mut cache = LogicalCoordinateCache::new();
        
        // Both should get the same logical coordinates from the shared cache
        let coords1 = cache.get_logical_coordinates(&ortho1.dimensions);
        let coords2 = cache.get_logical_coordinates(&ortho2.dimensions);
        assert_eq!(coords1, coords2);
        
        // Test different dimensions get different coordinates
        let ortho3 = Ortho::with_dimensions(3, vec![3, 2]);
        let coords3 = cache.get_logical_coordinates(&ortho3.dimensions);
        assert_ne!(coords1, coords3);
    }

    #[test]
    fn test_logical_coordinates_cached() {
        let ortho = Ortho::with_dimensions(1, vec![3, 2]);
        let mut cache = LogicalCoordinateCache::new();
        
        // Verify that the logical coordinates are generated correctly
        let logical_coords = cache.get_logical_coordinates(&ortho.dimensions);
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
        assert_eq!(ortho.get_current_logical_coordinate(&mut cache), vec![0, 0]);
        
        // Add some items and verify cache is still used correctly
        let ortho2 = ortho.add(100, 2);
        assert_eq!(ortho2.get_current_logical_coordinate(&mut cache), vec![0, 1]);
        
        // Verify that calling get_logical_coordinates multiple times returns consistent results
        let coords1 = cache.get_logical_coordinates(&ortho.dimensions);
        let coords2 = cache.get_logical_coordinates(&ortho.dimensions);
        assert_eq!(coords1, coords2);
    }

    #[test]
    fn test_forbidden_caching() {
        // Test that forbidden calculations are cached
        let mut ortho = Ortho::with_dimensions(1, vec![3, 2]);
        let mut cache = LogicalCoordinateCache::new();
        
        // Multiple calls should return same values (from cache)
        let forbidden1 = ortho.get_forbidden(&mut cache); 
        let forbidden2 = ortho.get_forbidden(&mut cache); 
        assert_eq!(forbidden1, forbidden2);
        assert!(forbidden1.is_empty()); // Empty storage means no forbidden values
        
        ortho.storage.push(100); // [0,0] shell 0
        ortho.storage.push(200); // [0,1] shell 1
        // Current position is [1,0] shell 1
        
        // Should cache forbidden values for this state
        let forbidden3 = ortho.get_forbidden(&mut cache); 
        let forbidden4 = ortho.get_forbidden(&mut cache);
        assert_eq!(forbidden3, forbidden4);
        assert_eq!(forbidden3, vec![200]); // Value at [0,1] has same shell as current [1,0]
        
        // Test different dimensions cache separately
        let ortho_diff = Ortho::with_dimensions(1, vec![2, 2]);
        let forbidden_diff = ortho_diff.get_forbidden(&mut cache);
        assert!(forbidden_diff.is_empty()); // Should work with different dimensions
    }

    #[test]
    fn test_required_values_caching() {
        let mut ortho = Ortho::with_dimensions(1, vec![3, 2]);
        let mut cache = LogicalCoordinateCache::new();
        ortho.storage.push(100); // [0,0]
        ortho.storage.push(200); // [0,1] 
        ortho.storage.push(300); // [1,0]
        // Current position is [1,1]
        
        // Get required values multiple times - should use cache after first call
        let required1 = ortho.get_required(&mut cache);
        let required2 = ortho.get_required(&mut cache);
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
        
        let mut cache = LogicalCoordinateCache::new();
        
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
        
        let forbidden1 = ortho1.get_forbidden(&mut cache);
        let forbidden2 = ortho2.get_forbidden(&mut cache);
        
        // These should be different because the storage contents are different
        // But with the current buggy caching, they will be the same
        assert_eq!(forbidden1, vec![200]);
        assert_eq!(forbidden2, vec![888]); // This will fail with current buggy cache
        assert_ne!(forbidden1, forbidden2);
    }
}
