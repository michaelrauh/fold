use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::{process_text, process_text_parallel};
use std::collections::{HashMap, HashSet};

fn bench_process_text_sequential(c: &mut Criterion) {
    let text = "the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy cat";
    
    c.bench_function("process_text_sequential", |b| {
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

fn bench_process_text_parallel(c: &mut Criterion) {
    let text = "the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy cat";
    
    c.bench_function("process_text_parallel", |b| {
        b.iter(|| {
            let mut seen_ids = HashSet::new();
            let mut optimal_ortho = None;
            let mut frontier = HashSet::new();
            let mut frontier_orthos_saved = HashMap::new();
            
            let _ = process_text_parallel(
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

fn bench_process_text_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("process_text_comparison");
    
    let text = "the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy cat the big red dog runs fast";
    
    group.bench_with_input(BenchmarkId::new("sequential", "medium"), &text, |b, &text| {
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
    
    group.bench_with_input(BenchmarkId::new("parallel", "medium"), &text, |b, &text| {
        b.iter(|| {
            let mut seen_ids = HashSet::new();
            let mut optimal_ortho = None;
            let mut frontier = HashSet::new();
            let mut frontier_orthos_saved = HashMap::new();
            
            let _ = process_text_parallel(
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
    
    group.finish();
}

criterion_group!(
    benches,
    bench_process_text_sequential,
    bench_process_text_parallel,
    bench_process_text_comparison,
);
criterion_main!(benches);
