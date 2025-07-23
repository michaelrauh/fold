use std::{cell::RefCell, cmp::Ordering, collections::HashMap};

use itertools::Itertools;

thread_local! {
    static REQUIREMENTS_CACHE: RefCell<HashMap<(usize, Vec<usize>), (Vec<Vec<usize>>, Vec<usize>)>> = RefCell::new(HashMap::new());
    static BASE_CACHE: RefCell<HashMap<Vec<usize>, bool>> = RefCell::new(HashMap::new());
    static EXPAND_UP_CACHE: RefCell<HashMap<(Vec<usize>, usize), Vec<(Vec<usize>, usize, Vec<usize>)>>> = RefCell::new(HashMap::new());
    static EXPAND_OVER_CACHE: RefCell<HashMap<Vec<usize>, Vec<(Vec<usize>, usize, Vec<usize>)>>> = RefCell::new(HashMap::new());
    static AXIS_POSITIONS_CACHE: RefCell<HashMap<Vec<usize>, Vec<usize>>> = RefCell::new(HashMap::new());
}

pub fn get_requirements(loc: usize, dims: &[usize]) -> (Vec<Vec<usize>>, Vec<usize>) {
    let key = (loc, dims.to_vec());
    REQUIREMENTS_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(result) = cache.get(&key) {
            result.clone()
        } else {
            let result = requirement_locations_at(loc, dims);
            cache.insert(key, result.clone());
            result
        }
    })
}

pub fn get_axis_positions(dims: &[usize]) -> Vec<usize> {
    let key = dims.to_vec();
    AXIS_POSITIONS_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(result) = cache.get(&key) {
            result.clone()
        } else {
            let result = axis_positions(dims);
            cache.insert(key, result.clone());
            result
        }
    })
}

pub fn is_base(dims: &[usize]) -> bool {
    let key = dims.to_vec();
    BASE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(&result) = cache.get(&key) {
            result
        } else {
            let result = base(dims);
            cache.insert(key, result);
            result
        }
    })
}

pub fn expand_up(old_dims: &[usize], position: usize) -> Vec<(Vec<usize>, usize, Vec<usize>)> {
    let key = (old_dims.to_vec(), position);
    EXPAND_UP_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(result) = cache.get(&key) {
            result.clone()
        } else {
            let result = expand_for_up(old_dims, position);
            cache.insert(key, result.clone());
            result
        }
    })
}

pub fn expand_over(old_dims: &[usize]) -> Vec<(Vec<usize>, usize, Vec<usize>)> {
    let key = old_dims.to_vec();
    EXPAND_OVER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(result) = cache.get(&key) {
            result.clone()
        } else {
            let result = expand_for_over(old_dims);
            cache.insert(key, result.clone());
            result
        }
    })
}

pub fn capacity(dims: &[usize]) -> usize {
    dims.iter().product()
}

fn apply_mapping(positions: &[Vec<usize>], mapping: &HashMap<Vec<usize>, usize>) -> Vec<usize> {
    positions
        .iter()
        .map(|pos| {
            *mapping
                .get(pos)
                .expect("Position not found in new dimensions")
        })
        .collect()
}

fn remap(old_dims: &[usize], new_dims: &[usize]) -> Vec<usize> {
    let old_positions = indices_in_order(old_dims);
    let mapping = location_to_index_mapping(new_dims);

    apply_mapping(&old_positions, &mapping)
}

fn remap_for_up(old_dims: &[usize], position: usize) -> Vec<usize> {
    let padded_positions = pad(old_dims, position);
    let new_dims = old_dims
        .iter()
        .chain(std::iter::once(&2))
        .cloned()
        .collect::<Vec<usize>>();
    let mapping = location_to_index_mapping(&new_dims);

    apply_mapping(&padded_positions, &mapping)
}

fn pad(dims: &[usize], position: usize) -> Vec<Vec<usize>> {
    indices_in_order(dims)
        .into_iter()
        .map(|mut indices| {
            indices.insert(dims.len() - position, 0);
            indices
        })
        .collect()
}

fn base(dims: &[usize]) -> bool {
    dims.iter().all(|&x| x == 2)
}

