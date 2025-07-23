use crate::spatial;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(PartialEq, Debug, Clone)]
pub struct Ortho {
    version: usize,
    dims: Vec<usize>,
    payload: Vec<Option<usize>>,
}

impl Ortho {
    pub fn new(version: usize) -> Self {
        Ortho {
            version,
            dims: vec![2, 2],
            payload: vec![None, None, None, None],
        }
    }

    pub fn id(&self) -> usize {
        if self.dims.is_empty() && self.payload.is_empty() {
            let mut hasher = DefaultHasher::new();
            self.version.hash(&mut hasher);
            hasher.finish() as usize
        } else {
            let mut hasher = DefaultHasher::new();
            self.dims.hash(&mut hasher);
            self.payload.hash(&mut hasher);
            hasher.finish() as usize
        }
    }

    pub fn add(&self, value: usize) -> Vec<Self> {
        if let Some(position) = self.get_current_position() {
            // Check if adding this value would make the ortho completely full
            let remaining_none_count = self.payload.iter().filter(|x| x.is_none()).count();
            
            if remaining_none_count == 1 {
                // This would be the last item - expand eagerly before adding
                if spatial::is_base(&self.dims) {
                    return Self::expand(self, spatial::expand_up(&self.dims, self.get_insert_position(value)), value);
                } else {
                    return Self::expand(self, spatial::expand_over(&self.dims), value);
                }
            }
            
            // Regular insertion - not the last position
            let mut new_payload = self.payload.clone();
            
            // Special case: if this is the third item in a [2,2] structure (position 2), 
            // we need to sort the second and third items to establish axis order
            if self.dims == vec![2, 2] && position == 2 {
                let first_value = self.payload[0].unwrap();
                let second_value = self.payload[1].unwrap();
                
                // Sort values 1, 2, and new value, then place them in positions 0, 1, 2
                let mut values = vec![first_value, second_value, value];
                values.sort();
                
                new_payload[0] = Some(values[0]);
                new_payload[1] = Some(values[1]);
                new_payload[2] = Some(values[2]);
            } else {
                new_payload[position] = Some(value);
            }
            
            return vec![Ortho {
                version: self.version,
                dims: self.dims.clone(),
                payload: new_payload,
            }];
        }
        
        // This should not happen with eager expansion, but keep as fallback
        if spatial::is_base(&self.dims) {
            Self::expand(self, spatial::expand_up(&self.dims, self.get_insert_position(value)), value)
        } else {
            Self::expand(self, spatial::expand_over(&self.dims), value)
        }
    }

    fn expand(
        ortho: &Ortho,
        expansions: Vec<(Vec<usize>, usize, Vec<usize>)>,
        value: usize,
    ) -> Vec<Ortho> {
        expansions
            .into_iter()
            .map(|(new_dims, new_capacity, reorganization_pattern)| {
                let mut new_payload = vec![None; new_capacity];
                for (i, &pos) in reorganization_pattern.iter().enumerate() {
                    new_payload[pos] = ortho.payload.get(i).cloned().flatten();
                }
                let new_insert_position = new_payload.iter().position(|x| x.is_none()).unwrap();
                new_payload[new_insert_position] = Some(value);
                Ortho {
                    version: ortho.version,
                    dims: new_dims,
                    payload: new_payload,
                }
            })
            .collect()
    }

    pub fn get_current_position(&self) -> Option<usize> {
        self.payload.iter().position(|x| x.is_none())
    }

    pub fn get_insert_position(&self, to_add: usize) -> usize {
        let axis_positions = spatial::get_axis_positions(&self.dims);
        let mut idx = 0;
        for &pos in axis_positions.iter() {
            if let Some(&axis) = self.payload.get(pos).and_then(|x| x.as_ref()) {
                if to_add < axis {
                    return idx;
                }
                idx += 1;
            }
        }
        idx
    }

