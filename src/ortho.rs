use crate::spatial;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::fmt;
use bincode::Encode;
use bincode::Decode;

#[derive(PartialEq, Debug, Clone, Encode, Decode)]
pub struct Ortho {
    version: usize,
    dims: Vec<usize>,
    payload: Vec<Option<usize>>,
}

impl Ortho {
    fn compute_id(version: usize, dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
        if payload.iter().all(|x| x.is_none()) {
            let mut hasher = DefaultHasher::new();
            version.hash(&mut hasher);
            (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
        } else {
            let mut hasher = DefaultHasher::new();
            dims.hash(&mut hasher);
            payload.hash(&mut hasher);
            (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
        }
    }
    pub fn new(version: usize) -> Self {
        let dims = vec![2,2];
        let payload = vec![None; 4];
        Ortho { version, dims, payload }
    }
    pub fn id(&self) -> usize { Self::compute_id(self.version, &self.dims, &self.payload) }
    pub fn get_current_position(&self) -> usize { self.payload.iter().position(|x| x.is_none()).unwrap_or(self.payload.len()) }
    pub fn add(&self, value: usize, version: usize) -> Vec<Self> {
        let insertion_index = self.get_current_position();
        let total_empty = self.payload.iter().filter(|x| x.is_none()).count();
        if total_empty == 1 {
            if spatial::is_base(&self.dims) {
                return Self::expand(
                    self,
                    spatial::expand_up(&self.dims, self.get_insert_position(value)),
                    value,
                    version,
                );
            } else {
                return Self::expand(self, spatial::expand_over(&self.dims), value, version);
            }
        }
        if insertion_index == 2 && self.dims.as_slice() == [2, 2] {
            let mut new_payload: Vec<Option<usize>> = self.payload.clone();
            new_payload[insertion_index] = Some(value);
            if let (Some(second), Some(third)) = (new_payload[1], new_payload[2]) {
                if second > third { new_payload[1] = Some(third); new_payload[2] = Some(second); }
            }
            return vec![Ortho { version, dims: self.dims.clone(), payload: new_payload }];
        }
        let len = self.payload.len();
        let mut new_payload: Vec<Option<usize>> = Vec::with_capacity(len);
        unsafe { new_payload.set_len(len); std::ptr::copy_nonoverlapping(self.payload.as_ptr(), new_payload.as_mut_ptr(), len); }
        if insertion_index < new_payload.len() { new_payload[insertion_index] = Some(value); }
        vec![Ortho { version, dims: self.dims.clone(), payload: new_payload }]
    }
    fn expand(
        ortho: &Ortho,
        expansions: Vec<(Vec<usize>, usize, Vec<usize>)>,
        value: usize,
        version: usize,
    ) -> Vec<Ortho> {
        let mut old_payload_with_value = ortho.payload.clone();
        let insert_pos = old_payload_with_value.iter().position(|x| x.is_none()).unwrap();
        old_payload_with_value[insert_pos] = Some(value);
        
        let mut out = Vec::with_capacity(expansions.len());
        for (new_dims_vec, new_capacity, reorg) in expansions.into_iter() {
            let mut new_payload = vec![None; new_capacity];
            for (i, &pos) in reorg.iter().enumerate() { 
                new_payload[pos] = old_payload_with_value.get(i).cloned().flatten(); 
            }
            out.push(Ortho { version, dims: new_dims_vec, payload: new_payload });
        }
        out
    }
    fn get_insert_position(&self, to_add: usize) -> usize {
        let axis_positions = spatial::get_axis_positions(&self.dims);
        let mut idx = 0;
        for &pos in axis_positions.iter() {
            if let Some(&axis) = self.payload.get(pos).and_then(|x| x.as_ref()) {
                if to_add < axis { return idx; }
                idx += 1;
            }
        }
        idx
    }
    pub fn get_requirements(&self) -> (Vec<usize>, Vec<Vec<usize>>) {
        let pos = self.get_current_position();
        let (prefixes, diagonals) = spatial::get_requirements(pos, &self.dims);
        let forbidden: Vec<usize> = diagonals
            .into_iter()
            .filter_map(|i| self.payload.get(i).and_then(|v| *v))
            .collect();
        let required: Vec<Vec<usize>> = prefixes
            .into_iter()
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| {
                prefix
                    .iter()
                    .filter_map(|&i| self.payload.get(i).cloned().flatten())
                    .collect::<Vec<usize>>()
            })
            .collect();
        (forbidden, required)
    }
    pub fn version(&self) -> usize { self.version }
    pub fn prefixes(&self) -> Vec<Vec<usize>> {
        let mut result = Vec::new();
        for pos in 0..self.payload.len() {
            let (prefixes, _diagonals) = spatial::get_requirements(pos, &self.dims);
            for prefix in prefixes {
                if !prefix.is_empty() {
                    let values: Vec<usize> = prefix
                        .iter()
                        .filter_map(|&i| self.payload.get(i).cloned().flatten())
                        .collect();
                    if !values.is_empty() { result.push(values); }
                }
            }
        }
        result
    }
    pub fn prefixes_for_last_filled(&self) -> Vec<Vec<usize>> {
        if self.get_current_position() == 0 { return vec![]; }
        let pos = self.get_current_position() - 1;
        let (prefixes, _diagonals) = spatial::get_requirements(pos, &self.dims);
        prefixes.into_iter()
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| {
                prefix.iter()
                    .filter_map(|&i| self.payload.get(i).cloned().flatten())
                    .collect::<Vec<usize>>()
            })
            .filter(|v| !v.is_empty())
            .collect()
    }
    pub fn dims(&self) -> &Vec<usize> { &self.dims }
    pub fn payload(&self) -> &Vec<Option<usize>> { &self.payload }
    
