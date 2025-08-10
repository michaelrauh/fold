use crate::spatial;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use bincode::Encode;
use bincode::Decode;

#[derive(PartialEq, Debug, Clone, Encode, Decode)]
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
        if self.get_current_position() == 0 {
            let mut hasher = DefaultHasher::new();
            self.version.hash(&mut hasher);
            (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
        } else {
            let mut hasher = DefaultHasher::new();
            self.dims.hash(&mut hasher);
            self.payload.hash(&mut hasher);
            (hasher.finish() & 0x7FFF_FFFF_FFFF_FFFF) as usize
        }
    }

    pub fn add(&self, value: usize, version: usize) -> Vec<Self> {
        let position = self.get_current_position();

        let remaining_empty = self
            .payload
            .iter()
            .skip(position)
            .filter(|x| x.is_none())
            .count();

        if remaining_empty == 1 {
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

        // Special case: third insert (position 2) needs axis sorting
        if position == 2 && self.dims == vec![2, 2] {
            let mut new_payload = self.payload.clone();
            new_payload[position] = Some(value);

            // Sort the second and third items by value to establish axis order
            if let (Some(second), Some(third)) = (new_payload[1], new_payload[2]) {
                if second > third {
                    new_payload[1] = Some(third);
                    new_payload[2] = Some(second);
                }
            }

            return vec![Ortho {
                version,
                dims: self.dims.clone(),
                payload: new_payload,
            }];
        }

        // Normal case: just add to the current position
        let mut new_payload = self.payload.clone();
        new_payload[position] = Some(value);
        return vec![Ortho {
            version,
            dims: self.dims.clone(),
            payload: new_payload,
        }];
    }

    fn expand(
        ortho: &Ortho,
        expansions: Vec<(Vec<usize>, usize, Vec<usize>)>,
        value: usize,
        version: usize,
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
                    version,
                    dims: new_dims,
                    payload: new_payload,
                }
            })
            .collect()
    }

    pub fn get_current_position(&self) -> usize {
        self.payload.iter().position(|x| x.is_none()).unwrap()
    }

    fn get_insert_position(&self, to_add: usize) -> usize {
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

    pub fn version(&self) -> usize {
        self.version
    }

    pub fn prefixes(&self) -> Vec<Vec<usize>> {
        // For each position in the payload, get the prefix indices from spatial::get_requirements
        let mut result = Vec::new();
        for pos in 0..self.payload.len() {
            let (prefixes, _diagonals) = spatial::get_requirements(pos, &self.dims);
            for prefix in prefixes {
                if !prefix.is_empty() {
                    // Map indices to payload values, skipping None
                    let values: Vec<usize> = prefix
                        .iter()
                        .filter_map(|&i| self.payload.get(i).cloned().flatten())
                        .collect();
                    if !values.is_empty() {
                        result.push(values);
                    }
                }
            }
        }
        result
    }

    pub fn prefixes_for_last_filled(&self) -> Vec<Vec<usize>> {
        let pos = if let Some(p) = self.payload.iter().position(|x| x.is_none()) {
            if p == 0 { return vec![]; }
            p - 1
        } else {
            self.payload.len() - 1
        };
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

    pub fn dims(&self) -> &Vec<usize> {
        &self.dims
    }

    pub(crate) fn set_version(&self, version: usize) -> Ortho {
        Ortho {
            version,
            dims: self.dims.clone(),
            payload: self.payload.clone(),
        }
    }

    pub fn payload(&self) -> &Vec<Option<usize>> {
        &self.payload
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
}
