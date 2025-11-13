use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::{disk_backed_queue::DiskBackedQueue, ortho::Ortho};
use tempfile::TempDir;

fn bench_queue_push_before_spill(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let queue_path = temp_dir.path().join("queue");
    
    c.bench_function("queue_push_in_memory", |b| {
        b.iter(|| {
            let mut queue = DiskBackedQueue::new_from_path(
                queue_path.to_str().unwrap(),
                1000
            ).unwrap();
            
            for i in 0..100 {
                queue.push(Ortho::new(black_box(i))).unwrap();
            }
            
            black_box(queue.len())
        });
    });
}

fn bench_queue_push_with_spill(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_push_spill");
    
    for buffer_size in [50, 100, 200] {
        group.bench_with_input(
            BenchmarkId::new("buffer", buffer_size),
            &buffer_size,
            |b, &buffer_size| {
                b.iter(|| {
                    let temp_dir = TempDir::new().unwrap();
                    let queue_path = temp_dir.path().join("queue");
                    let mut queue = DiskBackedQueue::new_from_path(
                        queue_path.to_str().unwrap(),
                        buffer_size
                    ).unwrap();
                    
                    for i in 0..buffer_size * 3 {
                        queue.push(Ortho::new(black_box(i))).unwrap();
                    }
                    
                    black_box(queue.len())
                });
            },
        );
    }
    
    group.finish();
}

fn bench_queue_pop_from_memory(c: &mut Criterion) {
    c.bench_function("queue_pop_memory", |b| {
        b.iter(|| {
            let temp_dir = TempDir::new().unwrap();
            let queue_path = temp_dir.path().join("queue");
            let mut queue = DiskBackedQueue::new_from_path(
                queue_path.to_str().unwrap(),
                1000
            ).unwrap();
            
            for i in 0..100 {
                queue.push(Ortho::new(i)).unwrap();
            }
            
            let mut count = 0;
            while queue.pop().unwrap().is_some() {
                count += 1;
            }
            black_box(count)
        });
    });
}

fn bench_queue_pop_from_disk(c: &mut Criterion) {
    let mut group = c.benchmark_group("queue_pop_disk");
    
    for items in [100, 500, 1000] {
        group.bench_with_input(
            BenchmarkId::new("items", items),
            &items,
            |b, &items| {
                b.iter(|| {
                    let temp_dir = TempDir::new().unwrap();
                    let queue_path = temp_dir.path().join("queue");
                    let mut queue = DiskBackedQueue::new_from_path(
                        queue_path.to_str().unwrap(),
                        50
                    ).unwrap();
                    
                    for i in 0..items {
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

fn bench_queue_mixed_operations(c: &mut Criterion) {
    c.bench_function("queue_mixed_push_pop", |b| {
        b.iter(|| {
            let temp_dir = TempDir::new().unwrap();
            let queue_path = temp_dir.path().join("queue");
            let mut queue = DiskBackedQueue::new_from_path(
                queue_path.to_str().unwrap(),
                100
            ).unwrap();
            
            for i in 0..50 {
                queue.push(Ortho::new(i)).unwrap();
            }
            
            for _ in 0..25 {
                queue.pop().unwrap();
            }
            
            for i in 50..100 {
                queue.push(Ortho::new(i)).unwrap();
            }
            
            let mut count = 0;
            while queue.pop().unwrap().is_some() {
                count += 1;
            }
            
            black_box(count)
        });
    });
}

fn bench_queue_persistence(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let queue_path = temp_dir.path().join("persist_queue");
    
    c.bench_function("queue_persist_and_reload", |b| {
        b.iter(|| {
            {
                let mut queue = DiskBackedQueue::new_from_path(
                    queue_path.to_str().unwrap(),
                    50
                ).unwrap();
                
                for i in 0..150 {
                    queue.push(Ortho::new(i)).unwrap();
                }
                
                queue.flush().unwrap();
            }
            
            {
                let mut queue = DiskBackedQueue::new_from_path(
                    queue_path.to_str().unwrap(),
                    50
                ).unwrap();
                
                let mut count = 0;
                while queue.pop().unwrap().is_some() {
                    count += 1;
                }
                black_box(count)
            }
        });
    });
}

fn bench_queue_len_with_disk(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let queue_path = temp_dir.path().join("queue");
    let mut queue = DiskBackedQueue::new_from_path(
        queue_path.to_str().unwrap(),
        50
    ).unwrap();
    
    for i in 0..200 {
        queue.push(Ortho::new(i)).unwrap();
    }
    
    c.bench_function("queue_len_tracking", |b| {
        b.iter(|| {
            black_box(queue.len())
        });
    });
}

criterion_group!(
    benches,
    bench_queue_push_before_spill,
    bench_queue_push_with_spill,
    bench_queue_pop_from_memory,
    bench_queue_pop_from_disk,
    bench_queue_mixed_operations,
    bench_queue_persistence,
    bench_queue_len_with_disk,
);
criterion_main!(benches);
