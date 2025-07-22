use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fold::spatial::{get_requirements, get_capacity, is_base, expand_up, expand_over};

fn bench_get_requirements(c: &mut Criterion) {
    c.bench_function("get_requirements", |b| {
        b.iter(|| {
            get_requirements(black_box(3), black_box(&[2, 2]))
        })
    });
}

fn bench_get_capacity(c: &mut Criterion) {
    c.bench_function("get_capacity", |b| {
        b.iter(|| {
            get_capacity(black_box(&[3, 3, 2]))
        })
    });
}

fn bench_is_base(c: &mut Criterion) {
    c.bench_function("is_base", |b| {
        b.iter(|| {
            is_base(black_box(&[2, 2, 2]))
        })
    });
}

fn bench_expand_up(c: &mut Criterion) {
    c.bench_function("expand_up", |b| {
        b.iter(|| {
            expand_up(black_box(&[3, 2]), black_box(1))
        })
    });
}

fn bench_expand_over(c: &mut Criterion) {
    c.bench_function("expand_over", |b| {
        b.iter(|| {
            expand_over(black_box(&[3, 2]))
        })
    });
}

fn bench_cached_vs_uncached(c: &mut Criterion) {
    // This benchmark calls the same function multiple times to demonstrate caching benefits
    c.bench_function("repeated_calls_expand_over", |b| {
        b.iter(|| {
            for _ in 0..10 {
                expand_over(black_box(&[3, 2]));
            }
        })
    });
    
    c.bench_function("repeated_calls_get_requirements", |b| {
        b.iter(|| {
            for i in 0..4 {
                get_requirements(black_box(i), black_box(&[2, 2]));
            }
        })
    });
}

criterion_group!(
    benches,
    bench_get_requirements,
    bench_get_capacity,
    bench_is_base,
    bench_expand_up,
    bench_expand_over,
    bench_cached_vs_uncached
);
criterion_main!(benches);