fn next_dims_over(dims: &[usize]) -> Vec<Vec<usize>> {
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, &val) in dims.iter().enumerate() {
        if seen.insert(val) {
            let mut new_dims = dims.to_vec();
            new_dims[i] += 1;
            results.push(new_dims);
        }
    }
    results
}

fn expand_for_over(old_dims: &[usize]) -> Vec<(Vec<usize>, usize, Vec<usize>)> {
    let over_dims = next_dims_over(old_dims);

    over_dims
        .into_iter()
        .map(|new_dims| {
            let reorganization_pattern = remap(old_dims, &new_dims);
            let cap = capacity(&new_dims);
            (new_dims, cap, reorganization_pattern)
        })
        .collect()
}

fn expand_for_up(old_dims: &[usize], position: usize) -> Vec<(Vec<usize>, usize, Vec<usize>)> {
    let mut over_results = expand_for_over(old_dims);

    let mut up_dims = old_dims.to_vec();
    up_dims.insert(position, 2);
    let up_reorganization = remap_for_up(old_dims, position);
    let up_cap = capacity(&up_dims);
    let up_result = (up_dims, up_cap, up_reorganization);

    over_results.push(up_result);
    over_results
}

fn requirement_locations_at(loc: usize, dims: &[usize]) -> (Vec<Vec<usize>>, Vec<usize>) {
    (
        impacted_phrase_location_at(loc, dims),
        diagonal_at(loc, dims),
    )
}

fn impacted_phrase_location_at(loc: usize, dims: &[usize]) -> Vec<Vec<usize>> {
    let impacted = get_impacted_phrase_locations(dims);
    impacted[loc].clone()
}

fn diagonal_at(loc: usize, dims: &[usize]) -> Vec<usize> {
    let diagonal = get_diagonals(dims);
    diagonal[loc].clone()
}

fn indices_in_order(dims: &[usize]) -> Vec<Vec<usize>> {
    order_by_distance(index_array(dims))
}

fn index_to_location_mapping(dims: &[usize]) -> HashMap<usize, Vec<usize>> {
    indices_in_order(dims).into_iter().enumerate().collect()
}

fn location_to_index_mapping(dims: &[usize]) -> HashMap<Vec<usize>, usize> {
    indices_in_order(dims)
        .into_iter()
        .enumerate()
        .map(|(x, y)| (y, x))
        .collect()
}

fn impacted_locations(
    location: usize,
    location_to_index: &HashMap<usize, Vec<usize>>,
    index_to_location: &HashMap<Vec<usize>, usize>,
) -> Vec<Vec<usize>> {
    let mut res = vec![];
    let index = &location_to_index[&location];
    let indices = 0..index.len();
    for focus in indices {
        let cur = index[focus];
        let mut subres = vec![];
        for i in 0..cur {
            let mut location = index.clone();
            location[focus] = i;
            let fin = index_to_location[&location];
            subres.push(fin);
        }
        res.push(subres)
    }
    res
}

fn get_impacted_phrase_locations(dims: &[usize]) -> Vec<Vec<Vec<usize>>> {
    let index_to_location = index_to_location_mapping(dims);
    let location_to_index = location_to_index_mapping(dims);

    (0..dims.iter().product::<usize>())
        .map(|location| impacted_locations(location, &index_to_location, &location_to_index))
        .collect()
}

fn get_diagonals(dims: &[usize]) -> Vec<Vec<usize>> {
    let location_to_index = location_to_index_mapping(dims);
    let index_to_location = index_to_location_mapping(dims);
    let indices = indices_in_order(dims);

    (0..dims.iter().product::<usize>())
        .map(|location| {
            let current_index = &index_to_location[&location];
            let current_distance: usize = current_index.iter().sum();
            indices
                .iter()
                .filter(|index| {
                    index < &current_index && index.iter().sum::<usize>() == current_distance
                })
                .map(|x| location_to_index[x])
                .collect_vec()
        })
        .collect_vec()
}

fn index_array(dims: &[usize]) -> Vec<Vec<usize>> {
    cartesian_product(dims.iter().map(|x| (0..*x).collect()).collect())
}

