use std::collections::HashMap;
use bitvec::prelude::*;

pub struct Interner {
    version: u64,
    vocabulary: Vec<String>,
    prefix_to_completions: HashMap<Vec<u16>, BitVec<u64>>,
}

impl Interner {
    pub fn new() -> Self {
        Interner { 
            version: 0,
            vocabulary: Vec::new(),
            prefix_to_completions: HashMap::new(),
        }
    }

    pub fn add(&mut self, vocabulary: Vec<String>, phrases: Vec<Vec<u16>>) {
        // Increment version by one
        self.version += 1;
        
        // Extend vocabulary with new vocab (avoid duplicates)
        for word in vocabulary {
            if !self.vocabulary.contains(&word) {
                self.vocabulary.push(word);
            }
        }
        
        // Extend existing bitsets to match new vocabulary length
        let new_vocab_len = self.vocabulary.len();
        for bitset in self.prefix_to_completions.values_mut() {
            bitset.resize(new_vocab_len, false);
        }
        
        // Process phrases to build prefix to completions mapping
        for phrase in phrases {
            if phrase.len() < 2 {
                continue; // Skip phrases with less than 2 words
            }
            
            // For each possible prefix of the phrase (all but the last word)
            for i in 1..phrase.len() {
                let prefix = phrase[..i].to_vec();
                let completion_word_index = phrase[i] as usize;
                
                // Ensure the completion index is valid
                if completion_word_index < new_vocab_len {
                    // Get or create the bitset for this prefix
                    let bitset = self.prefix_to_completions
                        .entry(prefix)
                        .or_insert_with(|| bitvec![u64, bitvec::order::LocalBits; 0; new_vocab_len]);
                    
                    // Set the bit for the completion word
                    bitset.set(completion_word_index, true);
                }
            }
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn update(&self) -> Interner {
        todo!()
    }

    pub(crate) fn get_required_bits(&self, _required: &[Vec<u16>]) -> Vec<u64> {
        todo!()
    }

    pub(crate) fn get_forbidden_bits(&self, _forbidden: &[u16]) -> Vec<u64> {
        todo!()
    }

    pub fn intersect(&self, _required: Vec<u64>, _forbidden: Vec<u64>) -> Vec<u16> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_interner() {
        let interner = Interner::new();
        assert_eq!(interner.version(), 0);
        assert_eq!(interner.vocabulary.len(), 0);
        assert_eq!(interner.prefix_to_completions.len(), 0);
    }

    #[test]
    fn test_add_increments_version() {
        let mut interner = Interner::new();
        assert_eq!(interner.version(), 0);
        
        interner.add(vec!["hello".to_string(), "world".to_string()], vec![]);
        assert_eq!(interner.version(), 1);
        
        interner.add(vec!["test".to_string()], vec![]);
        assert_eq!(interner.version(), 2);
    }

    #[test]
    fn test_add_extends_vocabulary() {
        let mut interner = Interner::new();
        
        interner.add(vec!["hello".to_string(), "world".to_string()], vec![]);
        assert_eq!(interner.vocabulary, vec!["hello", "world"]);
        
        interner.add(vec!["test".to_string(), "hello".to_string()], vec![]);
        assert_eq!(interner.vocabulary, vec!["hello", "world", "test"]);
    }

    #[test]
    fn test_add_builds_prefix_mapping() {
        let mut interner = Interner::new();
        
        // Add vocabulary first
        let vocabulary = vec!["word0".to_string(), "word1".to_string(), "word2".to_string()];
        // Phrases: [0, 1] and [0, 2] as described in the issue
        let phrases = vec![vec![0, 1], vec![0, 2]];
        
        interner.add(vocabulary, phrases);
        
        // Check that prefix [0] maps to bits 1 and 2 being set (110 in binary)
        let prefix = vec![0];
        let bitset = interner.prefix_to_completions.get(&prefix).unwrap();
        
        assert_eq!(bitset.len(), 3);
        assert!(!bitset[0]); // bit 0 is not set
        assert!(bitset[1]);  // bit 1 is set (for word1)
        assert!(bitset[2]);  // bit 2 is set (for word2)
    }

    #[test]
    fn test_add_handles_longer_phrases() {
        let mut interner = Interner::new();
        
        let vocabulary = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let phrases = vec![vec![0, 1, 2]]; // phrase "a b c"
        
        interner.add(vocabulary, phrases);
        
        // Should have prefix [0] -> bit 1 set (for "b")
        let prefix_0 = vec![0];
        let bitset_0 = interner.prefix_to_completions.get(&prefix_0).unwrap();
        assert!(!bitset_0[0]);
        assert!(bitset_0[1]);
        assert!(!bitset_0[2]);
        
        // Should have prefix [0, 1] -> bit 2 set (for "c")
        let prefix_01 = vec![0, 1];
        let bitset_01 = interner.prefix_to_completions.get(&prefix_01).unwrap();
        assert!(!bitset_01[0]);
        assert!(!bitset_01[1]);
        assert!(bitset_01[2]);
    }

    #[test]
    fn test_add_no_duplicates_in_suffixes() {
        let mut interner = Interner::new();
        
        let vocabulary = vec!["a".to_string(), "b".to_string()];
        // Same phrase added twice - should only count once
        let phrases = vec![vec![0, 1], vec![0, 1]];
        
        interner.add(vocabulary, phrases);
        
        let prefix = vec![0];
        let bitset = interner.prefix_to_completions.get(&prefix).unwrap();
        assert!(!bitset[0]);
        assert!(bitset[1]); // Should be set only once
    }

    #[test]
    fn test_add_ignores_short_phrases() {
        let mut interner = Interner::new();
        
        let vocabulary = vec!["a".to_string(), "b".to_string()];
        let phrases = vec![vec![0], vec![1]]; // Single-word phrases should be ignored
        
        interner.add(vocabulary, phrases);
        
        // No prefix mappings should be created for single-word phrases
        assert_eq!(interner.prefix_to_completions.len(), 0);
    }

    #[test]
    fn test_add_extends_existing_bitsets() {
        let mut interner = Interner::new();
        
        // First add with 2 words
        let vocabulary1 = vec!["a".to_string(), "b".to_string()];
        let phrases1 = vec![vec![0, 1]];
        interner.add(vocabulary1, phrases1);
        
        // Second add with 1 more word
        let vocabulary2 = vec!["c".to_string()];
        let phrases2 = vec![vec![0, 2]];
        interner.add(vocabulary2, phrases2);
        
        let prefix = vec![0];
        let bitset = interner.prefix_to_completions.get(&prefix).unwrap();
        
        // Bitset should now have length 3 and both bits 1 and 2 should be set
        assert_eq!(bitset.len(), 3);
        assert!(!bitset[0]);
        assert!(bitset[1]); // from first add
        assert!(bitset[2]); // from second add
    }
}