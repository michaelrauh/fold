use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::{
    checkpoint_manager::CheckpointManager,
    disk_backed_queue::DiskBackedQueue,
    interner::Interner,
    memory_config::MemoryConfig,
    ortho::Ortho,
    seen_tracker::SeenTracker,
};
use tempfile::TempDir;
use std::fs;

/// Benchmark checkpoint loading steps in detail
/// Lower sample count for faster execution

fn bench_interner_deserialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_interner");
    group.sample_size(20); // Reduced from default 100
    
    // Create interners of different sizes
    for vocab_size in [10, 50, 100] {
        let text = (0..vocab_size)
            .map(|i| format!("word{} follows word{} here.", i, i + 1))
            .collect::<Vec<_>>()
            .join(" ");
        
        let interner = Interner::from_text(&text);
        let encoded = bincode::encode_to_vec(&interner, bincode::config::standard()).unwrap();
        
        group.bench_with_input(
            BenchmarkId::new("deserialize", vocab_size),
            &encoded,
            |b, encoded| {
                b.iter(|| {
                    bincode::decode_from_slice::<Interner, _>(
                        black_box(encoded),
                        bincode::config::standard()
                    ).unwrap()
                });
            },
        );
    }
    
    group.finish();
}

fn bench_results_queue_reconstruction(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_queue_rebuild");
    group.sample_size(10); // Lower for expensive operations
    
    let memory_config = MemoryConfig::default_config();
    
    // Test with different numbers of results
    for num_results in [10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("consume_and_rebuild", num_results),
            &num_results,
            |b, &num_results| {
                b.iter(|| {
                    let temp_dir = TempDir::new().unwrap();
                    let source_path = temp_dir.path().join("source");
                    let dest_path = temp_dir.path().join("dest");
                    
                    // Create source queue with orthos
                    let mut source_queue = DiskBackedQueue::new_from_path(
                        source_path.to_str().unwrap(),
                        memory_config.queue_buffer_size
                    ).unwrap();
                    
                    for i in 0..num_results {
                        source_queue.push(Ortho::new(i)).unwrap();
                    }
                    
                    // Simulate consumption and rebuild
                    let mut dest_queue = DiskBackedQueue::new_from_path(
                        dest_path.to_str().unwrap(),
                        memory_config.queue_buffer_size
                    ).unwrap();
                    
                    let mut tracker = SeenTracker::new(num_results * 2);
                    
                    while let Some(ortho) = source_queue.pop().unwrap() {
                        let ortho_id = ortho.id();
                        tracker.insert(ortho_id);
                        dest_queue.push(ortho).unwrap();
                    }
                    
                    black_box(tracker.len())
                });
            },
        );
    }
    
    group.finish();
}

fn bench_tracker_reconstruction(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_tracker_rebuild");
    group.sample_size(20);
    
    // Test inserting IDs into tracker during load
    for num_items in [100, 500, 1000] {
        group.bench_with_input(
            BenchmarkId::new("insert_ids", num_items),
            &num_items,
            |b, &num_items| {
                b.iter(|| {
                    let mut tracker = SeenTracker::new(num_items * 2);
                    
                    for i in 0..num_items {
                        tracker.insert(black_box(i));
                    }
                    
                    black_box(tracker.len())
                });
            },
        );
    }
    
    group.finish();
}

fn bench_checkpoint_save_steps(c: &mut Criterion) {
    let mut group = c.benchmark_group("checkpoint_save_steps");
    group.sample_size(10);
    
    // Benchmark individual save operations
    let text = "The quick brown fox jumps. The lazy dog sleeps.";
    let interner = Interner::from_text(text);
    
    group.bench_function("interner_serialization", |b| {
        b.iter(|| {
            bincode::encode_to_vec(black_box(&interner), bincode::config::standard()).unwrap()
        });
    });
    
    // Benchmark queue flush
    group.bench_function("queue_flush", |b| {
        b.iter(|| {
            let temp_dir = TempDir::new().unwrap();
            let queue_path = temp_dir.path().join("flush_test");
            let memory_config = MemoryConfig::default_config();
            
            let mut queue = DiskBackedQueue::new_from_path(
                queue_path.to_str().unwrap(),
                memory_config.queue_buffer_size
            ).unwrap();
            
            for i in 0..20 {
                queue.push(Ortho::new(i)).unwrap();
            }
            
            queue.flush().unwrap();
        });
    });
    
    group.finish();
}

fn bench_full_checkpoint_cycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_checkpoint_cycle");
    group.sample_size(10); // Minimum for criterion
    
    for num_results in [10, 25] {
        group.bench_with_input(
            BenchmarkId::new("save_and_load", num_results),
            &num_results,
            |b, &num_results| {
                b.iter(|| {
                    let temp_dir = TempDir::new().unwrap();
                    let fold_state = temp_dir.path().join("fold_state");
                    fs::create_dir_all(&fold_state).unwrap();
                    
                    let manager = CheckpointManager::with_base_dir(&fold_state);
                    let text = "Test text for checkpoint.";
                    let interner = Interner::from_text(text);
                    
                    let results_path = fold_state.join("results");
                    let memory_config = MemoryConfig::default_config();
                    let mut results_queue = DiskBackedQueue::new_from_path(
                        results_path.to_str().unwrap(),
                        memory_config.queue_buffer_size
                    ).unwrap();
                    
                    for i in 0..num_results {
                        results_queue.push(Ortho::new(i)).unwrap();
                    }
                    
                    // Save checkpoint
                    manager.save(&interner, &mut results_queue).unwrap();
                    
                    // Load checkpoint
                    let loaded = manager.load(&memory_config).unwrap();
                    
                    black_box(loaded.is_some())
                });
            },
        );
    }
    
    group.finish();
}

fn bench_seed_ortho_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("seed_operations");
    group.sample_size(20);
    
    group.bench_function("create_seed_ortho", |b| {
        b.iter(|| {
            let version = black_box(1);
            let seed_ortho = Ortho::new(version);
            let seed_id = seed_ortho.id();
            black_box((seed_ortho, seed_id))
        });
    });
    
    group.bench_function("insert_seed_and_push", |b| {
        b.iter(|| {
            let temp_dir = TempDir::new().unwrap();
            let queue_path = temp_dir.path().join("seed_queue");
            let memory_config = MemoryConfig::default_config();
            
            let mut tracker = SeenTracker::new(1000);
            let mut queue = DiskBackedQueue::new_from_path(
                queue_path.to_str().unwrap(),
                memory_config.queue_buffer_size
            ).unwrap();
            
            let seed_ortho = Ortho::new(1);
            let seed_id = seed_ortho.id();
            
            tracker.insert(seed_id);
            queue.push(seed_ortho).unwrap();
            
            black_box(tracker.len())
        });
    });
    
    group.finish();
}

criterion_group!(
    benches,
    bench_interner_deserialization,
    bench_results_queue_reconstruction,
    bench_tracker_reconstruction,
    bench_checkpoint_save_steps,
    bench_full_checkpoint_cycle,
    bench_seed_ortho_creation,
);
criterion_main!(benches);
