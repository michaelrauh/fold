use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::interner::Interner;
use fixedbitset::FixedBitSet;

fn bench_interner_intersect_edge_cases(c: &mut Criterion) {
    let text = "The quick brown fox jumps over the lazy dog. \
                A lazy cat sleeps near the dog. \
                The fox runs through the forest.";
    let interner = Interner::from_text(text);
    
    let mut group = c.benchmark_group("interner_intersect_cases");
    
    group.bench_function("empty_required_empty_forbidden", |b| {
        b.iter(|| {
            interner.intersect(black_box(&vec![]), black_box(&vec![]))
        });
    });
    
    group.bench_function("single_required_no_forbidden", |b| {
        b.iter(|| {
            interner.intersect(black_box(&vec![vec![0]]), black_box(&vec![]))
        });
    });
    
    group.bench_function("multiple_required_no_forbidden", |b| {
        b.iter(|| {
            interner.intersect(
                black_box(&vec![vec![0], vec![1], vec![0, 1]]),
                black_box(&vec![])
            )
        });
    });
    
    group.bench_function("multiple_required_with_forbidden", |b| {
        b.iter(|| {
            interner.intersect(
                black_box(&vec![vec![0], vec![1]]),
                black_box(&vec![5, 6, 7])
            )
        });
    });
    
    group.finish();
}

fn bench_interner_vocabulary_access(c: &mut Criterion) {
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(100);
    let interner = Interner::from_text(&text);
    
    c.bench_function("interner_vocabulary_len", |b| {
        b.iter(|| {
            black_box(&interner).vocabulary().len()
        });
    });
    
    c.bench_function("interner_string_for_index", |b| {
        b.iter(|| {
            interner.string_for_index(black_box(0))
        });
    });
}

fn bench_interner_from_text_complexity(c: &mut Criterion) {
    let mut group = c.benchmark_group("interner_from_text_complexity");
    
    for vocab_size in [10, 50, 100] {
        let text = (0..vocab_size)
            .map(|i| format!("word{} follows word{} in sequence.", i, i + 1))
            .collect::<Vec<_>>()
            .join(" ");
        
        group.bench_with_input(
            BenchmarkId::new("vocab_size", vocab_size),
            &text,
            |b, text| {
                b.iter(|| {
                    Interner::from_text(black_box(text))
                });
            },
        );
    }
    
    group.finish();
}

fn bench_prefix_to_completions_building(c: &mut Criterion) {
    let mut group = c.benchmark_group("prefix_building");
    
    for phrase_count in [10, 50, 100] {
        let text = (0..phrase_count)
            .map(|i| format!("phrase {} contains multiple words here.", i))
            .collect::<Vec<_>>()
            .join(" ");
        
        group.bench_with_input(
            BenchmarkId::new("phrases", phrase_count),
            &text,
            |b, text| {
                b.iter(|| {
                    Interner::from_text(black_box(text))
                });
            },
        );
    }
    
    group.finish();
}

fn bench_fixedbitset_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("fixedbitset_ops");
    
    for size in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::new("create_and_set", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut fbs = FixedBitSet::with_capacity(size);
                    for i in 0..size / 2 {
                        fbs.insert(i);
                    }
                    black_box(fbs)
                });
            },
        );
        
        group.bench_with_input(
            BenchmarkId::new("intersect", size),
            &size,
            |b, &size| {
                let mut fbs1 = FixedBitSet::with_capacity(size);
                let mut fbs2 = FixedBitSet::with_capacity(size);
                for i in 0..size / 2 {
                    fbs1.insert(i);
                }
                for i in size / 4..size * 3 / 4 {
                    fbs2.insert(i);
                }
                
                b.iter(|| {
                    let mut result = fbs1.clone();
                    result.intersect_with(&fbs2);
                    black_box(result)
                });
            },
        );
    }
    
    group.finish();
}

fn bench_interner_version_comparison(c: &mut Criterion) {
    let text1 = "The cat sat on the mat. The dog ran fast.";
    let text2 = "The bird flew high. The sky was blue.";
    
    let int1 = Interner::from_text(text1);
    let int2 = int1.add_text(text2);
    
    c.bench_function("impacted_keys_full", |b| {
        b.iter(|| {
            black_box(&int1).impacted_keys(black_box(&int2))
        });
    });
}

fn bench_interner_serialization(c: &mut Criterion) {
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(50);
    let interner = Interner::from_text(&text);
    
    c.bench_function("interner_encode", |b| {
        b.iter(|| {
            bincode::encode_to_vec(black_box(&interner), bincode::config::standard()).unwrap()
        });
    });
    
    let encoded = bincode::encode_to_vec(&interner, bincode::config::standard()).unwrap();
    
    c.bench_function("interner_decode", |b| {
        b.iter(|| {
            bincode::decode_from_slice::<Interner, _>(
                black_box(&encoded),
                bincode::config::standard()
            ).unwrap()
        });
    });
}

criterion_group!(
    benches,
    bench_interner_intersect_edge_cases,
    bench_interner_vocabulary_access,
    bench_interner_from_text_complexity,
    bench_prefix_to_completions_building,
    bench_fixedbitset_operations,
    bench_interner_version_comparison,
    bench_interner_serialization,
);
criterion_main!(benches);
