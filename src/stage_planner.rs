use crate::{FoldError, splitter::Splitter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const STAGE_MANIFEST_FILENAME: &str = "stage_manifest.json";
pub const PLANNER_META_FILENAME: &str = "planner_meta.json";

const DEFAULT_TARGET_CHUNK_WORDS: usize = 256;
const DEFAULT_SOFT_MAX_CHUNK_WORDS: usize = 384;
const DEFAULT_MIN_CHUNK_WORDS: usize = 64;
const DEFAULT_TARGET_CHUNK_COST: u64 = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageManifestEntry {
    pub chunk_index: usize,
    pub chunk_filename: String,
    pub source_file: String,
    pub word_count: usize,
    pub unique_vocab_count: usize,
    pub planner_cost: u64,
    pub sentence_start: usize,
    pub sentence_end_exclusive: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageManifest {
    pub entries: Vec<StageManifestEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannerMeta {
    pub range_start: usize,
    pub range_end_exclusive: usize,
    pub leaf_count: usize,
    pub merge_level: usize,
    pub planner_cost: u64,
    pub word_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StagePlannerConfig {
    pub target_chunk_words: usize,
    pub soft_max_chunk_words: usize,
    pub min_chunk_words: usize,
    pub target_chunk_cost: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagePlanResult {
    pub manifest: StageManifest,
    pub chunks_written: usize,
    pub avg_words: usize,
    pub median_words: usize,
    pub avg_cost: u64,
    pub max_cost: u64,
}

#[derive(Clone, Debug)]
struct PlannedChunk {
    sentence_start: usize,
    sentence_end_exclusive: usize,
    text: String,
    word_count: usize,
    unique_vocab_count: usize,
    planner_cost: u64,
}

impl Default for StagePlannerConfig {
    fn default() -> Self {
        Self {
            target_chunk_words: DEFAULT_TARGET_CHUNK_WORDS,
            soft_max_chunk_words: DEFAULT_SOFT_MAX_CHUNK_WORDS,
            min_chunk_words: DEFAULT_MIN_CHUNK_WORDS,
            target_chunk_cost: DEFAULT_TARGET_CHUNK_COST,
        }
    }
}

impl StagePlannerConfig {
    pub fn from_env(min_chunk_words_override: Option<usize>) -> Self {
        let mut config = Self::default();

        if let Some(value) = read_env_usize("FOLD_STAGE_TARGET_WORDS") {
            config.target_chunk_words = value;
        }
        if let Some(value) = read_env_usize("FOLD_STAGE_SOFT_MAX_WORDS") {
            config.soft_max_chunk_words = value;
        }
        if let Some(value) = read_env_usize("FOLD_STAGE_MIN_WORDS") {
            config.min_chunk_words = value;
        }
        if let Some(value) = min_chunk_words_override {
            config.min_chunk_words = value;
        }
        if let Some(value) = read_env_u64("FOLD_STAGE_TARGET_COST") {
            config.target_chunk_cost = value;
        }

        if config.soft_max_chunk_words < config.target_chunk_words {
            config.soft_max_chunk_words = config.target_chunk_words;
        }

        config
    }
}

impl PlannerMeta {
    pub fn merge(a: &Self, b: &Self) -> Option<Self> {
        let (left, right) = if a.range_start <= b.range_start {
            (a, b)
        } else {
            (b, a)
        };
        if left.range_end_exclusive != right.range_start {
            return None;
        }

        Some(Self {
            range_start: left.range_start,
            range_end_exclusive: right.range_end_exclusive,
            leaf_count: left.leaf_count.saturating_add(right.leaf_count),
            merge_level: left.merge_level.max(right.merge_level).saturating_add(1),
            planner_cost: left.planner_cost.saturating_add(right.planner_cost),
            word_count: left.word_count.saturating_add(right.word_count),
        })
    }
}

pub fn planner_meta_for_chunk(filename: &str, text: &str) -> Option<PlannerMeta> {
    let chunk_index = parse_chunk_index_from_filename(filename)?;
    Some(PlannerMeta {
        range_start: chunk_index,
        range_end_exclusive: chunk_index.saturating_add(1),
        leaf_count: 1,
        merge_level: 0,
        planner_cost: planner_cost_for_text(text),
        word_count: count_words(text),
    })
}

pub fn planner_cost_for_text(text: &str) -> u64 {
    let splitter = Splitter::new();
    let sentences = splitter.sentence_units(text);
    planner_cost_for_sentence_words(sentences.iter().map(|unit| unit.words.as_slice()))
}

pub fn write_stage_manifest(state_dir: &Path, manifest: &StageManifest) -> Result<(), FoldError> {
    let manifest_path = state_dir.join(STAGE_MANIFEST_FILENAME);
    let bytes = serde_json::to_vec_pretty(manifest).map_err(json_to_io)?;
    fs::write(manifest_path, bytes).map_err(FoldError::Io)
}

pub fn write_planner_meta(
    archive_path: &Path,
    planner_meta: &PlannerMeta,
) -> Result<(), FoldError> {
    let meta_path = archive_path.join(PLANNER_META_FILENAME);
    let bytes = serde_json::to_vec_pretty(planner_meta).map_err(json_to_io)?;
    fs::write(meta_path, bytes).map_err(FoldError::Io)
}

pub fn load_planner_meta(archive_path: &Path) -> Result<PlannerMeta, FoldError> {
    let meta_path = archive_path.join(PLANNER_META_FILENAME);
    let bytes = fs::read(meta_path).map_err(FoldError::Io)?;
    serde_json::from_slice(&bytes).map_err(json_to_io)
}

pub fn stage_input_file(
    input_file: &Path,
    state_dir: &Path,
    min_chunk_words_override: Option<usize>,
) -> Result<StagePlanResult, FoldError> {
    let input_text = fs::read_to_string(input_file).map_err(FoldError::Io)?;
    let config = StagePlannerConfig::from_env(min_chunk_words_override);
    let planned_chunks = plan_chunks(&input_text, config);
    let input_dir = state_dir.join("input");
    fs::create_dir_all(&input_dir).map_err(FoldError::Io)?;

    let basename = input_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            FoldError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "input file must have a valid UTF-8 stem",
            ))
        })?;
    clear_existing_chunk_files(&input_dir, basename)?;

    let source_file = input_file.to_string_lossy().to_string();
    let mut manifest_entries = Vec::with_capacity(planned_chunks.len());
    let mut word_counts = Vec::with_capacity(planned_chunks.len());
    let mut planner_costs = Vec::with_capacity(planned_chunks.len());

    for (chunk_index, chunk) in planned_chunks.into_iter().enumerate() {
        let file_name = format!("{}_chunk_{:04}.txt", basename, chunk_index + 1);
        let file_path = input_dir.join(&file_name);
        fs::write(&file_path, chunk.text).map_err(FoldError::Io)?;

        word_counts.push(chunk.word_count);
        planner_costs.push(chunk.planner_cost);
        manifest_entries.push(StageManifestEntry {
            chunk_index,
            chunk_filename: file_name,
            source_file: source_file.clone(),
            word_count: chunk.word_count,
            unique_vocab_count: chunk.unique_vocab_count,
            planner_cost: chunk.planner_cost,
            sentence_start: chunk.sentence_start,
            sentence_end_exclusive: chunk.sentence_end_exclusive,
        });
    }

    let manifest = StageManifest {
        entries: manifest_entries,
    };
    write_stage_manifest(state_dir, &manifest)?;

    word_counts.sort_unstable();
    planner_costs.sort_unstable();
    let chunks_written = manifest.entries.len();
    let avg_words = average_usize(&word_counts);
    let median_words = median_usize(&word_counts);
    let avg_cost = average_u64(&planner_costs);
    let max_cost = planner_costs.last().copied().unwrap_or(0);

    Ok(StagePlanResult {
        manifest,
        chunks_written,
        avg_words,
        median_words,
        avg_cost,
        max_cost,
    })
}

fn read_env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse::<usize>().ok()
}

