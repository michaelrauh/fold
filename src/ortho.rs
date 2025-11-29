use crate::spatial;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::fmt;
use bincode::Encode;
use bincode::Decode;

#[derive(PartialEq, Debug, Clone, Encode, Decode)]
pub struct Ortho {
    dims: Vec<usize>,
    payload: Vec<Option<usize>>,
    volume: usize,
    fullness: usize,
}

impl Ortho {
    fn compute_id(dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> usize {
        let mut hasher = DefaultHasher::new();
        dims.hash(&mut hasher);
        payload.hash(&mut hasher);
        (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
    }
    pub fn new() -> Self {
        let dims = vec![2,2];
        let payload = vec![None; 4];
        let volume = 1; // (2-1) * (2-1) = 1
        let fullness = 0;
        Ortho { dims, payload, volume, fullness }
    }
    

    pub fn id(&self) -> usize { Self::compute_id(&self.dims, &self.payload) }
    pub fn get_current_position(&self) -> usize { self.payload.iter().position(|x| x.is_none()).unwrap_or(self.payload.len()) }
    pub fn add(&self, value: usize) -> Vec<Self> {
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
            return vec![Ortho { dims: self.dims.clone(), payload: new_payload, volume: self.volume, fullness: self.fullness + 1 }];
        }
        let len = self.payload.len();
        let mut new_payload: Vec<Option<usize>> = Vec::with_capacity(len);
        unsafe { new_payload.set_len(len); std::ptr::copy_nonoverlapping(self.payload.as_ptr(), new_payload.as_mut_ptr(), len); }
        if insertion_index < new_payload.len() { new_payload[insertion_index] = Some(value); }
        vec![Ortho { dims: self.dims.clone(), payload: new_payload, volume: self.volume, fullness: self.fullness + 1 }]
    }
    fn expand(
        ortho: &Ortho,
        expansions: Vec<(Vec<usize>, usize, Vec<usize>)>,
        value: usize,
    ) -> Vec<Ortho> {
        let mut old_payload_with_value = ortho.payload.clone();
        let insert_pos = old_payload_with_value.iter().position(|x| x.is_none()).unwrap();
        old_payload_with_value[insert_pos] = Some(value);
        let new_fullness = ortho.fullness + 1;
        
        let mut out = Vec::with_capacity(expansions.len());
        for (new_dims_vec, new_capacity, reorg) in expansions.into_iter() {
            let mut new_payload = vec![None; new_capacity];
            for (i, &pos) in reorg.iter().enumerate() { 
                new_payload[pos] = old_payload_with_value.get(i).cloned().flatten(); 
            }
            let new_volume = new_dims_vec.iter().map(|x| x.saturating_sub(1)).product::<usize>();
            out.push(Ortho { dims: new_dims_vec, payload: new_payload, volume: new_volume, fullness: new_fullness });
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
    
    /// Remap an ortho's payload to use new vocabulary indices
    pub fn remap(&self, vocab_map: &[usize]) -> Option<Self> {
        // Remap payload: translate old vocab indices to new vocab indices
        let new_payload: Vec<Option<usize>> = self.payload.iter().map(|opt_idx| {
            opt_idx.map(|old_idx| vocab_map[old_idx])
        }).collect();
        
        // Create new ortho with remapped payload (volume and fullness unchanged)
        Some(Ortho {
            dims: self.dims.clone(),
            payload: new_payload,
            volume: self.volume,
            fullness: self.fullness,
        })
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
    pub fn volume(&self) -> usize { self.volume }
    pub fn fullness(&self) -> usize { self.fullness }
    pub fn score(&self) -> (usize, usize) { (self.volume, self.fullness) }
    
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

    fn compute_score(dims: &Vec<usize>, payload: &Vec<Option<usize>>) -> (usize, usize) {
        let volume = dims.iter().map(|x| x.saturating_sub(1)).product::<usize>();
        let fullness = payload.iter().filter(|x| x.is_some()).count();
        (volume, fullness)
    }

    #[test]
    fn test_new() {
        let ortho = Ortho::new();
        assert_eq!(ortho.dims, vec![2,2]);
        assert_eq!(ortho.payload, vec![None, None, None, None]);
        assert_eq!(ortho.volume, 1, "volume should be (2-1)*(2-1) = 1");
        assert_eq!(ortho.fullness, 0, "fullness should be 0 (no filled slots)");
        assert_eq!(ortho.score(), (1, 0));
    }

    #[test]
    fn test_get_current() {
        let ortho = Ortho::new();
        assert_eq!(ortho.get_current_position(), 0);

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![None, None, None, None],
                volume: 1,
                fullness: 0,
            }
            .get_current_position(),
            0
        );

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![Some(1), None, None, None],
                volume: 1,
                fullness: 1,
            }
            .get_current_position(),
            1
        );

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![Some(1), Some(2), None, None],
                volume: 1,
                fullness: 2,
            }
            .get_current_position(),
            2
        );

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![Some(1), Some(2), Some(3), None],
                volume: 1,
                fullness: 3,
            }
            .get_current_position(),
            3
        );
    }

    #[test]
    fn test_get_insert_position() {
        let ortho = Ortho::new();
        assert_eq!(ortho.get_insert_position(5), 0);

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![Some(0), Some(15), None, None],
                volume: 1,
                fullness: 2,
            }
            .get_insert_position(14),
            0
        );

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![Some(0), Some(15), None, None],
                volume: 1,
                fullness: 2,
            }
            .get_insert_position(20),
            1
        );

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), None],
                volume: 1,
                fullness: 3,
            }
            .get_insert_position(5),
            0
        );

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), None],
                volume: 1,
                fullness: 3,
            }
            .get_insert_position(15),
            1
        );

        assert_eq!(
            Ortho {
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), None],
                volume: 1,
                fullness: 3,
            }
            .get_insert_position(1000),
            2
        );
    }

    #[test]
    fn test_add_simple() {
        let ortho = Ortho::new();
        let orthos = ortho.add(10);
        assert_eq!(
            orthos,
            vec![Ortho {
                dims: vec![2, 2],
                payload: vec![Some(10), None, None, None],
                volume: 1,
                fullness: 1,
            }]
        );
    }

    #[test]
    fn test_add_multiple() {
        let ortho = Ortho::new();
        let orthos1 = ortho.add(1);
        let ortho = &orthos1[0];
        let orthos2 = ortho.add(2);
        assert_eq!(
            orthos2,
            vec![Ortho {
                dims: vec![2, 2],
                payload: vec![Some(1), Some(2), None, None],
                volume: 1,
                fullness: 2,
            }]
        );
    }

    #[test]
    fn test_add_order_independent_ids() {
        let ortho1 = Ortho::new();
        let ortho2 = Ortho::new();
        let ortho1 = &ortho1.add(1)[0];
        let ortho2 = &ortho2.add(1)[0];
        assert_eq!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(2)[0];
        let ortho2 = &ortho2.add(3)[0];
        assert_ne!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(3)[0];
        let ortho2 = &ortho2.add(2)[0];
        assert_eq!(ortho1.id(), ortho2.id());
        let ortho1 = &ortho1.add(4)[0];
        let ortho2 = &ortho2.add(4)[0];
        assert_eq!(ortho1.id(), ortho2.id());
    }

    #[test]
    fn test_add_shape_expansion() {
        let ortho = Ortho::new();
        let orthos = ortho.add(1);
        let ortho = &orthos[0];
        let orthos2 = ortho.add(2);
        let ortho = &orthos2[0];
        assert_eq!(ortho.dims, vec![2, 2]);
        assert_eq!(ortho.payload, vec![Some(1), Some(2), None, None]);
        let orthos3 = ortho.add(3);
        let ortho = &orthos3[0];
        assert_eq!(ortho.dims, vec![2, 2]);
        assert_eq!(ortho.payload, vec![Some(1), Some(2), Some(3), None]);
    }

    #[test]
    fn test_up_and_over_expansions_full_coverage() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(1)[0];
        let ortho = &ortho.add(2)[0];
        let ortho = &ortho.add(3)[0];

        let expansions = ortho.add(4);
        assert_eq!(
            expansions,
            vec![
                Ortho {
                    dims: vec![2, 3],
                    payload: vec![Some(1), Some(2), Some(3), None, Some(4), None],
                    volume: 2,
                    fullness: 4,
                },
                Ortho {
                    dims: vec![2, 2, 2],
                    payload: vec![Some(1), Some(2), Some(3), None, Some(4), None, None, None],
                    volume: 1,
                    fullness: 4,
                }
            ]
        );
    }

    #[test]
    fn test_insert_position_middle() {
        let ortho = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(10), Some(20), None, None],
            volume: 1,
            fullness: 2,
        };
        let orthos = ortho.add(15);
        assert_eq!(
            orthos,
            vec![Ortho {
                dims: vec![2, 2],
                payload: vec![Some(10), Some(15), Some(20), None],
                volume: 1,
                fullness: 3,
            }]
        );
    }

    #[test]
    fn test_insert_position_middle_and_reorg() {
        let ortho = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(10), None, Some(20), Some(30)],
            volume: 1,
            fullness: 3,
        };

        let mut orthos = ortho.add(15);
        orthos.sort_by(|a, b| a.dims.cmp(&b.dims));
        assert_eq!(
            orthos,
            vec![
                Ortho {
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
                    volume: 1,
                    fullness: 4,
                },
                Ortho {
                    dims: vec![2, 3],
                    payload: vec![Some(10), Some(15), Some(20), None, Some(30), None],
                    volume: 2,
                    fullness: 4,
                },
            ]
        );
    }

    #[test]
    fn test_get_requirements_empty() {
        let ortho = Ortho::new();
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, Vec::<Vec<usize>>::new());
    }

    #[test]
    fn test_get_requirements_simple() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(10)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, vec![vec![10]]);
    }

    #[test]
    fn test_get_requirements_multiple() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, vec![20]);
        assert_eq!(required, vec![vec![10]]);
    }

    #[test]
    fn test_get_requirements_full() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];
        let ortho = &ortho.add(30)[0];
        let ortho = &ortho.add(40)[0];

        // With sorted dims [2,3] instead of old [3,2]:
        // payload = [Some(10), Some(20), Some(30), None, Some(40), None]
        // current_position = 3 (first None)
        // At position 3 (index [0,2], distance 2):
        // Position 4 (index [1,1], also distance 2) is in the same shell
        // Position 4 has content (40) from the reorg, so 40 is forbidden
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, vec![40]);
        assert_eq!(required, vec![vec![10, 20]]);
    }

    #[test]
    fn test_get_requirements_expansion() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(1)[0];
        let ortho = &ortho.add(2)[0];
        let ortho = &ortho.add(3)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, vec![vec![2], vec![3]]);
    }

    #[test]
    fn test_get_requirements_order_independent() {
        let ortho1 = Ortho::new();
        let ortho2 = Ortho::new();
        let ortho1 = &ortho1.add(1)[0];
        let ortho2 = &ortho2.add(1)[0];
        let ortho1 = &ortho1.add(2)[0];
        let ortho2 = &ortho2.add(3)[0];
        let ortho1 = &ortho1.add(3)[0];
        let ortho2 = &ortho2.add(2)[0];
        let (forbidden1, required1) = ortho1.get_requirements();
        let (forbidden2, required2) = ortho2.get_requirements();
        assert_eq!(forbidden1, forbidden2);
        assert_eq!(required1, vec![vec![2], vec![3]]);
        assert_eq!(required2, vec![vec![2], vec![3]]);
    }

    #[test]
    fn test_id_version_behavior() {
        // Test that orthos with same contents have same IDs
        let ortho_with_content_1 = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
            volume: 1,
            fullness: 1,
        };
        let ortho_with_content_2 = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
            volume: 1,
            fullness: 1,
        };
        assert_eq!(ortho_with_content_1.id(), ortho_with_content_2.id());

        // Test that orthos with different contents have different IDs
        let ortho_content_a = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
            volume: 1,
            fullness: 1,
        };
        let ortho_content_b = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(20), None, None, None],
            volume: 1,
            fullness: 1,
        };
        assert_ne!(ortho_content_a.id(), ortho_content_b.id());
    }

    #[test]
    fn test_id_collision_for_different_payloads() {
        use super::*;
        // These are the payloads seen in the logs
        let ortho0 = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(0), None, None, None],
            volume: 1,
            fullness: 1,
        };
        let ortho1 = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(1), None, None, None],
            volume: 1,
            fullness: 1,
        };
        let ortho2 = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(2), None, None, None],
            volume: 1,
            fullness: 1,
        };
        let ortho3 = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(3), None, None, None],
            volume: 1,
            fullness: 1,
        };
        let ortho4 = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(4), None, None, None],
            volume: 1,
            fullness: 1,
        };
        let ortho5 = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(5), None, None, None],
            volume: 1,
            fullness: 1,
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
        let mut o1 = Ortho::new();
        o1 = o1.add(10).pop().unwrap(); // a
        o1 = o1.add(20).pop().unwrap(); // b
        o1 = o1.add(30).pop().unwrap(); // c
        // Path 2: a < c but b < c (second and third swapped relative to path 1)
        let mut o2 = Ortho::new();
        o2 = o2.add(10).pop().unwrap(); // a
        o2 = o2.add(30).pop().unwrap(); // c
        o2 = o2.add(20).pop().unwrap(); // b (unsorted axis order)
        // Insert 4th token to force expansion candidates
        let children1 = o1.add(40);
        let children2 = o2.add(40);
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
            dims: vec![2, 2],
            payload: vec![Some(0), Some(1), Some(2), Some(3)],
            volume: 1,
            fullness: 4,
        };
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "   a    b\n   c    d");
    }
    
    #[test]
    fn test_display_2d_with_nones() {
        use crate::interner::Interner;
        let interner = Interner::from_text("hello world");
        let ortho = Ortho {
            dims: vec![2, 2],
            payload: vec![Some(0), Some(1), None, None],
            volume: 1,
            fullness: 2,
        };
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "hello world\n    ·     ·");
    }
    
    #[test]
    fn test_display_3x2() {
        use crate::interner::Interner;
        let interner = Interner::from_text("a b c d e");
        let ortho = Ortho {
            dims: vec![3, 2],
            payload: vec![Some(0), Some(1), Some(2), Some(3), Some(4), None],
            volume: 2,
            fullness: 5,
        };
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "   a    b\n   c    d\n   e    ·");
    }
    
    #[test]
    fn test_display_3d_tiled() {
        use crate::interner::Interner;
        let interner = Interner::from_text("a b c d e f g");
        let ortho = Ortho {
            dims: vec![2, 2, 2],
            payload: vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), Some(6), None],
            volume: 1,
            fullness: 7,
        };
        let display_str = format!("{}", ortho.display(&interner));
        assert_eq!(display_str, "[dim0=0]\n   a    b\n   c    e\n\n[dim0=1]\n   d    f\n   g    ·");
    }

    #[test]
    fn test_get_requirement_phrases() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];
        
        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![10]]);
        
        let ortho = &ortho.add(30)[0];
        let ortho = &ortho.add(40)[0];
        // With sorted dims [2,3], the required phrases at position 3 are [[10, 20]]
        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![10, 20]]);
    }

    #[test]
    fn test_get_requirement_phrases_expansion() {
        let ortho = Ortho::new();
        let ortho = &ortho.add(1)[0];
        let ortho = &ortho.add(2)[0];
        let ortho = &ortho.add(3)[0];
        
        let phrases = ortho.get_requirement_phrases();
        assert_eq!(phrases, vec![vec![2], vec![3]]);
    }
    
    #[test]
    fn test_cached_score_matches_computed() {
        // Helper to compute score the old way
        fn compute_score_old_way(ortho: &Ortho) -> (usize, usize) {
            let volume = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
            let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
            (volume, fullness)
        }
        
        // Test new ortho
        let ortho = Ortho::new();
        assert_eq!(ortho.score(), compute_score_old_way(&ortho), "New ortho score mismatch");
        
        // Test after adding values
        let ortho = ortho.add(1).pop().unwrap();
        assert_eq!(ortho.score(), compute_score_old_way(&ortho), "After add(1) score mismatch");
        
        let ortho = ortho.add(2).pop().unwrap();
        assert_eq!(ortho.score(), compute_score_old_way(&ortho), "After add(2) score mismatch");
        
        let ortho = ortho.add(3).pop().unwrap();
        assert_eq!(ortho.score(), compute_score_old_way(&ortho), "After add(3) score mismatch");
        
        // Test expansion - this returns multiple orthos
        let expansions = ortho.add(4);
        for (i, expanded_ortho) in expansions.iter().enumerate() {
            assert_eq!(
                expanded_ortho.score(), 
                compute_score_old_way(expanded_ortho),
                "Expansion {} score mismatch", i
            );
        }
        
        // Test remap
        let ortho_to_remap = Ortho::new().add(5).pop().unwrap();
        let vocab_map = vec![0, 1, 2, 3, 4, 5];
        if let Some(remapped) = ortho_to_remap.remap(&vocab_map) {
            assert_eq!(
                remapped.score(),
                compute_score_old_way(&remapped),
                "Remapped ortho score mismatch"
            );
        }
    }
}

