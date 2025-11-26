use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::interner::Interner;

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
        b.iter(|| interner1.completions_equal_up_to_vocab(black_box(&interner2), black_box(&prefix)))
    });
}

fn bench_all_completions_equal_up_to_vocab(c: &mut Criterion) {
    let interner1 = Interner::from_text(SAMPLE_TEXT);
    let interner2 = interner1.add_text("Additional text here.");
    let prefixes = vec![vec![0], vec![1], vec![2]];
    
    c.bench_function("interner_all_completions_equal_up_to_vocab", |b| {
        b.iter(|| interner1.all_completions_equal_up_to_vocab(black_box(&interner2), black_box(&prefixes)))
    });
}

fn bench_string_for_index(c: &mut Criterion) {
    let interner = Interner::from_text(SAMPLE_TEXT);
    
    c.bench_function("interner_string_for_index", |b| {
        b.iter(|| interner.string_for_index(black_box(5)))
    });
}

fn bench_vocab_accessors(c: &mut Criterion) {
    let interner = Interner::from_text(SAMPLE_TEXT);
    
    c.bench_function("interner_vocabulary", |b| {
        b.iter(|| interner.vocabulary())
    });
    
    c.bench_function("interner_vocab_size", |b| {
        b.iter(|| interner.vocab_size())
    });
    
    c.bench_function("interner_version", |b| {
        b.iter(|| interner.version())
    });
}

criterion_group!(
    benches,
    bench_from_text,
    bench_from_text_large,
    bench_add_text,
    bench_intersect_simple,
    bench_intersect_complex,
    bench_intersect_many_forbidden,
    bench_merge,
    bench_completions_for_prefix,
    bench_impacted_keys,
    bench_completions_equal_up_to_vocab,
    bench_all_completions_equal_up_to_vocab,
    bench_string_for_index,
    bench_vocab_accessors,
);
criterion_main!(benches);
