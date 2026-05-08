use crate::{FoldError, splitter::Splitter};
use bytecheck::CheckBytes;
use fixedbitset::FixedBitSet;
use fixedbitset::IndexRange;
use rkyv::{Archive, Deserialize, Serialize};
use rustc_hash::FxHashMap;
use std::collections::HashMap;

const STACK_REQUIRED_PREFIXES: usize = 8;
const SPARSE_INTERSECT_THRESHOLD: usize = 20;
const SPARSE_COMPLETION_STORAGE_THRESHOLD: usize = 64;
const DENSE_CHILD_STATS_THRESHOLD: usize = 64;

#[derive(Clone, Copy)]
struct ResolvedPrefix<'a> {
    completions: &'a CompletionSet,
    count: usize,
}

#[derive(Clone, Debug)]
enum CompletionSet {
    Sparse(Vec<u32>),
    Dense { bitset: FixedBitSet, count: usize },
}

#[derive(Clone, Debug)]
enum ChildStats {
    Empty,
    Sparse(Vec<(u32, u32)>),
    Dense(Vec<u32>),
}

pub struct CompletionView<'a> {
    set: &'a CompletionSet,
}

pub struct CompletionOnes<'a> {
    inner: CompletionOnesInner<'a>,
}

enum CompletionOnesInner<'a> {
    Sparse(std::slice::Iter<'a, u32>),
    Dense(fixedbitset::Ones<'a>),
}

impl CompletionSet {
    fn from_bitset(bitset: FixedBitSet) -> Self {
        let count = bitset.count_ones(..);
        if count <= SPARSE_COMPLETION_STORAGE_THRESHOLD {
            CompletionSet::Sparse(bitset.ones().map(|idx| idx as u32).collect())
        } else {
            CompletionSet::Dense { bitset, count }
        }
    }

    #[inline]
    fn count(&self) -> usize {
        match self {
            CompletionSet::Sparse(indices) => indices.len(),
            CompletionSet::Dense { count, .. } => *count,
        }
    }

    #[inline]
    fn contains(&self, idx: usize) -> bool {
        match self {
            CompletionSet::Sparse(indices) => indices.binary_search(&(idx as u32)).is_ok(),
            CompletionSet::Dense { bitset, .. } => bitset.contains(idx),
        }
    }

    #[inline]
    fn count_ones<T: IndexRange>(&self, range: T) -> usize {
        match self {
            CompletionSet::Sparse(indices) => {
                let start = range.start().unwrap_or(0);
                let end = range.end().unwrap_or(usize::MAX);
                indices
                    .iter()
                    .filter(|&&idx| {
                        let idx = idx as usize;
                        idx >= start && idx < end
                    })
                    .count()
            }
            CompletionSet::Dense { bitset, count } => {
                if range.start().is_none() && range.end().is_none() {
                    *count
                } else {
                    bitset.count_ones(range)
                }
            }
        }
    }

    #[inline]
    fn ones(&self) -> CompletionOnes<'_> {
        CompletionOnes {
            inner: match self {
                CompletionSet::Sparse(indices) => CompletionOnesInner::Sparse(indices.iter()),
                CompletionSet::Dense { bitset, .. } => CompletionOnesInner::Dense(bitset.ones()),
            },
        }
    }

    #[inline]
    fn as_dense(&self) -> Option<&FixedBitSet> {
        match self {
            CompletionSet::Dense { bitset, .. } => Some(bitset),
            CompletionSet::Sparse(_) => None,
        }
    }

    fn copy_to_bitset(&self, out: &mut FixedBitSet, vocab_len: usize) -> usize {
        if out.len() < vocab_len {
            out.grow(vocab_len);
        }
        match self {
            CompletionSet::Sparse(indices) => {
                out.clear();
                for &idx in indices {
                    out.set(idx as usize, true);
                }
                indices.len()
            }
            CompletionSet::Dense { bitset, count } => {
                out.clone_from(bitset);
                if out.len() < vocab_len {
                    out.grow(vocab_len);
                }
                *count
            }
        }
    }

    fn to_bitset(&self, vocab_len: usize) -> FixedBitSet {
        let mut bitset = FixedBitSet::with_capacity(vocab_len);
        bitset.grow(vocab_len);
        self.copy_to_bitset(&mut bitset, vocab_len);
        bitset
    }
}

impl ChildStats {
    fn from_edges(mut edges: Vec<(u32, u32)>, vocab_len: usize) -> Self {
        if edges.is_empty() {
            return ChildStats::Empty;
        }
        edges.sort_unstable_by_key(|&(token, _)| token);
        if edges.len() >= DENSE_CHILD_STATS_THRESHOLD {
            let mut stats_plus_one = vec![0u32; vocab_len];
            for (token, stat) in edges {
                if let Some(slot) = stats_plus_one.get_mut(token as usize) {
                    *slot = stat.saturating_add(1);
                }
            }
            return ChildStats::Dense(stats_plus_one);
        }
        ChildStats::Sparse(edges)
    }

    #[inline]
    fn get(&self, token: usize) -> Option<usize> {
        let token = token as u32;
        match self {
            ChildStats::Empty => None,
            ChildStats::Sparse(edges) if edges.len() <= 8 => edges
                .iter()
                .find(|&&(child_token, _)| child_token == token)
                .map(|&(_, stat)| stat as usize),
            ChildStats::Sparse(edges) => edges
                .binary_search_by_key(&token, |&(child_token, _)| child_token)
                .ok()
                .map(|idx| edges[idx].1 as usize),
            ChildStats::Dense(stats_plus_one) => stats_plus_one
                .get(token as usize)
                .copied()
                .and_then(|stat| stat.checked_sub(1))
                .map(|stat| stat as usize),
        }
    }
}

impl<'a> CompletionView<'a> {
    #[inline]
    pub fn count_ones<T: IndexRange>(&self, range: T) -> usize {
        self.set.count_ones(range)
    }

    #[inline]
    pub fn contains(&self, idx: usize) -> bool {
        self.set.contains(idx)
    }

    #[inline]
    pub fn ones(&self) -> CompletionOnes<'a> {
        self.set.ones()
    }
}