fn read_env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse::<u64>().ok()
}

fn average_usize(values: &[usize]) -> usize {
    if values.is_empty() {
        0
    } else {
        values.iter().copied().sum::<usize>() / values.len()
    }
}

fn average_u64(values: &[u64]) -> u64 {
    if values.is_empty() {
        0
    } else {
        values.iter().copied().sum::<u64>() / values.len() as u64
    }
}

fn median_usize(values: &[usize]) -> usize {
    if values.is_empty() {
        0
    } else {
        values[values.len() / 2]
    }
}

fn clear_existing_chunk_files(input_dir: &Path, basename: &str) -> Result<(), FoldError> {
    if !input_dir.exists() {
        return Ok(());
    }

    let prefix = format!("{}_chunk_", basename);
    for entry in fs::read_dir(input_dir).map_err(FoldError::Io)? {
        let entry = entry.map_err(FoldError::Io)?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name.starts_with(&prefix)
            && path.extension().and_then(|ext| ext.to_str()) == Some("txt")
        {
            fs::remove_file(path).map_err(FoldError::Io)?;
        }
    }

    Ok(())
}

fn plan_chunks(text: &str, config: StagePlannerConfig) -> Vec<PlannedChunk> {
    let splitter = Splitter::new();
    let sentences = splitter.sentence_units(text);
    if sentences.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < sentences.len() {
        let mut end = start;
        let mut current_words = 0usize;

        while end < sentences.len() {
            let sentence_words = sentences[end].words.len();
            let candidate_end = end + 1;
            let candidate_words = current_words.saturating_add(sentence_words);
            let candidate_cost = planner_cost_for_sentence_words(
                sentences[start..candidate_end]
                    .iter()
                    .map(|unit| unit.words.as_slice()),
            );
            let allow_first_sentence = end == start;
            let within_limits = candidate_words <= config.soft_max_chunk_words
                && candidate_cost <= config.target_chunk_cost
                && candidate_words <= config.target_chunk_words.max(config.soft_max_chunk_words);

            if allow_first_sentence || within_limits {
                current_words = candidate_words;
                end = candidate_end;
            } else {
                break;
            }
        }

        chunks.push(build_chunk(&sentences, start, end));
        start = end;
    }

    merge_small_chunks(&mut chunks, config.min_chunk_words);
    chunks
}

