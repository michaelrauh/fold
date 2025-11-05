use crate::spatial;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use bincode::Encode;
use bincode::Decode;

#[derive(PartialEq, Debug, Clone, Encode, Decode)]
pub struct Ortho {
    version: usize,
    dims: Vec<usize>,
    payload: Vec<Option<usize>>, // length == capacity(dims)
    // removed precomputed id field; id now computed on demand
}

impl Ortho {
    fn compute_id(version: usize, dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
        // Empty (first None at 0 => all None) still uses version so distinct across versions
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
        // Determine insertion index as first None (walk forward)
        let insertion_index = self.get_current_position();
        // Count remaining empty slots (walk) to determine if expansion follows this insert
        let total_empty = self.payload.iter().filter(|x| x.is_none()).count();
        if total_empty == 1 { // This insert triggers expansion
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
        if insertion_index == 2 && self.dims.as_slice() == [2, 2] { // axis canonicalization step
            let mut new_payload: Vec<Option<usize>> = self.payload.clone();
            new_payload[insertion_index] = Some(value); // INSERT (bug fix)
            // Canonicalize axis token order (positions 1 & 2)
            if let (Some(second), Some(third)) = (new_payload[1], new_payload[2]) {
                if second > third { new_payload[1] = Some(third); new_payload[2] = Some(second); }
            }
            return vec![Ortho { version, dims: self.dims.clone(), payload: new_payload }];
        }
        // Normal (non-expansion) path
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
        let mut out = Vec::with_capacity(expansions.len());
        for (new_dims_vec, new_capacity, reorg) in expansions.into_iter() {
            let mut new_payload = vec![None; new_capacity];
            for (i, &pos) in reorg.iter().enumerate() { new_payload[pos] = ortho.payload.get(i).cloned().flatten(); }
            if let Some(insert_pos) = new_payload.iter().position(|x| x.is_none()) { new_payload[insert_pos] = Some(value); }
            else if !new_payload.is_empty() { let last = new_payload.len() - 1; new_payload[last] = Some(value); }
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
    /// Rebuild this ortho up to a specific target position.
    /// This creates a new ortho containing only the values up to (but not including) target_position.
    /// Returns None if target_position is invalid (> current_position).
    /// Returns empty ortho if target_position is 0.
    pub fn rebuild_to_position(&self, target_position: usize) -> Option<Self> {
        let current_position = self.get_current_position();
        
        // Validate target position
        if target_position > current_position {
            return None;
        }
        
        if target_position == 0 {
            // Return empty ortho
            return Some(Ortho::new(self.version));
        }
        
        if target_position == current_position {
            // No rebuild needed - return a clone
            return Some(self.clone());
        }
        
        // Collect values up to target position
        let mut values_to_keep: Vec<usize> = Vec::new();
        for (idx, &opt_val) in self.payload.iter().enumerate() {
            if idx < target_position {
                if let Some(val) = opt_val {
                    values_to_keep.push(val);
                }
            }
        }
        
        // If no values to keep, return empty ortho
        if values_to_keep.is_empty() {
            return Some(Ortho::new(self.version));
        }
        
        // Rebuild ortho by adding values one at a time
        // We need to track the target shape to choose the right branch when expansion occurs
        let mut rebuilt = Ortho::new(self.version);
        for (i, &value) in values_to_keep.iter().enumerate() {
            // Add the value
            let candidates = rebuilt.add(value, self.version);
            if candidates.is_empty() {
                // This shouldn't happen, but handle gracefully
                return None;
            }
            
            // If this is the last value, we need to determine the target shape
            if i == values_to_keep.len() - 1 {
                // Choose the candidate that has the appropriate capacity/shape
                // Base shape [2,2] has capacity 4. If target_position <= 4, use base shape
                rebuilt = if target_position <= 4 {
                    // Below or at base capacity - should be base [2,2]
                    candidates.iter()
                        .find(|c| c.dims == vec![2, 2])
                        .unwrap_or(&candidates[0])
                        .clone()
                } else {
                    // Above base capacity - need expanded shape
                    // Choose candidate that matches current ortho's dimensional structure
                    candidates.iter()
                        .find(|c| c.dims.len() == self.dims.len())
                        .unwrap_or(&candidates[0])
                        .clone()
                };
            } else {
                // Not the last value - just take the first candidate (usually only one)
                rebuilt = candidates[0].clone();
            }
        }
        
        Some(rebuilt)
    }
    
    pub fn get_requirements(&self) -> (Vec<usize>, Vec<Vec<usize>>) {
        let pos = self.get_current_position(); // next insert position
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
    pub(crate) fn set_version(&self, version: usize) -> Ortho {
        Ortho { version, dims: self.dims.clone(), payload: self.payload.clone() }
    }
    pub fn payload(&self) -> &Vec<Option<usize>> { &self.payload }
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
                    payload: vec![Some(1), Some(2), Some(3), Some(4), None, None, None, None],
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
                        Some(15),
                        None,
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
    fn test_rebuild_to_any_point_in_search_tree() {
        // Demonstrate that rebuild_to_position can reconstruct any point in a search tree
        // by rebuilding based on payload positions
        
        // Build a search tree
        let root = Ortho::new(1);
        
        // Simple linear path: 10 -> 20 -> 30 -> 40
        let n1 = root.add(10, 1)[0].clone();
        let n2 = n1.add(20, 1)[0].clone();
        let n3 = n2.add(30, 1)[0].clone();
        let n4 = n3.add(40, 1)[0].clone();
        
        // Demonstrate we can rebuild from n4 to any earlier point
        
        // Rebuild to position 3 (should have first 3 payload values: [10, 20, 30])
        let rebuild_3 = n4.rebuild_to_position(3).expect("Should rebuild to pos 3");
        assert_eq!(rebuild_3.get_current_position(), 3);
        let mut vals_3: Vec<usize> = rebuild_3.payload().iter()
            .filter_map(|&v| v).collect();
        vals_3.sort();
        assert_eq!(vals_3, vec![10, 20, 30], "Should have first 3 values");
        
        // Rebuild to position 2 (should have first 2 payload values: [10, 20])
        let rebuild_2 = n4.rebuild_to_position(2).expect("Should rebuild to pos 2");
        assert_eq!(rebuild_2.get_current_position(), 2);
        let mut vals_2: Vec<usize> = rebuild_2.payload().iter()
            .filter_map(|&v| v).collect();
        vals_2.sort();
        assert_eq!(vals_2, vec![10, 20], "Should have first 2 values");
        
        // Rebuild to position 1 (should have first payload value: [10])
        let rebuild_1 = n4.rebuild_to_position(1).expect("Should rebuild to pos 1");
        assert_eq!(rebuild_1.get_current_position(), 1);
        assert_eq!(rebuild_1.payload()[0], Some(10), "Should have first value");
        
        // Test with canonicalization path: 10 -> 50 -> 30 (causes swap)
        let c1 = root.add(10, 1)[0].clone();
        let c2 = c1.add(50, 1)[0].clone();
        let c3 = c2.add(30, 1)[0].clone(); // Triggers canonicalization: 30 < 50
        
        // After canonicalization, payload is [10, 30, 50, None]
        assert_eq!(c3.payload()[0], Some(10));
        assert_eq!(c3.payload()[1], Some(30));
        assert_eq!(c3.payload()[2], Some(50));
        
        // Rebuild to position 2 - takes first 2 payload values [10, 30]
        let c_rebuild_2 = c3.rebuild_to_position(2).expect("Should rebuild to pos 2");
        assert_eq!(c_rebuild_2.get_current_position(), 2);
        assert_eq!(c_rebuild_2.payload()[0], Some(10));
        assert_eq!(c_rebuild_2.payload()[1], Some(30));
        
        // Rebuild to position 1 - takes first payload value [10]
        let c_rebuild_1 = c3.rebuild_to_position(1).expect("Should rebuild to pos 1");
        assert_eq!(c_rebuild_1.get_current_position(), 1);
        assert_eq!(c_rebuild_1.payload()[0], Some(10));
        
        // Key insight: rebuild_to_position operates on payload positions,
        // collecting the first N values from the current payload and rebuilding them.
        // This correctly handles all cases including canonicalization.
    }

    #[test]
    fn test_rebuild_places_impacted_key_furthest_right() {
        // Prove that when rebuilding an ortho to a specific position,
        // the value at that position ends up at the "most advanced position"
        // (furthest right, ready for next add)
        
        // Build an ortho with values [10, 20, 30, 40]
        let ortho = Ortho::new(1);
        let ortho = ortho.add(10, 1)[0].clone();
        let ortho = ortho.add(20, 1)[0].clone();
        let ortho = ortho.add(30, 1)[0].clone();
        let ortho = ortho.add(40, 1)[0].clone();
        
        // Current position should be 4 (all 4 slots filled in base [2,2])
        assert_eq!(ortho.get_current_position(), 4);
        
        // Rebuild to position 3 - keeps [10, 20, 30]
        let rebuild3 = ortho.rebuild_to_position(3).expect("Should rebuild");
        assert_eq!(rebuild3.get_current_position(), 3, "Should have 3 values");
        // The value 30 should be at position 2 (furthest right)
        assert_eq!(rebuild3.payload()[2], Some(30), "Last value should be at rightmost position");
        assert_eq!(rebuild3.payload()[3], None, "Position after last value should be None");
        
        // Rebuild to position 2 - keeps [10, 20]
        let rebuild2 = ortho.rebuild_to_position(2).expect("Should rebuild");
        assert_eq!(rebuild2.get_current_position(), 2, "Should have 2 values");
        // The value 20 should be at position 1 (furthest right)
        assert_eq!(rebuild2.payload()[1], Some(20), "Last value should be at rightmost position");
        assert_eq!(rebuild2.payload()[2], None, "Position after last value should be None");
        
        // Rebuild to position 1 - keeps [10]
        let rebuild1 = ortho.rebuild_to_position(1).expect("Should rebuild");
        assert_eq!(rebuild1.get_current_position(), 1, "Should have 1 value");
        // The value 10 should be at position 0 (furthest right for 1 value)
        assert_eq!(rebuild1.payload()[0], Some(10), "Last value should be at rightmost position");
        assert_eq!(rebuild1.payload()[1], None, "Position after last value should be None");
        
        // This demonstrates that after rebuild, the last included value is always
        // at the "most advanced position" - ready for the next add operation
    }

    #[test]
    fn test_rebuild_reverses_through_full_add_tree() {
        // Prove that rebuild_to_position can handle the full add tree
        // with different canonicalization patterns
        
        // Start with empty ortho
        let empty = Ortho::new(1);
        
        // Build a tree with two different canonicalization patterns
        // Pattern 1: ascending order (10 < 30 < 50)
        let p1_step1 = empty.add(10, 1)[0].clone();
        let p1_step2 = p1_step1.add(30, 1)[0].clone();
        let p1_step3 = p1_step2.add(50, 1)[0].clone();
        
        // Pattern 2: canonicalization order (10 < 50 < 30, triggers swap)
        let p2_step1 = empty.add(10, 1)[0].clone();
        let p2_step2 = p2_step1.add(50, 1)[0].clone();
        let p2_step3 = p2_step2.add(30, 1)[0].clone(); // This triggers canonicalization
        
        // Verify canonicalization happened in pattern 2
        assert_eq!(p2_step3.payload()[0], Some(10));
        assert_eq!(p2_step3.payload()[1], Some(30)); // Swapped to canonical order
        assert_eq!(p2_step3.payload()[2], Some(50));
        
        // Now prove we can rebuild from p1_step3 back through the tree
        let p1_rebuild2 = p1_step3.rebuild_to_position(2).expect("Should rebuild");
        assert_eq!(p1_rebuild2.get_current_position(), 2);
        let mut vals_p1_r2: Vec<usize> = p1_rebuild2.payload().iter()
            .filter_map(|&v| v).collect();
        vals_p1_r2.sort();
        assert_eq!(vals_p1_r2, vec![10, 30], "Should have first 2 values");
        
        let p1_rebuild1 = p1_step3.rebuild_to_position(1).expect("Should rebuild");
        assert_eq!(p1_rebuild1.get_current_position(), 1);
        assert_eq!(p1_rebuild1.payload()[0], Some(10), "Should have first value");
        
        let p1_rebuild0 = p1_step3.rebuild_to_position(0).expect("Should rebuild");
        assert_eq!(p1_rebuild0.get_current_position(), 0);
        assert_eq!(p1_rebuild0.payload()[0], None, "Should be empty");
        
        // Now prove we can rebuild from p2_step3 back through the tree
        // Even though it went through canonicalization
        let p2_rebuild2 = p2_step3.rebuild_to_position(2).expect("Should rebuild");
        assert_eq!(p2_rebuild2.get_current_position(), 2);
        let mut vals_p2_r2: Vec<usize> = p2_rebuild2.payload().iter()
            .filter_map(|&v| v).collect();
        vals_p2_r2.sort();
        assert_eq!(vals_p2_r2, vec![10, 30], "Should have first 2 values in canonical order");
        
        let p2_rebuild1 = p2_step3.rebuild_to_position(1).expect("Should rebuild");
        assert_eq!(p2_rebuild1.get_current_position(), 1);
        assert_eq!(p2_rebuild1.payload()[0], Some(10), "Should have first value");
        
        let p2_rebuild0 = p2_step3.rebuild_to_position(0).expect("Should rebuild");
        assert_eq!(p2_rebuild0.get_current_position(), 0);
        assert_eq!(p2_rebuild0.payload()[0], None, "Should be empty");
        
        // Both patterns rebuild cleanly, proving rebuild_to_position handles
        // any path through the add tree correctly
    }

    #[test]
    fn test_rebuild_to_multiple_impacted_positions() {
        // Prove that we can rebuild a single ortho to multiple different impacted positions,
        // and each time the rightmost value matches the impacted interner key.
        // This demonstrates why we need to rebuild to ALL impacted positions, not just the earliest.
        
        // Create an ortho with values at indices [0, 1, 2, 3]
        let ortho = Ortho::new(1);
        let ortho = ortho.add(100, 1)[0].clone();  // Index 100 at position 0
        let ortho = ortho.add(200, 1)[0].clone();  // Index 200 at position 1
        let ortho = ortho.add(300, 1)[0].clone();  // Index 300 at position 2
        let ortho = ortho.add(400, 1)[0].clone();  // Index 400 at position 3
        
        assert_eq!(ortho.get_current_position(), 4);
        assert_eq!(ortho.payload()[0], Some(100));
        assert_eq!(ortho.payload()[1], Some(200));
        assert_eq!(ortho.payload()[2], Some(300));
        assert_eq!(ortho.payload()[3], Some(400));
        
        // Scenario: Interner changed keys at indices 100 and 300
        // (e.g., new completions added for those keys)
        // We need to rebuild to BOTH positions to explore both changed paths
        
        // Rebuild to position 1 (so index 100 is furthest right)
        let rebuild_to_1 = ortho.rebuild_to_position(1).expect("Should rebuild to position 1");
        assert_eq!(rebuild_to_1.get_current_position(), 1);
        assert_eq!(rebuild_to_1.payload()[0], Some(100), "Index 100 should be at rightmost position");
        assert_eq!(rebuild_to_1.payload()[1], None, "Position 1 should be empty");
        // Next add operation will happen at position 1, using new completions for index 100
        
        // Rebuild to position 3 (so index 300 is furthest right)
        let rebuild_to_3 = ortho.rebuild_to_position(3).expect("Should rebuild to position 3");
        assert_eq!(rebuild_to_3.get_current_position(), 3);
        assert_eq!(rebuild_to_3.payload()[0], Some(100));
        assert_eq!(rebuild_to_3.payload()[1], Some(200));
        assert_eq!(rebuild_to_3.payload()[2], Some(300), "Index 300 should be at rightmost position");
        assert_eq!(rebuild_to_3.payload()[3], None, "Position 3 should be empty");
        // Next add operation will happen at position 3, using new completions for index 300
        
        // This proves the approach: the rightmost item in each rebuilt ortho
        // corresponds to the changed interner key. By creating both rewound orthos,
        // we ensure the search explores the new paths available from BOTH changed keys.
    }

}
