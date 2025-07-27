use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::interner::Interner;

fn bench_from_text(c: &mut Criterion) {
    c.bench_function("interner_from_text_short", |b| {
        b.iter(|| {
            Interner::from_text(black_box(
                "a b c d e f g h i j k l m n o p q r s t u v w x y z",
            ))
        })
    });
    c.bench_function("interner_from_text_long", |b| {
        // Reduced input size for faster benchmarking
        let text = (0..50)
            .map(|i| format!("word{}", i))
            .collect::<Vec<_>>()
            .join(" ");
        b.iter(|| Interner::from_text(black_box(&text)))
    });
}

fn bench_add_text(c: &mut Criterion) {
    let base = Interner::from_text("a b c d");
    c.bench_function("interner_add_text_small", |b| {
        b.iter(|| base.add_text(black_box("e f g h")))
    });
    let base_large = Interner::from_text(
        &(0..100)
            .map(|i| format!("w{}", i))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let add_large = (100..200)
        .map(|i| format!("w{}", i))
        .collect::<Vec<_>>()
        .join(" ");
    c.bench_function("interner_add_text_large", |b| {
        b.iter(|| base_large.add_text(black_box(&add_large)))
    });
}

use fold::interner::InternerHolder;

fn bench_interner_intersect(c: &mut Criterion) {
    // Create a realistic interner with a moderate vocabulary
    let holder = InternerHolder::from_text(
        "the quick brown fox jumps over the lazy dog and then runs away"
    );
    let interner = holder.get_latest().clone();
    // Use the vocabulary indices for required and forbidden
    let vocab: Vec<_> = (0..interner.vocabulary().len()).collect();
    let required: Vec<Vec<usize>> = vec![vocab.iter().cloned().take(3).collect()];
    let forbidden: Vec<usize> = vocab.iter().cloned().skip(3).take(2).collect();
    c.bench_function("interner_intersect_realistic", |b| {
        b.iter(|| {
            let _ = interner.intersect(black_box(required.as_slice()), black_box(forbidden.as_slice()));
        })
    });
}

criterion_group!(benches, bench_from_text, bench_add_text, bench_interner_intersect);
criterion_main!(benches);