fn build_chunk(
    sentences: &[crate::splitter::SentenceUnit],
    sentence_start: usize,
    sentence_end_exclusive: usize,
) -> PlannedChunk {
    let sentence_slice = &sentences[sentence_start..sentence_end_exclusive];
    let text = sentence_slice
        .iter()
        .map(|sentence| sentence.raw.as_str())
        .collect::<Vec<_>>()
        .join(". ");
    let word_count = sentence_slice
        .iter()
        .map(|sentence| sentence.words.len())
        .sum();
    let unique_vocab_count = sentence_slice
        .iter()
        .flat_map(|sentence| sentence.words.iter().cloned())
        .collect::<BTreeSet<_>>()
        .len();
    let planner_cost = planner_cost_for_sentence_words(
        sentence_slice
            .iter()
            .map(|sentence| sentence.words.as_slice()),
    );

    PlannedChunk {
        sentence_start,
        sentence_end_exclusive,
        text,
        word_count,
        unique_vocab_count,
        planner_cost,
    }
}

fn merge_small_chunks(chunks: &mut Vec<PlannedChunk>, min_chunk_words: usize) {
    if min_chunk_words == 0 || chunks.len() <= 1 {
        return;
    }

    loop {
        let Some(index) = chunks
            .iter()
            .position(|chunk| chunk.word_count < min_chunk_words)
        else {
            break;
        };

        if chunks.len() == 1 {
            break;
        }

        let merge_into_left = match index {
            0 => false,
            i if i + 1 == chunks.len() => true,
            i => chunks[i - 1].planner_cost <= chunks[i + 1].planner_cost,
        };

        if merge_into_left {
            let right = chunks.remove(index);
            let left = &mut chunks[index - 1];
            merge_chunk_into(left, right);
        } else {
            let right = chunks.remove(index + 1);
            let current = &mut chunks[index];
            merge_chunk_into(current, right);
        }
    }
}

fn merge_chunk_into(target: &mut PlannedChunk, other: PlannedChunk) {
    target.sentence_end_exclusive = other.sentence_end_exclusive;
    target.text = format!("{}. {}", target.text, other.text);
    target.word_count = target.word_count.saturating_add(other.word_count);
    target.unique_vocab_count = 0;
    target.planner_cost = 0;

    let splitter = Splitter::new();
    let sentences = splitter.sentence_units(&target.text);
    target.word_count = sentences.iter().map(|sentence| sentence.words.len()).sum();
    target.unique_vocab_count = sentences
        .iter()
        .flat_map(|sentence| sentence.words.iter().cloned())
        .collect::<BTreeSet<_>>()
        .len();
    target.planner_cost =
        planner_cost_for_sentence_words(sentences.iter().map(|sentence| sentence.words.as_slice()));
}

fn planner_cost_for_sentence_words<'a, I>(sentences: I) -> u64
where
    I: IntoIterator<Item = &'a [String]>,
{
    let mut unique_vocab = BTreeSet::new();
    let mut phrase_cost = 0u64;

    for words in sentences {
        let len = words.len() as u64;
        if len >= 2 {
            phrase_cost = phrase_cost.saturating_add(len.saturating_mul(len - 1) / 2);
        }
        for word in words {
            unique_vocab.insert(word.clone());
        }
    }

    phrase_cost.saturating_add((unique_vocab.len() as u64).saturating_mul(8))
}

fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

pub fn parse_chunk_index_from_filename(filename: &str) -> Option<usize> {
    let stem = PathBuf::from(filename)
        .file_stem()?
        .to_string_lossy()
        .to_string();
    let (_, suffix) = stem.rsplit_once("_chunk_")?;
    let chunk_number = suffix.parse::<usize>().ok()?;
    chunk_number.checked_sub(1)
}

