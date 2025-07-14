use std::collections::HashMap;
use bit_set::BitSet;

pub struct Interner {
    version: u64,
    vocabulary: Vec<String>,
    prefix_to_completions: HashMap<Vec<u16>, BitSet>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Self {
        Interner { 
            version: 0,
            vocabulary: Vec::new(),
            prefix_to_completions: HashMap::new(),
        }
    }

    pub fn add(&mut self, vocabulary: Vec<String>, phrases: Vec<Vec<String>>) {
        // Increment version by one
        self.version += 1;
        
        // Extend vocabulary with new vocab, avoiding duplicates (append-only to keep indices stable)
        for word in vocabulary {
            if !self.vocabulary.contains(&word) {
                self.vocabulary.push(word);
            }
        }
        
        // Process each phrase to extract prefix and completion
        for phrase in phrases {
            if phrase.len() >= 2 {
                // Convert string phrase to u16 indices
                let phrase_indices: Vec<u16> = phrase.iter()
                    .map(|word| {
                        self.vocabulary.iter()
                            .position(|v| v == word)
                            .unwrap_or_else(|| panic!("Word '{}' should be in vocabulary", word)) as u16
                    })
                    .collect();
                
                let prefix = phrase_indices[..phrase_indices.len() - 1].to_vec();
                let completion = phrase_indices[phrase_indices.len() - 1];
                
                // Get or create bitset for this prefix
                let bitset = self.prefix_to_completions.entry(prefix).or_default();
                
                // Set the bit for the completion (suffixes that appear twice are counted once)
                bitset.insert(completion as usize);
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
    }

    #[test]
    fn test_add_increments_version() {
        let mut interner = Interner::new();
        assert_eq!(interner.version(), 0);
        
        interner.add(vec!["hello".to_string(), "world".to_string()], vec![vec!["hello".to_string(), "world".to_string()]]);
        assert_eq!(interner.version(), 1);
        
        interner.add(vec!["foo".to_string()], vec![vec!["hello".to_string(), "foo".to_string()]]);
        assert_eq!(interner.version(), 2);
    }

    #[test]
    fn test_add_extends_vocabulary() {
        let mut interner = Interner::new();
        
        interner.add(vec!["hello".to_string(), "world".to_string()], vec![vec!["hello".to_string(), "world".to_string()]]);
        assert_eq!(interner.vocabulary, vec!["hello", "world"]);
        
        interner.add(vec!["foo".to_string(), "bar".to_string()], vec![vec!["foo".to_string(), "bar".to_string()]]);
        assert_eq!(interner.vocabulary, vec!["hello", "world", "foo", "bar"]);
    }

    #[test]
    fn test_add_avoids_duplicate_vocabulary() {
        let mut interner = Interner::new();
        
        interner.add(vec!["hello".to_string(), "world".to_string()], vec![vec!["hello".to_string(), "world".to_string()]]);
        assert_eq!(interner.vocabulary, vec!["hello", "world"]);
        
        interner.add(vec!["hello".to_string(), "foo".to_string()], vec![vec!["hello".to_string(), "foo".to_string()]]);
        assert_eq!(interner.vocabulary, vec!["hello", "world", "foo"]);
    }

    #[test]
    fn test_add_creates_prefix_to_completions_mapping() {
        let mut interner = Interner::new();
        
        // Add phrases: ["word0", "word1"] and ["word0", "word2"] 
        interner.add(
            vec!["word0".to_string(), "word1".to_string(), "word2".to_string()], 
            vec![vec!["word0".to_string(), "word1".to_string()], vec!["word0".to_string(), "word2".to_string()]]
        );
        
        // Prefix [0] should map to bitset with bits 1 and 2 set
        let prefix = vec![0];
        let bitset = interner.prefix_to_completions.get(&prefix).unwrap();
        assert!(bitset.contains(1));
        assert!(bitset.contains(2));
        assert!(!bitset.contains(0));
    }

    #[test]
    fn test_add_deduplicates_suffixes() {
        let mut interner = Interner::new();
        
        // Add the same phrase twice: ["word0", "word1"] appears twice
        interner.add(
            vec!["word0".to_string(), "word1".to_string()], 
            vec![vec!["word0".to_string(), "word1".to_string()], vec!["word0".to_string(), "word1".to_string()]]
        );
        
        // Prefix [0] should map to bitset with only bit 1 set (no duplicates)
        let prefix = vec![0];
        let bitset = interner.prefix_to_completions.get(&prefix).unwrap();
        assert!(bitset.contains(1));
        assert!(!bitset.contains(0));
        
        // Should only have one bit set
        assert_eq!(bitset.len(), 1);
    }

    #[test]
    fn test_add_handles_different_prefix_lengths() {
        let mut interner = Interner::new();
        
        // Add phrases with different lengths
        interner.add(
            vec!["a".to_string(), "b".to_string(), "c".to_string(), "d".to_string()], 
            vec![
                vec!["a".to_string(), "b".to_string()], 
                vec!["a".to_string(), "b".to_string(), "c".to_string()], 
                vec!["b".to_string(), "c".to_string(), "d".to_string()]
            ]
        );
        
        // Check prefix [0] -> completion 1
        let prefix1 = vec![0];
        let bitset1 = interner.prefix_to_completions.get(&prefix1).unwrap();
        assert!(bitset1.contains(1));
        
        // Check prefix [0, 1] -> completion 2  
        let prefix2 = vec![0, 1];
        let bitset2 = interner.prefix_to_completions.get(&prefix2).unwrap();
        assert!(bitset2.contains(2));
        
        // Check prefix [1, 2] -> completion 3
        let prefix3 = vec![1, 2];
        let bitset3 = interner.prefix_to_completions.get(&prefix3).unwrap();
        assert!(bitset3.contains(3));
    }

    #[test]
    fn test_add_ignores_single_word_phrases() {
        let mut interner = Interner::new();
        
        // Add single word phrases (should be ignored) and valid phrases
        interner.add(
            vec!["word0".to_string(), "word1".to_string()], 
            vec![vec!["word0".to_string()], vec!["word1".to_string()], vec!["word0".to_string(), "word1".to_string()]]
        );
        
        // Only the ["word0", "word1"] phrase should create a prefix mapping
        assert_eq!(interner.prefix_to_completions.len(), 1);
        
        let prefix = vec![0];
        let bitset = interner.prefix_to_completions.get(&prefix).unwrap();
        assert!(bitset.contains(1));
    }

    #[test]
    fn test_add_example_from_issue() {
        let mut interner = Interner::new();
        
        // Example from issue: phrases ["word0", "word1"] and ["word0", "word2"] should create prefix [0] -> 110 (bits 1 and 2)
        interner.add(
            vec!["word0".to_string(), "word1".to_string(), "word2".to_string()], 
            vec![vec!["word0".to_string(), "word1".to_string()], vec!["word0".to_string(), "word2".to_string()]]
        );
        
        let prefix = vec![0];
        let bitset = interner.prefix_to_completions.get(&prefix).unwrap();
        
        // Verify bits 1 and 2 are set (110 in binary)
        assert!(bitset.contains(1), "Bit 1 should be set");
        assert!(bitset.contains(2), "Bit 2 should be set");
        assert!(!bitset.contains(0), "Bit 0 should not be set");
        
        // Verify exactly 2 bits are set
        assert_eq!(bitset.len(), 2);
    }

    #[test]
    fn test_add_multiple_calls_accumulate() {
        let mut interner = Interner::new();
        
        // First call
        interner.add(
            vec!["a".to_string(), "b".to_string()], 
            vec![vec!["a".to_string(), "b".to_string()]]
        );
        
        // Second call with overlapping prefix but different completion
        interner.add(
            vec!["c".to_string()], 
            vec![vec!["a".to_string(), "c".to_string()]]
        );
        
        // Prefix [0] should now map to both completions 1 and 2
        let prefix = vec![0];
        let bitset = interner.prefix_to_completions.get(&prefix).unwrap();
        assert!(bitset.contains(1));
        assert!(bitset.contains(2));
        assert_eq!(bitset.len(), 2);
    }
}
