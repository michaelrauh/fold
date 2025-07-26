use crate::queue::Queue;
use crate::splitter::Splitter;
use fixedbitset::FixedBitSet;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct Interner {
    version: usize,
    vocabulary: Vec<String>,
    prefix_to_completions: HashMap<Vec<usize>, FixedBitSet>,
}

impl Interner {
    pub fn from_text(text: &str) -> Self {
        let splitter = Splitter::new();
        let vocab = splitter.vocabulary(text);
        let phrases = splitter.phrases(text);
        let mut vocabulary = Vec::new();
        for word in &vocab {
            if !vocabulary.contains(word) {
                vocabulary.push(word.clone());
            }
        }
        let new_vocab_len = vocabulary.len();
        let mut prefix_to_completions = HashMap::new();
        for phrase in &phrases {
            if phrase.len() < 2 {
                continue;
            }
            let indices: Vec<usize> = phrase
                .iter()
                .map(|word| {
                    vocabulary
                        .iter()
                        .position(|v| v == word)
                        .expect("Word should be in vocabulary")
                })
                .collect();
            let prefix = indices[..indices.len() - 1].to_vec();
            let completion_word_index = indices[indices.len() - 1];
            if completion_word_index < new_vocab_len {
                let bitset = prefix_to_completions.entry(prefix).or_insert_with(|| {
                    let mut fbs = FixedBitSet::with_capacity(new_vocab_len);
                    fbs.grow(new_vocab_len);
                    fbs
                });
                bitset.insert(completion_word_index);
            }
        }
        Interner {
            version: 1,
            vocabulary,
            prefix_to_completions,
        }
    }

    pub fn add_text(&self, text: &str) -> Self {
        if text.trim().is_empty() {
            return Interner {
                version: self.version + 1,
                vocabulary: self.vocabulary.clone(),
                prefix_to_completions: self.prefix_to_completions.clone(),
            };
        }
        let splitter = Splitter::new();
        let vocab = splitter.vocabulary(text);
        let phrases = splitter.phrases(text);

        let mut vocabulary = self.vocabulary.clone();
        for word in &vocab {
            if !vocabulary.contains(word) {
                vocabulary.push(word.clone());
            }
        }
        let new_vocab_len = vocabulary.len();

        let mut prefix_to_completions = self.prefix_to_completions.clone();
        for bitset in prefix_to_completions.values_mut() {
            bitset.grow(new_vocab_len);
        }

        for phrase in &phrases {
            if phrase.len() < 2 {
                continue;
            }
            let indices: Vec<usize> = phrase
                .iter()
                .map(|word| {
                    vocabulary
                        .iter()
                        .position(|v| v == word)
                        .expect("Word should be in vocabulary")
                })
                .collect();
            let prefix = indices[..indices.len() - 1].to_vec();
            let completion_word_index = indices[indices.len() - 1];
            if completion_word_index < new_vocab_len {
                let bitset = prefix_to_completions.entry(prefix).or_insert_with(|| {
                    let mut fbs = FixedBitSet::with_capacity(new_vocab_len);
                    fbs.grow(new_vocab_len);
                    fbs
                });
                bitset.insert(completion_word_index);
            }
        }
        Interner {
            version: self.version + 1,
            vocabulary,
            prefix_to_completions,
        }
    }

    pub fn version(&self) -> usize {
        self.version
    }

    pub fn vocabulary(&self) -> &[String] {
        &self.vocabulary
    }

    pub fn string_for_index(&self, index: usize) -> &str {
        self.vocabulary
            .get(index)
            .map(|s| s.as_str())
            .expect("Index out of bounds in Interner::string_for_index")
    }

    fn get_required_bits(&self, required: &[Vec<usize>]) -> FixedBitSet {
        let mut result = FixedBitSet::with_capacity(self.vocabulary.len());
        result.grow(self.vocabulary.len());
        if required.is_empty() {
            result.set_range(.., true);
            return result;
        }
        let mut first = true;
        for prefix in required {
            if let Some(bitset) = self.prefix_to_completions.get(prefix) {
                if first {
                    result.clone_from(bitset);
                    first = false;
                } else {
                    result.intersect_with(bitset);
                }
            } else {
                // If any required prefix is missing, intersection is empty
                result.clear();
                break;
            }
        }
        result
    }

