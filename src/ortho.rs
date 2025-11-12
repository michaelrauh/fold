use crate::spatial;
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};
use std::fmt;
use bincode::Encode;
use bincode::Decode;

#[derive(PartialEq, Debug, Clone, Encode, Decode)]
pub struct Ortho {
    id: usize,
    dims: Vec<usize>,
    payload: Vec<Option<usize>>,
}

impl Ortho {
    fn compute_id(dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
        // Compute ID based on canonical state (dims + payload)
        // This ensures path-independent IDs - orthos with same final state get same ID
        let mut hasher = FxHasher::default();
        dims.hash(&mut hasher);
        payload.hash(&mut hasher);
        (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
    }
    
    pub fn new(_version: usize) -> Self {
        let dims = vec![2,2];
        let payload = vec![None; 4];
        let id = Self::compute_id(&dims, &payload);
        Ortho { id, dims, payload }
    }
    pub fn id(&self) -> usize { self.id }
    pub fn get_current_position(&self) -> usize { self.payload.iter().position(|x| x.is_none()).unwrap_or(self.payload.len()) }
    pub fn add(&self, value: usize, _version: usize) -> Vec<Self> {
        let insertion_index = self.get_current_position();
        let total_empty = self.payload.iter().filter(|x| x.is_none()).count();
        
        if total_empty == 1 {
            if spatial::is_base(&self.dims) {
                return Self::expand(
                    self,
                    spatial::expand_up(&self.dims, self.get_insert_position(value)),
                    value,
                );
            } else {
                return Self::expand(self, spatial::expand_over(&self.dims), value);
            }
        }
        if insertion_index == 2 && self.dims.as_slice() == [2, 2] {
            let mut new_payload: Vec<Option<usize>> = self.payload.clone();
            new_payload[insertion_index] = Some(value);
            if let (Some(second), Some(third)) = (new_payload[1], new_payload[2]) {
                if second > third { new_payload[1] = Some(third); new_payload[2] = Some(second); }
            }
            let new_id = Self::compute_id(&self.dims, &new_payload);
            return vec![Ortho { id: new_id, dims: self.dims.clone(), payload: new_payload }];
        }
        let len = self.payload.len();
        let mut new_payload: Vec<Option<usize>> = Vec::with_capacity(len);
        unsafe { new_payload.set_len(len); std::ptr::copy_nonoverlapping(self.payload.as_ptr(), new_payload.as_mut_ptr(), len); }
        if insertion_index < new_payload.len() { new_payload[insertion_index] = Some(value); }
        let new_id = Self::compute_id(&self.dims, &new_payload);
        vec![Ortho { id: new_id, dims: self.dims.clone(), payload: new_payload }]
    }
    fn expand(
        ortho: &Ortho,
        expansions: Vec<(Vec<usize>, usize, Vec<usize>)>,
        value: usize,
    ) -> Vec<Ortho> {
        // Find insert position once
        let insert_pos = ortho.payload.iter().position(|x| x.is_none()).unwrap();
        
        let mut out = Vec::with_capacity(expansions.len());
        for (new_dims_vec, new_capacity, reorg) in expansions.into_iter() {
            let mut new_payload = vec![None; new_capacity];
            // Directly reorganize old payload, inserting value at the right position
            for (i, &pos) in reorg.iter().enumerate() {
                if i == insert_pos {
                    new_payload[pos] = Some(value);
                } else {
                    new_payload[pos] = ortho.payload.get(i).cloned().flatten();
                }
            }
            let new_id = Self::compute_id(&new_dims_vec, &new_payload);
            out.push(Ortho { id: new_id, dims: new_dims_vec, payload: new_payload });
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

    pub fn get_requirement_phrases(&self) -> Vec<Vec<usize>> {
        let (_forbidden, required) = self.get_requirements();
        required
    }
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
    
    fn get_index_at_coord(&self, coord: &[usize]) -> Option<usize> {
        spatial::get_location_to_index(self.dims.as_slice()).get(coord).copied()
    }
}

pub struct OrthoDisplay<'a> {
    ortho: &'a Ortho,
    interner: &'a crate::interner::Interner,
}

impl<'a> OrthoDisplay<'a> {
    pub fn new(ortho: &'a Ortho, interner: &'a crate::interner::Interner) -> Self {
        Self { ortho, interner }
    }
}

impl<'a> fmt::Display for OrthoDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rows = self.ortho.dims[self.ortho.dims.len() - 2];
        let cols = self.ortho.dims[self.ortho.dims.len() - 1];
        let higher_dims = &self.ortho.dims[..self.ortho.dims.len() - 2];
        
        let max_width = self.ortho.payload.iter()
            .filter_map(|&opt| opt)
            .map(|token_id| self.interner.string_for_index(token_id).len())
            .max()
            .unwrap_or(1)
            .max(4);
        
        let format_cell = |token_id: Option<usize>| -> String {
            token_id
                .map(|id| format!("{:>width$}", self.interner.string_for_index(id), width = max_width))
                .unwrap_or_else(|| format!("{:>width$}", "·", width = max_width))
        };
        
        let format_2d_slice = |prefix: &[usize]| -> String {
            (0..rows)
                .map(|row| {
                    (0..cols)
                        .map(|col| {
                            let coords: Vec<usize> = prefix.iter().copied().chain([row, col]).collect();
                            self.ortho.get_index_at_coord(&coords)
                                .filter(|&idx| idx < self.ortho.payload.len())
                                .and_then(|idx| self.ortho.payload[idx])
                                .map(|token_id| format_cell(Some(token_id)))
                                .unwrap_or_else(|| format_cell(None))
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        
        if higher_dims.is_empty() {
            return write!(f, "{}", format_2d_slice(&[]));
        }
        
        let tile_coords = Ortho::generate_tile_coords(higher_dims);
        
        let output = tile_coords.iter()
            .enumerate()
            .map(|(tile_idx, coords)| {
                let separator = if tile_idx > 0 { "\n\n" } else { "" };
                let dims_str = coords.iter()
                    .enumerate()
                    .map(|(i, &val)| format!("dim{}={}", i, val))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}[{}]\n{}", separator, dims_str, format_2d_slice(coords))
            })
            .collect::<Vec<_>>()
            .join("");
        
        write!(f, "{}", output)
    }
}

impl Ortho {
    pub fn display<'a>(&'a self, interner: &'a crate::interner::Interner) -> OrthoDisplay<'a> {
        OrthoDisplay::new(self, interner)
    }

    fn generate_tile_coords(dims: &[usize]) -> Vec<Vec<usize>> {
        if dims.is_empty() {
            return vec![vec![]];
        }
        
        let total: usize = dims.iter().product();
        (0..total)
            .map(|mut idx| {
                let mut coord = Vec::with_capacity(dims.len());
                for &dim_size in dims.iter().rev() {
                    coord.push(idx % dim_size);
                    idx /= dim_size;
                }
                coord.reverse();
                coord
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let ortho = Ortho::new(1);
        let expected_id = Ortho::compute_id(&vec![2,2], &vec![None, None, None, None]);
        assert_eq!(ortho.id, expected_id);
        assert_eq!(ortho.dims, vec![2,2]);
        assert_eq!(ortho.payload, vec![None, None, None, None]);
    }

    #[test]
    fn test_get_current() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.get_current_position(), 0);

        assert_eq!(
            Ortho {
                id: 0,
                dims: vec![2, 2],
                payload: vec![None, None, None, None],
            }
            .get_current_position(),
            0
        );

        assert_eq!(
            Ortho {
                id: 0,
                dims: vec![2, 2],
                payload: vec![Some(1), None, None, None],
            }
            .get_current_position(),
            1
        );

        assert_eq!(
            Ortho {
                id: 0,
                dims: vec![2, 2],
                payload: vec![Some(1), Some(2), None, None],
            }
            .get_current_position(),
            2
        );

        assert_eq!(
            Ortho {
                id: 0,
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
                id: 0,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(15), None, None],
            }
            .get_insert_position(14),
            0
        );

        assert_eq!(
            Ortho {
                id: 0,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(15), None, None],
            }
            .get_insert_position(20),
            1
        );

        assert_eq!(
            Ortho {
                id: 0,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), None],
            }
            .get_insert_position(5),
            0
        );

        assert_eq!(
            Ortho {
                id: 0,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), None],
            }
            .get_insert_position(15),
            1
        );

        assert_eq!(
            Ortho {
                id: 0,
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
        assert_eq!(orthos.len(), 1);
        assert_eq!(orthos[0].dims, vec![2, 2]);
        assert_eq!(orthos[0].payload, vec![Some(10), None, None, None]);
    }

    #[test]
    fn test_add_multiple() {
        let ortho = Ortho::new(1);
        let orthos1 = ortho.add(1, 1);
        let ortho = &orthos1[0];
        let orthos2 = ortho.add(2, 1);
        assert_eq!(orthos2.len(), 1);
        assert_eq!(orthos2[0].dims, vec![2, 2]);
        assert_eq!(orthos2[0].payload, vec![Some(1), Some(2), None, None]);
    }

    #[test]
    fn test_add_path_independent_ids() {
        // IDs should be based on canonical state, not addition path
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(1);
        let ortho1 = &ortho1.add(1, 1)[0];
        let ortho2 = &ortho2.add(1, 1)[0];
        assert_eq!(ortho1.id(), ortho2.id()); // Same so far
        
        // Add in different order
        let ortho1 = &ortho1.add(20, 1)[0];  // [1, 20]
        let ortho2 = &ortho2.add(30, 1)[0];  // [1, 30]
        assert_ne!(ortho1.id(), ortho2.id()); // Different payloads -> different IDs
        
        // Complete with different order, but canonicalization makes them same
        let ortho1 = &ortho1.add(30, 1)[0];  // [1, 20, 30] (already sorted)
        let ortho2 = &ortho2.add(20, 1)[0];  // [1, 30, 20] -> [1, 20, 30] (canonicalized by swap)
        
        // After canonicalization, both have [Some(1), Some(20), Some(30), None]
        assert_eq!(ortho1.payload(), ortho2.payload(), "Payloads should be same after canonicalization");
        assert_eq!(ortho1.id(), ortho2.id(), "IDs should be same for same canonical state");
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
        assert_eq!(expansions.len(), 2);
        assert_eq!(expansions[0].dims, vec![3, 2]);
        assert_eq!(expansions[0].payload, vec![Some(1), Some(2), Some(3), Some(4), None, None]);
        assert_eq!(expansions[1].dims, vec![2, 2, 2]);
        assert_eq!(expansions[1].payload, vec![Some(1), Some(2), Some(3), None, Some(4), None, None, None]);
    }

    #[test]
    fn test_insert_position_middle() {
        let ortho = Ortho {
            id: 0,
            dims: vec![2, 2],
            payload: vec![Some(10), Some(20), None, None],
        };
        let orthos = ortho.add(15, 1);
        assert_eq!(orthos.len(), 1);
        assert_eq!(orthos[0].dims, vec![2, 2]);
        assert_eq!(orthos[0].payload, vec![Some(10), Some(15), Some(20), None]);
    }

    #[test]
    fn test_insert_position_middle_and_reorg() {
        let ortho = Ortho {
            id: 0,
            dims: vec![2, 2],
            payload: vec![Some(10), None, Some(20), Some(30)],
        };

        let mut orthos = ortho.add(15, 1);
        orthos.sort_by(|a, b| a.dims.cmp(&b.dims));
        assert_eq!(orthos.len(), 2);
        assert_eq!(orthos[0].dims, vec![2, 2, 2]);
        assert_eq!(orthos[0].payload, vec![Some(10), None, Some(15), Some(20), None, None, Some(30), None]);
        assert_eq!(orthos[1].dims, vec![3, 2]);
        assert_eq!(orthos[1].payload, vec![Some(10), Some(15), Some(20), Some(30), None, None]);
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
    fn test_id_path_independent_behavior() {
        // Test that empty orthos have the same ID (version parameter ignored)
        let ortho_1 = Ortho::new(1);
        let ortho_2 = Ortho::new(2);
        assert_eq!(ortho_1.id(), ortho_2.id(), "Empty orthos should have same ID");

        // Test that manually constructed orthos report their assigned IDs
        let ortho_a = Ortho {
            id: 100,
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
        };
        let ortho_b = Ortho {
            id: 200,
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
        };
        assert_eq!(ortho_a.id(), 100);
        assert_eq!(ortho_b.id(), 200);
        assert_ne!(ortho_a.id(), ortho_b.id());

        // Test that orthos created by adding different values have different IDs
        let base = Ortho::new(1);
        let ortho_add_10 = base.add(10, 1)[0].clone();
        let ortho_add_20 = base.add(20, 1)[0].clone();
        assert_ne!(ortho_add_10.id(), ortho_add_20.id(), "Different additions should create different IDs");
        
        // Test that orthos with same canonical state have same ID (path-independent)
        // Canonicalization happens at position 2 for [2,2] dims
        let ortho_path1 = Ortho::new(1);
        let ortho_path1 = &ortho_path1.add(10, 1)[0];
        let ortho_path1 = &ortho_path1.add(20, 1)[0];  // [10, 20, _, _]
        let ortho_path1 = &ortho_path1.add(30, 1)[0];  // [10, 20, 30, _] - no swap needed
        
        let ortho_path2 = Ortho::new(1);
        let ortho_path2 = &ortho_path2.add(10, 1)[0];
        let ortho_path2 = &ortho_path2.add(30, 1)[0];  // [10, 30, _, _]
        let ortho_path2 = &ortho_path2.add(20, 1)[0];  // [10, 30, 20, _] -> [10, 20, 30, _] (swapped!)
        
        // Both should end up as [Some(10), Some(20), Some(30), None] due to canonicalization swap
        assert_eq!(ortho_path1.payload(), ortho_path2.payload(), "Should have same payload after canonicalization");
        assert_eq!(ortho_path1.id(), ortho_path2.id(), "Should have same ID for same canonical state");
    }

    #[test]
    fn test_id_collision_for_different_payloads() {
        // Test manually assigned IDs (used in deserialization)
        let ortho0 = Ortho {
            id: 1,
            dims: vec![2, 2],
            payload: vec![Some(0), None, None, None],
        };
        let ortho1 = Ortho {
            id: 1,
            dims: vec![2, 2],
            payload: vec![Some(1), None, None, None],
        };
        let ortho2 = Ortho {
            id: 1,
            dims: vec![2, 2],
            payload: vec![Some(2), None, None, None],
        };
        let ortho3 = Ortho {
            id: 1,
            dims: vec![2, 2],
            payload: vec![Some(3), None, None, None],
        };
        let ortho4 = Ortho {
            id: 1,
            dims: vec![2, 2],
            payload: vec![Some(4), None, None, None],
        };
        let ortho5 = Ortho {
            id: 1,
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
        // All manually created with id:1, so all will have id 1
        assert_eq!(ids, vec![1, 1, 1, 1, 1, 1]);
        
        // But if we create them via add(), they should have different IDs
        let base = Ortho::new(1);
        let created0 = base.add(0, 1)[0].clone();
        let created1 = base.add(1, 1)[0].clone();
        let created2 = base.add(2, 1)[0].clone();
        assert_ne!(created0.id(), created1.id());
        assert_ne!(created1.id(), created2.id());
        assert_ne!(created0.id(), created2.id());
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
        use crate::interner::Interner;
        let interner = Interner::from_text("a b c d");
        let ortho = Ortho {
            id: 0,
            dims: vec![2, 2],
            payload: vec![Some(0), Some(1), Some(2), Some(3)],
        };
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "   a    b\n   c    d");
    }
    
    #[test]
    fn test_display_2d_with_nones() {
        use crate::interner::Interner;
        let interner = Interner::from_text("hello world");
        let ortho = Ortho {
            id: 0,
            dims: vec![2, 2],
            payload: vec![Some(0), Some(1), None, None],
        };
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "hello world\n    ·     ·");
    }
    
    #[test]
    fn test_display_3x2() {
        use crate::interner::Interner;
        let interner = Interner::from_text("a b c d e");
        let ortho = Ortho {
            id: 0,
            dims: vec![3, 2],
            payload: vec![Some(0), Some(1), Some(2), Some(3), Some(4), None],
        };
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "   a    b\n   c    d\n   e    ·");
    }
    
    #[test]
    fn test_display_3d_tiled() {
        use crate::interner::Interner;
        let interner = Interner::from_text("a b c d e f g");
        let ortho = Ortho {
            id: 0,
            dims: vec![2, 2, 2],
            payload: vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), None],
        };
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "[dim0=0]\n   a    b\n   c    e\n\n[dim0=1]\n   d    f\n   g    ·");
    }

    #[test]
    fn test_get_requirement_phrases() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10, 1)[0];
        let ortho = &ortho.add(20, 1)[0];
        
        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![10]]);
        
        let ortho = &ortho.add(30, 1)[0];
        let ortho = &ortho.add(40, 1)[0];
        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![10, 30]]);
    }

    #[test]
    fn test_get_requirement_phrases_expansion() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(1, 1)[0];
        let ortho = &ortho.add(2, 1)[0];
        let ortho = &ortho.add(3, 1)[0];
        
        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![2], vec![3]]);
    }
}

