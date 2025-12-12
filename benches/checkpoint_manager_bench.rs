use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::checkpoint_manager::CheckpointManager;
use fold::disk_backed_queue::DiskBackedQueue;
use fold::interner::Interner;
use fold::memory_config::MemoryConfig;
use fold::ortho::Ortho;
use tempfile::TempDir;

fn bench_new(c: &mut Criterion) {
    c.bench_function("checkpoint_manager_new", |b| {
        b.iter(|| CheckpointManager::new())
    });
}

fn bench_with_base_dir(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path();

    c.bench_function("checkpoint_manager_with_base_dir", |b| {
        b.iter(|| CheckpointManager::with_base_dir(black_box(path)))
    });
}

fn bench_save(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let base_path = temp_dir.path();

    c.bench_function("checkpoint_manager_save", |b| {
        b.iter_batched(
            || {
                let manager = CheckpointManager::with_base_dir(base_path);
                let interner = Interner::from_text("the quick brown fox jumps over the lazy dog");

                let queue_path = base_path.join("results");
                let mut results =
                    DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 100).unwrap();

                for _ in 0..10 {
                    results.push(Ortho::new()).unwrap();
                }

                (manager, interner, results)
            },
            |(manager, interner, mut results)| manager.save(&interner, &mut results).unwrap(),
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_save_large(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let base_path = temp_dir.path();

    c.bench_function("checkpoint_manager_save_large", |b| {
        b.iter_batched(
            || {
                let manager = CheckpointManager::with_base_dir(base_path);
                let interner = Interner::from_text(
                    "the quick brown fox jumps over the lazy dog a journey of a thousand miles begins with a single step"
                );

                let queue_path = base_path.join("results_large");
                let mut results = DiskBackedQueue::new_from_path(
                    queue_path.to_str().unwrap(),
                    1000
                ).unwrap();

                for _ in 0..100 {
                    results.push(Ortho::new()).unwrap();
                }

                (manager, interner, results)
            },
            |(manager, interner, mut results)| {
                manager.save(&interner, &mut results).unwrap()
            },
            criterion::BatchSize::SmallInput
        )
    });
}

fn bench_load(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let base_path = temp_dir.path();

    // Setup: save a checkpoint first
    let manager = CheckpointManager::with_base_dir(base_path);
    let interner = Interner::from_text("the quick brown fox");

    let queue_path = base_path.join("results_load");
    let mut results = DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 100).unwrap();

    for _ in 0..10 {
        results.push(Ortho::new()).unwrap();
    }

    manager.save(&interner, &mut results).unwrap();

    let interner_bytes = bincode::encode_to_vec(&interner, bincode::config::standard())
        .unwrap()
        .len();
    let memory_config = MemoryConfig::calculate(interner_bytes, 0);

    c.bench_function("checkpoint_manager_load", |b| {
        b.iter(|| manager.load(black_box(&memory_config)).unwrap())
    });
}

criterion_group!(
    benches,
    bench_new,
    bench_with_base_dir,
    bench_save,
    bench_save_large,
    bench_load,
);
criterion_main!(benches);