    fn get_forbidden_bits(&self, forbidden: &[usize]) -> FixedBitSet {
        let mut bitset = FixedBitSet::with_capacity(self.vocabulary.len());
        bitset.grow(self.vocabulary.len());
        if forbidden.is_empty() {
            bitset.set_range(.., true);
            return bitset;
        }
        bitset.set_range(.., true);
        for &idx in forbidden {
            bitset.set(idx, false);
        }
        bitset
    }

    pub fn intersect(&self, required: &[Vec<usize>], forbidden: &[usize]) -> Vec<usize> {
        let required_bits = self.get_required_bits(required);
        let forbidden_bits = self.get_forbidden_bits(forbidden);
        let mut intersection = required_bits.clone();
        intersection.intersect_with(&forbidden_bits);
        intersection.ones().collect()
    }
}

pub struct InternerHolder {
    pub interners: std::collections::HashMap<usize, Interner>,
    pub workq: Arc<Queue>,
}

impl InternerHolder {
    pub fn from_text(text: &str, workq: Arc<Queue>) -> Self {
        let interner = Interner::from_text(text);
        let mut interners = HashMap::new();
        interners.insert(interner.version(), interner);
        InternerHolder { interners, workq }
    }

    pub async fn add_with_seed(&mut self, interner: Interner) {
        let version = interner.version();
        self.interners.insert(version, interner.clone());
        let ortho_seed = crate::ortho::Ortho::new(version);
        self.workq.push_many(vec![ortho_seed]).await;
    }

    pub fn get(&self, version: usize) -> &Interner {
        self.interners
            .get(&version)
            .expect("Version not found in InternerHolder")
    }

    pub fn latest_version(&self) -> usize {
        *self.interners.keys().max().expect("No interners in holder")
    }

    pub fn get_latest(&self) -> &Interner {
        let latest_version = self.latest_version();
        self.get(latest_version)
    }

    pub fn compare_prefix_bitsets(&self, prefix: Vec<usize>, v1: usize, v2: usize) -> bool {
        let interner1 = match self.interners.get(&v1) {
            Some(i) => i,
            None => return false,
        };
        let interner2 = match self.interners.get(&v2) {
            Some(i) => i,
            None => return false,
        };
        let bitset1 = interner1.prefix_to_completions.get(&prefix);
        let bitset2 = interner2.prefix_to_completions.get(&prefix);
        match (bitset1, bitset2) {
            (Some(b1), Some(b2)) => b1 == b2,
            _ => false,
        }
    }

    pub fn remove_by_version(&mut self, version: usize) -> bool {
        self.interners.remove(&version).is_some()
    }

    pub fn completion_map_size(&self) -> usize {
        self.get_latest().prefix_to_completions.len()
    }

    pub async fn with_seed(text: &str, workq: Arc<Queue>) -> Self {
        let holder = InternerHolder::from_text(text, workq);
        let version = holder.latest_version();
        let ortho_seed = crate::ortho::Ortho::new(version);
        holder.workq.push_many(vec![ortho_seed.clone()]).await;
        holder
    }

    pub async fn add_text_with_seed(&mut self, text: &str) {
        let interner = if self.interners.is_empty() {
            Interner::from_text(text)
        } else {
            let latest = self.interners.values().max_by_key(|i| i.version()).unwrap();
            latest.add_text(text)
        };

        self.interners.insert(interner.version(), interner.clone());

        let version = interner.version();

        let ortho_seed = crate::ortho::Ortho::new(version);

        self.workq.push_many(vec![ortho_seed]).await;
    }

