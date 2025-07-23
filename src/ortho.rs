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
            dims: vec![],
            payload: vec![],
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
            let mut new_payload = self.payload.clone();
            new_payload[position] = Some(value);
            return vec![Ortho {
                version: self.version,
                dims: self.dims.clone(),
                payload: new_payload,
            }];
        }
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
    /// If the shape is full, returns requirements for the first position after expansion.
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
                // Shape is full - look ahead to expansion requirements
                // We'll use position 0 as a representative case for expansion requirements
                let expansions = if spatial::is_base(&self.dims) {
                    spatial::expand_up(&self.dims, 0)
                } else {
                    spatial::expand_over(&self.dims)
                };
                
                if let Some((new_dims, _, reorganization_pattern)) = expansions.first() {
                    // Create the expanded payload with existing elements reorganized
                    let mut new_payload = vec![None; spatial::capacity(new_dims)];
                    for (i, &pos) in reorganization_pattern.iter().enumerate() {
                        new_payload[pos] = self.payload.get(i).cloned().flatten();
                    }
                    
                    // Find the first empty position in the expanded shape
                    if let Some(first_empty_pos) = new_payload.iter().position(|x| x.is_none()) {
                        let (prefixes, diagonals) = spatial::get_requirements(first_empty_pos, new_dims);
                        let forbidden: Vec<usize> = diagonals
                            .into_iter()
                            .filter_map(|i| new_payload.get(i).and_then(|v| *v))
                            .collect();
                        let required: Vec<Vec<Option<usize>>> = prefixes
                            .into_iter()
                            .map(|prefix| prefix.into_iter().map(|i| new_payload.get(i).cloned().unwrap_or(None)).collect())
                            .collect();
                        return (forbidden, required);
                    }
                }
                
                // Fallback to empty if expansion calculation fails
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
        assert_eq!(ortho.dims, vec![]);
        assert_eq!(ortho.payload, vec![]);
    }

    #[test]
    fn test_get_current() {
        let ortho = Ortho::new(1);
        assert_eq!(ortho.get_current_position(), None);

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
            dims: vec![2],
            payload: vec![Some(10), None],
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
            dims: vec![2],
            payload: vec![Some(1), Some(2)],
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
        assert_eq!(ortho.dims, vec![2]);
        assert_eq!(ortho.payload, vec![Some(1), Some(2)]);
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
        assert_eq!(ortho.dims, vec![2, 2]);
        assert_eq!(ortho.payload, vec![Some(1), Some(2), Some(3), Some(4)]);
        let mut expansions = ortho.add(5);
        expansions.sort_by(|a, b| a.dims.cmp(&b.dims));
        assert_eq!(expansions, vec![
            Ortho {
                version: 1,
                dims: vec![2, 2, 2],
                payload: vec![Some(1), Some(2), Some(3), Some(5), Some(4), None, None, None],
            },
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

        let ortho = Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), None, Some(20), Some(30)],
        };
        let orthos = ortho.add(15);
        assert_eq!(orthos, vec![Ortho {
            version: 1,
            dims: vec![2, 2],
            payload: vec![Some(10), Some(15), Some(20), Some(30)],
        }]);
    }

    #[test]
    fn test_get_requirements_empty() {
        let ortho = Ortho::new(1);
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        // Empty ortho with dims=[] will look ahead to expansion to [2] which has requirements
        assert_eq!(required, vec![vec![]]);
    }

    #[test]
    fn test_get_requirements_simple() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        assert_eq!(required, vec![vec![Some(10)]]);
    }

    #[test]
    fn test_get_requirements_multiple() {
        let ortho = Ortho::new(1);
        let ortho = &ortho.add(10)[0];
        let ortho = &ortho.add(20)[0];
        let (forbidden, required) = ortho.get_requirements();
        assert_eq!(forbidden, Vec::<usize>::new());
        // Full [2] shape will look ahead to expansion to [2,2] which has requirements
        assert_eq!(required, vec![vec![], vec![Some(10)]]);
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
        let (forbidden1, required1) = ortho1.get_requirements();
        let (forbidden2, required2) = ortho2.get_requirements();
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
        // Test that empty orthos with different versions have different IDs
        let ortho_v1 = Ortho::new(1);
        let ortho_v2 = Ortho::new(2);
        assert_ne!(ortho_v1.id(), ortho_v2.id());

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