fn order_by_distance(indices: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
    let mut sorted = indices;
    sorted.sort_by(|a, b| {
        match a.iter().sum::<usize>().cmp(&b.iter().sum()) {
            Ordering::Less => Ordering::Less,
            Ordering::Equal => {
                // distance is equal, and so order by each
                for (x, y) in a.iter().zip(b) {
                    if x > y {
                        return Ordering::Greater;
                    }

                    if x < y {
                        return Ordering::Less;
                    }
                }
                unreachable!("There should not be duplicate indices")
            }
            Ordering::Greater => Ordering::Greater,
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
        None => {
            vec![]
        }
    }
}

fn axis_positions(dims: &[usize]) -> Vec<usize> {
    (1..=dims.len()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_produces_an_index_matrix_with_dims() {
        assert_eq!(
            index_array(&vec![3, 3]),
            vec![
                vec![0, 0],
                vec![0, 1],
                vec![0, 2],
                vec![1, 0],
                vec![1, 1],
                vec![1, 2],
                vec![2, 0],
                vec![2, 1],
                vec![2, 2]
            ]
        );
        assert_eq!(
            index_array(&vec![2, 2, 2]),
            vec![
                vec![0, 0, 0],
                vec![0, 0, 1],
                vec![0, 1, 0],
                vec![0, 1, 1],
                vec![1, 0, 0],
                vec![1, 0, 1],
                vec![1, 1, 0],
                vec![1, 1, 1]
            ]
        );
    }

    #[test]
    fn it_orders_indices() {
        assert_eq!(
            order_by_distance(vec![
                vec![0, 0],
                vec![0, 1],
                vec![0, 2],
                vec![1, 0],
                vec![1, 1],
                vec![1, 2],
                vec![2, 0],
                vec![2, 1],
                vec![2, 2]
            ]),
            vec![
                vec![0, 0],
                vec![0, 1],
                vec![1, 0],
                vec![0, 2],
                vec![1, 1],
                vec![2, 0],
                vec![1, 2],
                vec![2, 1],
                vec![2, 2]
            ]
        );
    }

    #[test]
    fn it_gets_impacted_phrase_locations() {
        assert_eq!(
            get_impacted_phrase_locations(&vec![2, 2]),
            vec![
                vec![vec![], vec![]],
                vec![vec![], vec![0]],
                vec![vec![0], vec![]],
                vec![vec![1], vec![2]]
            ]
        )
    }

    #[test]
    fn it_gets_impacted_diagonals() {
        assert_eq!(
            get_diagonals(&vec![3, 3]),
            vec![
                vec![],
                vec![],
                vec![1],
                vec![],
                vec![3],
                vec![3, 4],
                vec![],
                vec![6],
                vec![]
            ]
        )
    }

    #[test]
    fn it_takes_a_position_and_returns_impacted_phrases() {
        let (phrases, _diagonal) = get_requirements(3, &[2, 2]);
        assert_eq!(phrases, vec![vec![1], vec![2]])
    }

    #[test]
    fn it_takes_a_position_and_returns_impacted_diagonal() {
        let (_phrases, diagonal) = get_requirements(5, &[3, 3]);
        assert_eq!(diagonal, vec![3, 4])
    }

    #[test]
    fn it_provides_capacity_information_by_dims() {
        assert_eq!(capacity(&vec![2, 2]), 4);
        assert_eq!(capacity(&vec![2, 2, 2]), 8);
        assert_eq!(capacity(&vec![3, 2]), 6);
        assert_eq!(capacity(&vec![4, 2]), 8);
        assert_eq!(capacity(&vec![3, 3]), 9);
    }

    #[test]
    fn it_determines_if_dims_are_base() {
        assert_eq!(is_base(&vec![2, 2]), true);
        assert_eq!(is_base(&vec![2, 2, 2]), true);
        assert_eq!(is_base(&vec![3, 2]), false);
        assert_eq!(is_base(&vec![4, 2]), false);
        assert_eq!(is_base(&vec![3, 3]), false);
    }

    #[test]
    fn it_determines_the_reorganization_pattern_for_over() {
        assert_eq!(remap(&vec![2, 2], &vec![3, 2]), vec![0, 1, 2, 3]);
        assert_eq!(remap(&vec![3, 2], &vec![4, 2]), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(remap(&vec![3, 2], &vec![3, 3]), vec![0, 1, 2, 4, 5, 7]);
    }

    #[test]
    fn it_pads_at_a_position_for_up() {
        assert_eq!(
            pad(&vec![2, 2], 0),
            vec![vec![0, 0, 0], vec![0, 1, 0], vec![1, 0, 0], vec![1, 1, 0]]
        );
        assert_eq!(
            pad(&vec![2, 2], 1),
            vec![vec![0, 0, 0], vec![0, 0, 1], vec![1, 0, 0], vec![1, 0, 1]]
        );
        assert_eq!(
            pad(&vec![2, 2], 2),
            vec![vec![0, 0, 0], vec![0, 0, 1], vec![0, 1, 0], vec![0, 1, 1]]
        );
    }

    #[test]
    fn it_determines_the_reorganization_pattern_for_up() {
        assert_eq!(remap_for_up(&vec![2, 2], 0), vec![0, 2, 3, 6]);
        assert_eq!(remap_for_up(&vec![2, 2], 1), vec![0, 1, 3, 5]);
        assert_eq!(remap_for_up(&vec![2, 2], 2), vec![0, 1, 2, 4]);
    }

    #[test]
    fn it_expands_for_over() {
        assert_eq!(
            expand_over(&vec![2, 2]),
            vec![(vec![3, 2], 6, vec![0, 1, 2, 3])]
        );

        assert_eq!(
            expand_over(&vec![3, 2]),
            vec![
                (vec![4, 2], 8, vec![0, 1, 2, 3, 4, 5]),
                (vec![3, 3], 9, vec![0, 1, 2, 4, 5, 7])
            ]
        );

        assert_eq!(
            expand_over(&vec![3, 3]),
            vec![(vec![4, 3], 12, vec![0, 1, 2, 3, 4, 5, 6, 7, 9])]
        );
    }

    #[test]
    fn it_expands_for_up() {
        assert_eq!(
            expand_up(&vec![2, 2], 0),
            vec![
                (vec![3, 2], 6, vec![0, 1, 2, 3]),
                (vec![2, 2, 2], 8, vec![0, 2, 3, 6])
            ]
        );

        assert_eq!(
            expand_up(&vec![2, 2], 1),
            vec![
                (vec![3, 2], 6, vec![0, 1, 2, 3]),
                (vec![2, 2, 2], 8, vec![0, 1, 3, 5])
            ]
        );

        assert_eq!(
            expand_up(&vec![2, 2], 2),
            vec![
                (vec![3, 2], 6, vec![0, 1, 2, 3]),
                (vec![2, 2, 2], 8, vec![0, 1, 2, 4])
            ]
        );
    }

    #[test]
    fn it_caches_results_correctly() {
        // Test that calling the same function multiple times returns the same result
        let result1 = capacity(&vec![3, 3]);
        let result2 = capacity(&vec![3, 3]);
        assert_eq!(result1, result2);
        assert_eq!(result1, 9);

        // Test expand_over caching
        let expand1 = expand_over(&vec![2, 2]);
        let expand2 = expand_over(&vec![2, 2]);
        assert_eq!(expand1, expand2);

        // Test is_base caching
        let base1 = is_base(&vec![2, 2, 2]);
        let base2 = is_base(&vec![2, 2, 2]);
        assert_eq!(base1, base2);
        assert_eq!(base1, true);

        // Test get_requirements caching
        let req1 = get_requirements(3, &[2, 2]);
        let req2 = get_requirements(3, &[2, 2]);
        assert_eq!(req1, req2);
    }

    #[test]
    fn it_gets_axis_positions() {
        assert_eq!(get_axis_positions(&[2, 2]), vec![1, 2]);
        assert_eq!(get_axis_positions(&[3, 2, 4]), vec![1, 2, 3]);
    }
}
