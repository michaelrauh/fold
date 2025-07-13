use std::collections::BTreeSet;

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

    pub fn phrases(&self, text: &str) -> Vec<Vec<u16>> {
        // Split into sentences using delimiters: . ? \n\n ;
        let sentences = self.split_into_sentences(text);
        
        // Collect all phrases (word sequences) from all sentences
        let mut all_phrases = BTreeSet::new();
        
        for sentence in &sentences {
            let cleaned_words = self.clean_sentence(sentence);
            if cleaned_words.len() >= 2 {
                // Generate all substrings of length >= 2
                for phrase in self.generate_substrings(&cleaned_words) {
                    all_phrases.insert(phrase);
                }
            }
        }
        
        // Build vocabulary from the same cleaned text that we use for phrases
        let vocabulary = self.build_cleaned_vocabulary(text);
        
        // Convert phrases to indices
        all_phrases.into_iter()
            .map(|phrase| self.phrase_to_indices(&phrase, &vocabulary))
            .collect()
    }
    
    fn split_into_sentences(&self, text: &str) -> Vec<String> {
        // First split by \n\n, then split each part by other delimiters
        let mut sentences = Vec::new();
        
        for paragraph in text.split("\n\n") {
            for sentence in paragraph.split(|c| matches!(c, '.' | '?' | ';')) {
                let trimmed = sentence.trim();
                if !trimmed.is_empty() {
                    sentences.push(trimmed.to_string());
                }
            }
        }
        
        sentences
    }
    
    fn clean_sentence(&self, sentence: &str) -> Vec<String> {
        sentence
            .chars()
            .map(|c| {
                // Keep 's as part of words, remove other punctuation
                if c.is_alphabetic() || c.is_whitespace() || c == '\'' {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .map(|word| word.to_lowercase())
            .filter(|word| !word.is_empty())
            .collect()
    }
    
    fn generate_substrings(&self, words: &[String]) -> Vec<Vec<String>> {
        let mut substrings = Vec::new();
        
        // Generate all substrings of length >= 2
        for start in 0..words.len() {
            for end in (start + 2)..=words.len() {
                substrings.push(words[start..end].to_vec());
            }
        }
        
        substrings
    }
    
    fn phrase_to_indices(&self, phrase: &[String], vocabulary: &[String]) -> Vec<u16> {
        phrase.iter()
            .map(|word| {
                vocabulary.iter()
                    .position(|v| v == word)
                    .expect("Word should be in vocabulary") as u16
            })
            .collect()
    }
    
    fn build_cleaned_vocabulary(&self, text: &str) -> Vec<String> {
        let cleaned_text = text
            .chars()
            .map(|c| {
                // Keep 's as part of words, remove other punctuation
                if c.is_alphabetic() || c.is_whitespace() || c == '\'' {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>();
            
        cleaned_text
            .split_whitespace()
            .map(|word| word.to_lowercase())
            .filter(|word| !word.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
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

    #[test]
    fn test_phrases_minimum_length() {
        let splitter = Splitter::new();
        let text = "hello world";
        let phrases = splitter.phrases(text);
        
        // Should have one phrase of length 2: ["hello", "world"]
        assert_eq!(phrases.len(), 1);
        assert_eq!(phrases[0].len(), 2);
    }

    #[test]
    fn test_phrases_no_single_words() {
        let splitter = Splitter::new();
        let text = "word";
        let phrases = splitter.phrases(text);
        
        // Single word should produce no phrases
        assert!(phrases.is_empty());
    }

    #[test]
    fn test_phrases_substrings() {
        let splitter = Splitter::new();
        let text = "one two three four";
        let phrases = splitter.phrases(text);
        
        // Should have 6 phrases: 1 of length 4, 2 of length 3, 3 of length 2
        assert_eq!(phrases.len(), 6);
        
        // Check we have different lengths
        let mut lengths: Vec<usize> = phrases.iter().map(|p| p.len()).collect();
        lengths.sort();
        assert_eq!(lengths, vec![2, 2, 2, 3, 3, 4]);
    }

    #[test]
    fn test_phrases_sentence_delimiters() {
        let splitter = Splitter::new();
        let text = "hello world. foo bar?";
        let phrases = splitter.phrases(text);
        
        // Should have phrases from both sentences
        // "hello world" -> 1 phrase
        // "foo bar" -> 1 phrase
        // Total: 2 phrases (assuming no duplicates)
        assert_eq!(phrases.len(), 2);
    }

    #[test]
    fn test_phrases_punctuation_removal() {
        let splitter = Splitter::new();
        let text = "hello, world! it's working.";
        let phrases = splitter.phrases(text);
        
        // Should clean punctuation but keep 's
        // Should have phrases with cleaned words
        assert!(!phrases.is_empty());
    }

    #[test]
    fn test_phrases_vocabulary_indexing() {
        let splitter = Splitter::new();
        let text = "apple banana apple";
        let phrases = splitter.phrases(text);
        
        // Vocabulary should be ["apple", "banana"] (sorted)
        // Phrases should reference indices correctly
        // For "apple banana apple" we expect:
        // - "apple banana" (length 2)
        // - "banana apple" (length 2) 
        // - "apple banana apple" (length 3)
        assert_eq!(phrases.len(), 3); // Updated expectation
        
        // Check that indices are valid u16 values
        for phrase in &phrases {
            for &index in phrase {
                assert!(index < 2); // Only 2 words in vocabulary
            }
        }
    }

    #[test] 
    fn test_phrases_no_duplicates() {
        let splitter = Splitter::new();
        let text = "hello world. hello world?";
        let phrases = splitter.phrases(text);
        
        // Both sentences produce the same phrase, should only appear once
        assert_eq!(phrases.len(), 1);
    }

    #[test]
    fn test_phrases_apostrophe_handling() {
        let splitter = Splitter::new();
        let text = "it's working here";
        let phrases = splitter.phrases(text);
        
        // Should preserve 's but remove other punctuation
        // Expect: ["it's", "working"], ["working", "here"], ["it's", "working", "here"]
        assert_eq!(phrases.len(), 3);
        
        // Verify no empty or single word phrases
        for phrase in &phrases {
            assert!(phrase.len() >= 2);
        }
    }

    #[test]
    fn test_phrases_comprehensive() {
        let splitter = Splitter::new();
        let text = "The cat sat. The dog ran?";
        let phrases = splitter.phrases(text);
        
        // First sentence: "the cat sat" -> ["the", "cat"], ["cat", "sat"], ["the", "cat", "sat"] = 3 phrases
        // Second sentence: "the dog ran" -> ["the", "dog"], ["dog", "ran"], ["the", "dog", "ran"] = 3 phrases
        // But "the" appears in both, so some deduplication may occur
        // Minimum expected: 5 unique phrases (since both sentences share "the")
        assert!(phrases.len() >= 5);
        
        // All phrases should have length >= 2
        for phrase in &phrases {
            assert!(phrase.len() >= 2);
        }
    }

    #[test]
    fn test_phrases_paragraph_separation() {
        let splitter = Splitter::new();
        let text = "hello world\n\nfoo bar";
        let phrases = splitter.phrases(text);
        
        // Two paragraphs should be treated as separate sentences
        // "hello world" -> 1 phrase
        // "foo bar" -> 1 phrase
        assert_eq!(phrases.len(), 2);
    }

    #[test]
    fn test_phrases_exact_requirement_example() {
        let splitter = Splitter::new();
        let text = "these words are test";
        let phrases = splitter.phrases(text);
        
        // For a 4-word sentence, we should have:
        // Length 2: ["these", "words"], ["words", "are"], ["are", "test"] = 3 phrases
        // Length 3: ["these", "words", "are"], ["words", "are", "test"] = 2 phrases  
        // Length 4: ["these", "words", "are", "test"] = 1 phrase
        // Total: 6 phrases
        assert_eq!(phrases.len(), 6);
        
        // Verify all have length >= 2
        for phrase in &phrases {
            assert!(phrase.len() >= 2);
            assert!(phrase.len() <= 4);
        }
        
        // Verify we have correct distribution of lengths
        let mut lengths: Vec<usize> = phrases.iter().map(|p| p.len()).collect();
        lengths.sort();
        assert_eq!(lengths, vec![2, 2, 2, 3, 3, 4]);
    }

    #[test]
    fn test_phrases_indexing_example_from_requirements() {
        let splitter = Splitter::new();
        // Create a scenario like the one described in requirements
        let text = "these words are";
        let phrases = splitter.phrases(text);
        
        // Build vocabulary separately to understand indexing
        let vocab = splitter.vocabulary(text);
        // Should be ["are", "these", "words"] (alphabetically sorted)
        assert_eq!(vocab, vec!["are", "these", "words"]);
        
        // The phrases should include substrings with proper indexing
        // "these" = index 1, "words" = index 2, "are" = index 0
        // So phrase ["these", "words"] should be [1, 2]
        // And phrase ["words", "are"] should be [2, 0]
        // And phrase ["these", "words", "are"] should be [1, 2, 0]
        
        // Find the phrase [1, 2, 0] which represents ["these", "words", "are"]
        assert!(phrases.contains(&vec![1, 2, 0]));
        
        // Find the phrase [1, 2] which represents ["these", "words"]
        assert!(phrases.contains(&vec![1, 2]));
        
        // Find the phrase [2, 0] which represents ["words", "are"]
        assert!(phrases.contains(&vec![2, 0]));
    }
}