    // Get linear index from coordinates using spatial ordering
    fn get_index_at_coord(&self, coord: &[usize]) -> Option<usize> {
        let meta = spatial_helper::get_meta(self.dims.as_slice());
        meta.location_to_index.get(coord).copied()
    }
}

// Helper function to access spatial module internals
mod spatial_helper {
    use rustc_hash::FxHashMap;
    
    pub struct Meta {
        pub location_to_index: FxHashMap<Vec<usize>, usize>,
    }
    
    pub fn get_meta(dims: &[usize]) -> Meta {
        // Recompute the spatial ordering
        let indices_in_order = indices_in_order_compute(dims);
        let location_to_index: FxHashMap<Vec<usize>, usize> = indices_in_order
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, loc)| (loc, i))
            .collect();
        Meta {
            location_to_index,
        }
    }
    
    fn index_array(dims: &[usize]) -> Vec<Vec<usize>> {
        cartesian_product(dims.iter().map(|x| (0..*x).collect()).collect())
    }
    
    fn indices_in_order_compute(dims: &[usize]) -> Vec<Vec<usize>> {
        order_by_distance(index_array(dims))
    }
    
    fn order_by_distance(indices: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
        let mut sorted = indices;
        sorted.sort_by(|a, b| {
            match a.iter().sum::<usize>().cmp(&b.iter().sum()) {
                std::cmp::Ordering::Less => std::cmp::Ordering::Less,
                std::cmp::Ordering::Equal => {
                    for (x, y) in a.iter().zip(b) {
                        if x > y {
                            return std::cmp::Ordering::Greater;
                        }
                        if x < y {
                            return std::cmp::Ordering::Less;
                        }
                    }
                    unreachable!("Duplicate indices impossible")
                }
                std::cmp::Ordering::Greater => std::cmp::Ordering::Greater,
            }
        });
        sorted
    }
    
    fn partial_cartesian<T: Clone>(a: Vec<Vec<T>>, b: Vec<T>) -> Vec<Vec<T>> {
        a.into_iter()
            .flat_map(|xs| {
                b.iter()
                    .cloned()
                    .map(|y| {
                        let mut vec = xs.clone();
                        vec.push(y);
                        vec
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
    
    fn cartesian_product<T: Clone>(lists: Vec<Vec<T>>) -> Vec<Vec<T>> {
        match lists.split_first() {
            Some((first, rest)) => {
                let init: Vec<Vec<T>> = first.iter().cloned().map(|n| vec![n]).collect();
                rest.iter()
                    .cloned()
                    .fold(init, |vec, list| partial_cartesian(vec, list))
            }
            None => vec![],
        }
    }
}

impl fmt::Display for Ortho {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Orthos are at least 2D
        if self.dims.len() < 2 {
            return write!(f, "Invalid ortho: dimensions < 2");
        }
        
        // Get the last two dimensions for the 2D display
        let rows = self.dims[self.dims.len() - 2];
        let cols = self.dims[self.dims.len() - 1];
        
        // For higher dimensions, we'll tile 2D slices
        if self.dims.len() == 2 {
            // Simple 2D case
            self.format_2d_slice(f, &[], rows, cols)
        } else {
            // Higher dimensions: tile 2D slices
            self.format_tiled(f, rows, cols)
        }
    }
}

impl Ortho {
    fn format_2d_slice(&self, f: &mut fmt::Formatter<'_>, prefix: &[usize], rows: usize, cols: usize) -> fmt::Result {
        // Find the maximum width needed for display
        let max_width = self.payload.iter()
            .filter_map(|opt| opt.as_ref())
            .map(|v| format!("{}", v).len())
            .max()
            .unwrap_or(1)
            .max(4); // At least 4 characters for "None"
        
        // Print the 2D slice
        for row in 0..rows {
            for col in 0..cols {
                let mut coords = prefix.to_vec();
                coords.push(row);
                coords.push(col);
                
                if col > 0 {
                    write!(f, " ")?;
                }
                
                if let Some(linear_idx) = self.get_index_at_coord(&coords) {
                    if linear_idx < self.payload.len() {
                        match self.payload[linear_idx] {
                            Some(val) => write!(f, "{:>width$}", val, width = max_width)?,
                            None => write!(f, "{:>width$}", "·", width = max_width)?,
                        }
                    } else {
                        write!(f, "{:>width$}", "?", width = max_width)?;
                    }
                } else {
                    write!(f, "{:>width$}", "?", width = max_width)?;
                }
            }
            if row < rows - 1 {
                writeln!(f)?;
            }
        }
        Ok(())
    }
    
    fn format_tiled(&self, f: &mut fmt::Formatter<'_>, rows: usize, cols: usize) -> fmt::Result {
        // For dimensions beyond 2D, we create tiles
        let higher_dims = &self.dims[..self.dims.len() - 2];
        
        // Generate all tile coordinates
        let mut tile_coords = Vec::new();
        self.generate_tile_coords(higher_dims, &mut vec![], &mut tile_coords);
        
        for (tile_idx, coords) in tile_coords.iter().enumerate() {
            if tile_idx > 0 {
                writeln!(f)?;
                writeln!(f)?;
            }
            
            // Print which slice we're showing
            write!(f, "[")?;
            for (i, axis_names) in ["dim0", "dim1", "dim2", "dim3", "dim4", "dim5", "dim6", "dim7"]
                .iter()
                .take(coords.len())
                .enumerate()
            {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}={}", axis_names, coords[i])?;
            }
            writeln!(f, "]")?;
            
            self.format_2d_slice(f, coords, rows, cols)?;
        }
        
        Ok(())
    }
    
    fn generate_tile_coords(&self, dims: &[usize], current: &mut Vec<usize>, results: &mut Vec<Vec<usize>>) {
        if current.len() == dims.len() {
            results.push(current.clone());
            return;
        }
        
        let dim_idx = current.len();
        for i in 0..dims[dim_idx] {
            current.push(i);
            self.generate_tile_coords(dims, current, results);
            current.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.version, 1);
        assert_eq!(ortho.dims, vec![2,2]);
        assert_eq!(ortho.payload, vec![None, None, None, None]);
    }

    #[test]
    fn test_get_current() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.get_current_position(), 0);

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![None, None, None, None],
            }
            .get_current_position(),
            0
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(1), None, None, None],
            }
            .get_current_position(),
            1
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(1), Some(2), None, None],
            }
            .get_current_position(),
            2
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(1), Some(2), Some(3), None],
            }
            .get_current_position(),
            3
        );
    }

    #[test]
    fn test_get_insert_position() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.get_insert_position(5), 0);

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(15), None, None],
            }
            .get_insert_position(14),
            0
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(15), None, None],
            }
            .get_insert_position(20),
            1
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), None],
            }
            .get_insert_position(5),
            0
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), None],
            }
            .get_insert_position(15),
            1
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), None],
            }
            .get_insert_position(1000),
            2
        );
    }

    #[test]
    fn test_add_simple() {
        let ortho = Ortho::new(1);
        let orthos = ortho.add(10, 1);
        assert_eq!(
            orthos,
            vec![Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(10), None, None, None],
            }]
        );
    }

    #[test]
    fn test_add_multiple() {
        let ortho = Ortho::new(1);
        let orthos1 = ortho.add(1, 1);
        let ortho = &orthos1[0];
        let orthos2 = ortho.add(2, 1);
        assert_eq!(
            orthos2,
            vec![Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(1), Some(2), None, None],
            }]
        );
    }

    #[test]
    fn test_add_order_independent_ids() {
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(1);
        let ortho1 = &ortho1.add(1, 1)[0];
        let ortho2 = &ortho2.add(1, 1)[0];
        assert_eq!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(2, 1)[0];
        let ortho2 = &ortho2.add(3, 1)[0];
        assert_ne!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(3, 1)[0];
        let ortho2 = &ortho2.add(2, 1)[0];
        assert_eq!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(4, 1)[0];
        let ortho2 = &ortho2.add(4, 1)[0];
        assert_eq!(ortho1.id(), ortho2.id());
    }

    #[test]
    fn test_add_shape_expansion() {
        let ortho = Ortho::new(1);
        let orthos = ortho.add(1, 1);
        let ortho = &orthos[0];
        let orthos2 = ortho.add(2, 1);
        let ortho = &orthos2[0];
        assert_eq!(ortho.dims, vec![2, 2]);
        assert_eq!(ortho.payload, vec![Some(1), Some(2), None, None]);
        let orthos3 = ortho.add(3, 1);
        let ortho = &orthos3[0];
        assert_eq!(ortho.dims, vec![2, 2]);
        assert_eq!(ortho.payload, vec![Some(1), Some(2), Some(3), None]);
    }

    #[test]
    fn test_up_and_over_expansions_full_coverage() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(1, 1)[0];
        let ortho = &ortho.add(2, 1)[0];
        let ortho = &ortho.add(3, 1)[0];

        let expansions = ortho.add(4, 1);
        assert_eq!(
            expansions,
            vec![
                Ortho {
                    version: 1,
                    dims: vec![3, 2],
                    payload: vec![Some(1), Some(2), Some(3), Some(4), None, None],
                },
                Ortho {
                    version: 1,
                    dims: vec![2, 2, 2],
                    payload: vec![Some(1), Some(2), Some(3), None, Some(4), None, None, None],
                }
            ]
        );
    }

    #[test]
    fn test_insert_position_middle() {
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), Some(20), None, None],
        };
        let orthos = ortho.add(15, 1);
        assert_eq!(
            orthos,
            vec![Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(10), Some(15), Some(20), None],
            }]
        );
    }

    #[test]
    fn test_insert_position_middle_and_reorg() {
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), None, Some(20), Some(30)],
        };

        let mut orthos = ortho.add(15, 1);
        orthos.sort_by(|a, b| a.dims.cmp(&b.dims));
        assert_eq!(
            orthos,
            vec![
                Ortho {
                    version: 1,
                    dims: vec![2, 2, 2],
                    payload: vec![
                        Some(10),
                        None,
                        Some(15),
                        Some(20),
                        None,
                        None,
                        Some(30),
                        None
                    ],
                },
                Ortho {
                    version: 1,
                    dims: vec![3, 2],
                    payload: vec![Some(10), Some(15), Some(20), Some(30), None, None],
                },
            ]
        );
    }

    #[test]
    fn test_get_requirements_empty() {
        let ortho = Ortho::new(1);
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, Vec::<Vec<usize>>::new());
    }

    #[test]
    fn test_get_requirements_simple() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10, 1)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, vec![vec![10]]);
    }

    #[test]
    fn test_get_requirements_multiple() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10, 1)[0];
        let ortho = &ortho.add(20, 1)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, vec![20]);
        assert_eq!(required, vec![vec![10]]);
    }

    #[test]
    fn test_get_requirements_full() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10, 1)[0];
        let ortho = &ortho.add(20, 1)[0];
        let ortho = &ortho.add(30, 1)[0];
        let ortho = &ortho.add(40, 1)[0];

        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, vec![40]);
        assert_eq!(required, vec![vec![10, 30]]);
    }

    #[test]
    fn test_get_requirements_expansion() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(1, 1)[0];
        let ortho = &ortho.add(2, 1)[0];
        let ortho = &ortho.add(3, 1)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, vec![vec![2], vec![3]]);
    }

    #[test]
    fn test_get_requirements_order_independent() {
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(1);
        let ortho1 = &ortho1.add(1, 1)[0];
        let ortho2 = &ortho2.add(1, 1)[0];
        let ortho1 = &ortho1.add(2, 1)[0];
        let ortho2 = &ortho2.add(3, 1)[0];
        let ortho1 = &ortho1.add(3, 1)[0];
        let ortho2 = &ortho2.add(2, 1)[0];
        let (forbidden1, required1) = ortho1.get_requirements();
        let (forbidden2, required2) = ortho2.get_requirements();
        assert_eq!(forbidden1, forbidden2);
        assert_eq!(required1, vec![vec![2], vec![3]]);
        assert_eq!(required2, vec![vec![2], vec![3]]);
    }

    #[test]
    fn test_id_version_behavior() {
        // Test that empty orthos with different versions have different IDs
        let ortho_v1 = Ortho::new(1);
        let ortho_v2 = Ortho::new(2);
        assert_ne!(ortho_v1.id(), ortho_v2.id());

        // Test that orthos with different versions but same contents have same IDs
        let ortho_v1_with_content = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
        };
        let ortho_v2_with_content = Ortho {
            version: 2,
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
        };
        assert_eq!(ortho_v1_with_content.id(), ortho_v2_with_content.id());

        // Test that orthos with same version but different contents have different IDs
        let ortho_v1_content_a = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
        };
        let ortho_v1_content_b = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(20), None, None, None],
        };
        assert_ne!(ortho_v1_content_a.id(), ortho_v1_content_b.id());
    }

    #[test]
    fn test_id_collision_for_different_payloads() {
        use super::*;
        // These are the payloads seen in the logs
        let ortho0 = Ortho {
            version: 2,
            dims: vec![2, 2],
            payload: vec![Some(0), None, None, None],
        };
        let ortho1 = Ortho {
            version: 2,
            dims: vec![2, 2],
            payload: vec![Some(1), None, None, None],
        };
        let ortho2 = Ortho {
            version: 2,
            dims: vec![2, 2],
            payload: vec![Some(2), None, None, None],
        };
        let ortho3 = Ortho {
            version: 2,
            dims: vec![2, 2],
            payload: vec![Some(3), None, None, None],
        };
        let ortho4 = Ortho {
            version: 2,
            dims: vec![2, 2],
            payload: vec![Some(4), None, None, None],
        };
        let ortho5 = Ortho {
            version: 2,
            dims: vec![2, 2],
            payload: vec![Some(5), None, None, None],
        };
        let ids = vec![
            ortho0.id(),
            ortho1.id(),
            ortho2.id(),
            ortho3.id(),
            ortho4.id(),
            ortho5.id(),
        ];
        // If there are collisions, there will be fewer unique IDs than payloads
        let unique_ids: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(
            unique_ids.len(),
            ids.len(),
            "Ortho::id() should be unique for different payloads, but got collisions: {:?}",
            ids
        );
    }

    #[test]
    fn test_canonicalization_invariant_axis_permutation() {
        // This test is intended to expose the canonicalization issue: inserting the two axis tokens
        // in different orders should yield (after inserting the 4th token that triggers expansion)
        // an equivalent canonical set of children. Currently (with the swap removed) they differ.
        // Axis tokens are the 2nd and 3rd overall inserts into base dims [2,2].
        // Path 1: a < b < c
        let mut o1 = Ortho::new(1);
        o1 = o1.add(10, 1).pop().unwrap(); // a
        o1 = o1.add(20, 1).pop().unwrap(); // b
        o1 = o1.add(30, 1).pop().unwrap(); // c
        // Path 2: a < c but b < c (second and third swapped relative to path 1)
        let mut o2 = Ortho::new(1);
        o2 = o2.add(10, 1).pop().unwrap(); // a
        o2 = o2.add(30, 1).pop().unwrap(); // c
        o2 = o2.add(20, 1).pop().unwrap(); // b (unsorted axis order)
        // Insert 4th token to force expansion candidates
        let children1 = o1.add(40, 1);
        let children2 = o2.add(40, 1);
        // Normalize each child to (dims, filled_values_in_order)
        fn norm(o: &Ortho) -> (Vec<usize>, Vec<usize>) {
            (o.dims.clone(), o.payload.iter().filter_map(|x| *x).collect())
        }
        let mut norms1: Vec<_> = children1.iter().map(norm).collect();
        let mut norms2: Vec<_> = children2.iter().map(norm).collect();
        norms1.sort();
        norms2.sort();
        assert_eq!(norms1, norms2, "Canonicalization mismatch between axis insertion orders. norms1={:?} norms2={:?}", norms1, norms2);
    }
    
    #[test]
    fn test_display_2d_simple() {
        // Test a simple 2x2 ortho
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(1), Some(2), Some(3), Some(4)],
        };
        let display_str = format!("{}", ortho);
        // Should display as a 2x2 grid
        assert!(display_str.contains("1"));
        assert!(display_str.contains("2"));
        assert!(display_str.contains("3"));
        assert!(display_str.contains("4"));
    }
    
    #[test]
    fn test_display_2d_with_nones() {
        // Test a 2x2 ortho with some None values
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), Some(20), None, None],
        };
        let display_str = format!("{}", ortho);
        assert!(display_str.contains("10"));
        assert!(display_str.contains("20"));
        // None values should display as dots
        assert!(display_str.contains("·"));
    }
    
    #[test]
    fn test_display_3x2() {
        // Test a 3x2 ortho
        let ortho = Ortho {
            version: 1,
            dims: vec![3, 2],
            payload: vec![Some(1), Some(2), Some(3), Some(4), Some(5), None],
        };
        let display_str = format!("{}", ortho);
        assert!(display_str.contains("1"));
        assert!(display_str.contains("5"));
        // Should have 3 rows
        let lines: Vec<&str> = display_str.lines().collect();
        assert_eq!(lines.len(), 3);
    }
    
    #[test]
    fn test_display_3d_tiled() {
        // Test a 2x2x2 ortho (3D)
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2, 2],
            payload: vec![Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), Some(7), None],
        };
        let display_str = format!("{}", ortho);
        
        // Should have tile labels
        assert!(display_str.contains("dim0"));
        
        // Should contain all values
        for i in 1..=7 {
            assert!(display_str.contains(&i.to_string()));
        }
        
        // Should have multiple tiles separated by blank lines
        assert!(display_str.contains("[dim0=0]") || display_str.contains("dim0=0"));
        assert!(display_str.contains("[dim0=1]") || display_str.contains("dim0=1"));
    }
    
    #[test]
    fn test_display_visual_output() {
        // This test is for visual verification
        println!("\n=== 2x2 Ortho ===");
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), Some(20), Some(30), None],
        };
        println!("{}", ortho);
        
        println!("\n=== 3x2 Ortho ===");
        let ortho = Ortho {
            version: 1,
            dims: vec![3, 2],
            payload: vec![Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)],
        };
        println!("{}", ortho);
        
        println!("\n=== 2x2x2 Ortho (3D) ===");
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2, 2],
            payload: vec![Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), Some(7), Some(8)],
        };
        println!("{}", ortho);
        
        println!("\n=== 2x3x2 Ortho (3D with different sizes) ===");
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 3, 2],
            payload: vec![Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), Some(7), Some(8), Some(9), Some(10), Some(11), Some(12)],
        };
        println!("{}", ortho);
        
        println!("\n=== 2x2x2x2 Ortho (4D) ===");
        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2, 2, 2],
            payload: vec![
                Some(1), Some(2), Some(3), Some(4),
                Some(5), Some(6), Some(7), Some(8),
                Some(9), Some(10), Some(11), Some(12),
                Some(13), Some(14), Some(15), Some(16)
            ],
        };
        println!("{}", ortho);
    }
}