    pub async fn has_version(&self, version: usize) -> bool {
        self.interners.contains_key(&version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Queue;
    use fixedbitset::FixedBitSet;
    use std::sync::Arc;

    fn holder_from_text(text: &str) -> InternerHolder {
        InternerHolder::from_text(text, Arc::new(Queue::new("test", 8)))
    }

    #[test]
    fn test_from_text_creates_interner() {
        let holder = holder_from_text("hello world");
        let interner = holder.get(holder.latest_version());
        assert_eq!(interner.version(), 1);
        assert_eq!(interner.vocabulary.len(), 2);
        assert_eq!(interner.prefix_to_completions.len(), 1);
    }
    #[test]
    fn test_add_increments_version() {
        let holder = holder_from_text("hello world");
        let interner = holder.get(holder.latest_version());
        let interner2 = interner.add_text("test");
        assert_eq!(interner2.version(), 2);
    }
    #[test]
    fn test_add_extends_vocabulary() {
        let holder = holder_from_text("hello world");
        let interner = holder.get(holder.latest_version());
        assert_eq!(interner.vocabulary, vec!["hello", "world"]);
        let interner2 = interner.add_text("test hello");
        assert_eq!(interner2.vocabulary, vec!["hello", "world", "test"]);
    }
    #[test]
    fn test_add_builds_prefix_mapping() {
        let holder = holder_from_text("a b c");
        let interner = holder.get(holder.latest_version());
        let vocab_len = interner.vocabulary.len();
        // prefix [0] should map to {1}
        let bitset_0 = interner.prefix_to_completions.get(&vec![0]).unwrap();
        let mut expected_0 = FixedBitSet::with_capacity(vocab_len);
        expected_0.grow(vocab_len);
        expected_0.insert(1);
        assert_eq!(*bitset_0, expected_0, "prefix [0] mismatch");
        // prefix [1] should map to {2}
        let bitset_1 = interner.prefix_to_completions.get(&vec![1]).unwrap();
        let mut expected_1 = FixedBitSet::with_capacity(vocab_len);
        expected_1.grow(vocab_len);
        expected_1.insert(2);
        assert_eq!(*bitset_1, expected_1, "prefix [1] mismatch");
        // prefix [0, 1] should map to {2}
        let bitset_01 = interner.prefix_to_completions.get(&vec![0, 1]).unwrap();
        let mut expected_01 = FixedBitSet::with_capacity(vocab_len);
        expected_01.grow(vocab_len);
        expected_01.insert(2);
        assert_eq!(*bitset_01, expected_01, "prefix [0, 1] mismatch");
    }
    #[test]
    fn test_add_handles_longer_phrases() {
        let holder = holder_from_text("a b c");
        let interner = holder.get(holder.latest_version());
        // Should have prefix [0] -> bit 1 set (for "b")
        let prefix_0 = vec![0];
        let bitset_0 = interner.prefix_to_completions.get(&prefix_0).unwrap();
        assert!(!bitset_0.contains(0));
        assert!(bitset_0.contains(1));
        assert!(!bitset_0.contains(2));
        // Should have prefix [0, 1] -> bit 2 set (for "c")
        let prefix_01 = vec![0, 1];
        let bitset_01 = interner.prefix_to_completions.get(&prefix_01).unwrap();
        assert!(!bitset_01.contains(0));
        assert!(!bitset_01.contains(1));
        assert!(bitset_01.contains(2));
    }
    #[test]
    fn test_add_extends_existing_bitsets() {
        // First add with 2 words
        let holder = holder_from_text("a b");
        let interner = holder.get(holder.latest_version());
        // Second add with 1 more word
        let interner2 = interner.add_text("a c");
        let prefix = vec![0];
        let bitset = interner2.prefix_to_completions.get(&prefix).unwrap();
        // Bitset should now have length 3 and both bits 1 and 2 should be set
        assert_eq!(bitset.len(), 3);
        assert!(!bitset.contains(0));
        assert!(bitset.contains(1)); // from first add
        assert!(bitset.contains(2)); // from second add
    }
    #[test]
    fn test_get_required_bits() {
        let holder = holder_from_text("a b c");
        let interner = holder.get(holder.latest_version());
        // prefix [0] should map to {1}
        let required = vec![vec![0]];
        let bits = interner.get_required_bits(&required);
        // The bitset for prefix [0] should have bit 1 set
        let bitset = interner.prefix_to_completions.get(&vec![0]).unwrap();
        assert_eq!(bits, *bitset);
    }
    #[test]
    fn test_string_for_index() {
        let holder = holder_from_text("foo bar baz");
        let interner = holder.get(holder.latest_version());
        let vocab = interner.vocabulary();
        assert_eq!(interner.string_for_index(0), vocab[0]);
        assert_eq!(interner.string_for_index(1), vocab[1]);
        assert_eq!(interner.string_for_index(2), vocab[2]);
    }
    #[test]
    #[should_panic(expected = "Index out of bounds in Interner::string_for_index")]
    fn test_string_for_index_out_of_bounds_panics() {
        let holder = holder_from_text("foo bar baz");
        let interner = holder.get(holder.latest_version());
        interner.string_for_index(3);
    }
}

#[cfg(test)]
mod container_tests {
    use super::*;
    #[test]
    fn test_insert_and_get() {
        let holder = InternerHolder::from_text("a b", Arc::new(Queue::new("test", 8)));
        let latest_version = holder.latest_version();
        let interner = holder.get(latest_version);
        assert_eq!(interner.version(), latest_version);
    }
    #[test]
    fn test_compare_prefix_bitsets_equal() {
        let mut holder = InternerHolder::from_text("a b", Arc::new(Queue::new("test", 8)));
        let interner1 = holder.get(holder.latest_version());
        let interner2 = interner1.add_text("");
        holder
            .interners
            .insert(interner2.version(), interner2.clone());
        let prefix = vec![0];
        let v1 = 1;
        let v2 = 2;
        assert!(holder.compare_prefix_bitsets(prefix, v1, v2));
    }
    #[test]
    fn test_compare_prefix_bitsets_different() {
        let mut holder = InternerHolder::from_text("a b", Arc::new(Queue::new("test", 8)));
        let interner1 = holder.get(holder.latest_version());
        let interner2 = interner1.add_text("c");
        holder
            .interners
            .insert(interner2.version(), interner2.clone());
        let prefix = vec![0];
        let v1 = 1;
        let v2 = 2;
        assert!(!holder.compare_prefix_bitsets(prefix, v1, v2));
    }
    #[test]
    fn test_compare_prefix_bitsets_missing() {
        let mut holder = InternerHolder::from_text("a b", Arc::new(Queue::new("test", 8)));
        let interner1 = holder.get(holder.latest_version());
        let interner2 = interner1.add_text("c");
        holder
            .interners
            .insert(interner2.version(), interner2.clone());
        let prefix = vec![99]; // non-existent prefix
        let v1 = 1;
        let v2 = 2;
        assert!(!holder.compare_prefix_bitsets(prefix, v1, v2));
    }
}

#[cfg(test)]
mod holder_tests {
    use super::*;
    use crate::queue::Queue;
    use std::sync::Arc;
    #[tokio::test]
    async fn test_holder_new_initializes_empty() {
        let holder = InternerHolder::from_text("", Arc::new(Queue::new("test", 8)));
        assert_eq!(holder.interners.len(), 1);
        assert_eq!(holder.latest_version(), 1);
    }
    #[tokio::test]
    async fn test_holder_add_text_with_seed_increments_version() {
        let mut holder = InternerHolder::from_text("", Arc::new(Queue::new("test", 8)));
        holder.add_text_with_seed("foo bar").await;
        assert_eq!(holder.latest_version(), 2);
        holder.add_text_with_seed("baz").await;
        assert_eq!(holder.latest_version(), 3);
    }
    #[tokio::test]
    async fn test_holder_latest_version_returns_correct_value() {
        let mut holder = InternerHolder::from_text("", Arc::new(Queue::new("test", 8)));
        holder.add_text_with_seed("a b").await;
        holder.add_text_with_seed("c").await;
        assert_eq!(holder.latest_version(), 3);
    }
    #[tokio::test]
    async fn test_holder_compare_prefix_bitsets() {
        let mut holder = InternerHolder::from_text("a b", Arc::new(Queue::new("test", 8)));
        holder.add_text_with_seed("").await;
        let prefix = vec![0];
        let v1 = 1;
        let v2 = 2;
        let b1 = holder
            .interners
            .get(&v1)
            .unwrap()
            .prefix_to_completions
            .get(&prefix);
        let b2 = holder
            .interners
            .get(&v2)
            .unwrap()
            .prefix_to_completions
            .get(&prefix);

        assert!(holder.compare_prefix_bitsets(prefix.clone(), v1, v2));
        holder.add_text_with_seed("c").await;
        let v3 = 3;
        assert!(!holder.compare_prefix_bitsets(vec![0], v1, v3));
    }
    #[tokio::test]
    async fn test_holder_remove_by_version() {
        let mut holder = InternerHolder::from_text("a b", Arc::new(Queue::new("test", 8)));
        holder.add_text_with_seed("c").await;
        let v1 = 1;
        let v2 = 2;
        assert!(holder.interners.contains_key(&v1));
        assert!(holder.interners.contains_key(&v2));
        assert!(holder.remove_by_version(v1));
        assert!(!holder.interners.contains_key(&v1));
        assert!(holder.interners.contains_key(&v2));
        // Removing again should return false
        assert!(!holder.remove_by_version(v1));
    }
}

#[cfg(test)]
mod intersect_logic_tests {
    use super::*;
    use fixedbitset::FixedBitSet;

    fn make_interner_with_vocab(vocab: Vec<&str>, prefix_map: Vec<(Vec<usize>, Vec<usize>)>) -> Interner {
        let vocabulary = vocab.into_iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut prefix_to_completions = std::collections::HashMap::new();
        let vocab_len = vocabulary.len();
        for (prefix, completions) in prefix_map {
            let mut fbs = FixedBitSet::with_capacity(vocab_len);
            fbs.grow(vocab_len);
            for idx in completions {
                fbs.insert(idx);
            }
            prefix_to_completions.insert(prefix, fbs);
        }
        Interner {
            version: 1,
            vocabulary,
            prefix_to_completions,
        }
    }

    #[test]
    fn test_intersect_all_empty_returns_all_indexes() {
        let interner = make_interner_with_vocab(vec!["a", "b", "c"], vec![]);
        let result = interner.intersect(&[], &[]);
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn test_intersect_required_and_forbidden() {
        // required: [00110, 00010] (as bitsets)
        // forbidden: [1]
        // expected: 00010 (index 3)
        let interner = make_interner_with_vocab(
            vec!["a", "b", "c", "d", "e"],
            vec![
                (vec![0], vec![2, 3]), // 00110
                (vec![1], vec![3]),    // 00010
            ],
        );
        let required = vec![vec![0], vec![1]];
        let forbidden = vec![1];
        let result = interner.intersect(&required, &forbidden);
        // Only index 3 should be present
        assert_eq!(result, vec![3]);
    }

    #[test]
    fn test_intersect_required_anded() {
        // required: [101, 110] => AND = 100
        let interner = make_interner_with_vocab(
            vec!["a", "b", "c"],
            vec![
                (vec![0], vec![0, 2]), // 101
                (vec![1], vec![0, 1]), // 110
            ],
        );
        let required = vec![vec![0], vec![1]];
        let forbidden = vec![];
        let result = interner.intersect(&required, &forbidden);
        // Only index 0 should be present
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn test_intersect_forbidden_zeroes_out() {
        // required: [111]
        // forbidden: [1]
        let interner = make_interner_with_vocab(
            vec!["a", "b", "c"],
            vec![(vec![0], vec![0, 1, 2])], // 111
        );
        let required = vec![vec![0]];
        let forbidden = vec![1];
        let result = interner.intersect(&required, &forbidden);
        // Should be [0, 2]
        assert_eq!(result, vec![0, 2]);
    }

    #[test]
    fn test_intersect_bug_case() {
        // This test is expected to fail with the current implementation
        // required: [00110, 00010] (as bitsets)
        // forbidden: []
        // expected: 00010 (index 3)
        let interner = make_interner_with_vocab(
            vec!["a", "b", "c", "d", "e"],
            vec![
                (vec![0], vec![2, 3]), // 00110
                (vec![1], vec![3]),    // 00010
            ],
        );
        let required = vec![vec![0], vec![1]];
        let forbidden = vec![];
        let result = interner.intersect(&required, &forbidden);
        // Only index 3 should be present
        assert_eq!(result, vec![3]); // This will fail with the current code
    }
}
