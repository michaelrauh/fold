use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::ortho::Ortho;
use fold::interner::Interner;

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

fn bench_ortho_get_requirements(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    let ortho = ortho.add(2)[0].clone();
    
    c.bench_function("ortho_get_requirements", |b| {
        b.iter(|| ortho.get_requirements())
    });
}

fn bench_ortho_get_requirement_phrases(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    let ortho = ortho.add(2)[0].clone();
    
    c.bench_function("ortho_get_requirement_phrases", |b| {
        b.iter(|| ortho.get_requirement_phrases())
    });
}

fn bench_ortho_remap(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    let ortho = ortho.add(2)[0].clone();
    let vocab_map: Vec<usize> = (0..100).collect();
    
    c.bench_function("ortho_remap", |b| {
        b.iter(|| ortho.remap(black_box(&vocab_map)))
    });
}

fn bench_ortho_prefixes(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    let ortho = ortho.add(2)[0].clone();
    
    c.bench_function("ortho_prefixes", |b| {
        b.iter(|| ortho.prefixes())
    });
}

fn bench_ortho_prefixes_for_last_filled(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    let ortho = ortho.add(2)[0].clone();
    
    c.bench_function("ortho_prefixes_for_last_filled", |b| {
        b.iter(|| ortho.prefixes_for_last_filled())
    });
}

fn bench_ortho_get_current_position(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    
    c.bench_function("ortho_get_current_position", |b| {
        b.iter(|| ortho.get_current_position())
    });
}

fn bench_ortho_dims(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    
    c.bench_function("ortho_dims", |b| {
        b.iter(|| ortho.dims())
    });
}

fn bench_ortho_payload(c: &mut Criterion) {
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    
    c.bench_function("ortho_payload", |b| {
        b.iter(|| ortho.payload())
    });
}

fn bench_ortho_display(c: &mut Criterion) {
    let interner = Interner::from_text("the quick brown fox");
    let ortho = Ortho::new();
    let ortho = ortho.add(1)[0].clone();
    let ortho = ortho.add(2)[0].clone();
    
    c.bench_function("ortho_display", |b| {
        b.iter(|| ortho.display(black_box(&interner)))
    });
}

criterion_group!(
    benches,
    bench_ortho_new,
    bench_ortho_add_simple,
    bench_ortho_add_multiple,
    bench_ortho_id,
    bench_ortho_add_shape_expansion,
    bench_ortho_get_requirements,
    bench_ortho_get_requirement_phrases,
    bench_ortho_remap,
    bench_ortho_prefixes,
    bench_ortho_prefixes_for_last_filled,
    bench_ortho_get_current_position,
    bench_ortho_dims,
    bench_ortho_payload,
    bench_ortho_display,
);
criterion_main!(benches);
