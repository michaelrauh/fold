use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::{
    disk_backed_queue::DiskBackedQueue,
    interner::Interner,
    memory_config::MemoryConfig,
    ortho::Ortho,
    seen_tracker::SeenTracker,
};
use tempfile::TempDir;

fn calculate_score(ortho: &Ortho) -> (usize, usize) {
    let volume = ortho.dims().iter().map(|x| x.saturating_sub(1)).product::<usize>();
    let fullness = ortho.payload().iter().filter(|x| x.is_some()).count();
    (volume, fullness)
}

fn bench_full_workflow_small(c: &mut Criterion) {
    let text = "The quick brown fox jumps over the lazy dog. \
                The dog was sleeping under a tree. \
                A bird sang in the tree branches.";
    
    c.bench_function("end_to_end_small_text", |b| {
        b.iter(|| {
            let temp_dir = TempDir::new().unwrap();
            let queue_path = temp_dir.path().join("queue");
            let interner = Interner::from_text(black_box(text));
            let version = interner.version();
            let memory_config = MemoryConfig::default_config();
            
            let mut work_queue = DiskBackedQueue::new_from_path(
                queue_path.to_str().unwrap(),
                memory_config.queue_buffer_size
            ).unwrap();
            let mut tracker = SeenTracker::new(1000);
            
            let seed_ortho = Ortho::new(version);
            tracker.insert(seed_ortho.id());
            work_queue.push(seed_ortho.clone()).unwrap();
            
            let mut best = seed_ortho;
            let mut best_score = calculate_score(&best);
            let mut processed_count = 0;
            
            while let Some(ortho) = work_queue.pop().unwrap() {
                processed_count += 1;
                if processed_count > 100 {
                    break;
                }
                
                let (forbidden, required) = ortho.get_requirements();
                let completions = interner.intersect(&required, &forbidden);
                
                for completion in completions {
                    let children = ortho.add(completion, version);
                    for child in children {
                        let child_id = child.id();
                        if !tracker.contains(&child_id) {
                            tracker.insert(child_id);
                            let score = calculate_score(&child);
                            if score > best_score {
                                best = child.clone();
                                best_score = score;
                            }
                            work_queue.push(child).unwrap();
                        }
                    }
                }
            }
            
            black_box((best, processed_count))
        });
    });
}

fn bench_full_workflow_varying_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end_text_size");
    
    for size in [50, 100, 200] {
        let text = format!(
            "{}",
            (0..size)
                .map(|i| format!("Sentence number {} contains some unique words like word{}.", i, i))
                .collect::<Vec<_>>()
                .join(" ")
        );
        
        group.bench_with_input(BenchmarkId::from_parameter(size), &text, |b, text| {
            b.iter(|| {
                let temp_dir = TempDir::new().unwrap();
                let queue_path = temp_dir.path().join("queue");
                let interner = Interner::from_text(black_box(text));
                let version = interner.version();
                let memory_config = MemoryConfig::default_config();
                
                let mut work_queue = DiskBackedQueue::new_from_path(
                    queue_path.to_str().unwrap(),
                    memory_config.queue_buffer_size
                ).unwrap();
                let mut tracker = SeenTracker::new(1000);
                
                let seed_ortho = Ortho::new(version);
                tracker.insert(seed_ortho.id());
                work_queue.push(seed_ortho).unwrap();
                
                let mut processed_count = 0;
                while let Some(ortho) = work_queue.pop().unwrap() {
                    processed_count += 1;
                    if processed_count > 50 {
                        break;
                    }
                    
                    let (forbidden, required) = ortho.get_requirements();
                    let completions = interner.intersect(&required, &forbidden);
                    
                    for completion in completions.into_iter().take(5) {
                        let children = ortho.add(completion, version);
                        for child in children {
                            let child_id = child.id();
                            if !tracker.contains(&child_id) {
                                tracker.insert(child_id);
                                work_queue.push(child).unwrap();
                            }
                        }
                    }
                }
                
                black_box(processed_count)
            });
        });
    }
    
    group.finish();
}

fn bench_interner_creation_from_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("interner_from_text");
    
    for size in [100, 500, 1000] {
        let text = (0..size)
            .map(|i| format!("Unique sentence {} with different content and words.", i))
            .collect::<Vec<_>>()
            .join(" ");
        
        group.bench_with_input(BenchmarkId::from_parameter(size), &text, |b, text| {
            b.iter(|| {
                Interner::from_text(black_box(text))
            });
        });
    }
    
    group.finish();
}

criterion_group!(
    benches,
    bench_full_workflow_small,
    bench_full_workflow_varying_sizes,
    bench_interner_creation_from_text,
);
criterion_main!(benches);
