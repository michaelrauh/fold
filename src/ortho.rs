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
    pub fn subtract(&self) -> Option<Self> {
        // Find the last filled position (position of last Some value before first None)
        let current_position = self.get_current_position();
        if current_position == 0 {
            // Nothing to subtract - ortho is empty
            return None;
        }
        
        // Rebuild approach: collect values to keep, then rebuild from scratch
        // This is simpler than the contraction logic and handles all cases uniformly
        
        // Collect all values except the last one, in order
        let mut values_to_keep: Vec<usize> = Vec::new();
        for (idx, &opt_val) in self.payload.iter().enumerate() {
            if idx < current_position - 1 {
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
        let mut rebuilt = Ortho::new(self.version);
        for &value in &values_to_keep {
            // Add the value and take the first result (canonicalized path)
            let candidates = rebuilt.add(value, self.version);
            if candidates.is_empty() {
                // This shouldn't happen, but handle gracefully
                return None;
            }
            rebuilt = candidates[0].clone();
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
    fn test_subtract_empty_ortho() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.subtract(), None);
    }

    #[test]
    fn test_subtract_single_value() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10, 1)[0];
        let subtracted = ortho.subtract().expect("Should be able to subtract");
        assert_eq!(subtracted.payload, vec![None, None, None, None]);
        assert_eq!(subtracted.dims, vec![2, 2]);
        assert_eq!(subtracted.version, 1);
    }

    #[test]
    fn test_subtract_multiple_values() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10, 1)[0];
        let ortho = &ortho.add(20, 1)[0];
        let ortho = &ortho.add(30, 1)[0];
        
        let subtracted = ortho.subtract().expect("Should be able to subtract");
        assert_eq!(subtracted.payload, vec![Some(10), Some(20), None, None]);
        
        let subtracted2 = subtracted.subtract().expect("Should be able to subtract again");
        assert_eq!(subtracted2.payload, vec![Some(10), None, None, None]);
        
        let subtracted3 = subtracted2.subtract().expect("Should be able to subtract once more");
        assert_eq!(subtracted3.payload, vec![None, None, None, None]);
        
        let subtracted4 = subtracted3.subtract();
        assert_eq!(subtracted4, None);
    }

    #[test]
    fn test_subtract_preserves_dims() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10, 1)[0];
        let ortho = &ortho.add(20, 1)[0];
        
        let subtracted = ortho.subtract().expect("Should be able to subtract");
        assert_eq!(subtracted.dims, vec![2, 2]);
    }

    #[test]
    fn test_subtract_preserves_version() {
        let ortho = Ortho::new(5);
        let ortho = &ortho.add(10, 5)[0];
        
        let subtracted = ortho.subtract().expect("Should be able to subtract");
        assert_eq!(subtracted.version, 5);
    }

    #[test]
    fn test_subtract_and_add_roundtrip() {
        let ortho = Ortho::new(1);
        let ortho1 = &ortho.add(10, 1)[0];
        let ortho2 = &ortho1.add(20, 1)[0];
        
        let subtracted = ortho2.subtract().expect("Should be able to subtract");
        assert_eq!(subtracted.payload, ortho1.payload);
        assert_eq!(subtracted.dims, ortho1.dims);
        
        // Add the same value back
        let re_added = &subtracted.add(20, 1)[0];
        assert_eq!(re_added.payload, ortho2.payload);
        assert_eq!(re_added.dims, ortho2.dims);
    }

    #[test]
    fn test_subtract_with_canonicalized_ortho() {
        // Create an ortho that goes through canonicalization
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10, 1)[0]; // position 0
        let ortho = &ortho.add(30, 1)[0]; // position 1
        let ortho = &ortho.add(20, 1)[0]; // position 2 - triggers canonicalization (20 < 30)
        
        // After canonicalization, positions 1 and 2 should be swapped if 20 < 30
        assert_eq!(ortho.payload[0], Some(10));
        assert_eq!(ortho.payload[1], Some(20));
        assert_eq!(ortho.payload[2], Some(30));
        
        // Subtract should remove position 2
        let subtracted = ortho.subtract().expect("Should be able to subtract");
        assert_eq!(subtracted.payload[0], Some(10));
        assert_eq!(subtracted.payload[1], Some(20));
        assert_eq!(subtracted.payload[2], None);
    }

    #[test]
    fn test_subtract_from_expanded_ortho_contracts() {
        // Build up an ortho through multiple adds with different values to test complex canonicalization
        let ortho0 = Ortho::new(1);
        assert_eq!(ortho0.dims(), &vec![2, 2]);
        assert_eq!(ortho0.get_current_position(), 0);
        
        // Add first value
        let ortho1 = ortho0.add(10, 1)[0].clone();
        assert_eq!(ortho1.dims(), &vec![2, 2]);
        assert_eq!(ortho1.get_current_position(), 1);
        assert_eq!(ortho1.payload()[0], Some(10));
        
        // Add second value (larger)
        let ortho2 = ortho1.add(30, 1)[0].clone();
        assert_eq!(ortho2.dims(), &vec![2, 2]);
        assert_eq!(ortho2.get_current_position(), 2);
        assert_eq!(ortho2.payload()[0], Some(10));
        assert_eq!(ortho2.payload()[1], Some(30));
        
        // Add third value (triggers canonicalization since 20 < 30)
        let ortho3 = ortho2.add(20, 1)[0].clone();
        assert_eq!(ortho3.dims(), &vec![2, 2]);
        assert_eq!(ortho3.get_current_position(), 3);
        assert_eq!(ortho3.payload()[0], Some(10));
        assert_eq!(ortho3.payload()[1], Some(20)); // Canonicalized: swapped with 30
        assert_eq!(ortho3.payload()[2], Some(30));
        
        // Add fourth value (triggers expansion to [3,2])
        let expanded_orthos = ortho3.add(40, 1);
        let ortho4 = expanded_orthos.iter().find(|o| o.dims() == &vec![3, 2])
            .expect("Should have [3,2] expansion").clone();
        assert_eq!(ortho4.dims(), &vec![3, 2]);
        assert_eq!(ortho4.get_current_position(), 4);
        
        // Now test subtracting back through the states
        // Subtract from ortho4 should contract back to ortho3 state
        let back_to_3 = ortho4.subtract().expect("Should subtract from expanded");
        assert_eq!(back_to_3.dims(), &vec![2, 2], "Should contract back to [2,2]");
        assert_eq!(back_to_3.get_current_position(), 3);
        let mut values3: Vec<usize> = back_to_3.payload().iter().filter_map(|&v| v).collect();
        values3.sort();
        assert_eq!(values3, vec![10, 20, 30], "Should have original 3 values");
        
        // Subtract again should go back to ortho2 state
        let back_to_2 = back_to_3.subtract().expect("Should subtract again");
        assert_eq!(back_to_2.dims(), &vec![2, 2]);
        assert_eq!(back_to_2.get_current_position(), 2);
        assert_eq!(back_to_2.payload()[0], Some(10));
        assert_eq!(back_to_2.payload()[1], Some(20)); // Should preserve the canonicalized value
        
        // Subtract again should go back to ortho1 state
        let back_to_1 = back_to_2.subtract().expect("Should subtract again");
        assert_eq!(back_to_1.dims(), &vec![2, 2]);
        assert_eq!(back_to_1.get_current_position(), 1);
        assert_eq!(back_to_1.payload()[0], Some(10));
        assert_eq!(back_to_1.payload()[1], None);
        
        // Subtract once more should go back to ortho0 state
        let back_to_0 = back_to_1.subtract().expect("Should subtract to empty");
        assert_eq!(back_to_0.dims(), &vec![2, 2]);
        assert_eq!(back_to_0.get_current_position(), 0);
        assert_eq!(back_to_0.payload()[0], None);
        
        // One more subtract should return None
        assert_eq!(back_to_0.subtract(), None, "Subtracting from empty should return None");
    }

    #[test]
    fn test_subtract_from_expanded_ortho_222_contracts() {
        // Build up an ortho through multiple adds with different values to test complex canonicalization
        let ortho0 = Ortho::new(1);
        assert_eq!(ortho0.dims(), &vec![2, 2]);
        assert_eq!(ortho0.get_current_position(), 0);
        
        // Add first value
        let ortho1 = ortho0.add(5, 1)[0].clone();
        assert_eq!(ortho1.dims(), &vec![2, 2]);
        assert_eq!(ortho1.get_current_position(), 1);
        assert_eq!(ortho1.payload()[0], Some(5));
        
        // Add second value (larger)
        let ortho2 = ortho1.add(25, 1)[0].clone();
        assert_eq!(ortho2.dims(), &vec![2, 2]);
        assert_eq!(ortho2.get_current_position(), 2);
        assert_eq!(ortho2.payload()[0], Some(5));
        assert_eq!(ortho2.payload()[1], Some(25));
        
        // Add third value (triggers canonicalization since 15 < 25)
        let ortho3 = ortho2.add(15, 1)[0].clone();
        assert_eq!(ortho3.dims(), &vec![2, 2]);
        assert_eq!(ortho3.get_current_position(), 3);
        assert_eq!(ortho3.payload()[0], Some(5));
        assert_eq!(ortho3.payload()[1], Some(15)); // Canonicalized: swapped with 25
        assert_eq!(ortho3.payload()[2], Some(25));
        
        // Add fourth value (triggers expansion to [2,2,2])
        let expanded_orthos = ortho3.add(35, 1);
        let ortho4 = expanded_orthos.iter().find(|o| o.dims() == &vec![2, 2, 2])
            .expect("Should have [2,2,2] expansion").clone();
        assert_eq!(ortho4.dims(), &vec![2, 2, 2]);
        assert_eq!(ortho4.get_current_position(), 4);
        
        // Now test subtracting back through the states
        // Subtract from ortho4 should contract back to ortho3 state
        let back_to_3 = ortho4.subtract().expect("Should subtract from expanded");
        assert_eq!(back_to_3.dims(), &vec![2, 2], "Should contract back to [2,2]");
        assert_eq!(back_to_3.get_current_position(), 3);
        let mut values3: Vec<usize> = back_to_3.payload().iter().filter_map(|&v| v).collect();
        values3.sort();
        assert_eq!(values3, vec![5, 15, 25], "Should have original 3 values");
        
        // Subtract again should go back to ortho2 state
        let back_to_2 = back_to_3.subtract().expect("Should subtract again");
        assert_eq!(back_to_2.dims(), &vec![2, 2]);
        assert_eq!(back_to_2.get_current_position(), 2);
        assert_eq!(back_to_2.payload()[0], Some(5));
        assert_eq!(back_to_2.payload()[1], Some(15)); // Should preserve the canonicalized value
        
        // Subtract again should go back to ortho1 state
        let back_to_1 = back_to_2.subtract().expect("Should subtract again");
        assert_eq!(back_to_1.dims(), &vec![2, 2]);
        assert_eq!(back_to_1.get_current_position(), 1);
        assert_eq!(back_to_1.payload()[0], Some(5));
        assert_eq!(back_to_1.payload()[1], None);
        
        // Subtract once more should go back to ortho0 state
        let back_to_0 = back_to_1.subtract().expect("Should subtract to empty");
        assert_eq!(back_to_0.dims(), &vec![2, 2]);
        assert_eq!(back_to_0.get_current_position(), 0);
        assert_eq!(back_to_0.payload()[0], None);
        
        // One more subtract should return None
        assert_eq!(back_to_0.subtract(), None, "Subtracting from empty should return None");
    }

    #[test]
    fn test_subtract_from_expanded_ortho_with_more_values() {
        // Create an ortho, expand it, and add more values
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(1, 1)[0];
        let ortho = &ortho.add(2, 1)[0];
        let ortho = &ortho.add(3, 1)[0];
        
        // Expand to [3,2]
        let expanded_orthos = ortho.add(4, 1);
        let mut expanded = expanded_orthos.into_iter().find(|o| o.dims() == &vec![3, 2]).expect("Should have [3,2] expansion");
        
        // Add a 5th value
        expanded = expanded.add(5, 1)[0].clone();
        assert_eq!(expanded.get_current_position(), 5);
        
        // Subtracting should NOT contract (we're not at the boundary)
        let subtracted = expanded.subtract().expect("Should be able to subtract");
        assert_eq!(subtracted.dims(), &vec![3, 2], "Should stay [3,2]");
        assert_eq!(subtracted.get_current_position(), 4, "Should have 4 values");
    }

    #[test]
    fn test_subtract_rebuild_comprehensive() {
        // Test to verify the rebuild approach handles all scenarios correctly
        
        // Test 1: Simple subtract with no expansion
        let o1 = Ortho::new(1);
        let o1 = o1.add(5, 1)[0].clone();
        let o1 = o1.add(10, 1)[0].clone();
        let sub1 = o1.subtract().expect("Should subtract");
        assert_eq!(sub1.get_current_position(), 1);
        assert_eq!(sub1.payload()[0], Some(5));
        
        // Test 2: Subtract after canonicalization preserves canonical order
        let o2 = Ortho::new(1);
        let o2 = o2.add(10, 1)[0].clone();
        let o2 = o2.add(30, 1)[0].clone();
        let o2 = o2.add(20, 1)[0].clone(); // Canonicalized (20 < 30)
        let sub2 = o2.subtract().expect("Should subtract");
        assert_eq!(sub2.get_current_position(), 2);
        assert_eq!(sub2.payload()[0], Some(10));
        assert_eq!(sub2.payload()[1], Some(20)); // Should preserve canonicalized order
        
        // Test 3: Subtract from expanded [3,2] contracts back to [2,2]
        let o3 = Ortho::new(1);
        let o3 = o3.add(1, 1)[0].clone();
        let o3 = o3.add(2, 1)[0].clone();
        let o3 = o3.add(3, 1)[0].clone();
        let exp3 = o3.add(4, 1);
        let expanded = exp3.iter().find(|o| o.dims() == &vec![3, 2]).expect("Should have [3,2]");
        let sub3 = expanded.subtract().expect("Should subtract");
        assert_eq!(sub3.dims(), &vec![2, 2], "Should contract to [2,2]");
        assert_eq!(sub3.get_current_position(), 3);
        
        // Test 4: Subtract from expanded [2,2,2] contracts back to [2,2]
        let o4 = Ortho::new(1);
        let o4 = o4.add(5, 1)[0].clone();
        let o4 = o4.add(15, 1)[0].clone();
        let o4 = o4.add(10, 1)[0].clone(); // Canonicalized (10 < 15)
        let exp4 = o4.add(20, 1);
        let expanded222 = exp4.iter().find(|o| o.dims() == &vec![2, 2, 2]).expect("Should have [2,2,2]");
        let sub4 = expanded222.subtract().expect("Should subtract");
        assert_eq!(sub4.dims(), &vec![2, 2], "Should contract to [2,2]");
        assert_eq!(sub4.get_current_position(), 3);
        
        // Test 5: Multiple subtracts in sequence
        let o5 = Ortho::new(1);
        let o5 = o5.add(100, 1)[0].clone();
        let o5 = o5.add(200, 1)[0].clone();
        let o5 = o5.add(300, 1)[0].clone();
        
        let sub5_1 = o5.subtract().expect("First subtract");
        assert_eq!(sub5_1.get_current_position(), 2);
        
        let sub5_2 = sub5_1.subtract().expect("Second subtract");
        assert_eq!(sub5_2.get_current_position(), 1);
        
        let sub5_3 = sub5_2.subtract().expect("Third subtract");
        assert_eq!(sub5_3.get_current_position(), 0);
        
        assert_eq!(sub5_3.subtract(), None, "Should return None when empty");
    }
}
