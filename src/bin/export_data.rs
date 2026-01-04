use fold::error::FoldError;
use fold::generation_runner::run_generation_loop;
use fold::generation_store::{Config, GenerationStore, Role};
use fold::interner::Interner;
use fold::metrics::Metrics;
use fold::ortho::{Ortho, OrthoId};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

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
    order_bucket_files: Vec<String>,
}

#[derive(serde::Serialize)]
struct OrthoCandidate {
    word: String,
    word_id: usize,
    min_input: usize,
    child_id: OrthoId,
}

#[derive(serde::Serialize)]
struct OrthoRecord {
    ortho_id: OrthoId,
    required_keys: Vec<Vec<String>>,
    forbidden_keys: Vec<String>,
    candidates: Vec<OrthoCandidate>,
    display: String,
}

#[derive(serde::Serialize)]
struct OrthoArchive {
    stride_words: usize,
    records: Vec<OrthoRecord>,
}

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

fn truncate_text_to_words(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .take(limit)
        .collect::<Vec<&str>>()
        .join(" ")
}

fn export_interner_chunked(interner: &Interner, text: &str, out_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(out_dir)?;

    let vocab: Vec<String> = interner.vocabulary().to_vec();
    let mut word_to_idx = HashMap::new();
    for (idx, word) in vocab.iter().enumerate() {
        word_to_idx.insert(word.clone(), idx);
    }

    let sentences = tokenize_sentences(text);
    let mut first_positions: HashMap<(Vec<usize>, usize), usize> = HashMap::new();
    let mut word_first_pos: HashMap<usize, usize> = HashMap::new();
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
                    word_first_pos.entry(comp_idx).or_insert(word_pos);
                    first_positions
                        .entry((prefix_indices.clone(), comp_idx))
                        .or_insert(word_pos);
                }
            }
        }
        total_words += len;
    }

    let mut prefixes: Vec<_> = interner.prefix_entries().collect();
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

    let chunk_size: usize = 10_000;
    let mut chunk_id: usize = 1;
    let mut chunk_keys: Vec<KeyEntry> = Vec::with_capacity(chunk_size);
    let mut index_entries: Vec<IndexEntry> = Vec::with_capacity(prefixes.len());
    let mut per_key_bucket_counts: Vec<Vec<u32>> = Vec::with_capacity(prefixes.len());

    let flush_chunk = |chunk_id: usize, chunk_keys: &mut Vec<KeyEntry>| -> anyhow::Result<()> {
        if chunk_keys.is_empty() {
            return Ok(());
        }
        let chunk_name = format!("interner_keys_{:04}.json", chunk_id);
        let chunk_path = out_dir.join(&chunk_name);
        let file = fs::File::create(&chunk_path)?;
        serde_json::to_writer(file, &serde_json::json!({ "keys": chunk_keys }))?;
        chunk_keys.clear();
        Ok(())
    };

    for (prefix, bitset) in prefixes {
        let words = prefix
            .iter()
            .map(|&idx| interner.vocabulary()[idx].clone())
            .collect::<Vec<String>>();

        let mut completions_vec = Vec::new();
        let mut bucket_counts: Vec<u32> = vec![0u32; bucket_count];
        for completion_idx in bitset.ones() {
            let word = interner.vocabulary()[completion_idx].clone();
            let first_word_pos = first_positions
                .get(&(prefix.clone(), completion_idx))
                .copied()
                .unwrap_or(total_words);
            if !bucket_counts.is_empty() {
                let idx = (first_word_pos.min(total_words.saturating_sub(1))) / bucket_size_words;
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

    flush_chunk(chunk_id, &mut chunk_keys)?;

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
        let path = out_dir.join(&fname);
        {
            use std::io::Write;
            let mut file = std::io::BufWriter::new(fs::File::create(&path)?);
            for idx in order {
                file.write_all(&idx.to_le_bytes())?;
            }
        }
        order_bucket_files.push(fname);
    }

    let index = IndexFile {
        vocab,
        max_words: total_words,
        bucket_size_words,
        keys: index_entries,
        order_bucket_files,
    };
    let index_path = out_dir.join("interner_index.json");
    let index_file = fs::File::create(&index_path)?;
    serde_json::to_writer(index_file, &index)?;

    println!(
        "Interner export: index={} ({} keys), chunks up to {:04}",
        index_path.display(),
        index.keys.len(),
        chunk_id
    );

    Ok(())
}

fn export_ortho_archive(
    interner: &Interner,
    text: &str,
    stride_words: usize,
    out_dir: &Path,
) -> anyhow::Result<()> {
    fs::create_dir_all(out_dir)?;

    let vocab: Vec<String> = interner.vocabulary().to_vec();
    let mut word_to_idx = HashMap::new();
    for (idx, word) in vocab.iter().enumerate() {
        word_to_idx.insert(word.clone(), idx);
    }

    let sentences = tokenize_sentences(text);
    let mut first_positions: HashMap<(Vec<usize>, usize), usize> = HashMap::new();
    let mut word_first_pos: HashMap<usize, usize> = HashMap::new();
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
                    word_first_pos.entry(comp_idx).or_insert(word_pos);
                    first_positions
                        .entry((prefix_indices.clone(), comp_idx))
                        .or_insert(word_pos);
                }
            }
        }
        total_words += len;
    }

    println!(
        "Ortho export: stride={} words -> building store",
        stride_words
    );
    // Prepare generation store using the same configuration logic as main (leader role).
    let cfg = Config::compute_config(Role::Leader)
        .ok_or_else(|| anyhow::anyhow!("insufficient memory for generation config"))?;
    let work_dir = out_dir.join(format!("work_{}", stride_words));
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir)?;
    }
    let mut store = GenerationStore::new_with_config(work_dir.clone(), 8)?;
    store.configure(&cfg);

    let metrics = Metrics::new();
    let mut noop_housekeeping = || -> Result<(), FoldError> { Ok(()) };
    let progress_factory = |_gen: u64| -> Option<fold::generation_store::ProgressCallback> { None };

    println!("Ortho export: stride={} running generations…", stride_words);
    run_generation_loop(
        interner,
        &mut store,
        &cfg,
        Role::Leader,
        &metrics,
        || false,
        &mut noop_housekeeping,
        progress_factory,
        None,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    store.flush_all()?;

    let make_record = |ortho: &Ortho| -> OrthoRecord {
        let (forbidden_raw, required_raw) = ortho.get_requirements();
        let required_keys: Vec<Vec<String>> = required_raw
            .iter()
            .map(|prefix| {
                prefix
                    .iter()
                    .filter_map(|p| vocab.get(*p as usize).cloned())
                    .collect()
            })
            .collect();
        let forbidden_keys: Vec<String> = forbidden_raw
            .iter()
            .filter_map(|p| vocab.get(*p as usize).cloned())
            .collect();

        let mut required_sets = Vec::new();
        for prefix in required_raw.iter() {
            if prefix.is_empty() {
                continue;
            }
            let ids: Vec<usize> = prefix.iter().map(|p| *p as usize).collect();
            if let Some(bits) = interner.completions_for_prefix(&ids) {
                required_sets.push(bits);
            }
        }
        let candidate_ids: HashSet<usize> = if required_sets.is_empty() {
            (0..interner.vocabulary().len()).collect()
        } else {
            let mut acc = required_sets[0].clone();
            for bs in required_sets.iter().skip(1) {
                acc.intersect_with(bs);
            }
            acc.ones().collect()
        };

        let mut candidates = Vec::new();
        let forbidden_set: HashSet<usize> = forbidden_raw.iter().map(|v| *v as usize).collect();
        for cid in candidate_ids {
            if forbidden_set.contains(&cid) {
                continue;
            }
            let word = interner.vocabulary()[cid].clone();
            let min_input = if required_raw.is_empty() {
                *word_first_pos.get(&cid).unwrap_or(&total_words)
            } else {
                required_raw
                    .iter()
                    .filter_map(|pref| {
                        let ids: Vec<usize> = pref.iter().map(|p| *p as usize).collect();
                        first_positions.get(&(ids, cid)).copied()
                    })
                    .max()
                    .unwrap_or(total_words)
            };
            let child_opt = ortho.add(cid as u32).get(0).cloned();
            let child_id = child_opt.as_ref().map(|o| o.id()).unwrap_or(0);
            candidates.push(OrthoCandidate {
                word,
                word_id: cid,
                min_input,
                child_id,
            });
        }
        candidates.sort_by_key(|c| (c.min_input, c.word.clone()));

        let display = ortho.display(interner).to_string();

        OrthoRecord {
            ortho_id: ortho.id(),
            required_keys,
            forbidden_keys,
            candidates,
            display,
        }
    };

    println!("Ortho export: stride={} collecting records…", stride_words);
    let mut records = Vec::new();
    let mut id_index = HashSet::new();
    // Ensure the empty ortho is present
    let root = Ortho::new();
    if id_index.insert(root.id()) {
        records.push(make_record(&root));
    }
    for bucket in 0..8 {
        let mut bucket_count = 0usize;
        for res in store.history_iter_with_buffer(bucket, cfg.read_buf_bytes)? {
            let ortho_bytes = res?.bytes;
            let ortho = Ortho::from_bytes(ortho_bytes.as_ref())?;
            if !id_index.insert(ortho.id()) {
                continue;
            }
            records.push(make_record(&ortho));
            bucket_count += 1;
            if bucket_count % 10_000 == 0 {
                println!(
                    "  stride={} bucket={} … {} records",
                    stride_words, bucket, bucket_count
                );
            }
        }
    }

    let archive = OrthoArchive {
        stride_words,
        records,
    };

    let fname = format!("ortho_index_{:04}.json", stride_words);
    let out_path = out_dir.join(fname);
    let file = fs::File::create(&out_path)?;
    serde_json::to_writer_pretty(file, &archive)?;

    println!(
        "Ortho export: stride={} words, records={}, file={}",
        stride_words,
        archive.records.len(),
        out_path.display()
    );

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let text = fs::read_to_string("e.txt")?;

    // Interner export (full text) to data/interner_export
    println!("Interner export: building interner for full text…");
    let interner = Interner::from_text(&text);
    let interner_out = PathBuf::from("data/interner_export");
    println!(
        "Interner export: writing chunks/index to {}",
        interner_out.display()
    );
    export_interner_chunked(&interner, &text, &interner_out)?;

    // Ortho exports for small strides (500, 1000 words)
    let strides = [500usize, 1000usize];
    for &stride in &strides {
        let truncated = truncate_text_to_words(&text, stride);
        let interner_small = Interner::from_text(&truncated);
        let ortho_out = PathBuf::from("data/ortho_exports");
        fs::create_dir_all(&ortho_out)?;
        export_ortho_archive(&interner_small, &truncated, stride, &ortho_out)?;
    }

    Ok(())
}
