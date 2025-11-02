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
            let mut interner = Interner {
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
                    // Prefix not present: this can happen because ortho spatial prefixes are not guaranteed
                    // to correspond to linear phrase prefixes. Missing => zero completions.
                    static ONCE: std::sync::Once = std::sync::Once::new();
                    ONCE.call_once(|| {
                        eprintln!("[interner][warn] encountered missing prefix {:?}; treating as empty completion set (further occurrences suppressed)", prefix);
                    });
                    if first { /* intersection stays empty */ first = false; }
                    else { result.set_range(.., false); }
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

    pub fn differing_completions_indices_up_to_vocab(&self, other: &Interner, prefix: &Vec<usize>) -> Vec<usize> {
        // Compare completion sets for a prefix restricted to the lower (self) vocabulary size.
        // Return sorted indices (< self.vocabulary.len()) whose membership differs.
        let low_vocab_len = self.vocabulary.len();
        let word_bits = usize::BITS as usize;
        let words_needed = (low_vocab_len + word_bits - 1) / word_bits;
        let low_opt = self.prefix_to_completions.get(prefix);
        let high_opt = other.prefix_to_completions.get(prefix);
        // Fast path: both absent => no differences.
        if low_opt.is_none() && high_opt.is_none() { return Vec::new(); }
        // Represent missing bitset as zeroed slice.
        let zero_words: Vec<usize> = vec![0; words_needed];
        let low_slice: &[usize] = match low_opt { Some(bs) => bs.as_slice(), None => &zero_words }; // may be longer than needed
        let high_slice: &[usize] = match high_opt { Some(bs) => bs.as_slice(), None => &zero_words };
        let mut diffs = Vec::new();
        for w in 0..words_needed {
            let lw = *low_slice.get(w).unwrap_or(&0);
            let hw = *high_slice.get(w).unwrap_or(&0);
            let mut xor = lw ^ hw;
            // Mask off bits beyond vocab_len in final word
            if w == words_needed - 1 {
                let rem = low_vocab_len % word_bits;
                if rem != 0 { let mask = (1usize << rem) - 1; xor &= mask; }
            }
            while xor != 0 {
                let tz = xor.trailing_zeros() as usize;
                diffs.push(w * word_bits + tz);
                xor &= xor - 1; // clear lowest set bit
            }
        }
        diffs
    }

    pub fn completions_equal_up_to_vocab(&self, other: &Interner, prefix: &Vec<usize>) -> bool {
        self.differing_completions_indices_up_to_vocab(other, prefix).is_empty()
    }

    pub fn all_completions_equal_up_to_vocab(&self, other: &Interner, prefixes: &[Vec<usize>]) -> bool {
        prefixes.iter().all(|p| self.completions_equal_up_to_vocab(other, p))
    }
}
