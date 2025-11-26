use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::splitter::Splitter;

const SHORT_TEXT: &str = "The quick brown fox jumps over the lazy dog.";

const MEDIUM_TEXT: &str = "The quick brown fox jumps over the lazy dog. \
    A journey of a thousand miles begins with a single step. \
    To be or not to be, that is the question. \
    All that glitters is not gold.";

const LONG_TEXT: &str = "The quick brown fox jumps over the lazy dog. \
    A journey of a thousand miles begins with a single step. \
    To be or not to be, that is the question. \
    All that glitters is not gold. \
    Where there is a will, there is a way. \
    Actions speak louder than words. \
    The early bird catches the worm. \
    Better late than never. \
    Practice makes perfect. \
    Knowledge is power. \
    Time flies when you're having fun. \
    The pen is mightier than the sword. \
    Beauty is in the eye of the beholder. \
    Don't count your chickens before they hatch.";

fn bench_new(c: &mut Criterion) {
    c.bench_function("splitter_new", |b| {
        b.iter(|| Splitter::new())
    });
}

fn bench_vocabulary_short(c: &mut Criterion) {
    let splitter = Splitter::new();
    
    c.bench_function("splitter_vocabulary_short", |b| {
        b.iter(|| splitter.vocabulary(black_box(SHORT_TEXT)))
    });
}

fn bench_vocabulary_medium(c: &mut Criterion) {
    let splitter = Splitter::new();
    
    c.bench_function("splitter_vocabulary_medium", |b| {
        b.iter(|| splitter.vocabulary(black_box(MEDIUM_TEXT)))
    });
}

fn bench_vocabulary_long(c: &mut Criterion) {
    let splitter = Splitter::new();
    
    c.bench_function("splitter_vocabulary_long", |b| {
        b.iter(|| splitter.vocabulary(black_box(LONG_TEXT)))
    });
}

fn bench_phrases_short(c: &mut Criterion) {
    let splitter = Splitter::new();
    
    c.bench_function("splitter_phrases_short", |b| {
        b.iter(|| splitter.phrases(black_box(SHORT_TEXT)))
    });
}

fn bench_phrases_medium(c: &mut Criterion) {
    let splitter = Splitter::new();
    
    c.bench_function("splitter_phrases_medium", |b| {
        b.iter(|| splitter.phrases(black_box(MEDIUM_TEXT)))
    });
}

fn bench_phrases_long(c: &mut Criterion) {
    let splitter = Splitter::new();
    
    c.bench_function("splitter_phrases_long", |b| {
        b.iter(|| splitter.phrases(black_box(LONG_TEXT)))
    });
}

criterion_group!(
    benches,
    bench_new,
    bench_vocabulary_short,
    bench_vocabulary_medium,
    bench_vocabulary_long,
    bench_phrases_short,
    bench_phrases_medium,
    bench_phrases_long,
);
criterion_main!(benches);
