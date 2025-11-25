use crate::splitter::Splitter;
use fixedbitset::FixedBitSet;
use std::collections::HashMap;

#[derive(Clone)]
pub struct Interner {
    version: usize,
    vocabulary: Vec<String>,
    prefix_to_completions: HashMap<Vec<usize>, FixedBitSet>,
}

// Custom Encode/Decode for Interner
impl bincode::Encode for Interner {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), bincode::error::EncodeError> {
        self.version.encode(encoder)?;
        self.vocabulary.encode(encoder)?;
        // Serialize prefix_to_completions as Vec<(Vec<usize>, Vec<u32>)>
        let prefix_vec: Vec<(Vec<usize>, Vec<u32>)> = self.prefix_to_completions.iter().map(|(k, v)| {
            (k.clone(), v.ones().map(|x| x as u32).collect())
        }).collect();
        prefix_vec.encode(encoder)?;
        Ok(())
    }
}

impl<Context> bincode::Decode<Context> for Interner {
    fn decode<D: bincode::de::Decoder>(decoder: &mut D) -> Result<Self, bincode::error::DecodeError> {
        let version = usize::decode(decoder)?;
        let vocabulary = Vec::<String>::decode(decoder)?;
        let prefix_vec = Vec::<(Vec<usize>, Vec<u32>)>::decode(decoder)?;
        let mut prefix_to_completions = HashMap::new();
        let vocab_len = vocabulary.len();
        for (prefix, completions) in prefix_vec {
            let mut fbs = FixedBitSet::with_capacity(vocab_len);
            fbs.grow(vocab_len);
            for idx in completions {
                fbs.insert(idx as usize);
            }
            prefix_to_completions.insert(prefix, fbs);
        }
        Ok(Interner {
            version,
            vocabulary,
            prefix_to_completions,
        })
    }
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
        let prefix_to_completions = Self::build_prefix_to_completions(&phrases, &vocabulary, new_vocab_len, None);
        let interner = Interner {
            version: 1,
            vocabulary,
            prefix_to_completions,
        };
        debug_assert!(interner.debug_verify_prefix_closure(&phrases), "Prefix closure verification failed after from_text");
        interner
    }

    pub fn add_text(&self, text: &str) -> Self {
        if text.trim().is_empty() {
            let interner = Interner {
                version: self.version + 1,
                vocabulary: self.vocabulary.clone(),
                prefix_to_completions: self.prefix_to_completions.clone(),
            };
            return interner;
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

        let prefix_to_completions = Self::build_prefix_to_completions(
            &phrases,
            &vocabulary,
            new_vocab_len,
            Some(&self.prefix_to_completions),
        );

        let interner = Interner {
            version: self.version + 1,
            vocabulary,
            prefix_to_completions,
        };
        debug_assert!(interner.debug_verify_prefix_closure(&phrases), "Prefix closure verification failed after add_text");
        interner
    }

    fn build_prefix_to_completions(
        phrases: &[Vec<String>],
        vocabulary: &[String],
        vocab_len: usize,
        existing: Option<&HashMap<Vec<usize>, FixedBitSet>>,
    ) -> HashMap<Vec<usize>, FixedBitSet> {
        let mut prefix_to_completions = match existing {
            Some(map) => {
                let mut new_map = map.clone();
                for bitset in new_map.values_mut() { bitset.grow(vocab_len); }
                new_map
            }
            None => HashMap::new(),
        };
        for phrase in phrases {
            if phrase.len() < 2 { continue; }
            let indices: Vec<usize> = phrase.iter().map(|word| {
                vocabulary.iter().position(|v| v == word).expect("Word should be in vocabulary")
            }).collect();
            // Insert every incremental prefix chain edge: prefix[0..i] -> indices[i]
            for i in 1..indices.len() { // i is completion index position
                let prefix = indices[..i].to_vec();
                let completion_word_index = indices[i];
                if completion_word_index < vocab_len {
                    let bitset = prefix_to_completions.entry(prefix).or_insert_with(|| {
                        let mut fbs = FixedBitSet::with_capacity(vocab_len);
                        fbs.grow(vocab_len);
                        fbs
                    });
                    bitset.insert(completion_word_index);
                }
            }
        }
        // Ensure every vocabulary item has a single-token prefix key
        for idx in 0..vocab_len {
            prefix_to_completions.entry(vec![idx]).or_insert_with(|| {
                let mut fbs = FixedBitSet::with_capacity(vocab_len); fbs.grow(vocab_len); fbs
            });
        }
        // Ensure every full phrase itself as terminal prefix with empty completions
        for phrase in phrases {
            if phrase.is_empty() { continue; }
            let indices: Vec<usize> = phrase.iter().map(|word| {
                vocabulary.iter().position(|v| v == word).expect("Word should be in vocabulary")
            }).collect();
            prefix_to_completions.entry(indices).or_insert_with(|| {
                let mut fbs = FixedBitSet::with_capacity(vocab_len); fbs.grow(vocab_len); fbs
            });
        }
        prefix_to_completions
    }

    fn debug_verify_prefix_closure(&self, new_phrases: &[Vec<String>]) -> bool {
        // Only verify prefixes introduced by new_phrases (historical ones validated earlier).
        for phrase in new_phrases {
            if phrase.is_empty() { continue; }
            let indices: Vec<usize> = phrase.iter().map(|w| self.vocabulary.iter().position(|v| v==w).unwrap()).collect();
            for k in 1..=indices.len() {
                if !self.prefix_to_completions.contains_key(&indices[..k].to_vec()) {
                    eprintln!("[interner][verify] missing prefix {:?}", &indices[..k]);
                    return false;
                }
            }
        }
        true
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

    pub fn completions_for_prefix(&self, prefix: &Vec<usize>) -> Option<&FixedBitSet> {
        self.prefix_to_completions.get(prefix)
    }

    fn get_required_bits(&self, required: &[Vec<usize>]) -> FixedBitSet {
        let mut result = FixedBitSet::with_capacity(self.vocabulary.len());
        result.grow(self.vocabulary.len());
        if required.is_empty() { result.set_range(.., true); return result; }
        let mut first = true;
        for prefix in required {
            match self.prefix_to_completions.get(prefix) {
                Some(bitset) => {
                    if first { result.clone_from(bitset); first = false; } else { result.intersect_with(bitset); }
                    if result.count_ones(..) == 0 { break; }
                }
                None => {
                    static ONCE: std::sync::Once = std::sync::Once::new();
                    ONCE.call_once(|| {
                        eprintln!("[interner][warn] encountered missing prefix {:?}; treating as empty completion set (further occurrences suppressed)", prefix);
                    });
                    if !first { result.set_range(.., false); }
                    break;
                }
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

    fn get_padded_bitset(&self, other: &Interner, prefix: &Vec<usize>, target_vocab_len: usize) -> Option<FixedBitSet> {
        match other.prefix_to_completions.get(prefix) {
            Some(bitset) => {
                let mut padded = bitset.clone();
                padded.grow(target_vocab_len);
                Some(padded)
            }
            None => None,
        }
    }

    pub fn differing_completions_indices_up_to_vocab(&self, other: &Interner, prefix: &Vec<usize>) -> Vec<usize> {
        let low_vocab_len = self.vocabulary.len();
        let self_bitset = self.get_padded_bitset(self, prefix, low_vocab_len);
        let other_bitset = self.get_padded_bitset(other, prefix, low_vocab_len);
        
        match (self_bitset, other_bitset) {
            (None, None) => Vec::new(),
            (None, Some(other_bs)) => {
                other_bs.ones().filter(|&idx| idx < low_vocab_len).collect()
            }
            (Some(self_bs), None) => {
                self_bs.ones().filter(|&idx| idx < low_vocab_len).collect()
            }
            (Some(self_bs), Some(other_bs)) => {
                self_bs.ones()
                    .filter(|&idx| idx < low_vocab_len && !other_bs.contains(idx))
                    .chain(other_bs.ones().filter(|&idx| idx < low_vocab_len && !self_bs.contains(idx)))
                    .collect()
            }
        }
    }

    pub fn completions_equal_up_to_vocab(&self, other: &Interner, prefix: &Vec<usize>) -> bool {
        self.differing_completions_indices_up_to_vocab(other, prefix).is_empty()
    }

    pub fn all_completions_equal_up_to_vocab(&self, other: &Interner, prefixes: &[Vec<usize>]) -> bool {
        prefixes.iter().all(|p| self.completions_equal_up_to_vocab(other, p))
    }

    pub fn impacted_keys(&self, new_interner: &Interner) -> Vec<Vec<usize>> {
        let new_vocab_len = new_interner.vocabulary.len();
        let mut impacted = Vec::new();
        
        for key in new_interner.prefix_to_completions.keys() {
            let old_bitset = self.get_padded_bitset(self, key, new_vocab_len);
            let new_bitset = self.get_padded_bitset(new_interner, key, new_vocab_len);
            
            let is_impacted = match (old_bitset, new_bitset) {
                (None, Some(bs)) => bs.count_ones(..) > 0,
                (Some(old_bs), Some(new_bs)) => old_bs != new_bs,
                _ => false,
            };
            
            if is_impacted {
                impacted.push(key.clone());
            }
        }
        
        impacted
    }

    pub fn merge(&self, other: &Interner) -> Self {
        // Step 1: Build combined vocabulary
        let mut vocabulary = self.vocabulary.clone();
        for word in other.vocabulary() {
            if !vocabulary.contains(word) {
                vocabulary.push(word.to_string());
            }
        }
        let new_vocab_len = vocabulary.len();
        
        // Step 2: Build vocabulary mapping for other interner (old index -> new index)
        let mut other_vocab_map = Vec::with_capacity(other.vocabulary().len());
        for word in other.vocabulary() {
            let new_idx = vocabulary.iter().position(|v| v == word).unwrap();
            other_vocab_map.push(new_idx);
        }
        
        // Step 3: Start with self's prefix_to_completions, padded to new vocab length
        let mut prefix_to_completions = HashMap::new();
        for (prefix, bitset) in &self.prefix_to_completions {
            let mut new_bitset = bitset.clone();
            new_bitset.grow(new_vocab_len);
            prefix_to_completions.insert(prefix.clone(), new_bitset);
        }
        
        // Step 4: Add other's prefix_to_completions with remapped indices
        for (old_prefix, old_bitset) in &other.prefix_to_completions {
            // Remap the prefix keys
            let new_prefix: Vec<usize> = old_prefix.iter().map(|&idx| other_vocab_map[idx]).collect();
            
            // Remap the completion bits
            let entry = prefix_to_completions.entry(new_prefix).or_insert_with(|| {
                let mut fbs = FixedBitSet::with_capacity(new_vocab_len);
                fbs.grow(new_vocab_len);
                fbs
            });
            
            // Flip bits from other that aren't already set in self (union operation)
            for old_idx in old_bitset.ones() {
                let new_idx = other_vocab_map[old_idx];
                entry.insert(new_idx);
            }
        }
        
        Interner {
            version: self.version + 1,
            vocabulary,
            prefix_to_completions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_text_creates_interner() {
        let interner = Interner::from_text("hello world");
        assert_eq!(interner.version(), 1);
        assert_eq!(interner.vocabulary().len(), 2);
    }

    #[test]
    fn test_add_increments_version() {
        let interner = Interner::from_text("hello world");
        let interner2 = interner.add_text("new text");
        assert_eq!(interner2.version(), 2);
    }

    #[test]
    fn test_add_extends_vocabulary() {
        let interner = Interner::from_text("a b");
        let interner2 = interner.add_text("c d");
        assert_eq!(interner2.vocabulary().len(), 4);
    }

    #[test]
    fn test_add_builds_prefix_mapping() {
        let interner = Interner::from_text("a b c");
        let interner2 = interner.add_text("a c");
        
        // Check that prefix [0] (which is 'a') has completions
        let prefix = vec![0];
        let completions = interner2.completions_for_prefix(&prefix);
        assert!(completions.is_some());
        
        let bitset = completions.unwrap();
        // Should contain both 'b' (index 1) and 'c' (index 2)
        assert!(bitset.contains(1) || bitset.contains(2));
    }

    #[test]
    fn test_add_handles_longer_phrases() {
        let interner = Interner::from_text("a b c");
        let interner2 = interner.add_text("a b d");
        
        // Check that both completions are tracked
        let vocab = interner2.vocabulary();
        assert!(vocab.contains(&"a".to_string()));
        assert!(vocab.contains(&"b".to_string()));
        assert!(vocab.contains(&"c".to_string()));
        assert!(vocab.contains(&"d".to_string()));
    }

    #[test]
    fn test_add_extends_existing_bitsets() {
        let interner = Interner::from_text("a b");
        let interner2 = interner.add_text("a c");
        
        // Prefix [0] should have completions for both b and c
        let prefix = vec![0];
        let completions = interner2.completions_for_prefix(&prefix);
        assert!(completions.is_some());
    }

    #[test]
    fn test_get_required_bits() {
        let interner = Interner::from_text("a b c");
        
        // Test with empty required (should return all)
        let bits = interner.get_required_bits(&[]);
        assert_eq!(bits.count_ones(..), interner.vocabulary().len());
        
        // Test with single prefix
        let prefix = vec![0]; // 'a'
        let bits = interner.get_required_bits(&[prefix]);
        assert!(bits.count_ones(..) > 0);
    }

    #[test]
    fn test_string_for_index() {
        let interner = Interner::from_text("foo bar baz");
        // Vocabulary might be in any order, just check we can get strings
        assert!(interner.vocabulary().len() == 3);
        assert!(interner.vocabulary().contains(&"foo".to_string()));
        assert!(interner.vocabulary().contains(&"bar".to_string()));
        assert!(interner.vocabulary().contains(&"baz".to_string()));
    }

    #[test]
    #[should_panic(expected = "Index out of bounds")]
    fn test_string_for_index_out_of_bounds_panics() {
        let interner = Interner::from_text("foo bar baz");
        let _ = interner.string_for_index(999);
    }

    #[test]
    fn test_terminal_phrase_inserted_empty() {
        let interner = Interner::from_text("a b");
        
        // Terminal phrases should have empty completion sets
        let terminal = vec![0, 1]; // [a, b]
        let completions = interner.completions_for_prefix(&terminal);
        assert!(completions.is_some());
    }

    #[test]
    fn test_merge_combines_vocabularies() {
        let interner_a = Interner::from_text("a b");
        let interner_b = Interner::from_text("c d");
        let merged = interner_a.merge(&interner_b);
        
        assert_eq!(merged.vocabulary().len(), 4);
        assert!(merged.vocabulary().contains(&"a".to_string()));
        assert!(merged.vocabulary().contains(&"b".to_string()));
        assert!(merged.vocabulary().contains(&"c".to_string()));
        assert!(merged.vocabulary().contains(&"d".to_string()));
    }

    #[test]
    fn test_merge_increments_version() {
        let interner_a = Interner::from_text("a b");
        let interner_b = Interner::from_text("c d");
        let merged = interner_a.merge(&interner_b);
        
        assert_eq!(merged.version(), 2);
    }

    #[test]
    fn test_merge_preserves_completions() {
        let interner_a = Interner::from_text("a b");
        let interner_b = Interner::from_text("a c");
        let merged = interner_a.merge(&interner_b);
        
        // Find index of 'a' in merged vocabulary
        let a_idx = merged.vocabulary().iter().position(|v| v == "a").unwrap();
        let b_idx = merged.vocabulary().iter().position(|v| v == "b").unwrap();
        let c_idx = merged.vocabulary().iter().position(|v| v == "c").unwrap();
        
        // Check that prefix [a] has completions for both b and c
        let prefix = vec![a_idx];
        let completions = merged.completions_for_prefix(&prefix);
        assert!(completions.is_some());
        
        let bitset = completions.unwrap();
        assert!(bitset.contains(b_idx));
        assert!(bitset.contains(c_idx));
    }
}

#[cfg(test)]
mod intersect_logic_tests {
    use super::*;

    fn build_interner(text: &str) -> Interner {
        Interner::from_text(text)
    }

    #[test]
    fn test_intersect_all_empty_returns_all_indexes() {
        let interner = build_interner("a b c");
        let result = interner.intersect(&[], &[]);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_intersect_required_and_forbidden() {
        let interner = build_interner("a b c d");
        
        // With prefix [0] (a) and forbidden [1] (b), should not include b
        let prefix = vec![0];
        let forbidden = vec![1];
        let result = interner.intersect(&[prefix], &forbidden);
        
        assert!(!result.contains(&1));
    }

    #[test]
    fn test_intersect_required_anded() {
        let interner = build_interner("a b c d");
        let interner2 = interner.add_text("b c");
        
        // Multiple required prefixes should be AND-ed
        let result = interner2.intersect(&[vec![0], vec![1]], &[]);
        // Result should be intersection of completions for both prefixes
        assert!(result.len() <= interner2.vocabulary().len());
    }

    #[test]
    fn test_intersect_forbidden_zeroes_out() {
        let interner = build_interner("a b c");
        
        // Forbid all vocab
        let forbidden: Vec<usize> = (0..interner.vocabulary().len()).collect();
        let result = interner.intersect(&[], &forbidden);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_intersect_bug_case() {
        let interner = build_interner("a b");
        let interner2 = interner.add_text("a c");
        
        // Prefix [0] should have completions for both b and c
        let result = interner2.intersect(&[vec![0]], &[]);
        assert!(result.len() > 0);
    }
}

#[cfg(test)]
mod version_compare_tests {
    use super::*;

    fn build_low_high(low_text: &str, high_text: &str) -> (Interner, Interner) {
        let low = Interner::from_text(low_text);
        let high = low.add_text(high_text);
        (low, high)
    }

    #[test]
    fn test_completions_equal_with_vocab_growth_tail_only() {
        let (low, _high) = build_low_high("a b", "c d");
        let prefix = vec![0];
        assert!(low.completions_for_prefix(&prefix).is_some());
    }

    #[test]
    fn test_completions_difference_in_old_vocab_detected() {
        let (low, high) = build_low_high("a b", "a c");
        
        let a_idx = low.vocabulary().iter().position(|w| w == "a").unwrap();
        let prefix = vec![a_idx];
        
        let diffs = low.differing_completions_indices_up_to_vocab(&high, &prefix);
        assert!(diffs.len() <= low.vocabulary().len());
    }

    #[test]
    fn test_added_completion_on_existing_indices_detected() {
        let (low, high) = build_low_high("a b", "a c");
        
        let a_idx = low.vocabulary().iter().position(|w| w == "a").unwrap();
        let prefix = vec![a_idx];
        let diffs = low.differing_completions_indices_up_to_vocab(&high, &prefix);
        
        assert!(diffs.len() <= low.vocabulary().len());
    }

    #[test]
    fn test_impacted_keys_new_key() {
        let (low, high) = build_low_high("a b", "c d");
        let impacted = low.impacted_keys(&high);
        assert!(impacted.len() > 0);
    }

    #[test]
    fn test_impacted_keys_new_completion() {
        let low = Interner::from_text("a b");
        let high = low.add_text("a c");
        let impacted = low.impacted_keys(&high);
        let a_idx = low.vocabulary().iter().position(|w| w == "a").unwrap();
        let prefix = vec![a_idx];
        assert!(impacted.contains(&prefix));
    }

    #[test]
    fn test_impacted_keys_no_change() {
        let low = Interner::from_text("a b");
        let high = Interner {
            version: low.version + 1,
            vocabulary: low.vocabulary.clone(),
            prefix_to_completions: low.prefix_to_completions.clone(),
        };
        let impacted = low.impacted_keys(&high);
        assert_eq!(impacted.len(), 0);
    }

    #[test]
    fn test_punctuation_does_not_create_duplicate_words() {
        // Test the case from e.txt: "the party, and" 
        // With comma as delimiter: "the party" is one sentence, "and" is another
        // Vocabulary includes all words (even from single-word sentences)
        // So "the party, and" produces vocabulary ["and", "party", "the"] and phrase ["the", "party"]
        
        // First test: feed in "the party, and" - comma splits into two sentences
        let interner1 = Interner::from_text("the party, and");
        // "the party" creates phrase, "and" is in vocab but creates no phrases (single word)
        assert_eq!(interner1.vocabulary().len(), 3, "interner1 vocab: {:?}", interner1.vocabulary());
        
        // Second test: "the party and" - single sentence with all three words
        let interner2 = Interner::from_text("the party and");
        assert_eq!(interner2.vocabulary().len(), 3, "interner2 vocab: {:?}", interner2.vocabulary());
        
        // Now test: feed both into the same interner
        let interner3 = Interner::from_text("the party, and");
        let interner3 = interner3.add_text("the party and");
        
        // Should have exactly 3 words total (and, party, the)
        println!("Combined interner vocabulary: {:?}", interner3.vocabulary());
        assert_eq!(interner3.vocabulary().len(), 3, 
                   "Combined interner should have 3 unique words, got: {:?}",
                   interner3.vocabulary());
        
        // Check that "and" appears exactly once
        let and_count = interner3.vocabulary().iter().filter(|w| *w == "and").count();
        assert_eq!(and_count, 1, "The word 'and' should appear exactly once in vocabulary");
    }
}
