use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fixedbitset::FixedBitSet;
use fold::interner::Interner;
use std::fs;

const SAMPLE_TEXT: &str = "The quick brown fox jumps over the lazy dog. \
    A journey of a thousand miles begins with a single step. \
    To be or not to be, that is the question. \
    All that glitters is not gold.";

const LARGE_TEXT: &str = "The quick brown fox jumps over the lazy dog. \
    A journey of a thousand miles begins with a single step. \
    To be or not to be, that is the question. \
    All that glitters is not gold. \
    Where there is a will, there is a way. \
    Actions speak louder than words. \
    The early bird catches the worm. \
    Better late than never. \
    Practice makes perfect. \
    Knowledge is power.";

fn bench_from_text(c: &mut Criterion) {
    c.bench_function("interner_from_text", |b| {
        b.iter(|| Interner::from_text(black_box(SAMPLE_TEXT)))
    });
}

fn bench_from_text_large(c: &mut Criterion) {
    c.bench_function("interner_from_text_large", |b| {
        b.iter(|| Interner::from_text(black_box(LARGE_TEXT)))
    });
}

fn bench_add_text(c: &mut Criterion) {
    let interner = Interner::from_text(SAMPLE_TEXT);
    let additional = "The pen is mightier than the sword.";

    c.bench_function("interner_add_text", |b| {
        b.iter(|| interner.add_text(black_box(additional)))
    });
}

fn bench_intersect_simple(c: &mut Criterion) {
    let interner = Interner::from_text(SAMPLE_TEXT);
    let required = vec![vec![0]];
    let forbidden = vec![];

    c.bench_function("interner_intersect_simple", |b| {
        b.iter(|| interner.intersect(black_box(&required), black_box(&forbidden)))
    });
}

fn bench_intersect_complex(c: &mut Criterion) {
    let interner = Interner::from_text(LARGE_TEXT);
    let required = vec![vec![0], vec![1, 2]];
    let forbidden = vec![3, 4, 5];

    c.bench_function("interner_intersect_complex", |b| {
        b.iter(|| interner.intersect(black_box(&required), black_box(&forbidden)))
    });
}

fn bench_intersect_many_forbidden(c: &mut Criterion) {
    let interner = Interner::from_text(LARGE_TEXT);
    let required = vec![vec![0]];
    let forbidden: Vec<usize> = (1..20).collect();

    c.bench_function("interner_intersect_many_forbidden", |b| {
        b.iter(|| interner.intersect(black_box(&required), black_box(&forbidden)))
    });
}

fn high_fanout_single_token_prefixes(interner: &Interner, count: usize) -> Vec<Vec<usize>> {
    let mut prefixes: Vec<(usize, usize)> = (0..interner.vocab_size())
        .filter_map(|idx| {
            interner
                .completions_for_prefix(&[idx])
                .map(|bits| (idx, bits.count_ones(..)))
        })
        .collect();
    prefixes.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    prefixes
        .into_iter()
        .take(count)
        .map(|(idx, _)| vec![idx])
        .collect()
}

fn bench_intersect_e_txt_large_vocab(c: &mut Criterion) {
    let text = fs::read_to_string("e.txt").expect("failed to read e.txt from repository root");
    let interner = Interner::from_text(&text);
    assert!(
        interner.vocab_size() >= 1024,
        "e.txt should exercise a large vocabulary"
    );

    let required = high_fanout_single_token_prefixes(&interner, 2);
    let forbidden = Vec::new();
    let mut out = FixedBitSet::with_capacity(interner.vocab_size());
    out.grow(interner.vocab_size());
    black_box(interner.intersect_into_count(&required, &forbidden, &mut out));

    c.bench_function("interner_intersect_e_txt_large_vocab", |b| {
        b.iter(|| {
            let count = interner.intersect_into_count(
                black_box(&required),
                black_box(&forbidden),
                black_box(&mut out),
            );
            black_box(count)
        })
    });
}

fn bench_intersect_e_txt_large_vocab_prefix_ids(c: &mut Criterion) {
    let text = fs::read_to_string("e.txt").expect("failed to read e.txt from repository root");
    let interner = Interner::from_text(&text);
    assert!(
        interner.vocab_size() >= 1024,
        "e.txt should exercise a large vocabulary"
    );

    let required = high_fanout_single_token_prefixes(&interner, 2);
    let required_ids: Vec<u32> = required
        .iter()
        .map(|prefix| interner.prefix_id_for(prefix).expect("prefix should exist"))
        .collect();
    let forbidden = Vec::new();
    let mut out = FixedBitSet::with_capacity(interner.vocab_size());
    out.grow(interner.vocab_size());
    black_box(interner.intersect_prefix_ids_into_count(&required_ids, &forbidden, &mut out));

    c.bench_function("interner_intersect_e_txt_large_vocab_prefix_ids", |b| {
        b.iter(|| {
            let count = interner.intersect_prefix_ids_into_count(
                black_box(&required_ids),
                black_box(&forbidden),
                black_box(&mut out),
            );
            black_box(count)
        })
    });
}

