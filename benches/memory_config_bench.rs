use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::memory_config::MemoryConfig;

fn bench_calculate_small_interner(c: &mut Criterion) {
    c.bench_function("memory_config_calculate_small_interner", |b| {
        b.iter(|| MemoryConfig::calculate(black_box(1024), black_box(0)))
    });
}

fn bench_calculate_medium_interner(c: &mut Criterion) {
    c.bench_function("memory_config_calculate_medium_interner", |b| {
        b.iter(|| MemoryConfig::calculate(black_box(102400), black_box(0)))
    });
}

fn bench_calculate_large_interner(c: &mut Criterion) {
    c.bench_function("memory_config_calculate_large_interner", |b| {
        b.iter(|| MemoryConfig::calculate(black_box(1048576), black_box(0)))
    });
}

fn bench_calculate_with_results(c: &mut Criterion) {
    c.bench_function("memory_config_calculate_with_results", |b| {
        b.iter(|| MemoryConfig::calculate(black_box(102400), black_box(10000)))
    });
}

fn bench_calculate_with_many_results(c: &mut Criterion) {
    c.bench_function("memory_config_calculate_with_many_results", |b| {
        b.iter(|| MemoryConfig::calculate(black_box(102400), black_box(1000000)))
    });
}

fn bench_default_config(c: &mut Criterion) {
    c.bench_function("memory_config_default_config", |b| {
        b.iter(|| MemoryConfig::default_config())
    });
}

criterion_group!(
    benches,
    bench_calculate_small_interner,
    bench_calculate_medium_interner,
    bench_calculate_large_interner,
    bench_calculate_with_results,
    bench_calculate_with_many_results,
    bench_default_config,
);
criterion_main!(benches);
