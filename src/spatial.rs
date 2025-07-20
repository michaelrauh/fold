use std::cell::RefCell;
use std::{cmp::Ordering, collections::HashMap};

use itertools::Itertools;

thread_local! {
    static IMPACTED_CACHE: RefCell<HashMap<Vec<usize>, Vec<Vec<Vec<usize>>>>> = RefCell::new(HashMap::new());
    static DIAGONAL_CACHE: RefCell<HashMap<Vec<usize>, Vec<Vec<usize>>>> = RefCell::new(HashMap::new());
    static NEXT_SHAPES_CACHE: RefCell<HashMap<Vec<usize>, Vec<Vec<usize>>>> = RefCell::new(HashMap::new());
    static FULL_CACHE: RefCell<HashMap<(usize, Vec<usize>), bool>> = RefCell::new(HashMap::new());
}

fn _next_shapes(dims: &[usize]) -> Vec<Vec<usize>> {
    let mut results = Vec::new();

    if dims.iter().all(|&x| x == 2) {
        let mut up = dims.to_vec();
        up.push(2);
        results.push(up);
    }

    let mut seen = std::collections::HashSet::new();
    for (i, &val) in dims.iter().enumerate() {
        if seen.insert(val) {
            let mut new_shape = dims.to_vec();
            new_shape[i] += 1;
            results.push(new_shape);
        }
    }

    results
}

pub fn next_shapes(dims: &[usize]) -> Vec<Vec<usize>> {
    NEXT_SHAPES_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache
            .entry(dims.to_vec())
            .or_insert_with(|| _next_shapes(dims))
            .clone()
    })
}

fn _full(length: usize, dims: &[usize]) -> bool {
    let total = dims.iter().product::<usize>();
    length == total
}

pub fn full(length: usize, dims: &[usize]) -> bool {
    FULL_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache
            .entry((length, dims.to_vec()))
            .or_insert_with(|| _full(length, dims))
            .clone()
    })
}

fn requirement_locations_at(loc: usize, dims: &[usize]) -> (Vec<Vec<usize>>, Vec<usize>) {
    (
        impacted_phrase_location_at(loc, dims),
        diagonal_at(loc, dims),
    )
}

fn impacted_phrase_location_at(loc: usize, dims: &[usize]) -> Vec<Vec<usize>> {
    IMPACTED_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let impacted = cache
            .entry(dims.to_vec())
            .or_insert_with(|| get_impacted_phrase_locations(dims));
        impacted[loc].clone()
    })
}

fn diagonal_at(loc: usize, dims: &[usize]) -> Vec<usize> {
    DIAGONAL_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let diagonal = cache
            .entry(dims.to_vec())
            .or_insert_with(|| get_diagonals(dims));
        diagonal[loc].clone()
    })
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
    let location_to_index = location_to_index_mapping(dims);
    let index_to_location = index_to_location_mapping(dims);

    (0..dims.iter().product::<usize>())
        .into_iter()
        .map(|location| impacted_locations(location, &index_to_location, &location_to_index))
        .collect()
}

fn get_diagonals(dims: &[usize]) -> Vec<Vec<usize>> {
    let location_to_index = location_to_index_mapping(dims);
    let index_to_location = index_to_location_mapping(dims);
    let indices = indices_in_order(dims);

    (0..dims.iter().product::<usize>())
        .into_iter()
        .map(|location| {
            let current_index = &index_to_location[&location];
            let current_distance: usize = current_index.iter().sum();
            indices
                .iter()
                .filter(|index| {
                    *index < current_index && index.iter().sum::<usize>() == current_distance
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
        assert_eq!(
            impacted_phrase_location_at(3, &[2, 2]),
            vec![vec![1], vec![2]]
        )
    }

    #[test]
    fn it_takes_a_position_and_returns_impacted_diagonal() {
        assert_eq!(diagonal_at(5, &[3, 3]), vec![3, 4])
    }

    #[test]
    fn it_determines_if_a_shape_is_full() {
        assert_eq!(full(5, &[3, 3]), false);
        assert_eq!(full(9, &[3, 3]), true);
    }

    #[test]
    fn it_finds_next_shapes() {
        assert_eq!(next_shapes(&vec![2, 2]), vec![vec![2, 2, 2], vec![3, 2]]);

        // up result
        assert_eq!(
            next_shapes(&vec![2, 2, 2]),
            vec![vec![2, 2, 2, 2], vec![3, 2, 2]]
        );

        // over result
        assert_eq!(next_shapes(&vec![3, 2]), vec![vec![4, 2], vec![3, 3]]);

        // over tall
        assert_eq!(next_shapes(&vec![4, 2]), vec![vec![5, 2], vec![4, 3]]);

        // over squat
        assert_eq!(next_shapes(&vec![3, 3]), vec![vec![4, 3]]);
    }
}
