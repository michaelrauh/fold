use std::collections::HashMap;
use fixedbitset::FixedBitSet;
use crate::splitter::Splitter;

pub struct Interner {
    version: u64,
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
            let indices: Vec<usize> = phrase.iter().map(|word| {
                vocabulary.iter().position(|v| v == word).expect("Word should be in vocabulary")
            }).collect();
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
            let indices: Vec<usize> = phrase.iter().map(|word| {
                vocabulary.iter().position(|v| v == word).expect("Word should be in vocabulary")
            }).collect();
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

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn vocabulary(&self) -> &[String] {
        &self.vocabulary
    }

    pub fn string_for_index(&self, index: usize) -> &str {
        self.vocabulary.get(index)
            .map(|s| s.as_str())
            .expect("Index out of bounds in Interner::string_for_index")
    }

    fn get_required_bits(&self, required: &[Vec<usize>]) -> FixedBitSet {
        let mut result = FixedBitSet::with_capacity(self.vocabulary.len());
        if required.is_empty() {
            result.grow(self.vocabulary.len());
            result.set_range(.., true);
            return result;
        }
        for prefix in required {
            if let Some(bitset) = self.prefix_to_completions.get(prefix) {
                result.union_with(bitset);
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

#[cfg(test)]
mod tests {
    use super::*;
    use fixedbitset::FixedBitSet;

    #[test]
    fn test_from_text_creates_interner() {
        let interner = Interner::from_text("hello world");
        assert_eq!(interner.version(), 1);
        assert_eq!(interner.vocabulary.len(), 2);
        assert_eq!(interner.prefix_to_completions.len(), 1);
    }

    #[test]
    fn test_add_increments_version() {
        let interner = Interner::from_text("hello world");
        assert_eq!(interner.version(), 1);
        let interner2 = interner.add_text("test");
        assert_eq!(interner2.version(), 2);
    }

    #[test]
    fn test_add_extends_vocabulary() {
        let interner = Interner::from_text("hello world");
        assert_eq!(interner.vocabulary, vec!["hello", "world"]);
        let interner2 = interner.add_text("test hello");
        assert_eq!(interner2.vocabulary, vec!["hello", "world", "test"]);
    }

    #[test]
    fn test_add_builds_prefix_mapping() {
        let interner = Interner::from_text("a b c");
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
        let interner = Interner::from_text("a b c");
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
        let interner = Interner::from_text("a b");
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
        let interner = Interner::from_text("a b c");
        // prefix [0] should map to {1}
        let required = vec![vec![0]];
        let bits = interner.get_required_bits(&required);
        // The bitset for prefix [0] should have bit 1 set
        let bitset = interner.prefix_to_completions.get(&vec![0]).unwrap();
        assert_eq!(bits, *bitset);
    }

    #[test]
    fn test_string_for_index() {
        let interner = Interner::from_text("foo bar baz");
        let vocab = interner.vocabulary();
        assert_eq!(interner.string_for_index(0), vocab[0]);
        assert_eq!(interner.string_for_index(1), vocab[1]);
        assert_eq!(interner.string_for_index(2), vocab[2]);
    }

    #[test]
    #[should_panic(expected = "Index out of bounds in Interner::string_for_index")]
    fn test_string_for_index_out_of_bounds_panics() {
        let interner = Interner::from_text("foo bar baz");
        interner.string_for_index(3);
    }
}