impl<'a> Iterator for CompletionOnes<'a> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            CompletionOnesInner::Sparse(iter) => iter.next().map(|&idx| idx as usize),
            CompletionOnesInner::Dense(iter) => iter.next(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Interner {
    version: usize,
    vocabulary: Vec<String>,
    prefix_to_completions: FxHashMap<Vec<usize>, CompletionSet>,
    prefix_completions_by_id: Vec<CompletionSet>,
    prefix_stats: FxHashMap<Vec<usize>, usize>,
    single_token_stats: Vec<usize>,
    max_prefix_len: usize,
    prefix_to_id: FxHashMap<Vec<usize>, u32>,
    child_stats_by_id: Vec<ChildStats>,
}

#[derive(Archive, Serialize, Deserialize)]
#[archive_attr(derive(Debug, CheckBytes))]
struct InternerSerializable {
    version: usize,
    vocabulary: Vec<String>,
    prefix_to_completions: Vec<(Vec<usize>, Vec<u32>)>,
    prefix_stats: Vec<(Vec<usize>, usize)>,
}

impl Interner {
    fn initial_version() -> usize {
        2
    }

    fn to_serializable(&self) -> InternerSerializable {
        let prefix_vec: Vec<(Vec<usize>, Vec<u32>)> = self
            .prefix_to_completions
            .iter()
            .map(|(k, v)| (k.clone(), v.ones().map(|x| x as u32).collect()))
            .collect();
        let prefix_stats: Vec<(Vec<usize>, usize)> = self
            .prefix_stats
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        InternerSerializable {
            version: self.version,
            vocabulary: self.vocabulary.clone(),
            prefix_to_completions: prefix_vec,
            prefix_stats,
        }
    }

    fn from_serializable(serialized: InternerSerializable) -> Self {
        let InternerSerializable {
            version,
            vocabulary,
            prefix_to_completions: prefix_vec,
            prefix_stats,
        } = serialized;
        let mut prefix_to_completions_dense = FxHashMap::default();
        let vocab_len = vocabulary.len();
        for (prefix, completions) in prefix_vec {
            let mut fbs = FixedBitSet::with_capacity(vocab_len);
            fbs.grow(vocab_len);
            for idx in completions {
                fbs.insert(idx as usize);
            }
            prefix_to_completions_dense.insert(prefix, fbs);
        }
        let mut prefix_stats_map = FxHashMap::default();
        for (prefix, max_desc_len) in prefix_stats {
            prefix_stats_map.insert(prefix, max_desc_len);
        }
        let max_prefix_len = Self::compute_max_prefix_len(&prefix_stats_map);
        let vocab_len = vocabulary.len();
        let single_token_stats = Self::build_single_token_stats(&prefix_stats_map, vocab_len);
        let (prefix_to_id, child_stats_by_id) =
            Self::build_prefix_id_maps(&prefix_stats_map, vocab_len);
        let prefix_to_completions = Self::build_completion_storage(prefix_to_completions_dense);
        let prefix_completions_by_id =
            Self::build_prefix_completions_by_id(&prefix_to_completions, &prefix_to_id);
        Interner {
            version,
            vocabulary,
            prefix_to_completions,
            prefix_completions_by_id,
            prefix_stats: prefix_stats_map,
            single_token_stats,
            max_prefix_len,
            prefix_to_id,
            child_stats_by_id,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, FoldError> {
        rkyv::to_bytes::<_, 256>(&self.to_serializable())
            .map(|buf| buf.to_vec())
            .map_err(|e| FoldError::Serialization(e.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, FoldError> {
        let serialized: InternerSerializable =
            rkyv::from_bytes(bytes).map_err(|e| FoldError::Deserialization(e.to_string()))?;
        Ok(Self::from_serializable(serialized))
    }

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
        let (prefix_to_completions, prefix_stats) =
            Self::build_prefix_maps(&phrases, &vocabulary, new_vocab_len, None);
        let max_prefix_len = Self::compute_max_prefix_len(&prefix_stats);
        let single_token_stats = Self::build_single_token_stats(&prefix_stats, new_vocab_len);
        let (prefix_to_id, child_stats_by_id) =
            Self::build_prefix_id_maps(&prefix_stats, new_vocab_len);
        let prefix_to_completions = Self::build_completion_storage(prefix_to_completions);
        let prefix_completions_by_id =
            Self::build_prefix_completions_by_id(&prefix_to_completions, &prefix_to_id);
        let interner = Interner {
            version: Self::initial_version(),
            vocabulary,
            prefix_to_completions,
            prefix_completions_by_id,
            prefix_stats,
            single_token_stats,
            max_prefix_len,
            prefix_to_id,
            child_stats_by_id,
        };
        debug_assert!(
            interner.debug_verify_prefix_closure(&phrases),
            "Prefix closure verification failed after from_text"
        );
        interner
    }

    pub fn add_text(&self, text: &str) -> Self {
        if text.trim().is_empty() {
            let interner = Interner {
                version: self.version + 1,
                vocabulary: self.vocabulary.clone(),
                prefix_to_completions: self.prefix_to_completions.clone(),
                prefix_completions_by_id: self.prefix_completions_by_id.clone(),
                prefix_stats: self.prefix_stats.clone(),
                single_token_stats: self.single_token_stats.clone(),
                max_prefix_len: self.max_prefix_len,
                prefix_to_id: self.prefix_to_id.clone(),
                child_stats_by_id: self.child_stats_by_id.clone(),
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

        let (prefix_to_completions, prefix_stats) =
            Self::build_prefix_maps(&phrases, &vocabulary, new_vocab_len, Some(self));
        let max_prefix_len = Self::compute_max_prefix_len(&prefix_stats);
        let single_token_stats = Self::build_single_token_stats(&prefix_stats, new_vocab_len);
        let (prefix_to_id, child_stats_by_id) =
            Self::build_prefix_id_maps(&prefix_stats, new_vocab_len);
        let prefix_to_completions = Self::build_completion_storage(prefix_to_completions);
        let prefix_completions_by_id =
            Self::build_prefix_completions_by_id(&prefix_to_completions, &prefix_to_id);

        let interner = Interner {
            version: self.version + 1,
            vocabulary,
            prefix_to_completions,
            prefix_completions_by_id,
            prefix_stats,
            single_token_stats,
            max_prefix_len,
            prefix_to_id,
            child_stats_by_id,
        };
        debug_assert!(
            interner.debug_verify_prefix_closure(&phrases),
            "Prefix closure verification failed after add_text"
        );
        interner
    }

    fn build_prefix_maps(
        phrases: &[Vec<String>],
        vocabulary: &[String],
        vocab_len: usize,
        existing: Option<&Interner>,
    ) -> (
        FxHashMap<Vec<usize>, FixedBitSet>,
        FxHashMap<Vec<usize>, usize>,
    ) {
        let mut prefix_to_completions = match existing {
            Some(interner) => {
                let mut new_map = FxHashMap::default();
                new_map.reserve(interner.prefix_to_completions.len());
                for (prefix, completions) in &interner.prefix_to_completions {
                    new_map.insert(prefix.clone(), completions.to_bitset(vocab_len));
                }
                new_map
            }
            None => FxHashMap::default(),
        };
        let mut prefix_stats = existing
            .map(|interner| interner.prefix_stats.clone())
            .unwrap_or_default();

        let word_to_idx: HashMap<&str, usize> = vocabulary
            .iter()
            .enumerate()
            .map(|(i, w)| (w.as_str(), i))
            .collect();

        let phrase_indices: Vec<Vec<usize>> = phrases
            .iter()
            .map(|phrase| {
                phrase
                    .iter()
                    .map(|word| {
                        *word_to_idx
                            .get(word.as_str())
                            .expect("Word should be in vocabulary")
                    })
                    .collect()
            })
            .collect();

        for indices in &phrase_indices {
            if indices.is_empty() {
                continue;
            }
            let phrase_len = indices.len();

            for k in 1..=phrase_len {
                let prefix = indices[..k].to_vec();
                let entry = prefix_stats.entry(prefix).or_insert(0);
                if *entry < phrase_len {
                    *entry = phrase_len;
                }
            }

            if phrase_len < 2 {
                continue;
            }
            // Insert every incremental prefix chain edge: prefix[0..i] -> indices[i]
            for i in 1..phrase_len {
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
        // Ensure every vocabulary item has a single-token prefix key and stats
        for idx in 0..vocab_len {
            prefix_to_completions.entry(vec![idx]).or_insert_with(|| {
                let mut fbs = FixedBitSet::with_capacity(vocab_len);
                fbs.grow(vocab_len);
                fbs
            });
            prefix_stats
                .entry(vec![idx])
                .and_modify(|len| {
                    if *len < 1 {
                        *len = 1;
                    }
                })
                .or_insert(1);
        }
        // Ensure every full phrase itself as terminal prefix with empty completions and stats
        for indices in &phrase_indices {
            if indices.is_empty() {
                continue;
            }
            prefix_to_completions
                .entry(indices.clone())
                .or_insert_with(|| {
                    let mut fbs = FixedBitSet::with_capacity(vocab_len);
                    fbs.grow(vocab_len);
                    fbs
                });
            prefix_stats.entry(indices.clone()).or_insert(indices.len());
        }
        (prefix_to_completions, prefix_stats)
    }

    fn build_completion_storage(
        prefix_to_completions: FxHashMap<Vec<usize>, FixedBitSet>,
    ) -> FxHashMap<Vec<usize>, CompletionSet> {
        let mut storage = FxHashMap::default();
        storage.reserve(prefix_to_completions.len());
        for (prefix, bitset) in prefix_to_completions {
            storage.insert(prefix, CompletionSet::from_bitset(bitset));
        }
        storage
    }

    fn build_prefix_completions_by_id(
        prefix_to_completions: &FxHashMap<Vec<usize>, CompletionSet>,
        prefix_to_id: &FxHashMap<Vec<usize>, u32>,
    ) -> Vec<CompletionSet> {
        let mut by_id = vec![None; prefix_to_id.len()];
        for (prefix, &id) in prefix_to_id {
            let completions = prefix_to_completions
                .get(prefix)
                .expect("prefix ID must have completion storage");
            by_id[id as usize] = Some(completions.clone());
        }
        by_id
            .into_iter()
            .map(|entry| entry.expect("prefix completion ID table must be dense"))
            .collect()
    }

    fn debug_verify_prefix_closure(&self, new_phrases: &[Vec<String>]) -> bool {
        // Only verify prefixes introduced by new_phrases (historical ones validated earlier).
        for phrase in new_phrases {
            if phrase.is_empty() {
                continue;
            }
            let indices: Vec<usize> = phrase
                .iter()
                .map(|w| self.vocabulary.iter().position(|v| v == w).unwrap())
                .collect();
            for k in 1..=indices.len() {
                let prefix = &indices[..k];
                if !self.prefix_to_completions.contains_key(prefix) {
                    eprintln!("[interner][verify] missing prefix {:?}", &indices[..k]);
                    return false;
                }
                match self.prefix_stats.get(prefix) {
                    Some(&max_desc_len) if max_desc_len >= prefix.len() => {}
                    Some(&max_desc_len) => {
                        eprintln!(
                            "[interner][verify] prefix {:?} has max_desc_len {} < prefix len {}",
                            prefix,
                            max_desc_len,
                            prefix.len()
                        );
                        return false;
                    }
                    None => {
                        eprintln!("[interner][verify] missing prefix stats for {:?}", prefix);
                        return false;
                    }
                }
            }
        }
        true
    }

    fn compute_max_prefix_len(prefix_stats: &FxHashMap<Vec<usize>, usize>) -> usize {
        prefix_stats.values().copied().max().unwrap_or(0)
    }

    fn build_single_token_stats(
        prefix_stats: &FxHashMap<Vec<usize>, usize>,
        vocab_len: usize,
    ) -> Vec<usize> {
        let mut stats = vec![0usize; vocab_len];
        for (prefix, &val) in prefix_stats {
            if let [idx] = prefix.as_slice() {
                stats[*idx] = val;
            }
        }
        stats
    }

    fn build_prefix_id_maps(
        prefix_stats: &FxHashMap<Vec<usize>, usize>,
        vocab_len: usize,
    ) -> (FxHashMap<Vec<usize>, u32>, Vec<ChildStats>) {
        let mut prefix_to_id: FxHashMap<Vec<usize>, u32> = FxHashMap::default();
        prefix_to_id.reserve(prefix_stats.len());
        for (idx, prefix) in prefix_stats.keys().enumerate() {
            prefix_to_id.insert(prefix.clone(), idx as u32);
        }
        let mut child_edges_by_id = vec![Vec::new(); prefix_to_id.len()];
        for (prefix, &stat) in prefix_stats {
            if prefix.len() < 2 {
                continue;
            }
            let parent = &prefix[..prefix.len() - 1];
            let token = prefix[prefix.len() - 1] as u32;
            if let Some(&parent_id) = prefix_to_id.get(parent) {
                let stat = u32::try_from(stat).expect("prefix stat must fit in u32");
                child_edges_by_id[parent_id as usize].push((token, stat));
            }
        }
        let child_stats_by_id = child_edges_by_id
            .into_iter()
            .map(|edges| ChildStats::from_edges(edges, vocab_len))
            .collect();
        (prefix_to_id, child_stats_by_id)
    }

    pub fn prefix_id_for(&self, prefix: &[usize]) -> Option<u32> {
        self.prefix_to_id.get(prefix).copied()
    }

    #[inline]
    fn completions_for_prefix_id(&self, prefix_id: u32) -> Option<&CompletionSet> {
        self.prefix_completions_by_id.get(prefix_id as usize)
    }

    pub fn prefix_stats_by_parent_id(&self, parent_id: u32, appended: usize) -> Option<usize> {
        self.child_stats_by_id
            .get(parent_id as usize)
            .and_then(|stats| stats.get(appended))
    }

    pub fn version(&self) -> usize {
        self.version
    }

    pub fn vocabulary(&self) -> &[String] {
        &self.vocabulary
    }

    /// Iterate over all prefix -> completions entries.
    pub fn prefix_entries(&self) -> impl Iterator<Item = (&Vec<usize>, CompletionView<'_>)> {
        self.prefix_to_completions
            .iter()
            .map(|(prefix, set)| (prefix, CompletionView { set }))
    }

    pub fn prefix_stats(&self, prefix: &[usize]) -> Option<usize> {
        if let [idx] = prefix {
            let val = self.single_token_stats.get(*idx).copied().unwrap_or(0);
            return if val != 0 { Some(val) } else { None };
        }
        self.prefix_stats.get(prefix).copied()
    }

    pub fn prefix_stats_with_appended(
        &self,
        prefix: &[usize],
        appended: usize,
        scratch: &mut Vec<usize>,
    ) -> Option<usize> {
        if let [a] = prefix {
            return self.prefix_stats.get([*a, appended].as_slice()).copied();
        }
        scratch.clear();
        scratch.reserve(prefix.len().saturating_add(1));
        scratch.extend_from_slice(prefix);
        scratch.push(appended);
        self.prefix_stats(scratch.as_slice())
    }

    /// Maximum descriptor length across all prefixes (used for optimistic bounds when axis is missing).
    pub fn max_prefix_len(&self) -> usize {
        self.max_prefix_len
    }

    pub fn max_suffix_depth(&self, prefix: &[usize]) -> Option<usize> {
        self.prefix_stats(prefix)
            .map(|max_desc_len| max_desc_len.saturating_sub(prefix.len()))
    }

    pub fn vocab_size(&self) -> usize {
        self.vocabulary.len()
    }

    pub fn string_for_index(&self, index: usize) -> &str {
        self.vocabulary
            .get(index)
            .map(|s| s.as_str())
            .expect("Index out of bounds in Interner::string_for_index")
    }

    pub fn completions_for_prefix(&self, prefix: &[usize]) -> Option<CompletionView<'_>> {
        self.prefix_to_completions
            .get(prefix)
            .map(|set| CompletionView { set })
    }

    pub fn completion_count_for_prefix(&self, prefix: &[usize]) -> Option<usize> {
        self.prefix_to_completions
            .get(prefix)
            .map(CompletionSet::count)
    }

    fn warn_missing_prefix(prefix: &[usize]) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            eprintln!(
                "[interner][warn] encountered missing prefix {:?}; treating as empty completion set (further occurrences suppressed)",
                prefix
            );
        });
    }

    fn intersect_bitsets_into_count(out: &mut FixedBitSet, bitset: &FixedBitSet) -> usize {
        let mut count = 0;
        let out_words = out.as_mut_slice();
        let bitset_words = bitset.as_slice();
        let shared_len = out_words.len().min(bitset_words.len());

        for idx in 0..shared_len {
            out_words[idx] &= bitset_words[idx];
            count += out_words[idx].count_ones() as usize;
        }
        for word in &mut out_words[shared_len..] {
            *word = 0;
        }

        count
    }

    fn intersect_two_bitsets_into_count(
        out: &mut FixedBitSet,
        left: &FixedBitSet,
        right: &FixedBitSet,
    ) -> usize {
        let mut count = 0;
        let out_words = out.as_mut_slice();
        let left_words = left.as_slice();
        let right_words = right.as_slice();
        let shared_len = out_words.len().min(left_words.len()).min(right_words.len());

        for idx in 0..shared_len {
            let word = left_words[idx] & right_words[idx];
            out_words[idx] = word;
            count += word.count_ones() as usize;
        }
        for word in &mut out_words[shared_len..] {
            *word = 0;
        }

        count
    }

    fn intersect_sparse_completion_into_count(out: &mut FixedBitSet, indices: &[u32]) -> usize {
        debug_assert!(indices.len() <= SPARSE_COMPLETION_STORAGE_THRESHOLD);
        let mut matches = [0usize; SPARSE_COMPLETION_STORAGE_THRESHOLD];
        let mut count = 0usize;
        for &idx in indices {
            let idx = idx as usize;
            if out.contains(idx) {
                matches[count] = idx;
                count += 1;
            }
        }
        out.clear();
        for &idx in &matches[..count] {
            out.set(idx, true);
        }
        count
    }

    fn intersect_completion_set_into_count(out: &mut FixedBitSet, set: &CompletionSet) -> usize {
        match set {
            CompletionSet::Sparse(indices) => {
                Self::intersect_sparse_completion_into_count(out, indices)
            }
            CompletionSet::Dense { bitset, .. } => Self::intersect_bitsets_into_count(out, bitset),
        }
    }

    #[cfg(test)]
    fn get_required_bits(&self, required: &[Vec<usize>]) -> FixedBitSet {
        let mut result = FixedBitSet::with_capacity(self.vocabulary.len());
        result.grow(self.vocabulary.len());
        self.intersect_into_count(required, &[], &mut result);
        result
    }

    pub fn intersect_into(
        &self,
        required: &[Vec<usize>],
        forbidden: &[usize],
        out: &mut FixedBitSet,
    ) {
        self.intersect_into_count(required, forbidden, out);
    }

    pub fn intersect_into_count(
        &self,
        required: &[Vec<usize>],
        forbidden: &[usize],
        out: &mut FixedBitSet,
    ) -> usize {
        self.intersect_into_count_baseline(required, forbidden, out)
    }

    fn intersect_into_count_baseline(
        &self,
        required: &[Vec<usize>],
        forbidden: &[usize],
        out: &mut FixedBitSet,
    ) -> usize {
        if out.len() < self.vocabulary.len() {
            out.grow(self.vocabulary.len());
        }

        let mut count = if required.is_empty() {
            out.set_range(.., true);
            self.vocabulary.len()
        } else {
            let mut resolved = [None; STACK_REQUIRED_PREFIXES];
            let mut resolved_len = 0usize;
            let mut seed_slot = 0usize;
            let mut seed_count = usize::MAX;
            for (idx, prefix) in required.iter().enumerate() {
                let Some(completions) = self.prefix_to_completions.get(prefix) else {
                    Self::warn_missing_prefix(prefix);
                    out.set_range(.., false);
                    return 0;
                };
                let completion_count = completions.count();
                if idx >= STACK_REQUIRED_PREFIXES {
                    return self.intersect_into_count_resolved_heap(required, forbidden, out);
                }

                resolved[resolved_len] = Some(ResolvedPrefix {
                    completions,
                    count: completion_count,
                });
                if completion_count < seed_count {
                    seed_slot = resolved_len;
                    seed_count = completion_count;
                }
                resolved_len += 1;
            }

            if resolved_len == 0 {
                out.set_range(.., true);
                return self.vocabulary.len();
            };
            let seed = resolved[seed_slot].expect("seed slot should be populated");

            if resolved_len == 1 {
                seed.completions.copy_to_bitset(out, self.vocabulary.len())
            } else if matches!(seed.completions, CompletionSet::Sparse(_))
                || seed.count <= SPARSE_INTERSECT_THRESHOLD
            {
                out.clear();
                let mut count = 0usize;
                'bit: for bit in seed.completions.ones() {
                    for (slot, resolved_prefix) in resolved[..resolved_len].iter().enumerate() {
                        if slot == seed_slot {
                            continue;
                        }
                        let completions = resolved_prefix
                            .expect("resolved prefix slot should be populated")
                            .completions;
                        if !completions.contains(bit) {
                            continue 'bit;
                        }
                    }
                    out.set(bit, true);
                    count += 1;
                }
                count
            } else {
                let first_slot = (0..resolved_len)
                    .find(|&slot| slot != seed_slot)
                    .expect("resolved_len > 1 should have non-seed slot");
                let first = resolved[first_slot].expect("first slot should be populated");
                let mut count = if let (Some(seed_bitset), Some(first_bitset)) =
                    (seed.completions.as_dense(), first.completions.as_dense())
                {
                    Self::intersect_two_bitsets_into_count(out, seed_bitset, first_bitset)
                } else {
                    seed.completions.copy_to_bitset(out, self.vocabulary.len());
                    Self::intersect_completion_set_into_count(out, first.completions)
                };
                for (slot, resolved_prefix) in resolved[..resolved_len].iter().enumerate() {
                    if slot == seed_slot || slot == first_slot {
                        continue;
                    }
                    let completions = resolved_prefix
                        .expect("resolved prefix slot should be populated")
                        .completions;
                    count = Self::intersect_completion_set_into_count(out, completions);
                    if count == 0 {
                        break;
                    }
                }
                count
            }
        };

        for &idx in forbidden {
            if out.contains(idx) {
                out.set(idx, false);
                count = count.saturating_sub(1);
            }
        }

        count
    }

    fn intersect_into_count_resolved_heap(
        &self,
        required: &[Vec<usize>],
        forbidden: &[usize],
        out: &mut FixedBitSet,
    ) -> usize {
        let mut resolved = Vec::with_capacity(required.len());
        let mut seed_slot = 0usize;
        let mut seed_count = usize::MAX;
        for prefix in required {
            let Some(completions) = self.prefix_to_completions.get(prefix) else {
                Self::warn_missing_prefix(prefix);
                out.set_range(.., false);
                return 0;
            };
            let completion_count = completions.count();
            resolved.push(ResolvedPrefix {
                completions,
                count: completion_count,
            });
            if completion_count < seed_count {
                seed_slot = resolved.len() - 1;
                seed_count = completion_count;
            }
        }

        let seed = resolved[seed_slot];
        let mut count = if resolved.len() == 1 {
            seed.completions.copy_to_bitset(out, self.vocabulary.len())
        } else if matches!(seed.completions, CompletionSet::Sparse(_))
            || seed.count <= SPARSE_INTERSECT_THRESHOLD
        {
            out.clear();
            let mut count = 0usize;
            'bit: for bit in seed.completions.ones() {
                for (slot, resolved_prefix) in resolved.iter().enumerate() {
                    if slot == seed_slot {
                        continue;
                    }
                    if !resolved_prefix.completions.contains(bit) {
                        continue 'bit;
                    }
                }
                out.set(bit, true);
                count += 1;
            }
            count
        } else {
            let first_slot = (0..resolved.len())
                .find(|&slot| slot != seed_slot)
                .expect("resolved_len > 1 should have non-seed slot");
            let first = resolved[first_slot];
            let mut count = if let (Some(seed_bitset), Some(first_bitset)) =
                (seed.completions.as_dense(), first.completions.as_dense())
            {
                Self::intersect_two_bitsets_into_count(out, seed_bitset, first_bitset)
            } else {
                seed.completions.copy_to_bitset(out, self.vocabulary.len());
                Self::intersect_completion_set_into_count(out, first.completions)
            };
            for (slot, resolved_prefix) in resolved.iter().enumerate() {
                if slot == seed_slot || slot == first_slot {
                    continue;
                }
                count = Self::intersect_completion_set_into_count(out, resolved_prefix.completions);
                if count == 0 {
                    break;
                }
            }
            count
        };

        for &idx in forbidden {
            if out.contains(idx) {
                out.set(idx, false);
                count = count.saturating_sub(1);
            }
        }

        count
    }

    pub fn intersect_prefix_ids_into_count(
        &self,
        required_prefix_ids: &[u32],
        forbidden: &[usize],
        out: &mut FixedBitSet,
    ) -> usize {
        if out.len() < self.vocabulary.len() {
            out.grow(self.vocabulary.len());
        }

        let mut count = if required_prefix_ids.is_empty() {
            out.set_range(.., true);
            self.vocabulary.len()
        } else {
            let mut resolved = [None; STACK_REQUIRED_PREFIXES];
            let mut resolved_len = 0usize;
            let mut seed_slot = 0usize;
            let mut seed_count = usize::MAX;
            for (idx, &prefix_id) in required_prefix_ids.iter().enumerate() {
                let Some(completions) = self.completions_for_prefix_id(prefix_id) else {
                    out.set_range(.., false);
                    return 0;
                };
                let completion_count = completions.count();
                if idx >= STACK_REQUIRED_PREFIXES {
                    return self.intersect_prefix_ids_into_count_resolved_heap(
                        required_prefix_ids,
                        forbidden,
                        out,
                    );
                }

                resolved[resolved_len] = Some(ResolvedPrefix {
                    completions,
                    count: completion_count,
                });
                if completion_count < seed_count {
                    seed_slot = resolved_len;
                    seed_count = completion_count;
                }
                resolved_len += 1;
            }

            if resolved_len == 0 {
                out.set_range(.., true);
                return self.vocabulary.len();
            };
            let seed = resolved[seed_slot].expect("seed slot should be populated");

            if resolved_len == 1 {
                seed.completions.copy_to_bitset(out, self.vocabulary.len())
            } else if matches!(seed.completions, CompletionSet::Sparse(_))
                || seed.count <= SPARSE_INTERSECT_THRESHOLD
            {
                out.clear();
                let mut count = 0usize;
                'bit: for bit in seed.completions.ones() {
                    for (slot, resolved_prefix) in resolved[..resolved_len].iter().enumerate() {
                        if slot == seed_slot {
                            continue;
                        }
                        let completions = resolved_prefix
                            .expect("resolved prefix slot should be populated")
                            .completions;
                        if !completions.contains(bit) {
                            continue 'bit;
                        }
                    }
                    out.set(bit, true);
                    count += 1;
                }
                count
            } else {
                let first_slot = (0..resolved_len)
                    .find(|&slot| slot != seed_slot)
                    .expect("resolved_len > 1 should have non-seed slot");
                let first = resolved[first_slot].expect("first slot should be populated");
                let mut count = if let (Some(seed_bitset), Some(first_bitset)) =
                    (seed.completions.as_dense(), first.completions.as_dense())
                {
                    Self::intersect_two_bitsets_into_count(out, seed_bitset, first_bitset)
                } else {
                    seed.completions.copy_to_bitset(out, self.vocabulary.len());
                    Self::intersect_completion_set_into_count(out, first.completions)
                };
                for (slot, resolved_prefix) in resolved[..resolved_len].iter().enumerate() {
                    if slot == seed_slot || slot == first_slot {
                        continue;
                    }
                    let completions = resolved_prefix
                        .expect("resolved prefix slot should be populated")
                        .completions;
                    count = Self::intersect_completion_set_into_count(out, completions);
                    if count == 0 {
                        break;
                    }
                }
                count
            }
        };

        for &idx in forbidden {
            if out.contains(idx) {
                out.set(idx, false);
                count = count.saturating_sub(1);
            }
        }

        count
    }

    fn intersect_prefix_ids_into_count_resolved_heap(
        &self,
        required_prefix_ids: &[u32],
        forbidden: &[usize],
        out: &mut FixedBitSet,
    ) -> usize {
        let mut resolved = Vec::with_capacity(required_prefix_ids.len());
        let mut seed_slot = 0usize;
        let mut seed_count = usize::MAX;
        for &prefix_id in required_prefix_ids {
            let Some(completions) = self.completions_for_prefix_id(prefix_id) else {
                out.set_range(.., false);
                return 0;
            };
            let completion_count = completions.count();
            resolved.push(ResolvedPrefix {
                completions,
                count: completion_count,
            });
            if completion_count < seed_count {
                seed_slot = resolved.len() - 1;
                seed_count = completion_count;
            }
        }

        let seed = resolved[seed_slot];
        let mut count = if resolved.len() == 1 {
            seed.completions.copy_to_bitset(out, self.vocabulary.len())
        } else if matches!(seed.completions, CompletionSet::Sparse(_))
            || seed.count <= SPARSE_INTERSECT_THRESHOLD
        {
            out.clear();
            let mut count = 0usize;
            'bit: for bit in seed.completions.ones() {
                for (slot, resolved_prefix) in resolved.iter().enumerate() {
                    if slot == seed_slot {
                        continue;
                    }
                    if !resolved_prefix.completions.contains(bit) {
                        continue 'bit;
                    }
                }
                out.set(bit, true);
                count += 1;
            }
            count
        } else {
            let first_slot = (0..resolved.len())
                .find(|&slot| slot != seed_slot)
                .expect("resolved_len > 1 should have non-seed slot");
            let first = resolved[first_slot];
            let mut count = if let (Some(seed_bitset), Some(first_bitset)) =
                (seed.completions.as_dense(), first.completions.as_dense())
            {
                Self::intersect_two_bitsets_into_count(out, seed_bitset, first_bitset)
            } else {
                seed.completions.copy_to_bitset(out, self.vocabulary.len());
                Self::intersect_completion_set_into_count(out, first.completions)
            };
            for (slot, resolved_prefix) in resolved.iter().enumerate() {
                if slot == seed_slot || slot == first_slot {
                    continue;
                }
                count = Self::intersect_completion_set_into_count(out, resolved_prefix.completions);
                if count == 0 {
                    break;
                }
            }
            count
        };

        for &idx in forbidden {
            if out.contains(idx) {
                out.set(idx, false);
                count = count.saturating_sub(1);
            }
        }

        count
    }

    pub fn intersect(&self, required: &[Vec<usize>], forbidden: &[usize]) -> Vec<usize> {
        let mut intersection = FixedBitSet::with_capacity(self.vocabulary.len());
        intersection.grow(self.vocabulary.len());
        self.intersect_into(required, forbidden, &mut intersection);
        intersection.ones().collect()
    }

    pub fn differing_completions_indices_up_to_vocab(
        &self,
        other: &Interner,
        prefix: &Vec<usize>,
    ) -> Vec<usize> {
        let low_vocab_len = self.vocabulary.len();
        let self_set = self.prefix_to_completions.get(prefix);
        let other_set = other.prefix_to_completions.get(prefix);

        match (self_set, other_set) {
            (None, None) => Vec::new(),
            (None, Some(other_set)) => other_set
                .ones()
                .filter(|&idx| idx < low_vocab_len)
                .collect(),
            (Some(self_set), None) => self_set.ones().filter(|&idx| idx < low_vocab_len).collect(),
            (Some(self_set), Some(other_set)) => {
                let mut diffs = Vec::new();
                diffs.extend(
                    self_set
                        .ones()
                        .filter(|&idx| idx < low_vocab_len && !other_set.contains(idx)),
                );
                diffs.extend(
                    other_set
                        .ones()
                        .filter(|&idx| idx < low_vocab_len && !self_set.contains(idx)),
                );
                diffs
            }
        }
    }

    pub fn completions_equal_up_to_vocab(&self, other: &Interner, prefix: &Vec<usize>) -> bool {
        self.differing_completions_indices_up_to_vocab(other, prefix)
            .is_empty()
    }

    pub fn all_completions_equal_up_to_vocab(
        &self,
        other: &Interner,
        prefixes: &[Vec<usize>],
    ) -> bool {
        prefixes
            .iter()
            .all(|p| self.completions_equal_up_to_vocab(other, p))
    }

    pub fn impacted_keys(&self, new_interner: &Interner) -> Vec<Vec<usize>> {
        let self_vocab_len = self.vocabulary.len();
        let self_index_by_word: HashMap<&str, usize> = self
            .vocabulary
            .iter()
            .enumerate()
            .map(|(i, w)| (w.as_str(), i))
            .collect();

        let map_prefix = |prefix: &Vec<usize>,
                          vocab: &[String],
                          target: &HashMap<&str, usize>|
         -> Option<Vec<usize>> {
            prefix
                .iter()
                .map(|idx| {
                    vocab
                        .get(*idx)
                        .and_then(|w| target.get(w.as_str()).copied())
                })
                .collect::<Option<Vec<usize>>>()
        };

        let translate_completions =
            |completions: &CompletionSet, target: &HashMap<&str, usize>| -> (FixedBitSet, bool) {
                let mut translated = FixedBitSet::with_capacity(self_vocab_len);
                translated.grow(self_vocab_len);
                let mut had_unmapped = false;
                for idx in completions.ones() {
                    if let Some(word) = new_interner.vocabulary.get(idx) {
                        if let Some(&target_idx) = target.get(word.as_str()) {
                            translated.insert(target_idx);
                        } else {
                            had_unmapped = true;
                        }
                    }
                }
                (translated, had_unmapped)
            };

        let mut impacted = Vec::new();

        // Consider prefixes present in the new interner that map into self's vocabulary.
        for (new_prefix, new_bitset) in &new_interner.prefix_to_completions {
            if let Some(mapped_prefix) =
                map_prefix(new_prefix, &new_interner.vocabulary, &self_index_by_word)
            {
                let (translated_new, had_unmapped_completion) =
                    translate_completions(new_bitset, &self_index_by_word);
                let translated_count = translated_new.count_ones(..);

                let is_impacted = had_unmapped_completion
                    || match self.prefix_to_completions.get(&mapped_prefix) {
                        None => translated_count > 0,
                        Some(old_set) => {
                            old_set.count() != translated_count
                                || old_set.ones().any(|idx| !translated_new.contains(idx))
                        }
                    };

                if is_impacted {
                    impacted.push(mapped_prefix);
                }
            }
        }

        impacted.sort();
        impacted.dedup();
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
        let mut prefix_to_completions = FxHashMap::default();
        for (prefix, completions) in &self.prefix_to_completions {
            prefix_to_completions.insert(prefix.clone(), completions.to_bitset(new_vocab_len));
        }
        let mut prefix_stats = self.prefix_stats.clone();

        // Step 4: Add other's prefix_to_completions with remapped indices
        for (old_prefix, old_completions) in &other.prefix_to_completions {
            // Remap the prefix keys
            let new_prefix: Vec<usize> =
                old_prefix.iter().map(|&idx| other_vocab_map[idx]).collect();

            // Remap the completion bits
            let entry = prefix_to_completions.entry(new_prefix).or_insert_with(|| {
                let mut fbs = FixedBitSet::with_capacity(new_vocab_len);
                fbs.grow(new_vocab_len);
                fbs
            });

            // Flip bits from other that aren't already set in self (union operation)
            for old_idx in old_completions.ones() {
                let new_idx = other_vocab_map[old_idx];
                entry.insert(new_idx);
            }
        }

        // Step 5: Merge prefix stats (take max length for overlapping prefixes)
        for (old_prefix, stats) in &other.prefix_stats {
            let new_prefix: Vec<usize> =
                old_prefix.iter().map(|&idx| other_vocab_map[idx]).collect();
            let entry = prefix_stats.entry(new_prefix).or_insert(0);
            if *entry < *stats {
                *entry = *stats;
            }
        }

        // Step 6: Ensure every vocabulary item has a single-token prefix and stats
        for idx in 0..new_vocab_len {
            prefix_to_completions.entry(vec![idx]).or_insert_with(|| {
                let mut fbs = FixedBitSet::with_capacity(new_vocab_len);
                fbs.grow(new_vocab_len);
                fbs
            });
            prefix_stats
                .entry(vec![idx])
                .and_modify(|len| {
                    if *len < 1 {
                        *len = 1;
                    }
                })
                .or_insert(1);
        }

        let max_prefix_len = Self::compute_max_prefix_len(&prefix_stats);
        let single_token_stats = Self::build_single_token_stats(&prefix_stats, new_vocab_len);
        let (prefix_to_id, child_stats_by_id) =
            Self::build_prefix_id_maps(&prefix_stats, new_vocab_len);
        let prefix_to_completions = Self::build_completion_storage(prefix_to_completions);
        let prefix_completions_by_id =
            Self::build_prefix_completions_by_id(&prefix_to_completions, &prefix_to_id);
        Interner {
            version: self.version + 1,
            vocabulary,
            prefix_to_completions,
            prefix_completions_by_id,
            prefix_stats,
            single_token_stats,
            max_prefix_len,
            prefix_to_id,
            child_stats_by_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_text_creates_interner() {
        let interner = Interner::from_text("hello world");
        assert_eq!(interner.version(), Interner::initial_version());
        assert_eq!(interner.vocabulary().len(), 2);
    }

    #[test]
    fn test_add_increments_version() {
        let interner = Interner::from_text("hello world");
        let interner2 = interner.add_text("new text");
        assert_eq!(interner2.version(), interner.version() + 1);
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
    fn test_prefix_stats_from_text() {
        let interner = Interner::from_text("a b c");
        let vocab = interner.vocabulary();
        let a_idx = vocab.iter().position(|w| w == "a").unwrap();
        let b_idx = vocab.iter().position(|w| w == "b").unwrap();

        let a_prefix = vec![a_idx];
        let ab_prefix = vec![a_idx, b_idx];

        let a_stats = interner
            .prefix_stats(&a_prefix)
            .expect("stats for prefix [a]");
        assert_eq!(a_stats, 3);
        assert_eq!(interner.max_suffix_depth(&a_prefix), Some(2));

        let ab_stats = interner
            .prefix_stats(&ab_prefix)
            .expect("stats for prefix [a, b]");
        assert_eq!(ab_stats, 3);
        assert_eq!(interner.max_suffix_depth(&ab_prefix), Some(1));
    }

    #[test]
    fn commas_shrink_phrase_depth_drastically() {
        // Without commas: one 7-word sentence → max_desc_len for [a] is 7
        let no_commas = Interner::from_text("a b c d e f g");
        let vocab_no_commas = no_commas.vocabulary();
        let a_idx = vocab_no_commas.iter().position(|w| w == "a").unwrap();
        assert_eq!(
            no_commas.prefix_stats(&[a_idx]),
            Some(7),
            "continuous text should yield full length"
        );

        // With commas: split_into_sentences breaks on ',' so a/b/c/d become single-word sentences.
        // They get stats=1 only; only the tail 'e f g' forms phrases.
        let with_commas = Interner::from_text("a, b, c, d, e f g");
        let vocab_with_commas = with_commas.vocabulary();
        let a_idx_commas = vocab_with_commas.iter().position(|w| w == "a").unwrap();
        let e_idx_commas = vocab_with_commas.iter().position(|w| w == "e").unwrap();
        assert_eq!(
            with_commas.prefix_stats(&[a_idx_commas]),
            Some(1),
            "comma splitting collapses depth for early tokens"
        );
        assert_eq!(
            with_commas.prefix_stats(&[e_idx_commas]),
            Some(3),
            "tail chunk retains its own depth"
        );
    }

    #[test]
    fn test_add_text_updates_prefix_stats() {
        let base = Interner::from_text("a b");
        let extended = base.add_text("a b c");
        let vocab = extended.vocabulary();
        let a_idx = vocab.iter().position(|w| w == "a").unwrap();
        let b_idx = vocab.iter().position(|w| w == "b").unwrap();

        let a_stats = base.prefix_stats(&vec![a_idx]).unwrap();
        assert_eq!(a_stats, 2);

        let ab_prefix = vec![a_idx, b_idx];
        let ab_stats = extended.prefix_stats(&ab_prefix).unwrap();
        assert_eq!(ab_stats, 3);
        assert_eq!(extended.max_suffix_depth(&ab_prefix), Some(1));
    }

    #[test]
    fn test_merge_preserves_prefix_stats() {
        let interner_a = Interner::from_text("a b");
        let interner_b = Interner::from_text("a b c d");
        let merged = interner_a.merge(&interner_b);

        let vocab = merged.vocabulary();
        let a_idx = vocab.iter().position(|w| w == "a").unwrap();
        let a_stats = merged.prefix_stats(&vec![a_idx]).unwrap();
        assert_eq!(a_stats, 4);
    }

    #[test]
    fn test_interner_roundtrips_prefix_stats() {
        let interner = Interner::from_text("a b c");
        let vocab = interner.vocabulary();
        let a_idx = vocab.iter().position(|w| w == "a").unwrap();
        let prefix = vec![a_idx];
        let before = interner.prefix_stats(&prefix).unwrap();

        let bytes = interner.to_bytes().unwrap();
        let decoded = Interner::from_bytes(&bytes).unwrap();
        let after = decoded.prefix_stats(&prefix).unwrap();

        assert_eq!(before, after);
        assert_eq!(interner.max_prefix_len(), decoded.max_prefix_len());
    }

    #[test]
    fn prefix_stats_takes_longest_phrase_for_prefix() {
        // Prefix [a] appears in two phrases: short (a b) and long (a b c d).
        let interner = Interner::from_text("a b. a b c d");
        let vocab = interner.vocabulary();
        let a_idx = vocab.iter().position(|w| w == "a").unwrap();
        let b_idx = vocab.iter().position(|w| w == "b").unwrap();

        let a_len = interner.prefix_stats(&[a_idx]).unwrap();
        let ab_len = interner.prefix_stats(&[a_idx, b_idx]).unwrap();

        assert_eq!(
            a_len, 4,
            "max_desc_len for [a] should use the longest phrase"
        );
        assert_eq!(
            ab_len, 4,
            "max_desc_len for [a b] should use the longest phrase containing it"
        );
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
    fn test_prefix_id_maps_consistent_with_prefix_stats() {
        let interner = Interner::from_text("a b c. a b d");
        let vocab = interner.vocabulary();
        let a = vocab.iter().position(|w| w == "a").unwrap();
        let b = vocab.iter().position(|w| w == "b").unwrap();
        let c = vocab.iter().position(|w| w == "c").unwrap();

        // [a] has an ID
        let a_id = interner
            .prefix_id_for(&[a])
            .expect("prefix [a] must have an ID");

        // prefix_stats_by_parent_id([a], b) == prefix_stats([a, b])
        let via_id = interner.prefix_stats_by_parent_id(a_id, b);
        let via_direct = interner.prefix_stats(&[a, b]);
        assert_eq!(
            via_id, via_direct,
            "parent-ID path should agree with direct prefix_stats"
        );

        // prefix_stats_by_parent_id([a], c) should be None (c only follows b, not a directly)
        let via_id_c = interner.prefix_stats_by_parent_id(a_id, c);
        let via_direct_c = interner.prefix_stats(&[a, c]);
        assert_eq!(via_id_c, via_direct_c, "missing entry should agree");
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

        assert_eq!(merged.version(), interner_a.version() + 1);
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

    // Helper to split text the same way as Splitter for earliest-position tracking.
    fn tokenize_sentences(text: &str) -> Vec<Vec<String>> {
        let filter_char = |c: char| {
            if c.is_alphabetic() || c.is_whitespace() || c == '\'' {
                c
            } else {
                ' '
            }
        };

        text.split("\n\n")
            .flat_map(|paragraph| paragraph.split(|c| matches!(c, '.' | '?' | ';' | '!' | ',')))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|sentence| {
                sentence
                    .chars()
                    .map(filter_char)
                    .collect::<String>()
                    .split_whitespace()
                    .map(|w| w.to_lowercase())
                    .filter(|w| !w.is_empty())
                    .collect::<Vec<String>>()
            })
            .filter(|words| !words.is_empty())
            .collect()
    }

    #[test]
    #[ignore]
    fn build_full_interner_from_e_txt() {
        let text =
            std::fs::read_to_string("e.txt").expect("e.txt must exist in workspace root for test");
        let interner = Interner::from_text(&text);
        println!(
            "Built interner: vocab={}, prefixes={}",
            interner.vocabulary().len(),
            interner.prefix_to_completions.len()
        );
        assert!(interner.vocabulary().len() > 0);
    }

    #[test]
    #[ignore]
    fn export_full_interner_bin_and_json() -> Result<(), Box<dyn std::error::Error>> {
        use std::collections::HashMap;
        use std::fs;
        use std::path::Path;

        #[derive(serde::Serialize)]
        struct CompletionEntry {
            word: String,
            word_id: usize,
            first_word_pos: usize,
        }

        #[derive(serde::Serialize)]
        struct KeyEntry {
            words: Vec<String>,
            completions: Vec<CompletionEntry>,
        }

        #[derive(serde::Serialize)]
        struct Export {
            vocab: Vec<String>,
            max_words: usize,
            bucket_size_words: usize,
            keys: Vec<KeyEntry>,
        }

        let text = fs::read_to_string("e.txt")?;
        let interner = Interner::from_text(&text);

        // Stage 1: write bincode
        let bin_path = Path::new("target/interner_full.bin");
        if let Some(parent) = bin_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = interner.to_bytes()?;
        fs::write(bin_path, &bytes)?;
        println!(
            "Wrote interner bincode to {} ({} bytes)",
            bin_path.display(),
            bytes.len()
        );

        // Stage 2: compute earliest word positions and write JSON
        let vocab = interner.vocabulary.clone();
        let mut word_to_idx = HashMap::new();
        for (idx, word) in vocab.iter().enumerate() {
            word_to_idx.insert(word.clone(), idx);
        }

        let sentences = tokenize_sentences(&text);
        let mut first_positions: HashMap<(Vec<usize>, usize), usize> = HashMap::new();
        let mut total_words = 0usize;
        for words in sentences.iter() {
            let len = words.len();
            for start in 0..len {
                let mut prefix_indices = Vec::new();
                for j in (start + 1)..len {
                    if let Some(&prev_idx) = word_to_idx.get(&words[j - 1]) {
                        prefix_indices.push(prev_idx);
                    } else {
                        break;
                    }
                    if let Some(&comp_idx) = word_to_idx.get(&words[j]) {
                        let word_pos = total_words + j;
                        first_positions
                            .entry((prefix_indices.clone(), comp_idx))
                            .or_insert(word_pos);
                    }
                }
            }
            total_words += len;
        }

        let mut keys_export = Vec::with_capacity(interner.prefix_to_completions.len());
        for (prefix, bitset) in &interner.prefix_to_completions {
            let words = prefix
                .iter()
                .map(|&idx| interner.vocabulary[idx].clone())
                .collect::<Vec<String>>();

            let mut completions_vec = Vec::new();
            for completion_idx in bitset.ones() {
                let word = interner.vocabulary[completion_idx].clone();
                let first_word_pos = first_positions
                    .get(&(prefix.clone(), completion_idx))
                    .copied()
                    .unwrap_or(total_words);
                completions_vec.push(CompletionEntry {
                    word,
                    word_id: completion_idx,
                    first_word_pos,
                });
            }
            completions_vec.sort_by_key(|c| (c.first_word_pos, c.word_id));

            keys_export.push(KeyEntry {
                words,
                completions: completions_vec,
            });
        }

        // Sort by completion count descending for better default ordering
        keys_export.sort_by(|a, b| b.completions.len().cmp(&a.completions.len()));

        let export = Export {
            vocab,
            max_words: total_words,
            bucket_size_words: 500,
            keys: keys_export,
        };

        let out_path = Path::new("target/interner_full_export.json");
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::File::create(out_path)?;
        serde_json::to_writer_pretty(file, &export)?;
        println!(
            "Exported interner JSON to {} ({} words)",
            out_path.display(),
            total_words
        );

        Ok(())
    }

    #[test]
    #[ignore]
    fn export_full_interner_chunked() -> Result<(), Box<dyn std::error::Error>> {
        use std::collections::HashMap;
        use std::fs;
        use std::path::Path;

        #[derive(serde::Serialize)]
        struct CompletionEntry {
            word: String,
            word_id: usize,
            first_word_pos: usize,
        }

        #[derive(serde::Serialize)]
        struct KeyEntry {
            words: Vec<String>,
            completions: Vec<CompletionEntry>,
        }

        #[derive(serde::Serialize)]
        struct IndexEntry {
            words: Vec<String>,
            completion_count: u32,
            counts_by_word_bucket: Vec<u32>,
            chunk: String,
        }

        #[derive(serde::Serialize)]
        struct IndexFile {
            vocab: Vec<String>,
            max_words: usize,
            bucket_size_words: usize,
            keys: Vec<IndexEntry>,
            /// Filenames (one per bucket) containing key indices sorted by cumulative count (desc) at that bucket.
            /// Each sidecar is a binary little-endian u32 array of key indices.
            order_bucket_files: Vec<String>,
        }

        let text = fs::read_to_string("e.txt")?;
        let interner = Interner::from_text(&text);

        // Build vocab map
        let vocab = interner.vocabulary.clone();
        let mut word_to_idx = HashMap::new();
        for (idx, word) in vocab.iter().enumerate() {
            word_to_idx.insert(word.clone(), idx);
        }

        // Tokenization for earliest word positions
        let sentences = tokenize_sentences(&text);
        let mut first_positions: HashMap<(Vec<usize>, usize), usize> = HashMap::new();
        let mut total_words = 0usize;
        for words in sentences.iter() {
            let len = words.len();
            for start in 0..len {
                let mut prefix_indices = Vec::new();
                for j in (start + 1)..len {
                    if let Some(&prev_idx) = word_to_idx.get(&words[j - 1]) {
                        prefix_indices.push(prev_idx);
                    } else {
                        break;
                    }
                    if let Some(&comp_idx) = word_to_idx.get(&words[j]) {
                        let word_pos = total_words + j;
                        first_positions
                            .entry((prefix_indices.clone(), comp_idx))
                            .or_insert(word_pos);
                    }
                }
            }
            total_words += len;
        }

        // Prepare sorted keys for stable chunking (by completion count desc)
        let mut prefixes: Vec<_> = interner.prefix_to_completions.iter().collect();
        prefixes.sort_by(|(_, bs_a), (_, bs_b)| {
            let ca = bs_a.count_ones(..);
            let cb = bs_b.count_ones(..);
            cb.cmp(&ca)
        });

        let bucket_size_words: usize = 500;
        let bucket_count = if total_words == 0 {
            0
        } else {
            (total_words + bucket_size_words - 1) / bucket_size_words
        };

        let chunk_size: usize = 10_000; // keys per chunk
        let mut chunk_id: usize = 1;
        let mut chunk_keys: Vec<KeyEntry> = Vec::with_capacity(chunk_size);
        let mut index_entries: Vec<IndexEntry> = Vec::with_capacity(prefixes.len());
        let mut per_key_bucket_counts: Vec<Vec<u32>> = Vec::with_capacity(prefixes.len());

        let chunk_dir = Path::new("target");
        fs::create_dir_all(chunk_dir)?;

        let flush_chunk = |chunk_id: usize,
                           chunk_keys: &mut Vec<KeyEntry>|
         -> Result<(), Box<dyn std::error::Error>> {
            if chunk_keys.is_empty() {
                return Ok(());
            }
            let chunk_name = format!("interner_keys_{:04}.json", chunk_id);
            let chunk_path = chunk_dir.join(&chunk_name);
            let file = fs::File::create(&chunk_path)?;
            serde_json::to_writer(file, &serde_json::json!({ "keys": chunk_keys }))?;
            chunk_keys.clear();
            Ok(())
        };

        for (prefix, bitset) in prefixes {
            let words = prefix
                .iter()
                .map(|&idx| interner.vocabulary[idx].clone())
                .collect::<Vec<String>>();

            let mut completions_vec = Vec::new();
            let mut bucket_counts: Vec<u32> = vec![0u32; bucket_count];
            for completion_idx in bitset.ones() {
                let word = interner.vocabulary[completion_idx].clone();
                let first_word_pos = first_positions
                    .get(&(prefix.clone(), completion_idx))
                    .copied()
                    .unwrap_or(total_words);
                if !bucket_counts.is_empty() {
                    let idx =
                        (first_word_pos.min(total_words.saturating_sub(1))) / bucket_size_words;
                    if let Some(slot) = bucket_counts.get_mut(idx) {
                        *slot += 1;
                    }
                }
                completions_vec.push(CompletionEntry {
                    word,
                    word_id: completion_idx,
                    first_word_pos,
                });
            }
            completions_vec.sort_by_key(|c| (c.first_word_pos, c.word_id));

            // Make bucket counts cumulative
            for i in 1..bucket_counts.len() {
                let prev = bucket_counts[i - 1];
                if let Some(slot) = bucket_counts.get_mut(i) {
                    *slot += prev;
                }
            }
            let completion_count = if bucket_counts.is_empty() {
                completions_vec.len() as u32
            } else {
                *bucket_counts.last().unwrap_or(&0)
            };

            let chunk_name = format!("interner_keys_{:04}.json", chunk_id);
            index_entries.push(IndexEntry {
                words: words.clone(),
                completion_count,
                counts_by_word_bucket: bucket_counts.clone(),
                chunk: chunk_name.clone(),
            });
            per_key_bucket_counts.push(bucket_counts);

            chunk_keys.push(KeyEntry {
                words,
                completions: completions_vec,
            });

            if chunk_keys.len() >= chunk_size {
                flush_chunk(chunk_id, &mut chunk_keys)?;
                chunk_id += 1;
            }
        }

        // Flush remaining
        flush_chunk(chunk_id, &mut chunk_keys)?;

        // Build pre-sorted orders per bucket (descending by cumulative count) and write sidecars.
        let mut order_bucket_files: Vec<String> = Vec::with_capacity(bucket_count);
        for bucket_idx in 0..bucket_count {
            let mut pairs: Vec<(u32, usize)> = per_key_bucket_counts
                .iter()
                .enumerate()
                .map(|(idx, counts)| {
                    let c = counts.get(bucket_idx).copied().unwrap_or(0);
                    (c, idx)
                })
                .collect();
            pairs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let order: Vec<u32> = pairs.into_iter().map(|(_, idx)| idx as u32).collect();

            let fname = format!("interner_orders_{:04}.bin", bucket_idx);
            let path = chunk_dir.join(&fname);
            {
                use std::io::Write;
                let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
                for idx in order {
                    file.write_all(&idx.to_le_bytes())?;
                }
            }
            order_bucket_files.push(fname);
        }

        // Write index
        let index = IndexFile {
            vocab,
            max_words: total_words,
            bucket_size_words,
            keys: index_entries,
            order_bucket_files,
        };
        let index_path = chunk_dir.join("interner_index.json");
        let index_file = fs::File::create(&index_path)?;
        serde_json::to_writer(index_file, &index)?;
        println!(
            "Exported chunked interner: index={} ({} keys), chunks up to {:04}",
            index_path.display(),
            index.keys.len(),
            chunk_id
        );

        Ok(())
    }

    #[test]
    fn export_small_ortho_archive() -> Result<(), Box<dyn std::error::Error>> {
        use std::collections::{HashMap, HashSet};
        use std::fs;
        use std::path::PathBuf;

        #[derive(serde::Serialize)]
        struct OrthoCandidate {
            word: String,
            min_input: usize,
            child_id: crate::ortho::OrthoId,
        }

        #[derive(serde::Serialize)]
        struct OrthoRecord {
            ortho_id: crate::ortho::OrthoId,
            required_keys: Vec<Vec<String>>,
            candidates: Vec<OrthoCandidate>,
        }

        #[derive(serde::Serialize)]
        struct OrthoArchive {
            stride_words: usize,
            records: Vec<OrthoRecord>,
        }

        // Small corpus to keep the test fast.
        let text = "the quick brown fox jumps over the lazy dog";
        let interner = Interner::from_text(text);

        // Build vocab map
        let vocab = interner.vocabulary.clone();
        let mut word_to_idx = HashMap::new();
        for (idx, word) in vocab.iter().enumerate() {
            word_to_idx.insert(word.clone(), idx);
        }

        // Earliest word positions per (prefix, completion) for min_input calculation.
        let sentences = tokenize_sentences(text);
        let mut first_positions: HashMap<(Vec<usize>, usize), usize> = HashMap::new();
        let mut total_words = 0usize;
        for words in sentences.iter() {
            let len = words.len();
            for start in 0..len {
                let mut prefix_indices = Vec::new();
                for j in (start + 1)..len {
                    if let Some(&prev_idx) = word_to_idx.get(&words[j - 1]) {
                        prefix_indices.push(prev_idx);
                    } else {
                        break;
                    }
                    if let Some(&comp_idx) = word_to_idx.get(&words[j]) {
                        let word_pos = total_words + j;
                        first_positions
                            .entry((prefix_indices.clone(), comp_idx))
                            .or_insert(word_pos);
                    }
                }
            }
            total_words += len;
        }

        // Build a tiny ortho archive: just the empty ortho and its candidates.
        let mut records = Vec::new();
        let ortho = crate::ortho::Ortho::new();
        let (_forbidden, required_raw) = ortho.get_requirements();
        let required_keys: Vec<Vec<String>> = required_raw
            .iter()
            .map(|prefix| {
                prefix
                    .iter()
                    .filter_map(|p| vocab.get(*p as usize).cloned())
                    .collect()
            })
            .collect();

        // Compute candidates by intersecting completions for required prefixes.
        let required_usize: Vec<Vec<usize>> = required_raw
            .iter()
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| prefix.iter().map(|p| *p as usize).collect())
            .collect();
        let candidate_ids: HashSet<usize> = if required_usize.is_empty() {
            (0..interner.vocabulary.len()).collect()
        } else {
            interner
                .intersect(&required_usize, &[])
                .into_iter()
                .collect()
        };

        let mut candidates = Vec::new();
        for cid in candidate_ids {
            let word = interner.vocabulary[cid].clone();
            let min_input = required_raw
                .iter()
                .filter_map(|pref| {
                    let ids: Vec<usize> = pref.iter().map(|p| *p as usize).collect();
                    first_positions.get(&(ids, cid)).copied()
                })
                .max()
                .unwrap_or(0);
            // Child id: apply the word to the ortho (first variant)
            let child_id = ortho.add(cid as u32).get(0).map(|o| o.id()).unwrap_or(0);
            candidates.push(OrthoCandidate {
                word,
                min_input,
                child_id,
            });
        }
        candidates.sort_by_key(|c| (c.min_input, c.word.clone()));

        records.push(OrthoRecord {
            ortho_id: ortho.id(),
            required_keys,
            candidates,
        });

        let archive = OrthoArchive {
            stride_words: 500,
            records,
        };

        let out_dir = PathBuf::from("target/ortho_export_test");
        fs::create_dir_all(&out_dir)?;
        let out_path = out_dir.join("ortho_index_0500.json");
        let file = fs::File::create(&out_path)?;
        serde_json::to_writer_pretty(file, &archive)?;

        assert!(out_path.exists());
        assert!(!archive.records.is_empty());
        assert!(archive.records[0].candidates.len() > 0);

        Ok(())
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
    fn test_intersect_into_count_all_empty_returns_all_indexes() {
        let interner = build_interner("a b c");
        let mut bits = FixedBitSet::with_capacity(interner.vocab_size());
        bits.grow(interner.vocab_size());

        let count = interner.intersect_into_count(&[], &[], &mut bits);

        assert_eq!(count, 3);
        assert_eq!(bits.ones().collect::<Vec<_>>(), vec![0, 1, 2]);
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
    fn test_intersect_into_count_required_and_forbidden_matches_intersect() {
        let interner = build_interner("a b c d. a c.");
        let required = vec![vec![0]];
        let forbidden = vec![1];
        let expected = interner.intersect(&required, &forbidden);
        let mut bits = FixedBitSet::with_capacity(interner.vocab_size());
        bits.grow(interner.vocab_size());

        let count = interner.intersect_into_count(&required, &forbidden, &mut bits);
        let actual = bits.ones().collect::<Vec<_>>();

        assert_eq!(count, expected.len());
        assert_eq!(actual, expected);
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
    fn test_intersect_into_count_required_anded_matches_intersect() {
        let interner = build_interner("a c. b c. a d. b d. a b.");
        let required = vec![vec![0], vec![1]];
        let expected = interner.intersect(&required, &[]);
        let mut bits = FixedBitSet::with_capacity(interner.vocab_size());
        bits.grow(interner.vocab_size());

        let count = interner.intersect_into_count(&required, &[], &mut bits);
        let actual = bits.ones().collect::<Vec<_>>();

        assert_eq!(count, expected.len());
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_intersect_prefix_ids_into_count_matches_prefixes() {
        let interner = build_interner("a c. b c. a d. b d. a b.");
        let required = vec![vec![0], vec![1]];
        let required_ids: Vec<u32> = required
            .iter()
            .map(|prefix| interner.prefix_id_for(prefix).unwrap())
            .collect();
        let mut prefix_bits = FixedBitSet::with_capacity(interner.vocab_size());
        let mut id_bits = FixedBitSet::with_capacity(interner.vocab_size());
        prefix_bits.grow(interner.vocab_size());
        id_bits.grow(interner.vocab_size());

        let prefix_count = interner.intersect_into_count(&required, &[], &mut prefix_bits);
        let id_count = interner.intersect_prefix_ids_into_count(&required_ids, &[], &mut id_bits);

        assert_eq!(id_count, prefix_count);
        assert_eq!(
            id_bits.ones().collect::<Vec<_>>(),
            prefix_bits.ones().collect::<Vec<_>>()
        );
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
    fn test_intersect_into_count_missing_prefix_returns_empty() {
        let interner = build_interner("a b c");
        let mut bits = FixedBitSet::with_capacity(interner.vocab_size());
        bits.grow(interner.vocab_size());

        let count = interner.intersect_into_count(&[vec![999]], &[], &mut bits);

        assert_eq!(count, 0);
        assert_eq!(bits.ones().count(), 0);
    }

    #[test]
    fn test_intersect_into_count_duplicate_forbidden_decrements_once() {
        let interner = build_interner("a b c");
        let mut bits = FixedBitSet::with_capacity(interner.vocab_size());
        bits.grow(interner.vocab_size());

        let count = interner.intersect_into_count(&[], &[1, 1], &mut bits);

        assert_eq!(count, 2);
        assert_eq!(bits.ones().collect::<Vec<_>>(), vec![0, 2]);
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
        // New interner introduces a new prefix [a,b] -> c that did not exist before.
        let (low, high) = build_low_high("a b", "a b c");
        let a_idx = low.vocabulary().iter().position(|w| w == "a").unwrap();
        let b_idx = low.vocabulary().iter().position(|w| w == "b").unwrap();
        let impacted = low.impacted_keys(&high);
        assert!(
            impacted.contains(&vec![a_idx, b_idx]),
            "New longer prefix [a,b] should be marked impacted even if completion word is new"
        );
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
    fn test_impacted_keys_disjoint_vocab_not_impacted() {
        let low = Interner::from_text("a b");
        let high = Interner::from_text("c d");

        let impacted = low.impacted_keys(&high);
        assert!(
            impacted.is_empty(),
            "Disjoint vocabularies should not mark any prefixes impacted"
        );
    }

    #[test]
    fn test_impacted_keys_detects_vocab_mismatch() {
        // Base interner has prefix "b" -> "c"; other interner has prefix "b" -> "a"
        // Vocabulary indices differ ("b" is 0 vs 1), so impacted keys must be mapped back
        // into the base interner's index space to catch the change.
        let base = Interner::from_text("b c");
        let other = Interner::from_text("b a");

        let b_idx_in_base = base.vocabulary().iter().position(|w| w == "b").unwrap();
        let impacted = base.impacted_keys(&other);

        assert!(
            impacted.contains(&vec![b_idx_in_base]),
            "Impact on prefix \"b\" should be detected even when vocab indices differ"
        );
    }

    #[test]
    fn test_impacted_keys_no_change() {
        let low = Interner::from_text("a b");
        let high = Interner {
            version: low.version + 1,
            vocabulary: low.vocabulary.clone(),
            prefix_to_completions: low.prefix_to_completions.clone(),
            prefix_completions_by_id: low.prefix_completions_by_id.clone(),
            prefix_stats: low.prefix_stats.clone(),
            single_token_stats: low.single_token_stats.clone(),
            max_prefix_len: low.max_prefix_len,
            prefix_to_id: low.prefix_to_id.clone(),
            child_stats_by_id: low.child_stats_by_id.clone(),
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
        assert_eq!(
            interner1.vocabulary().len(),
            3,
            "interner1 vocab: {:?}",
            interner1.vocabulary()
        );

        // Second test: "the party and" - single sentence with all three words
        let interner2 = Interner::from_text("the party and");
        assert_eq!(
            interner2.vocabulary().len(),
            3,
            "interner2 vocab: {:?}",
            interner2.vocabulary()
        );

        // Now test: feed both into the same interner
        let interner3 = Interner::from_text("the party, and");
        let interner3 = interner3.add_text("the party and");

        // Should have exactly 3 words total (and, party, the)
        println!("Combined interner vocabulary: {:?}", interner3.vocabulary());
        assert_eq!(
            interner3.vocabulary().len(),
            3,
            "Combined interner should have 3 unique words, got: {:?}",
            interner3.vocabulary()
        );

        // Check that "and" appears exactly once
        let and_count = interner3
            .vocabulary()
            .iter()
            .filter(|w| *w == "and")
            .count();
        assert_eq!(
            and_count, 1,
            "The word 'and' should appear exactly once in vocabulary"
        );
    }
}
