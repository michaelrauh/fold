pub mod error;
pub mod interner;
pub mod ortho;
pub mod ortho_database;
pub mod queue;
pub mod spatial;
pub mod splitter;

pub use error::*;
pub use interner::*;
pub use ortho_database::*;
pub use queue::*;

#[cfg(test)]
mod follower_diff_tests {
    use super::*;
    use crate::interner::InMemoryInternerHolder;
    use crate::queue::MockQueue;

    fn build_low_high(low_text: &str, high_text: &str) -> (crate::interner::Interner, crate::interner::Interner) {
        // low_text ingested first, then high_text appended to create new version
        let mut holder = InMemoryInternerHolder::new().unwrap();
        let mut q = MockQueue::new();
        holder.add_text_with_seed(low_text, &mut q).unwrap(); // version 1
        holder.add_text_with_seed(high_text, &mut q).unwrap(); // version 2
        let low = holder.get(1).unwrap();
        let high = holder.get(2).unwrap();
        (low, high)
    }

    #[test]
    fn test_delta_intersection_adds_only_new() {
        let (low, high) = build_low_high("a b", "a c"); // low has a b; high adds a c
        // Construct ortho with single token 'a' (index 0)
        let mut o = crate::ortho::Ortho::new(low.version());
        o = o.add(0, low.version()).pop().unwrap();
        let (forbidden, required) = o.get_requirements();
        assert!(forbidden.is_empty());
        assert_eq!(required, vec![vec![0]]);
        let low_set: std::collections::HashSet<usize> = low.intersect(&required, &forbidden).into_iter().collect();
        let high_set: std::collections::HashSet<usize> = high.intersect(&required, &forbidden).into_iter().collect();
        assert!(low_set.contains(&1)); // 'b'
        assert!(!low_set.contains(&2)); // 'c' absent in low
        assert!(high_set.contains(&1) && high_set.contains(&2));
        let delta: Vec<usize> = high_set.difference(&low_set).copied().collect();
        assert_eq!(delta, vec![2]);
    }

    #[test]
    fn test_delta_union_intersection_logic() {
        let (low, high) = build_low_high("a b", "a c");
        let mut o = crate::ortho::Ortho::new(low.version());
        o = o.add(0, low.version()).pop().unwrap();
        let (forbidden, required) = o.get_requirements();
        let low_set: std::collections::HashSet<usize> = low.intersect(&required, &forbidden).into_iter().collect();
        let high_set: std::collections::HashSet<usize> = high.intersect(&required, &forbidden).into_iter().collect();
        assert!(low_set.contains(&1));
        assert!(!low_set.contains(&2));
        assert!(high_set.contains(&2));
        let diff: Vec<usize> = high_set.difference(&low_set).copied().collect();
        assert_eq!(diff, vec![2]);
    }
}
