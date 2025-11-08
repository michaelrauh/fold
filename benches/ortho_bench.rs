use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::ortho::Ortho;

fn bench_ortho_new(c: &mut Criterion) {
    c.bench_function("ortho_new", |b| b.iter(|| Ortho::new()));
}

fn bench_ortho_add_simple(c: &mut Criterion) {
    let ortho = Ortho::new();
    c.bench_function("ortho_add_simple", |b| {
        b.iter(|| ortho.add(black_box(10)))
    });
}

fn bench_ortho_add_multiple(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho1 = ortho.add(1)[0].clone();
    c.bench_function("ortho_add_multiple", |b| {
        b.iter(|| ortho1.add(black_box(2)))
    });
}

fn bench_ortho_id(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    let ortho = ortho.add(2)[0].clone();
    c.bench_function("ortho_id", |b| b.iter(|| ortho.id()));
}

fn bench_ortho_add_shape_expansion(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    let ortho = ortho.add(2)[0].clone();
    let ortho = ortho.add(3)[0].clone();
    let ortho = ortho.add(4)[0].clone();
    c.bench_function("ortho_add_shape_expansion", |b| {
        b.iter(|| ortho.add(black_box(5)))
    });
}

criterion_group!(
    benches,
    bench_ortho_new,
    bench_ortho_add_simple,
    bench_ortho_add_multiple,
    bench_ortho_id,
    bench_ortho_add_shape_expansion,
);
criterion_main!(benches);
