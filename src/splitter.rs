pub struct Splitter;

impl Splitter {
    pub fn new() -> Self {
        Splitter
    }

    pub fn vocabulary(&self, text: &str) -> Vec<String> {
        use std::collections::BTreeSet;
        
        text.split_whitespace()
            .map(|word| word.to_lowercase())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn phrases(&self, _text: &str) -> Vec<Vec<u16>> {
        todo!("Implement phrases extraction")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vocabulary_no_duplicates() {
        let splitter = Splitter::new();
        let text = "hello world hello rust world";
        let vocab = splitter.vocabulary(text);
        
        // Check that there are no duplicates by comparing length with unique count
        let unique_count = vocab.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(vocab.len(), unique_count, "Vocabulary should have no duplicates");
        
        // Verify the actual words are what we expect (3 unique words)
        assert_eq!(vocab.len(), 3);
        assert!(vocab.contains(&"hello".to_string()));
        assert!(vocab.contains(&"world".to_string()));
        assert!(vocab.contains(&"rust".to_string()));
    }

    #[test]
    fn test_vocabulary_all_lowercase() {
        let splitter = Splitter::new();
        let text = "Hello WORLD Rust MiXeD CaSe";
        let vocab = splitter.vocabulary(text);
        
        // Check that all words are lowercase
        for word in &vocab {
            assert_eq!(word, &word.to_lowercase(), "All words should be lowercase: {}", word);
        }
        
        // Verify specific lowercase conversions
        assert!(vocab.contains(&"hello".to_string()));
        assert!(vocab.contains(&"world".to_string()));
        assert!(vocab.contains(&"rust".to_string()));
        assert!(vocab.contains(&"mixed".to_string()));
        assert!(vocab.contains(&"case".to_string()));
    }

    #[test]
    fn test_vocabulary_contains_all_input_words() {
        let splitter = Splitter::new();
        let text = "the quick brown fox jumps over the lazy dog";
        let vocab = splitter.vocabulary(text);
        
        // Get all unique words from input (manually)
        let input_words: std::collections::HashSet<String> = text
            .split_whitespace()
            .map(|word| word.to_lowercase())
            .collect();
        
        // Check that vocabulary contains all unique words from input
        for input_word in input_words {
            assert!(vocab.contains(&input_word), "Vocabulary should contain word: {}", input_word);
        }
    }

    #[test]
    fn test_vocabulary_empty_input() {
        let splitter = Splitter::new();
        let vocab = splitter.vocabulary("");
        assert!(vocab.is_empty(), "Empty input should produce empty vocabulary");
    }

    #[test]
    fn test_vocabulary_single_word() {
        let splitter = Splitter::new();
        let vocab = splitter.vocabulary("HELLO");
        assert_eq!(vocab, vec!["hello"], "Single word should be lowercased");
    }
}
