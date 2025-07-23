use std::collections::BTreeSet;

pub struct Splitter;

impl Splitter {
    pub fn new() -> Self {
        Splitter
    }

    pub fn vocabulary(&self, text: &str) -> Vec<String> {
        let mut vocab_set = BTreeSet::new();
        for sentence in self.split_into_sentences(text) {
            for word in self.clean_sentence(&sentence) {
                vocab_set.insert(word);
            }
        }
        vocab_set.into_iter().collect()
    }

    pub fn phrases(&self, text: &str) -> Vec<Vec<String>> {
        self.split_into_sentences(text)
            .into_iter()
            .map(|sentence| self.clean_sentence(&sentence))
            .filter(|words| words.len() >= 2)
            .flat_map(|words| self.generate_substrings(&words))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    
    fn split_into_sentences(&self, text: &str) -> Vec<String> {
        text.split("\n\n")
            .flat_map(|paragraph| {
                paragraph.split(|c| matches!(c, '.' | '?' | ';' | '!'))
                    .map(|sentence| sentence.trim().to_string())
                    .filter(|sentence| !sentence.is_empty())
            })
            .collect()
    }
    
    fn clean_sentence(&self, sentence: &str) -> Vec<String> {
        sentence
            .chars()
            .map(|c| self.filter_char(c))
            .collect::<String>()
            .split_whitespace()
            .map(|word| word.to_lowercase())
            .filter(|word| !word.is_empty())
            .collect()
    }
    
    fn generate_substrings(&self, words: &[String]) -> Vec<Vec<String>> {
        (0..words.len())
            .flat_map(|start| {
                ((start + 2)..=words.len())
                    .map(move |end| words[start..end].to_vec())
            })
            .collect()
    }
    
    fn clean_text(&self, text: &str) -> String {
        text.chars()
            .map(|c| self.filter_char(c))
            .collect()
    }

    fn filter_char(&self, c: char) -> char {
        // Keep 's as part of words, remove other punctuation
        if c.is_alphabetic() || c.is_whitespace() || c == '\'' {
            c
        } else {
            ' '
        }
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
        
        assert_eq!(phrases, vec![vec!["hello".to_string(), "world".to_string()]]);
    }

    #[test]
    fn test_phrases_no_single_words() {
        let splitter = Splitter::new();
        let text = "word";
        let phrases = splitter.phrases(text);
        
        assert_eq!(phrases, Vec::<Vec<String>>::new());
    }

    #[test]
    fn test_phrases_substrings() {
        let splitter = Splitter::new();
        let text = "one two three four";
        let phrases = splitter.phrases(text);
        
        let expected = vec![
            vec!["one".to_string(), "two".to_string()],
            vec!["one".to_string(), "two".to_string(), "three".to_string()],
            vec!["one".to_string(), "two".to_string(), "three".to_string(), "four".to_string()],
            vec!["three".to_string(), "four".to_string()],
            vec!["two".to_string(), "three".to_string()],
            vec!["two".to_string(), "three".to_string(), "four".to_string()],
        ];
        assert_eq!(phrases, expected);
    }

    #[test]
    fn test_phrases_sentence_delimiters() {
        let splitter = Splitter::new();
        let text = "hello world. foo bar?";
        let phrases = splitter.phrases(text);
        
        let expected = vec![
            vec!["foo".to_string(), "bar".to_string()],
            vec!["hello".to_string(), "world".to_string()],
        ];
        assert_eq!(phrases, expected);
    }

    #[test]
    fn test_phrases_punctuation_removal() {
        let splitter = Splitter::new();
        let text = "hello, world! it's working.";
        let phrases = splitter.phrases(text);
        
        // This creates two sentences since sentences are split by . ? ; ! \n\n
        // "hello, world" and "it's working" become two separate sentences
        let expected = vec![
            vec!["hello".to_string(), "world".to_string()],
            vec!["it's".to_string(), "working".to_string()],
        ];
        assert_eq!(phrases, expected);
    }


    #[test] 
    fn test_phrases_no_duplicates() {
        let splitter = Splitter::new();
        let text = "hello world. hello world? world hello";
        let phrases = splitter.phrases(text);
        
        // Should deduplicate across and within sentences
        let expected = vec![
            vec!["hello".to_string(), "world".to_string()],
            vec!["world".to_string(), "hello".to_string()],
        ];
        assert_eq!(phrases, expected);
    }

    #[test]
    fn test_phrases_apostrophe_handling() {
        let splitter = Splitter::new();
        let text = "it's working here";
        let phrases = splitter.phrases(text);
        
        let expected = vec![
            vec!["it's".to_string(), "working".to_string()],
            vec!["it's".to_string(), "working".to_string(), "here".to_string()],
            vec!["working".to_string(), "here".to_string()],
        ];
        assert_eq!(phrases, expected);
    }

    #[test]
    fn test_phrases_comprehensive() {
        let splitter = Splitter::new();
        let text = "The cat sat. The dog ran?";
        let phrases = splitter.phrases(text);
        
        let expected = vec![
            vec!["cat".to_string(), "sat".to_string()],
            vec!["dog".to_string(), "ran".to_string()],
            vec!["the".to_string(), "cat".to_string()],
            vec!["the".to_string(), "cat".to_string(), "sat".to_string()],
            vec!["the".to_string(), "dog".to_string()],
            vec!["the".to_string(), "dog".to_string(), "ran".to_string()],
        ];
        assert_eq!(phrases, expected);
    }

    #[test]
    fn test_phrases_paragraph_separation() {
        let splitter = Splitter::new();
        let text = "hello world\n\nfoo bar";
        let phrases = splitter.phrases(text);
        
        let expected = vec![
            vec!["foo".to_string(), "bar".to_string()],
            vec!["hello".to_string(), "world".to_string()],
        ];
        assert_eq!(phrases, expected);
    }

    #[test]
    fn test_phrases_exact_requirement_example() {
        let splitter = Splitter::new();
        let text = "these words are test";
        let phrases = splitter.phrases(text);
        
        let expected = vec![
            vec!["are".to_string(), "test".to_string()],
            vec!["these".to_string(), "words".to_string()],
            vec!["these".to_string(), "words".to_string(), "are".to_string()],
            vec!["these".to_string(), "words".to_string(), "are".to_string(), "test".to_string()],
            vec!["words".to_string(), "are".to_string()],
            vec!["words".to_string(), "are".to_string(), "test".to_string()],
        ];
        assert_eq!(phrases, expected);
    }

    #[test]
    fn test_phrases_indexing() {
        let splitter = Splitter::new();
        let text = "these words are test";
        let phrases = splitter.phrases(text);
        
        let expected = vec![
            vec!["are".to_string(), "test".to_string()],
            vec!["these".to_string(), "words".to_string()],
            vec!["these".to_string(), "words".to_string(), "are".to_string()],
            vec!["these".to_string(), "words".to_string(), "are".to_string(), "test".to_string()],
            vec!["words".to_string(), "are".to_string()],
            vec!["words".to_string(), "are".to_string(), "test".to_string()],
        ];
        assert_eq!(phrases, expected);
    }
}