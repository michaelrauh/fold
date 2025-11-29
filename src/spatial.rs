use std::{cell::RefCell, cmp::Ordering};
use itertools::Itertools;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use rustc_hash::FxHashMap; // use concrete name

// Consolidated metadata per dims
struct DimMeta {
    indices_in_order: Vec<Vec<usize>>,
    axis_positions: Vec<usize>,
    impacted_phrase_locations: Vec<Vec<Vec<usize>>>,
    diagonals: Vec<Vec<usize>>,
    location_to_index: FxHashMap<Vec<usize>, usize>,
}

impl DimMeta {
    fn new(dims: &[usize]) -> Self {
        let indices_in_order = indices_in_order_compute(dims);
        let location_to_index: FxHashMap<Vec<usize>, usize> = indices_in_order
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, loc)| (loc, i))
            .collect();
        let index_to_location: FxHashMap<usize, Vec<usize>> = indices_in_order
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, loc)| (i, loc))
            .collect();
        let impacted_phrase_locations = get_impacted_phrase_locations_compute(dims, &index_to_location, &location_to_index, &indices_in_order);
        let diagonals = get_diagonals_compute(dims, &index_to_location, &location_to_index, &indices_in_order);
        DimMeta {
            indices_in_order,
            axis_positions: (1..=dims.len()).collect(),
            impacted_phrase_locations,
            diagonals,
            location_to_index,
        }
    }
}

thread_local! {
    static DIM_META_CACHE: RefCell<FxHashMap<Vec<usize>, Rc<DimMeta>>> = RefCell::new(FxHashMap::default());
    static EXPAND_UP_CACHE: RefCell<FxHashMap<(Vec<usize>, usize), Vec<(Vec<usize>, usize, Vec<usize>)>>> = RefCell::new(FxHashMap::default());
    static EXPAND_OVER_CACHE: RefCell<FxHashMap<Vec<usize>, Vec<(Vec<usize>, usize, Vec<usize>)>>> = RefCell::new(FxHashMap::default());
}

static META_HITS: AtomicUsize = AtomicUsize::new(0);
static META_MISSES: AtomicUsize = AtomicUsize::new(0);

fn get_meta(dims: &[usize]) -> Rc<DimMeta> {
    DIM_META_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(m) = cache.get(dims) { META_HITS.fetch_add(1, AtomicOrdering::Relaxed); return m.clone(); }
        META_MISSES.fetch_add(1, AtomicOrdering::Relaxed);
        let meta = Rc::new(DimMeta::new(dims));
        cache.insert(dims.to_vec(), meta.clone());
        meta
    })
}

pub fn meta_stats() -> (usize, usize) { (META_HITS.load(AtomicOrdering::Relaxed), META_MISSES.load(AtomicOrdering::Relaxed)) }

pub fn get_requirements(loc: usize, dims: &[usize]) -> (Vec<Vec<usize>>, Vec<usize>) {
    let meta = get_meta(dims);
    (
        meta.impacted_phrase_locations[loc].clone(),
        meta.diagonals[loc].clone(),
    )
}

pub fn get_axis_positions(dims: &[usize]) -> Vec<usize> { get_meta(dims).axis_positions.clone() }

pub fn get_location_to_index(dims: &[usize]) -> FxHashMap<Vec<usize>, usize> { get_meta(dims).location_to_index.clone() }

pub fn is_base(dims: &[usize]) -> bool { dims.iter().all(|&x| x == 2) }

pub fn expand_up(old_dims: &[usize], position: usize) -> Vec<(Vec<usize>, usize, Vec<usize>)> {
    let key = (old_dims.to_vec(), position);
    EXPAND_UP_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(result) = cache.get(&key) { result.clone() } else {
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
        if let Some(result) = cache.get(&key) { result.clone() } else {
            let result = expand_for_over(old_dims);
            cache.insert(key, result.clone());
            result
        }
    })
}

pub fn capacity(dims: &[usize]) -> usize { dims.iter().product() }

fn apply_mapping(positions: &[Vec<usize>], mapping: &FxHashMap<Vec<usize>, usize>) -> Vec<usize> {
    positions.iter().map(|pos| mapping[pos]).collect()
}

fn remap(old_dims: &[usize], new_dims: &[usize]) -> Vec<usize> {
    let old_positions = get_meta(old_dims).indices_in_order.clone();
    let mapping = get_meta(new_dims).location_to_index.clone();
    apply_mapping(&old_positions, &mapping)
}

fn remap_for_up(old_dims: &[usize], position: usize) -> Vec<usize> {
    let padded_positions = pad(old_dims, position);
    let mut new_dims = old_dims.to_vec();
    new_dims.insert(position, 2);
    let mapping = get_meta(&new_dims).location_to_index.clone();
    apply_mapping(&padded_positions, &mapping)
}

