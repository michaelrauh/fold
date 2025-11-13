use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::{interner::Interner, ortho::Ortho, disk_backed_queue::DiskBackedQueue, memory_config::MemoryConfig};
use tempfile::TempDir;
use std::collections::HashSet;

/// Benchmark the steps involved in finding impacted orthos and re-queuing them
/// Lower sample count for faster execution

fn bench_impacted_keys_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("impacted_detection");
    group.sample_size(20); // Reduced from default 100
    
    // Create two interners with differences
    let text1 = "The cat sat on the mat. The dog ran fast.";
    let text2 = "The bird flew high. The sky was blue.";
    
    let int1 = Interner::from_text(text1);
    let int2 = int1.add_text(text2);
    
    group.bench_function("impacted_keys_calculation", |b| {
        b.iter(|| {
            black_box(&int1).impacted_keys(black_box(&int2))
        });
    });
    
    group.finish();
}

fn bench_requirement_phrase_extraction(c: &mut Criterion) {
    let mut group = c.benchmark_group("requirement_phrases");
    group.sample_size(20);
    
    let version = 1;
    
    // Create orthos at different depths
    for depth in [0, 2, 4, 6] {
        let mut ortho = Ortho::new(version);
        
        for i in 0..depth {
            let children = ortho.add(i, version);
            if !children.is_empty() {
                ortho = children[0].clone();
            }
        }
        
        group.bench_with_input(
            BenchmarkId::new("get_requirement_phrases", depth),
            &ortho,
            |b, ortho| {
                b.iter(|| {
                    black_box(ortho).get_requirement_phrases()
                });
            },
        );
    }
    
    group.finish();
}

fn bench_impacted_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("impacted_check");
    group.sample_size(20);
    
    // Create sample requirement phrases (as indices)
    let phrases = vec![
        vec![0, 1],
        vec![1, 2],
        vec![0, 3],
    ];
    
    // Create impacted keys set (as indices)
    let impacted_keys: HashSet<Vec<usize>> = vec![
        vec![0, 4],
        vec![1, 2],
    ].into_iter().collect();
    
    group.bench_function("check_phrase_overlap", |b| {
        b.iter(|| {
            let is_impacted = black_box(&phrases).iter()
                .any(|phrase| black_box(&impacted_keys).contains(phrase));
            black_box(is_impacted)
        });
    });
    
    group.finish();
}

fn bench_queue_operations_during_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_scan_ops");
    group.sample_size(20);
    
    let version = 1;
    let temp_dir = TempDir::new().unwrap();
    let queue_path = temp_dir.path().join("scan_queue");
    let memory_config = MemoryConfig::default_config();
    
    // Create queue with orthos
    let mut results_queue = DiskBackedQueue::new_from_path(
        queue_path.to_str().unwrap(),
        memory_config.queue_buffer_size
    ).unwrap();
    
    for i in 0..50 {
        results_queue.push(Ortho::new(version + i)).unwrap();
    }
    
    group.bench_function("pop_and_check_ortho", |b| {
        b.iter(|| {
            let temp_dir = TempDir::new().unwrap();
            let queue_path = temp_dir.path().join("test_queue");
            let mut test_queue = DiskBackedQueue::new_from_path(
                queue_path.to_str().unwrap(),
                memory_config.queue_buffer_size
            ).unwrap();
            
            // Add some orthos
            for i in 0..10 {
                test_queue.push(Ortho::new(i)).unwrap();
            }
            
            // Pop one
            let ortho = test_queue.pop().unwrap();
            
            // Get requirements (simulating check)
            if let Some(ortho) = ortho {
                let _phrases = ortho.get_requirement_phrases();
            }
        });
    });
    
    group.finish();
}

fn bench_full_impacted_scan_simulation(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_scan_simulation");
    group.sample_size(10); // Even lower for this expensive operation
    
    let version = 1;
    
    // Small scale simulation of scanning process
    for num_results in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("scan_results", num_results),
            &num_results,
            |b, &num_results| {
                b.iter(|| {
                    let temp_dir = TempDir::new().unwrap();
                    let queue_path = temp_dir.path().join("results");
                    let work_queue_path = temp_dir.path().join("work");
                    let temp_path = temp_dir.path().join("temp");
                    let memory_config = MemoryConfig::default_config();
                    
                    // Create results queue
                    let mut results = DiskBackedQueue::new_from_path(
                        queue_path.to_str().unwrap(),
                        memory_config.queue_buffer_size
                    ).unwrap();
                    
                    for i in 0..num_results {
                        results.push(Ortho::new(version + i)).unwrap();
                    }
                    
                    // Simulate scan
                    let mut work_queue = DiskBackedQueue::new_from_path(
                        work_queue_path.to_str().unwrap(),
                        memory_config.queue_buffer_size
                    ).unwrap();
                    
                    let mut temp_results = DiskBackedQueue::new_from_path(
                        temp_path.to_str().unwrap(),
                        memory_config.queue_buffer_size
                    ).unwrap();
                    
                    let impacted_keys: HashSet<Vec<usize>> = vec![
                        vec![0, 1],
                    ].into_iter().collect();
                    
                    let mut requeued = 0;
                    while let Some(ortho) = results.pop().unwrap() {
                        let phrases = ortho.get_requirement_phrases();
                        let is_impacted = phrases.iter().any(|p| impacted_keys.contains(p));
                        
                        if is_impacted {
                            work_queue.push(ortho.clone()).unwrap();
                            requeued += 1;
                        }
                        
                        temp_results.push(ortho).unwrap();
                    }
                    
                    black_box(requeued)
                });
            },
        );
    }
    
    group.finish();
}

criterion_group!(
    benches,
    bench_impacted_keys_detection,
    bench_requirement_phrase_extraction,
    bench_impacted_check,
    bench_queue_operations_during_scan,
    bench_full_impacted_scan_simulation,
);
criterion_main!(benches);
