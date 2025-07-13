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
        
        // Should have exactly 3 unique words in alphabetical order
        assert_eq!(vocab, vec!["hello", "rust", "world"]);
    }

    #[test]
    fn test_vocabulary_all_lowercase() {
        let splitter = Splitter::new();
        let text = "Hello WORLD Rust MiXeD CaSe";
        let vocab = splitter.vocabulary(text);
        
        // Should have all words in lowercase and sorted alphabetically
        assert_eq!(vocab, vec!["case", "hello", "mixed", "rust", "world"]);
    }

    #[test]
    fn test_vocabulary_empty_input() {
        let splitter = Splitter::new();
        let vocab = splitter.vocabulary("");
        assert!(vocab.is_empty(), "Empty input should produce empty vocabulary");
    }
}