fn pad(dims: &[usize], position: usize) -> Vec<Vec<usize>> {
    get_meta(dims)
        .indices_in_order
        .iter()
        .cloned()
        .map(|mut indices| { indices.insert(dims.len() - position, 0); indices })
        .collect()
}

fn parent(dims: &[usize]) -> Option<Vec<usize>> {
    // Root shape [2,2] has no parent
    if dims == &[2, 2] {
        return None;
    }
    
    // If all entries are 2 and dims.len() > 2: remove one 2, then sort
    if dims.iter().all(|&x| x == 2) && dims.len() > 2 {
        let mut p = dims.to_vec();
        p.pop(); // Remove any element (all are 2, so choice doesn't matter)
        p.sort();
        return Some(p);
    }
    
    // Otherwise (some entry > 2): replace one occurrence of max with max-1, sort
    let m = *dims.iter().max().unwrap();
    let mut p = dims.to_vec();
    // Find and replace first occurrence of m with m-1
    for i in 0..p.len() {
        if p[i] == m {
            p[i] = m - 1;
            break;
        }
    }
    p.sort();
    Some(p)
}

fn next_dims_over(old_dims: &[usize]) -> Vec<Vec<usize>> {
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();
    
    // Generate candidates by incrementing each index by 1, then sorting
    for i in 0..old_dims.len() {
        let mut new_dims = old_dims.to_vec();
        new_dims[i] += 1;
        new_dims.sort();
        
        // Deduplicate
        if seen.insert(new_dims.clone()) {
            // Keep only if parent(new_dims) == old_dims
            if let Some(p) = parent(&new_dims) {
                if p == old_dims {
                    candidates.push(new_dims);
                }
            }
        }
    }
    
    candidates
}

fn expand_for_over(old_dims: &[usize]) -> Vec<(Vec<usize>, usize, Vec<usize>)> {
    let over_dims = next_dims_over(old_dims);
    let mut results = Vec::with_capacity(over_dims.len());
    for new_dims in over_dims { // preallocate reorg pattern
        let reorganization_pattern = remap(old_dims, &new_dims);
        let cap = capacity(&new_dims);
        results.push((new_dims, cap, reorganization_pattern));
    }
    results
}

fn expand_for_up(old_dims: &[usize], position: usize) -> Vec<(Vec<usize>, usize, Vec<usize>)> {
    let mut over_results = expand_for_over(old_dims);
    let mut up_dims = old_dims.to_vec();
    up_dims.insert(position, 2);
    let up_reorganization = remap_for_up(old_dims, position);
    let up_cap = capacity(&up_dims);
    over_results.push((up_dims, up_cap, up_reorganization));
    over_results
}

// ---- Internal computation helpers (formerly multiple cached functions) ----

fn impacted_locations(
    location: usize,
    location_to_index: &FxHashMap<usize, Vec<usize>>,
    index_to_location: &FxHashMap<Vec<usize>, usize>,
) -> Vec<Vec<usize>> {
    let mut res = vec![];
    let index = &location_to_index[&location];
    let indices = 0..index.len();
    for focus in indices {
        let cur = index[focus];
        let mut subres = vec![];
        for i in 0..cur {
            let mut loc = index.clone();
            loc[focus] = i;
            let fin = index_to_location[&loc];
            subres.push(fin);
        }
        res.push(subres)
    }
    res
}

fn get_impacted_phrase_locations_compute(
    dims: &[usize],
    index_to_location: &FxHashMap<usize, Vec<usize>>,
    location_to_index: &FxHashMap<Vec<usize>, usize>,
    _indices_in_order: &[Vec<usize>],
) -> Vec<Vec<Vec<usize>>> {
    (0..dims.iter().product::<usize>())
        .map(|location| impacted_locations(location, index_to_location, location_to_index))
        .collect()
}

fn get_diagonals_compute(
    dims: &[usize],
    index_to_location: &FxHashMap<usize, Vec<usize>>,
    location_to_index: &FxHashMap<Vec<usize>, usize>,
    indices: &[Vec<usize>],
) -> Vec<Vec<usize>> {
    (0..dims.iter().product::<usize>())
        .map(|location| {
            let current_index = &index_to_location[&location];
            let current_distance: usize = current_index.iter().sum();
            indices
                .iter()
                .filter(|index| *index != current_index && index.iter().sum::<usize>() == current_distance)
                .map(|x| location_to_index[x])
                .collect_vec()
        })
        .collect_vec()
}

