use std::collections::HashMap;
use bit_set::BitSet;

pub struct Interner {
    version: u64,
    vocabulary: Vec<String>,
    prefix_to_completions: HashMap<Vec<u16>, BitSet>,
}

pub struct InternerRegistry {
    // TODO: Add registry functionality
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for InternerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InternerRegistry {
    pub fn new() -> Self {
        todo!()
    }

    /// TODO: Get interner at latest version (replacement for update)
    pub fn get_latest(&self) -> Interner {
        todo!()
    }

    /// TODO: Find phrases with discrepancies across versions
    pub fn find_discrepancies(&self, _phrases: &[Vec<String>], _versions: &[u64]) -> Vec<Vec<String>> {
        todo!()
    }

    /// TODO: Deprecate a version by collapsing it into the next one
    pub fn deprecate_version(&mut self, _version: u64) {
        todo!()
    }

    /// TODO: Add interner to registry
    pub fn add_interner(&mut self, _interner: Interner) {
        todo!()
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

    pub fn add(self, vocabulary: Vec<String>, phrases: Vec<Vec<String>>) -> Self {
        // Create resultant vocabulary immutably
        let new_words: Vec<String> = vocabulary.into_iter()
            .filter(|word| !self.vocabulary.contains(word))
            .collect();
        let mut resultant_vocabulary = self.vocabulary.clone();
        resultant_vocabulary.extend(new_words);
        
        // Create prefix_to_completions mapping
        let mut new_prefix_to_completions = self.prefix_to_completions.clone();
        
        // Process each phrase to extract prefix and completion
        for phrase in phrases {
            if phrase.len() >= 2 {
                // Convert string phrase to u16 indices
                let phrase_indices: Vec<u16> = phrase.iter()
                    .map(|word| {
                        resultant_vocabulary.iter()
                            .position(|v| v == word)
                            .unwrap_or_else(|| panic!("Word '{word}' should be in vocabulary")) as u16
                    })
                    .collect();
                
                let prefix = phrase_indices[..phrase_indices.len() - 1].to_vec();
                let completion = phrase_indices[phrase_indices.len() - 1];
                
                // Get or create bitset for this prefix
                let bitset = new_prefix_to_completions.entry(prefix).or_default();
                
                // Set the bit for the completion (suffixes that appear twice are counted once)
                bitset.insert(completion as usize);
            }
        }
        
        // Create new interner at the end with all the pieces
        Interner {
            version: self.version + 1,
            vocabulary: resultant_vocabulary,
            prefix_to_completions: new_prefix_to_completions,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }



    /// TODO: Get required bits for required phrases 
    pub(crate) fn get_required_bits(&self, _required: &[Vec<u16>]) -> Vec<u64> {
        todo!()
    }

    /// TODO: Get forbidden bits for forbidden terms
    pub(crate) fn get_forbidden_bits(&self, _forbidden: &[u16]) -> Vec<u64> {
        todo!()
    }

    /// TODO: Intersect required and forbidden bits to get valid completions
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
        let interner = Interner::new();
        assert_eq!(interner.version(), 0);
        
        let interner = interner.add(vec!["hello".to_string(), "world".to_string()], vec![vec!["hello".to_string(), "world".to_string()]]);
        assert_eq!(interner.version(), 1);
        
        let interner = interner.add(vec!["foo".to_string()], vec![vec!["hello".to_string(), "foo".to_string()]]);
        assert_eq!(interner.version(), 2);
    }

    #[test]
    fn test_add_extends_vocabulary() {
        let interner = Interner::new();
        
        let interner = interner.add(vec!["hello".to_string(), "world".to_string()], vec![vec!["hello".to_string(), "world".to_string()]]);
        assert_eq!(interner.vocabulary, vec!["hello", "world"]);
        
        let interner = interner.add(vec!["foo".to_string(), "bar".to_string()], vec![vec!["foo".to_string(), "bar".to_string()]]);
        assert_eq!(interner.vocabulary, vec!["hello", "world", "foo", "bar"]);
    }

    #[test]
    fn test_add_avoids_duplicate_vocabulary() {
        let interner = Interner::new();
        
        let interner = interner.add(vec!["hello".to_string(), "world".to_string()], vec![vec!["hello".to_string(), "world".to_string()]]);
        assert_eq!(interner.vocabulary, vec!["hello", "world"]);
        
        let interner = interner.add(vec!["hello".to_string(), "foo".to_string()], vec![vec!["hello".to_string(), "foo".to_string()]]);
        assert_eq!(interner.vocabulary, vec!["hello", "world", "foo"]);
    }

    #[test]
    fn test_add_creates_prefix_to_completions_mapping() {
        let interner = Interner::new();
        
        // Add phrases: ["word0", "word1"] and ["word0", "word2"] 
        let interner = interner.add(
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
        let interner = Interner::new();
        
        // Add the same phrase twice: ["word0", "word1"] appears twice
        let interner = interner.add(
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
        let interner = Interner::new();
        
        // Add phrases with different lengths
        let interner = interner.add(
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
        let interner = Interner::new();
        
        // Add single word phrases (should be ignored) and valid phrases
        let interner = interner.add(
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
        let interner = Interner::new();
        
        // Example from issue: phrases ["word0", "word1"] and ["word0", "word2"] should create prefix [0] -> 110 (bits 1 and 2)
        let interner = interner.add(
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
        let interner = Interner::new();
        
        // First call
        let interner = interner.add(
            vec!["a".to_string(), "b".to_string()], 
            vec![vec!["a".to_string(), "b".to_string()]]
        );
        
        // Second call with overlapping prefix but different completion
        let interner = interner.add(
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