fn json_to_io(error: serde_json::Error) -> FoldError {
    FoldError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn planner_cost_counts_phrases_and_unique_vocab() {
        let text = "alpha beta. beta gamma";
        assert_eq!(planner_cost_for_text(text), 26);
    }

    #[test]
    fn chunk_parser_returns_zero_based_index() {
        assert_eq!(parse_chunk_index_from_filename("e_chunk_0001.txt"), Some(0));
        assert_eq!(
            parse_chunk_index_from_filename("e_chunk_0123.txt"),
            Some(122)
        );
        assert_eq!(parse_chunk_index_from_filename("plain.txt"), None);
    }

    #[test]
    fn planner_keeps_oversized_sentence_whole() {
        let text = "one two three four five six seven eight nine ten";
        let chunks = plan_chunks(
            text,
            StagePlannerConfig {
                target_chunk_words: 2,
                soft_max_chunk_words: 3,
                min_chunk_words: 0,
                target_chunk_cost: 3,
            },
        );

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].sentence_start, 0);
        assert_eq!(chunks[0].sentence_end_exclusive, 1);
    }

    #[test]
    fn planner_merges_tiny_tail_at_sentence_boundaries() {
        let text = "alpha beta gamma delta. one two three four. tail one";
        let chunks = plan_chunks(
            text,
            StagePlannerConfig {
                target_chunk_words: 4,
                soft_max_chunk_words: 4,
                min_chunk_words: 3,
                target_chunk_cost: 100,
            },
        );

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].sentence_start, 0);
        assert_eq!(chunks[0].sentence_end_exclusive, 1);
        assert_eq!(chunks[1].sentence_start, 1);
        assert_eq!(chunks[1].sentence_end_exclusive, 3);
    }

    #[test]
    fn planner_meta_merge_requires_adjacent_ranges() {
        let a = PlannerMeta {
            range_start: 0,
            range_end_exclusive: 2,
            leaf_count: 2,
            merge_level: 1,
            planner_cost: 100,
            word_count: 20,
        };
        let b = PlannerMeta {
            range_start: 2,
            range_end_exclusive: 5,
            leaf_count: 3,
            merge_level: 2,
            planner_cost: 200,
            word_count: 30,
        };

        let merged = PlannerMeta::merge(&a, &b).unwrap();
        assert_eq!(merged.range_start, 0);
        assert_eq!(merged.range_end_exclusive, 5);
        assert_eq!(merged.leaf_count, 5);
        assert_eq!(merged.merge_level, 3);
        assert_eq!(merged.planner_cost, 300);
        assert_eq!(merged.word_count, 50);
    }

    #[test]
    fn stage_input_writes_manifest_and_sentence_snapped_chunks() {
        let temp_dir = tempdir().unwrap();
        let input = temp_dir.path().join("book.txt");
        fs::write(
            &input,
            "Alpha beta. Gamma delta epsilon. Zeta eta theta. Tail end.",
        )
        .unwrap();

        let result = stage_input_file(&input, temp_dir.path(), Some(3)).unwrap();

        let manifest_path = temp_dir.path().join(STAGE_MANIFEST_FILENAME);
        assert!(manifest_path.exists());
        assert_eq!(result.manifest.entries.len(), result.chunks_written);
        assert_eq!(result.manifest.entries[0].sentence_start, 0);
        assert!(
            result
                .manifest
                .entries
                .windows(2)
                .all(|window| { window[0].sentence_end_exclusive == window[1].sentence_start })
        );

        for entry in &result.manifest.entries {
            let chunk_path = temp_dir.path().join("input").join(&entry.chunk_filename);
            assert!(chunk_path.exists());
            let chunk_text = fs::read_to_string(chunk_path).unwrap();
            let splitter = Splitter::new();
            let unit_count = splitter.sentence_units(&chunk_text).len();
            assert_eq!(
                unit_count,
                entry.sentence_end_exclusive - entry.sentence_start
            );
        }
    }

    #[test]
    fn planner_meta_round_trip() {
        let temp_dir = tempdir().unwrap();
        let planner_meta = PlannerMeta {
            range_start: 4,
            range_end_exclusive: 6,
            leaf_count: 2,
            merge_level: 1,
            planner_cost: 99,
            word_count: 123,
        };
        write_planner_meta(temp_dir.path(), &planner_meta).unwrap();
        let loaded = load_planner_meta(temp_dir.path()).unwrap();
        assert_eq!(loaded, planner_meta);
    }
}