fn index_array(dims: &[usize]) -> Vec<Vec<usize>> {
    cartesian_product(dims.iter().map(|x| (0..*x).collect()).collect())
}

fn indices_in_order_compute(dims: &[usize]) -> Vec<Vec<usize>> { order_by_distance(index_array(dims)) }

fn order_by_distance(indices: Vec<Vec<usize>>) -> Vec<Vec<usize>> {
    let mut sorted = indices;
    sorted.sort_by(|a, b| {
        match a.iter().sum::<usize>().cmp(&b.iter().sum()) {
            Ordering::Less => Ordering::Less,
            Ordering::Equal => {
                for (x, y) in a.iter().zip(b) {
                    if x > y { return Ordering::Greater; }
                    if x < y { return Ordering::Less; }
                }
                unreachable!("Duplicate indices impossible")
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
                .map(|y| { let mut vec = xs.clone(); vec.push(y); vec })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn cartesian_product<T: Clone>(lists: Vec<Vec<T>>) -> Vec<Vec<T>> {
    match lists.split_first() {
        Some((first, rest)) => {
            let init: Vec<Vec<T>> = first.iter().cloned().map(|n| vec![n]).collect();
            rest.iter().cloned().fold(init, |vec, list| partial_cartesian(vec, list))
        }
        None => vec![],
    }
}

// ----------------- Tests (adapted) -----------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_produces_an_index_matrix_with_dims() {
        assert_eq!(index_array(&vec![3, 3]), vec![
            vec![0, 0], vec![0, 1], vec![0, 2],
            vec![1, 0], vec![1, 1], vec![1, 2],
            vec![2, 0], vec![2, 1], vec![2, 2]
        ]);
    }

    #[test]
    fn it_orders_indices() {
        assert_eq!(order_by_distance(vec![
            vec![0, 0], vec![0, 1], vec![0, 2],
            vec![1, 0], vec![1, 1], vec![1, 2],
            vec![2, 0], vec![2, 1], vec![2, 2]
        ]), vec![
            vec![0, 0], vec![0, 1], vec![1, 0],
            vec![0, 2], vec![1, 1], vec![2, 0],
            vec![1, 2], vec![2, 1], vec![2, 2]
        ]);
    }

    #[test]
    fn it_gets_impacted_phrase_locations() {
        let (phrases, _diag) = get_requirements(3, &[2,2]);
        assert_eq!(phrases, vec![vec![1], vec![2]]);
    }

    #[test]
    fn it_gets_impacted_diagonals() {
        let (_, diag) = get_requirements(5, &[3,3]);
        assert_eq!(diag, vec![3,4]);
    }

    #[test]
    fn it_provides_capacity_information_by_dims() {
        assert_eq!(capacity(&vec![2,2]), 4);
        assert_eq!(capacity(&vec![2,2,2]), 8);
    }

    #[test]
    fn it_determines_if_dims_are_base() {
        assert!(is_base(&vec![2,2]));
        assert!(!is_base(&vec![3,2]));
    }

    #[test]
    fn it_determines_the_reorganization_pattern_for_over() {
        assert_eq!(remap(&vec![2,2], &vec![3,2]), vec![0,1,2,3]);
    }

    #[test]
    fn it_pads_at_a_position_for_up() {
        assert_eq!(pad(&vec![2,2], 0), vec![
            vec![0,0,0], vec![0,1,0], vec![1,0,0], vec![1,1,0]
        ]);
    }

    #[test]
    fn it_determines_the_reorganization_pattern_for_up() {
        assert_eq!(remap_for_up(&vec![2,2], 0), vec![0,2,3,6]);
    }

    #[test]
    fn it_expands_for_over() {
        assert_eq!(expand_over(&vec![2,2]), vec![(vec![2,3],6,vec![0,1,2,4])]);
    }

    #[test]
    fn it_expands_for_up() {
        assert_eq!(expand_up(&vec![2,2],0)[1].0, vec![2,2,2]);
    }

    #[test]
    fn it_gets_axis_positions() {
        assert_eq!(get_axis_positions(&[2,2]), vec![1,2]);
    }

    #[test]
    fn parent_of_root_is_none() {
        assert_eq!(parent(&[2, 2]), None);
    }

    #[test]
    fn parent_of_all_twos_removes_one_two() {
        // [2,2,2] with all 2s and len > 2 → remove one 2 → [2,2]
        assert_eq!(parent(&[2, 2, 2]), Some(vec![2, 2]));
        // [2,2,2,2] → [2,2,2]
        assert_eq!(parent(&[2, 2, 2, 2]), Some(vec![2, 2, 2]));
    }

    #[test]
    fn parent_of_shape_with_entry_greater_than_two_decrements_max() {
        // [2,3]: max=3, replace 3 with 2 → [2,2] sorted
        assert_eq!(parent(&[2, 3]), Some(vec![2, 2]));
        // [2,4]: max=4, replace 4 with 3 → [2,3] sorted
        assert_eq!(parent(&[2, 4]), Some(vec![2, 3]));
        // [3,3]: max=3, replace one 3 with 2 → [2,3] sorted
        assert_eq!(parent(&[3, 3]), Some(vec![2, 3]));
        // [2,2,3]: max=3, replace 3 with 2 → [2,2,2] sorted
        assert_eq!(parent(&[2, 2, 3]), Some(vec![2, 2, 2]));
        // [2,3,3]: max=3, replace one 3 with 2 → [2,2,3] sorted
        assert_eq!(parent(&[2, 3, 3]), Some(vec![2, 2, 3]));
    }

    #[test]
    fn expand_over_generates_children_matching_parent_rule() {
        // From [2,2]: increment each dimension, sort, keep if parent matches [2,2]
        // Increment 0th: [3,2] → sorted [2,3], parent([2,3]) = [2,2] ✓
        // Increment 1st: [2,3] → sorted [2,3], parent([2,3]) = [2,2] ✓
        // Deduplicated: [2,3]
        let result = expand_over(&vec![2, 2]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, vec![2, 3]);
        
        // From [2,3]: increment each dimension, sort, keep if parent matches [2,3]
        // Increment 0th: [3,3] → sorted [3,3], parent([3,3]) = [2,3] ✓
        // Increment 1st: [2,4] → sorted [2,4], parent([2,4]) = [2,3] ✓
        let result = expand_over(&vec![2, 3]);
        assert_eq!(result.len(), 2);
        // Results should be [3,3] and [2,4]
        let dims: Vec<_> = result.iter().map(|r| r.0.clone()).collect();
        assert!(dims.contains(&vec![2, 4]));
        assert!(dims.contains(&vec![3, 3]));
    }

    #[test]
    fn expand_over_from_all_twos_three_dim() {
        // From [2,2,2]: increment each dimension, sort, keep if parent matches [2,2,2]
        // Increment 0th: [3,2,2] → sorted [2,2,3], parent([2,2,3]) = [2,2,2] ✓
        // All increments give [2,2,3] after sorting
        let result = expand_over(&vec![2, 2, 2]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, vec![2, 2, 3]);
    }

    #[test]
    fn it_gets_diagonals_2x2() {
        // In [2,2]:
        // Position 0: [0,0] distance 0 - no other positions at distance 0
        // Position 1: [0,1] distance 1 - position 2 [1,0] is also at distance 1
        // Position 2: [1,0] distance 1 - position 1 [0,1] is also at distance 1
        // Position 3: [1,1] distance 2 - no other positions at distance 2
        let (_, diagonals_0) = get_requirements(0, &[2,2]);
        let (_, diagonals_1) = get_requirements(1, &[2,2]);
        let (_, diagonals_2) = get_requirements(2, &[2,2]);
        let (_, diagonals_3) = get_requirements(3, &[2,2]);
        assert_eq!(diagonals_0, Vec::<usize>::new());
        assert_eq!(diagonals_1, vec![2]);
        assert_eq!(diagonals_2, vec![1]);
        assert_eq!(diagonals_3, Vec::<usize>::new());
    }

    #[test]
    fn it_gets_diagonals_2x3() {
        // In [2,3]:
        // [[0,0], [0,1], [1,0], [0,2], [1,1], [1,2]]
        // Positions:  0      1      2      3      4      5
        // Distance 0: position 0 [0,0]
        // Distance 1: positions 1 [0,1], 2 [1,0]
        // Distance 2: positions 3 [0,2], 4 [1,1]
        // Distance 3: position 5 [1,2]
        let (_, diagonals_0) = get_requirements(0, &[2,3]);
        let (_, diagonals_1) = get_requirements(1, &[2,3]);
        let (_, diagonals_2) = get_requirements(2, &[2,3]);
        let (_, diagonals_3) = get_requirements(3, &[2,3]);
        let (_, diagonals_4) = get_requirements(4, &[2,3]);
        let (_, diagonals_5) = get_requirements(5, &[2,3]);
        assert_eq!(diagonals_0, Vec::<usize>::new());
        assert_eq!(diagonals_1, vec![2]);
        assert_eq!(diagonals_2, vec![1]);
        assert_eq!(diagonals_3, vec![4]);
        assert_eq!(diagonals_4, vec![3]);
        assert_eq!(diagonals_5, Vec::<usize>::new());
    }
}
