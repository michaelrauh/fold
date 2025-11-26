use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::disk_backed_queue::DiskBackedQueue;
use fold::ortho::Ortho;
use tempfile::TempDir;

fn bench_new(c: &mut Criterion) {
    c.bench_function("queue_new", |b| {
        b.iter(|| {
            DiskBackedQueue::new(black_box(100)).unwrap()
        })
    });
}

fn bench_new_from_path(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("queue_bench");
    let path_str = path.to_str().unwrap();
    
    c.bench_function("queue_new_from_path", |b| {
        b.iter(|| {
            DiskBackedQueue::new_from_path(black_box(path_str), black_box(100)).unwrap()
        })
    });
}

fn bench_push_no_spill(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("queue_push");
    let path_str = path.to_str().unwrap();
    
    c.bench_function("queue_push_no_spill", |b| {
        b.iter_batched(
            || DiskBackedQueue::new_from_path(path_str, 1000).unwrap(),
            |mut queue| {
                let ortho = Ortho::new();
                queue.push(black_box(ortho)).unwrap();
            },
            criterion::BatchSize::SmallInput
        )
    });
}

fn bench_push_with_spill(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("queue_spill");
    let path_str = path.to_str().unwrap();
    
    c.bench_function("queue_push_with_spill", |b| {
        b.iter_batched(
            || {
                let mut queue = DiskBackedQueue::new_from_path(path_str, 10).unwrap();
                // Fill to capacity
                for _ in 0..10 {
                    queue.push(Ortho::new()).unwrap();
                }
                queue
            },
            |mut queue| {
                let ortho = Ortho::new();
                queue.push(black_box(ortho)).unwrap();
            },
            criterion::BatchSize::SmallInput
        )
    });
}

fn bench_pop_from_memory(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("queue_pop_mem");
    let path_str = path.to_str().unwrap();
    
    c.bench_function("queue_pop_from_memory", |b| {
        b.iter_batched(
            || {
                let mut queue = DiskBackedQueue::new_from_path(path_str, 100).unwrap();
                queue.push(Ortho::new()).unwrap();
                queue
            },
            |mut queue| {
                queue.pop().unwrap()
            },
            criterion::BatchSize::SmallInput
        )
    });
}

fn bench_pop_from_disk(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("queue_pop_disk");
    let path_str = path.to_str().unwrap();
    
    c.bench_function("queue_pop_from_disk", |b| {
        b.iter_batched(
            || {
                let mut queue = DiskBackedQueue::new_from_path(path_str, 10).unwrap();
                // Fill past capacity to trigger disk write
                for _ in 0..20 {
                    queue.push(Ortho::new()).unwrap();
                }
                queue.flush().unwrap();
                // Pop from memory first
                for _ in 0..10 {
                    queue.pop().unwrap();
                }
                queue
            },
            |mut queue| {
                queue.pop().unwrap()
            },
            criterion::BatchSize::SmallInput
        )
    });
}

fn bench_flush(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("queue_flush");
    let path_str = path.to_str().unwrap();
    
    c.bench_function("queue_flush", |b| {
        b.iter_batched(
            || {
                let mut queue = DiskBackedQueue::new_from_path(path_str, 100).unwrap();
                for _ in 0..50 {
                    queue.push(Ortho::new()).unwrap();
                }
                queue
            },
            |mut queue| {
                queue.flush().unwrap()
            },
            criterion::BatchSize::SmallInput
        )
    });
}

fn bench_len(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("queue_len");
    let path_str = path.to_str().unwrap();
    let mut queue = DiskBackedQueue::new_from_path(path_str, 100).unwrap();
    
    for _ in 0..50 {
        queue.push(Ortho::new()).unwrap();
    }
    
    c.bench_function("queue_len", |b| {
        b.iter(|| queue.len())
    });
}

fn bench_base_path(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("queue_path");
    let path_str = path.to_str().unwrap();
    let queue = DiskBackedQueue::new_from_path(path_str, 100).unwrap();
    
    c.bench_function("queue_base_path", |b| {
        b.iter(|| queue.base_path())
    });
}

criterion_group!(
    benches,
    bench_new,
    bench_new_from_path,
    bench_push_no_spill,
    bench_push_with_spill,
    bench_pop_from_memory,
    bench_pop_from_disk,
    bench_flush,
    bench_len,
    bench_base_path,
);
criterion_main!(benches);