    /// Returns (forbidden_diagonals, required_prefixes) for the current position.
    /// With eager expansion, there should always be a current position available.
    /// forbidden_diagonals: Vec<Option<usize>> (diagonal indices in payload)
    /// required_prefixes: Vec<Vec<Option<usize>>> (prefix indices in payload)
    pub fn get_requirements(&self) -> (Vec<usize>, Vec<Vec<Option<usize>>>) {
        match self.get_current_position() {
            Some(pos) => {
                let (prefixes, diagonals) = spatial::get_requirements(pos, &self.dims);
                let forbidden: Vec<usize> = diagonals
                    .into_iter()
                    .filter_map(|i| self.payload.get(i).and_then(|v| *v))
                    .collect();
                let required: Vec<Vec<Option<usize>>> = prefixes
                    .into_iter()
                    .map(|prefix| prefix.into_iter().map(|i| self.payload.get(i).cloned().unwrap_or(None)).collect())
                    .collect();
                (forbidden, required)
            }
            None => {
                // With eager expansion, this should not happen, but provide fallback
                (vec![], vec![])
            }
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
        assert_eq!(ortho.dims, vec![2, 2]);
        assert_eq!(ortho.payload, vec![None, None, None, None]);
    }

    #[test]
    fn test_get_current() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.get_current_position(), Some(0));

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2],
                payload: vec![None, None],
            }
            .get_current_position(),
            Some(0)
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2],
                payload: vec![Some(1), None],
            }
            .get_current_position(),
            Some(1)
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2],
                payload: vec![Some(1), Some(2)],
            }
            .get_current_position(),
            None
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(1), Some(2), None, None],
            }
            .get_current_position(),
            Some(2)
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![None, Some(1), Some(2), None],
            }
            .get_current_position(),
            Some(0)
        );
    }

    #[test]
    fn test_get_insert_position() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.get_insert_position(5), 0);

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2],
                payload: vec![Some(0), Some(15)],
            }
            .get_insert_position(14),
            0
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2],
                payload: vec![Some(0), Some(15)],
            }
            .get_insert_position(20),
            1
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), Some(30)],
            }
            .get_insert_position(5),
            0
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), Some(30)],
            }
            .get_insert_position(15),
            1
        );

        assert_eq!(
            Ortho {
                version: 1,
                dims: vec![2, 2],
                payload: vec![Some(0), Some(10), Some(20), Some(30)],
            }
            .get_insert_position(1000),
            2
        );
    }

    #[test]
    fn test_add_simple() {
        let ortho = Ortho::new(1);
        let orthos = ortho.add(10);
        assert_eq!(orthos, vec![Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), None, None, None],
        }]);
    }

    #[test]
    fn test_add_multiple() {
        let ortho = Ortho::new(1);
        let orthos1 = ortho.add(1);
        let ortho = &orthos1[0];
        let orthos2 = ortho.add(2);
        assert_eq!(orthos2, vec![Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(1), Some(2), None, None],
        }]);
    }

    #[test]
    fn test_add_order_independent_ids() {
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(1);
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
        let ortho = Ortho::new(1);
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
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(1)[0];
        let ortho = &ortho.add(2)[0];
        let ortho = &ortho.add(3)[0];
        let ortho = &ortho.add(4)[0];
        // With eager expansion, the 4th add should trigger expansion
        assert_eq!(ortho.dims, vec![3, 2]);
        assert_eq!(ortho.payload, vec![Some(1), Some(2), Some(3), Some(4), None, None]);
        let mut expansions = ortho.add(5);
        expansions.sort_by(|a, b| a.dims.cmp(&b.dims));
        assert_eq!(expansions, vec![
            Ortho {
                version: 1,
                dims: vec![3, 2],
                payload: vec![Some(1), Some(2), Some(3), Some(4), Some(5), None],
            },
        ]);
    }

    #[test]
    fn test_insert_position_middle() {
        let ortho = Ortho {
            version: 1,
            dims: vec![2],
            payload: vec![Some(10), Some(20)],
        };
        let orthos = ortho.add(15);
        assert_eq!(orthos, vec![Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), Some(15), Some(20), None],
        }]);
    }

    #[test]
    fn test_insert_position_middle_and_reorg() {
        // With the new [2,2] starting behavior, this test focuses on eager expansion
        // when the structure is about to become completely full
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];  
        let ortho = &ortho.add(15)[0]; // Should trigger sorting for position 2
        assert_eq!(ortho, &Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), Some(15), Some(20), None],
        });
        
        // Adding the fourth item should trigger eager expansion
        let mut orthos = ortho.add(25);
        orthos.sort_by(|a, b| a.dims.cmp(&b.dims));
        assert_eq!(orthos.len(), 2);
        
        // Should have both expansion options: [3,2] and [2,2,2]
        assert_eq!(orthos, vec![
            Ortho {
                version: 1,
                dims: vec![2, 2, 2],
                payload: vec![Some(10), Some(15), Some(20), Some(25), None, None, None, None],
            },
            Ortho {
                version: 1,
                dims: vec![3, 2],
                payload: vec![Some(10), Some(15), Some(20), Some(25), None, None],
            },
        ]);
    }

    #[test]
    fn test_get_requirements_empty() {
        let ortho = Ortho::new(1);
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        // New ortho with dims=[2,2] has requirements based on position 0
        assert_eq!(required, vec![vec![], vec![]]);
    }

    #[test]
    fn test_get_requirements_simple() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, vec![vec![], vec![Some(10)]]);
    }

    #[test]
    fn test_get_requirements_multiple() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, vec![20]);
        // In [2,2] shape with two items, the requirements are for position 2
        // Position 2 has diagonal [1] (forbidden position 1 which has value 20)
        // And prefixes [[0], []] meaning position 0 is required in first axis
        assert_eq!(required, vec![vec![Some(10)], vec![]]);
    }

    #[test]
    fn test_get_requirements_full() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];
        let ortho = &ortho.add(30)[0];
        let ortho = &ortho.add(40)[0];
        let (forbidden, required) = ortho.get_requirements();
        // Full [2,2] shape will look ahead to expansion which has requirements
        assert_eq!(forbidden, vec![40]);
        // The actual requirements reflect the spatial reorganization pattern
        assert_eq!(required, vec![vec![Some(10), Some(30)], vec![]]);
    }

    #[test]
    fn test_get_requirements_expansion() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(1)[0];
        let ortho = &ortho.add(2)[0];
        let ortho = &ortho.add(3)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, vec![vec![Some(2)], vec![Some(3)]]);
    }

    #[test]
    fn test_get_requirements_order_independent() {
        let ortho1 = Ortho::new(1);
        let ortho2 = Ortho::new(1);
        let ortho1 = &ortho1.add(1)[0];
        let ortho2 = &ortho2.add(1)[0];
        let ortho1 = &ortho1.add(2)[0];
        let ortho2 = &ortho2.add(3)[0];
        let (_forbidden1, _required1) = ortho1.get_requirements();
        let (_forbidden2, _required2) = ortho2.get_requirements();
        let ortho1 = &ortho1.add(3)[0];
        let ortho2 = &ortho2.add(2)[0];
        let (forbidden1, required1) = ortho1.get_requirements();
        let (forbidden2, required2) = ortho2.get_requirements();
        assert_eq!(forbidden1, forbidden2);
        assert_eq!(required1, vec![vec![Some(2)], vec![Some(3)]]);
        assert_eq!(required2, vec![vec![Some(2)], vec![Some(3)]]);
    }

    #[test]
    fn test_id_version_behavior() {
        // Test that empty orthos with different versions have same IDs now (since they have same dims/payload)
        let ortho_v1 = Ortho::new(1);
        let ortho_v2 = Ortho::new(2);
        assert_eq!(ortho_v1.id(), ortho_v2.id());

        // Test that orthos with different versions but same contents have same IDs
        let ortho_v1_with_content = Ortho {
            version: 1,
            dims: vec![2],
            payload: vec![Some(10), None],
        };
        let ortho_v2_with_content = Ortho {
            version: 2,
            dims: vec![2],
            payload: vec![Some(10), None],
        };
        assert_eq!(ortho_v1_with_content.id(), ortho_v2_with_content.id());

        // Test that orthos with same version but different contents have different IDs
        let ortho_v1_content_a = Ortho {
            version: 1,
            dims: vec![2],
            payload: vec![Some(10), None],
        };
        let ortho_v1_content_b = Ortho {
            version: 1,
            dims: vec![2],
            payload: vec![Some(20), None],
        };
        assert_ne!(ortho_v1_content_a.id(), ortho_v1_content_b.id());

        // Test that orthos with different versions and different contents have different IDs
        let ortho_v1_10 = Ortho {
            version: 1,
            dims: vec![2],
            payload: vec![Some(10), None],
        };
        let ortho_v2_20 = Ortho {
            version: 2,
            dims: vec![2],
            payload: vec![Some(20), None],
        };
        assert_ne!(ortho_v1_10.id(), ortho_v2_20.id());

        // Test with more complex structures
        let ortho_v1_complex = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(1), Some(2), Some(3), None],
        };
        let ortho_v3_complex = Ortho {
            version: 3,
            dims: vec![2, 2],
            payload: vec![Some(1), Some(2), Some(3), None],
        };
        // Same contents, different versions should have same ID (dual nature)
        assert_eq!(ortho_v1_complex.id(), ortho_v3_complex.id());
    }
}