fn bench_merge(c: &mut Criterion) {
    let interner1 = Interner::from_text(SAMPLE_TEXT);
    let interner2 = Interner::from_text("The pen is mightier than the sword.");

    c.bench_function("interner_merge", |b| {
        b.iter(|| interner1.merge(black_box(&interner2)))
    });
}

fn bench_completions_for_prefix(c: &mut Criterion) {
    let interner = Interner::from_text(SAMPLE_TEXT);
    let prefix = vec![0];

    c.bench_function("interner_completions_for_prefix", |b| {
        b.iter(|| interner.completions_for_prefix(black_box(&prefix)))
    });
}

fn bench_impacted_keys(c: &mut Criterion) {
    let interner1 = Interner::from_text(SAMPLE_TEXT);
    let interner2 = interner1.add_text("The pen is mightier than the sword.");

    c.bench_function("interner_impacted_keys", |b| {
        b.iter(|| interner1.impacted_keys(black_box(&interner2)))
    });
}

fn bench_completions_equal_up_to_vocab(c: &mut Criterion) {
    let interner1 = Interner::from_text(SAMPLE_TEXT);
    let interner2 = interner1.add_text("Additional text here.");
    let prefix = vec![0];

    c.bench_function("interner_completions_equal_up_to_vocab", |b| {
        b.iter(|| {
            interner1.completions_equal_up_to_vocab(black_box(&interner2), black_box(&prefix))
        })
    });
}

fn bench_all_completions_equal_up_to_vocab(c: &mut Criterion) {
    let interner1 = Interner::from_text(SAMPLE_TEXT);
    let interner2 = interner1.add_text("Additional text here.");
    let prefixes = vec![vec![0], vec![1], vec![2]];

    c.bench_function("interner_all_completions_equal_up_to_vocab", |b| {
        b.iter(|| {
            interner1.all_completions_equal_up_to_vocab(black_box(&interner2), black_box(&prefixes))
        })
    });
}

fn bench_string_for_index(c: &mut Criterion) {
    let interner = Interner::from_text(SAMPLE_TEXT);

    c.bench_function("interner_string_for_index", |b| {
        b.iter(|| interner.string_for_index(black_box(5)))
    });
}

fn bench_prefix_stats_by_parent_id(c: &mut Criterion) {
    let interner = Interner::from_text(LARGE_TEXT);
    // Find a prefix of length >= 2 that has children in prefix_stats
    let prefix: Vec<usize> = (0..interner.vocab_size())
        .flat_map(|a| (0..interner.vocab_size()).map(move |b| vec![a, b]))
        .find(|p| interner.prefix_id_for(p).is_some())
        .expect("should find a 2-token prefix");
    let parent_id = interner.prefix_id_for(&prefix).unwrap();
    let appended = 0usize;

    c.bench_function("interner_prefix_stats_by_parent_id", |b| {
        b.iter(|| interner.prefix_stats_by_parent_id(black_box(parent_id), black_box(appended)))
    });
}

fn bench_vocab_accessors(c: &mut Criterion) {
    let interner = Interner::from_text(SAMPLE_TEXT);

    c.bench_function("interner_vocabulary", |b| b.iter(|| interner.vocabulary()));

    c.bench_function("interner_vocab_size", |b| b.iter(|| interner.vocab_size()));

    c.bench_function("interner_version", |b| b.iter(|| interner.version()));
}

criterion_group!(
    benches,
    bench_from_text,
    bench_from_text_large,
    bench_add_text,
    bench_intersect_simple,
    bench_intersect_complex,
    bench_intersect_many_forbidden,
    bench_intersect_e_txt_large_vocab,
    bench_intersect_e_txt_large_vocab_prefix_ids,
    bench_merge,
    bench_completions_for_prefix,
    bench_impacted_keys,
    bench_completions_equal_up_to_vocab,
    bench_all_completions_equal_up_to_vocab,
    bench_string_for_index,
    bench_prefix_stats_by_parent_id,
    bench_vocab_accessors,
);
criterion_main!(benches);
