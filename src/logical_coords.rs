use std::collections::HashMap;

/// Cache for logical coordinate computations
#[derive(Debug, Clone)]
pub struct LogicalCoordinateCache {
    logical_coordinates: HashMap<Vec<u16>, Vec<Vec<u16>>>,
    forbidden_indices: HashMap<(Vec<u16>, usize), Vec<usize>>,
    required_indices: HashMap<(Vec<u16>, Vec<u16>), Vec<Vec<usize>>>,
}

impl LogicalCoordinateCache {
    pub fn new() -> Self {
        Self {
            logical_coordinates: HashMap::new(),
            forbidden_indices: HashMap::new(),
            required_indices: HashMap::new(),
        }
    }

    /// Get cached logical coordinates or compute and cache them
    pub fn get_logical_coordinates(&mut self, dimensions: &[u16]) -> Vec<Vec<u16>> {
        if let Some(coords) = self.logical_coordinates.get(dimensions) {
            coords.clone()
        } else {
            let coords = generate_logical_coordinates(dimensions);
            self.logical_coordinates.insert(dimensions.to_vec(), coords.clone());
            coords
        }
    }

    /// Get cached forbidden indices or compute and cache them
    pub fn get_forbidden_indices(&mut self, dimensions: &[u16], storage_length: usize) -> Vec<usize> {
        let cache_key = (dimensions.to_vec(), storage_length);
        
        if let Some(indices) = self.forbidden_indices.get(&cache_key) {
            indices.clone()
        } else {
            let logical_coords = self.get_logical_coordinates(dimensions);
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
            
            self.forbidden_indices.insert(cache_key, forbidden_indices.clone());
            forbidden_indices
        }
    }

    /// Get cached required coordinate indices or compute and cache them
    pub fn get_required_coordinate_indices(&mut self, dimensions: &[u16], current_logical: &[u16]) -> Vec<Vec<usize>> {
        let cache_key = (dimensions.to_vec(), current_logical.to_vec());
        
        if let Some(indices) = self.required_indices.get(&cache_key) {
            indices.clone()
        } else {
            let logical_coords = self.get_logical_coordinates(dimensions);
            
            // Generate the list of list of indices for coordinates satisfying the property 
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
            
            self.required_indices.insert(cache_key, required_indices.clone());
            required_indices
        }
    }

    /// Get required values by computing coordinate indices and mapping to storage
    pub fn get_required_values(&mut self, dimensions: &[u16], storage: &[u16]) -> Vec<Vec<u16>> {
        let logical_coords = self.get_logical_coordinates(dimensions);
        let current_logical = logical_coords[storage.len()].clone();
        
        // Get cached coordinate indices
        let required_indices = self.get_required_coordinate_indices(dimensions, &current_logical);
        
        // Turn those indices into values contained by the storage by looking them up directly
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
    fn test_cache_new() {
        let cache = LogicalCoordinateCache::new();
        assert!(cache.logical_coordinates.is_empty());
        assert!(cache.forbidden_indices.is_empty());
        assert!(cache.required_indices.is_empty());
    }

    #[test]
    fn test_logical_coordinates_cached() {
        let mut cache = LogicalCoordinateCache::new();
        let dimensions = vec![2, 2];
        
        let coords1 = cache.get_logical_coordinates(&dimensions);
        let coords2 = cache.get_logical_coordinates(&dimensions);
        
        // Should be the same reference (cached)
        assert_eq!(coords1, coords2);
        
        let expected = vec![
            vec![0, 0],  // shell 0
            vec![0, 1],  // shell 1
            vec![1, 0],  // shell 1  
            vec![1, 1],  // shell 2
        ];
        
        assert_eq!(coords1, expected);
    }

    #[test]
    fn test_forbidden_indices_cached() {
        let mut cache = LogicalCoordinateCache::new();
        let dimensions = vec![2, 2];
        let storage_length = 2;
        
        let indices1 = cache.get_forbidden_indices(&dimensions, storage_length);
        let indices2 = cache.get_forbidden_indices(&dimensions, storage_length);
        
        assert_eq!(indices1, indices2);
        // At storage length 2, current position is [1,0] (shell 1)
        // Forbidden should include index 1 which is [0,1] (also shell 1)
        assert_eq!(indices1, vec![1]);
    }

    #[test]
    fn test_required_indices_cached() {
        let mut cache = LogicalCoordinateCache::new();
        let dimensions = vec![3, 2];
        let current_logical = vec![1, 1];
        
        let indices1 = cache.get_required_coordinate_indices(&dimensions, &current_logical);
        let indices2 = cache.get_required_coordinate_indices(&dimensions, &current_logical);
        
        assert_eq!(indices1, indices2);
        
        // For position [1,1]:
        // - Axis 0: need coord 0 with same axis 1 coord (1) -> that's [0,1] at index 1
        // - Axis 1: need coord 0 with same axis 0 coord (1) -> that's [1,0] at index 2
        assert_eq!(indices1.len(), 2);
        assert_eq!(indices1[0], vec![1]); // axis 0 requirement: [0,1]
        assert_eq!(indices1[1], vec![2]); // axis 1 requirement: [1,0]
    }

    #[test]
    fn test_required_values() {
        let mut cache = LogicalCoordinateCache::new();
        let dimensions = vec![3, 2];
        let storage = vec![100, 200, 300];
        
        let required = cache.get_required_values(&dimensions, &storage);
        
        // At storage length 3, current position is [1,1]
        // Required should be:
        // - Axis 0: value at [0,1] = 200
        // - Axis 1: value at [1,0] = 300
        assert_eq!(required.len(), 2);
        assert_eq!(required[0], vec![200]);
        assert_eq!(required[1], vec![300]);
    }
}