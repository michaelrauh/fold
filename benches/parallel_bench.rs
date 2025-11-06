use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::process_text;
use std::collections::{HashMap, HashSet};

fn bench_process_text_small(c: &mut Criterion) {
    let text = "the quick brown fox jumps over the lazy dog";
    
    c.bench_function("process_text_small", |b| {
        b.iter(|| {
            let mut seen_ids = HashSet::new();
            let mut optimal_ortho = None;
            let mut frontier = HashSet::new();
            let mut frontier_orthos_saved = HashMap::new();
            
            let _ = process_text(
                black_box(text),
                None,
                &mut seen_ids,
                &mut optimal_ortho,
                &mut frontier,
                &mut frontier_orthos_saved,
                |_| Ok(()),
            );
        })
    });
}

fn bench_process_text_medium(c: &mut Criterion) {
    let text = "the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy cat the big red dog runs fast";
    
    c.bench_function("process_text_medium", |b| {
        b.iter(|| {
            let mut seen_ids = HashSet::new();
            let mut optimal_ortho = None;
            let mut frontier = HashSet::new();
            let mut frontier_orthos_saved = HashMap::new();
            
            let _ = process_text(
                black_box(text),
                None,
                &mut seen_ids,
                &mut optimal_ortho,
                &mut frontier,
                &mut frontier_orthos_saved,
                |_| Ok(()),
            );
        })
    });
}

fn bench_process_text_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("process_text_workloads");
    
    let small_text = "the quick brown fox";
    let medium_text = "the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy cat";
    let large_text = "the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy cat the big red dog runs very fast through the tall green grass";
    
    for (name, text) in [("small", small_text), ("medium", medium_text), ("large", large_text)] {
        group.bench_with_input(BenchmarkId::from_parameter(name), &text, |b, &text| {
            b.iter(|| {
                let mut seen_ids = HashSet::new();
                let mut optimal_ortho = None;
                let mut frontier = HashSet::new();
                let mut frontier_orthos_saved = HashMap::new();
                
                let _ = process_text(
                    black_box(text),
                    None,
                    &mut seen_ids,
                    &mut optimal_ortho,
                    &mut frontier,
                    &mut frontier_orthos_saved,
                    |_| Ok(()),
                );
            })
        });
    }
    
    group.finish();
}

criterion_group!(
    benches,
    bench_process_text_small,
    bench_process_text_medium,
    bench_process_text_workloads,
);
criterion_main!(benches);
