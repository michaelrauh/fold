use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::spatial::{
    capacity, expand_over, expand_up, get_axis_positions, get_location_to_index, get_requirements,
    is_base,
};

fn bench_get_requirements(c: &mut Criterion) {
    c.bench_function("get_requirements", |b| {
        b.iter(|| get_requirements(black_box(3), black_box(&[2, 2]), black_box(None)))
    });
}

fn bench_is_base(c: &mut Criterion) {
    c.bench_function("is_base", |b| b.iter(|| is_base(black_box(&[2, 2, 2]))));
}

fn bench_expand_up(c: &mut Criterion) {
    c.bench_function("expand_up", |b| {
        b.iter(|| expand_up(black_box(&[2, 2]), black_box(1)))
    });
}

fn bench_expand_over(c: &mut Criterion) {
    c.bench_function("expand_over", |b| {
        b.iter(|| expand_over(black_box(&[3, 2])))
    });
}

fn bench_capacity(c: &mut Criterion) {
    c.bench_function("capacity_2d", |b| b.iter(|| capacity(black_box(&[3, 3]))));

    c.bench_function("capacity_3d", |b| {
        b.iter(|| capacity(black_box(&[2, 3, 4])))
    });
}

fn bench_get_axis_positions(c: &mut Criterion) {
    c.bench_function("get_axis_positions_2d", |b| {
        b.iter(|| get_axis_positions(black_box(&[3, 3])))
    });

    c.bench_function("get_axis_positions_3d", |b| {
        b.iter(|| get_axis_positions(black_box(&[2, 3, 4])))
    });
}

fn bench_get_location_to_index(c: &mut Criterion) {
    c.bench_function("get_location_to_index_2d", |b| {
        b.iter(|| get_location_to_index(black_box(&[3, 3])))
    });

    c.bench_function("get_location_to_index_3d", |b| {
        b.iter(|| get_location_to_index(black_box(&[2, 3, 4])))
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
                get_requirements(black_box(i), black_box(&[2, 2]), black_box(None));
            }
        })
    });
}

criterion_group!(
    benches,
    bench_get_requirements,
    bench_is_base,
    bench_expand_up,
    bench_expand_over,
    bench_capacity,
    bench_get_axis_positions,
    bench_get_location_to_index,
    bench_cached_vs_uncached
);
criterion_main!(benches);
