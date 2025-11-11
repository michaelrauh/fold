use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::ortho::Ortho;

/// Benchmark ortho expansion in detail
/// Lower sample count for faster execution

fn bench_expansion_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("expansion_paths");
    group.sample_size(20); // Reduced from default 100
    
    let version = 1;
    
    // Case 1: Base expansion (2x2 -> up)
    let base_ortho = Ortho::new(version);
    let base_ortho = base_ortho.add(0, version)[0].clone();
    let base_ortho = base_ortho.add(1, version)[0].clone();
    let base_ortho = base_ortho.add(2, version)[0].clone();
    // base_ortho is now [2,2] with 3 filled, next add triggers base expansion
    
    group.bench_function("base_expand_up", |b| {
        b.iter(|| {
            black_box(&base_ortho).add(black_box(3), version)
        });
    });
    
    // Case 2: Non-base expansion (over)
    let non_base = Ortho::new(version);
    let non_base = non_base.add(0, version)[0].clone();
    let non_base = non_base.add(1, version)[0].clone();
    let non_base = non_base.add(2, version)[0].clone();
    let non_base = non_base.add(3, version)[0].clone();
    // This creates a larger ortho, e.g. [3,2] or [2,3]
    let non_base = non_base.add(4, version)[0].clone();
    let non_base = non_base.add(5, version)[0].clone();
    // Fill until one empty left to trigger expansion
    let mut expansions_test = non_base.clone();
    let dims = expansions_test.dims().clone();
    let capacity: usize = dims.iter().product();
    let filled = expansions_test.payload().iter().filter(|x| x.is_some()).count();
    let empty_count = capacity - filled;
    
    if empty_count > 1 {
        // Fill more to get to last slot
        for i in 6..(6 + empty_count - 1) {
            let children = expansions_test.add(i, version);
            if !children.is_empty() {
                expansions_test = children[0].clone();
            }
        }
    }
    
    group.bench_function("non_base_expand_over", |b| {
        b.iter(|| {
            black_box(&expansions_test).add(black_box(100), version)
        });
    });
    
    // Case 3: Simple add (no expansion)
    let simple = Ortho::new(version);
    let simple = simple.add(0, version)[0].clone();
    
    group.bench_function("simple_add_no_expansion", |b| {
        b.iter(|| {
            black_box(&simple).add(black_box(1), version)
        });
    });
    
    // Case 4: Middle add (2x2 special case)
    let middle_case = Ortho::new(version);
    let middle_case = middle_case.add(0, version)[0].clone();
    
    group.bench_function("middle_add_2x2_reorder", |b| {
        b.iter(|| {
            black_box(&middle_case).add(black_box(1), version)
        });
    });
    
    group.finish();
}

fn bench_expansion_by_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("expansion_by_depth");
    group.sample_size(20); // Reduced from default 100
    
    let version = 1;
    
    // Build orthos at different depths and measure expansion cost
    for depth in [0, 1, 2, 3, 4] {
        let mut ortho = Ortho::new(version);
        
        // Add values to reach a certain depth/complexity
        for i in 0..depth {
            let children = ortho.add(i, version);
            if !children.is_empty() {
                ortho = children[0].clone();
            }
        }
        
        // Get to the last slot before expansion
        loop {
            let dims = ortho.dims().clone();
            let capacity: usize = dims.iter().product();
            let filled = ortho.payload().iter().filter(|x| x.is_some()).count();
            let empty_count = capacity - filled;
            
            if empty_count == 1 {
                break; // Next add will expand
            }
            
            let children = ortho.add(filled + 100, version);
            if !children.is_empty() {
                ortho = children[0].clone();
            } else {
                break;
            }
        }
        
        let final_ortho = ortho;
        
        group.bench_with_input(
            BenchmarkId::new("expansion", depth),
            &final_ortho,
            |b, ortho| {
                b.iter(|| {
                    black_box(ortho).add(black_box(9999), version)
                });
            },
        );
    }
    
    group.finish();
}

fn bench_expansion_components(c: &mut Criterion) {
    let mut group = c.benchmark_group("expansion_components");
    group.sample_size(20);
    
    let version = 1;
    
    // Build an ortho ready for expansion
    let ortho = Ortho::new(version);
    let ortho = ortho.add(0, version)[0].clone();
    let ortho = ortho.add(1, version)[0].clone();
    let ortho = ortho.add(2, version)[0].clone();
    // This ortho will expand on next add
    
    // Benchmark: get_insert_position
    group.bench_function("get_insert_position", |b| {
        b.iter(|| {
            // Access through public API
            let children = black_box(&ortho).add(black_box(3), version);
            black_box(children)
        });
    });
    
    // Benchmark: payload cloning in expansion
    let payload_clone = ortho.payload().clone();
    group.bench_function("payload_clone", |b| {
        b.iter(|| {
            black_box(&payload_clone).clone()
        });
    });
    
    // Benchmark: dims cloning
    let dims_clone = ortho.dims().clone();
    group.bench_function("dims_clone", |b| {
        b.iter(|| {
            black_box(&dims_clone).clone()
        });
    });
    
    group.finish();
}

criterion_group!(
    benches,
    bench_expansion_paths,
    bench_expansion_by_depth,
    bench_expansion_components,
);
criterion_main!(benches);
