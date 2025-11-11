use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::splitter::Splitter;

fn bench_splitter_vocabulary(c: &mut Criterion) {
    let mut group = c.benchmark_group("splitter_vocabulary");
    
    for size in [100, 500, 1000] {
        let text = (0..size)
            .map(|i| format!("Sentence {} with word{}.", i, i))
            .collect::<Vec<_>>()
            .join(" ");
        
        group.bench_with_input(
            BenchmarkId::new("size", size),
            &text,
            |b, text| {
                let splitter = Splitter::new();
                b.iter(|| {
                    splitter.vocabulary(black_box(text))
                });
            },
        );
    }
    
    group.finish();
}

fn bench_splitter_phrases(c: &mut Criterion) {
    let mut group = c.benchmark_group("splitter_phrases");
    
    for size in [50, 100, 200] {
        let text = (0..size)
            .map(|i| format!("Phrase number {} contains some words here.", i))
            .collect::<Vec<_>>()
            .join(" ");
        
        group.bench_with_input(
            BenchmarkId::new("size", size),
            &text,
            |b, text| {
                let splitter = Splitter::new();
                b.iter(|| {
                    splitter.phrases(black_box(text))
                });
            },
        );
    }
    
    group.finish();
}

fn bench_splitter_split_sentences(c: &mut Criterion) {
    let text = "Sentence one here. Sentence two there. Sentence three everywhere! \
                Sentence four now? Sentence five later; Sentence six always.";
    
    let splitter = Splitter::new();
    
    c.bench_function("split_into_sentences", |b| {
        b.iter(|| {
            // Access private method through public API
            splitter.vocabulary(black_box(text))
        });
    });
}

fn bench_splitter_clean_sentence(c: &mut Criterion) {
    let sentence = "The Quick Brown FOX jumps over the LAZY dog's tail!!!";
    
    let splitter = Splitter::new();
    
    c.bench_function("clean_and_lowercase", |b| {
        b.iter(|| {
            splitter.vocabulary(black_box(sentence))
        });
    });
}

fn bench_splitter_substring_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("substring_generation");
    
    for word_count in [5, 10, 20] {
        let text = (0..word_count)
            .map(|i| format!("word{}", i))
            .collect::<Vec<_>>()
            .join(" ");
        
        group.bench_with_input(
            BenchmarkId::new("words", word_count),
            &text,
            |b, text| {
                let splitter = Splitter::new();
                b.iter(|| {
                    splitter.phrases(black_box(text))
                });
            },
        );
    }
    
    group.finish();
}

fn bench_splitter_paragraph_handling(c: &mut Criterion) {
    let text = "First paragraph here.\n\nSecond paragraph there.\n\nThird paragraph everywhere.";
    
    let splitter = Splitter::new();
    
    c.bench_function("paragraph_splitting", |b| {
        b.iter(|| {
            splitter.phrases(black_box(text))
        });
    });
}

criterion_group!(
    benches,
    bench_splitter_vocabulary,
    bench_splitter_phrases,
    bench_splitter_split_sentences,
    bench_splitter_clean_sentence,
    bench_splitter_substring_generation,
    bench_splitter_paragraph_handling,
);
criterion_main!(benches);
