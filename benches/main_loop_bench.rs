use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::{
    disk_backed_queue::DiskBackedQueue,
    interner::Interner,
    ortho::Ortho,
    seen_tracker::SeenTracker,
};
use tempfile::TempDir;

fn bench_worker_loop_iteration(c: &mut Criterion) {
    let text = "The cat sat on the mat. The dog ran in the park. Birds fly in the sky.";
    let interner = Interner::from_text(text);
    let version = interner.version();
    
    let ortho = Ortho::new(version);
    let ortho = ortho.add(0, version)[0].clone();
    let ortho = ortho.add(1, version)[0].clone();
    
    c.bench_function("worker_loop_single_iteration", |b| {
        b.iter(|| {
            let (forbidden, required) = black_box(&ortho).get_requirements();
            let completions = interner.intersect(&required, &forbidden);
            
            let mut children_count = 0;
            for completion in completions {
                let children = ortho.add(completion, version);
                children_count += children.len();
            }
            
            black_box(children_count)
        });
    });
}

fn bench_interner_intersect_varying_requirements(c: &mut Criterion) {
    let text = "The quick brown fox jumps over the lazy dog. \
                The dog was sleeping peacefully. \
                A cat walked by slowly.";
    let interner = Interner::from_text(text);
    
    let mut group = c.benchmark_group("interner_intersect");
    
    let version = interner.version();
    let ortho0 = Ortho::new(version);
    let ortho1 = ortho0.add(0, version)[0].clone();
    let ortho2 = ortho1.add(1, version)[0].clone();
    let ortho3 = ortho2.add(2, version)[0].clone();
    
    for (depth, ortho) in [(0, &ortho0), (1, &ortho1), (2, &ortho2), (3, &ortho3)] {
        group.bench_with_input(
            BenchmarkId::new("depth", depth),
            ortho,
            |b, ortho| {
                b.iter(|| {
                    let (forbidden, required) = ortho.get_requirements();
                    interner.intersect(black_box(&required), black_box(&forbidden))
                });
            },
        );
    }
    
    group.finish();
}

fn bench_ortho_child_generation(c: &mut Criterion) {
    let version = 1;
    
    let mut group = c.benchmark_group("ortho_child_gen");
    
    let ortho0 = Ortho::new(version);
    let ortho1 = ortho0.add(0, version)[0].clone();
    let ortho2 = ortho1.add(1, version)[0].clone();
    let ortho3 = ortho2.add(2, version)[0].clone();
    
    for (depth, ortho) in [(0, &ortho0), (1, &ortho1), (2, &ortho2), (3, &ortho3)] {
        group.bench_with_input(
            BenchmarkId::new("depth", depth),
            ortho,
            |b, ortho| {
                b.iter(|| {
                    ortho.add(black_box(10), version)
                });
            },
        );
    }
    
    group.finish();
}

fn bench_seen_tracker_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("seen_tracker");
    
    for size in [1000, 10000, 100000] {
        group.bench_with_input(
            BenchmarkId::new("insert_and_check", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut tracker = SeenTracker::new(size);
                    for i in 0..size {
                        tracker.insert(i);
                    }
                    let mut found = 0;
                    for i in 0..size {
                        if tracker.contains(&i) {
                            found += 1;
                        }
                    }
                    black_box(found)
                });
            },
        );
    }
    
    group.finish();
}

fn bench_disk_backed_queue_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("disk_backed_queue");
    
    for buffer_size in [100, 500, 1000] {
        group.bench_with_input(
            BenchmarkId::new("push_pop", buffer_size),
            &buffer_size,
            |b, &buffer_size| {
                b.iter(|| {
                    let temp_dir = TempDir::new().unwrap();
                    let queue_path = temp_dir.path().join("queue");
                    let mut queue = DiskBackedQueue::new_from_path(
                        queue_path.to_str().unwrap(),
                        buffer_size
                    ).unwrap();
                    
                    for i in 0..buffer_size * 2 {
                        queue.push(Ortho::new(i)).unwrap();
                    }
                    
                    let mut count = 0;
                    while queue.pop().unwrap().is_some() {
                        count += 1;
                    }
                    
                    black_box(count)
                });
            },
        );
    }
    
    group.finish();
}

fn bench_interner_add_text(c: &mut Criterion) {
    let initial_text = "The quick brown fox jumps over the lazy dog.";
    let interner = Interner::from_text(initial_text);
    
    let new_text = "A bird flew over the mountains and valleys.";
    
    c.bench_function("interner_add_text", |b| {
        b.iter(|| {
            interner.add_text(black_box(new_text))
        });
    });
}

fn bench_impacted_keys_detection(c: &mut Criterion) {
    let text1 = "The cat sat on the mat.";
    let text2 = "The cat sat on the mat. The dog ran.";
    
    let int1 = Interner::from_text(text1);
    let int2 = int1.add_text(text2);
    
    c.bench_function("impacted_keys_detection", |b| {
        b.iter(|| {
            black_box(&int1).impacted_keys(black_box(&int2))
        });
    });
}

criterion_group!(
    benches,
    bench_worker_loop_iteration,
    bench_interner_intersect_varying_requirements,
    bench_ortho_child_generation,
    bench_seen_tracker_operations,
    bench_disk_backed_queue_operations,
    bench_interner_add_text,
    bench_impacted_keys_detection,
);
criterion_main!(benches);
