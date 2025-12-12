use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fold::seen_tracker::SeenTracker;
use tempfile::TempDir;

fn bench_new(c: &mut Criterion) {
    c.bench_function("tracker_new", |b| {
        b.iter(|| SeenTracker::new(black_box(10000)))
    });
}

fn bench_with_config(c: &mut Criterion) {
    c.bench_function("tracker_with_config", |b| {
        b.iter(|| SeenTracker::with_config(black_box(10000), black_box(16), black_box(4)))
    });
}

fn bench_with_path(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("tracker");
    let path_str = path.to_str().unwrap();

    c.bench_function("tracker_with_path", |b| {
        b.iter(|| {
            SeenTracker::with_path(
                black_box(path_str),
                black_box(10000),
                black_box(16),
                black_box(4),
            )
        })
    });
}

fn bench_contains_not_present(c: &mut Criterion) {
    let mut tracker = SeenTracker::new(10000);

    c.bench_function("tracker_contains_not_present", |b| {
        b.iter(|| tracker.contains(black_box(&999999)))
    });
}

fn bench_contains_bloom_miss(c: &mut Criterion) {
    let mut tracker = SeenTracker::new(10000);
    // Insert some items
    for i in 0..1000 {
        tracker.insert(i);
    }

    c.bench_function("tracker_contains_bloom_miss", |b| {
        b.iter(|| tracker.contains(black_box(&999999)))
    });
}

fn bench_contains_bloom_hit_shard_miss(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("tracker_hit");
    let path_str = path.to_str().unwrap();
    let mut tracker = SeenTracker::with_path(path_str, 10000, 16, 2);

    // Insert items to fill multiple shards
    for i in 0..5000 {
        tracker.insert(i);
    }

    c.bench_function("tracker_contains_bloom_hit_shard_miss", |b| {
        b.iter(|| tracker.contains(black_box(&999999)))
    });
}

fn bench_contains_found(c: &mut Criterion) {
    let mut tracker = SeenTracker::new(10000);
    tracker.insert(12345);

    c.bench_function("tracker_contains_found", |b| {
        b.iter(|| tracker.contains(black_box(&12345)))
    });
}

fn bench_insert_first(c: &mut Criterion) {
    let mut tracker = SeenTracker::new(10000);

    c.bench_function("tracker_insert_first", |b| {
        let mut counter = 0;
        b.iter(|| {
            tracker.insert(black_box(counter));
            counter += 1;
        })
    });
}

fn bench_insert_many(c: &mut Criterion) {
    let mut tracker = SeenTracker::new(10000);
    for i in 0..1000 {
        tracker.insert(i);
    }

    c.bench_function("tracker_insert_many", |b| {
        let mut counter = 1000;
        b.iter(|| {
            tracker.insert(black_box(counter));
            counter += 1;
        })
    });
}

fn bench_insert_with_shard_eviction(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("tracker_evict");
    let path_str = path.to_str().unwrap().to_string();

    let mut tracker = SeenTracker::with_path(&path_str, 10000, 16, 2);
    // Fill enough to cause evictions
    for i in 0..10000 {
        tracker.insert(i);
    }

    c.bench_function("tracker_insert_with_shard_eviction", |b| {
        let mut counter = 10000;
        b.iter(|| {
            tracker.insert(black_box(counter));
            counter += 1;
        })
    });
}

fn bench_flush(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("tracker_flush");
    let path_str = path.to_str().unwrap();

    c.bench_function("tracker_flush", |b| {
        b.iter_batched(
            || {
                let mut tracker = SeenTracker::with_path(path_str, 10000, 16, 4);
                for i in 0..1000 {
                    tracker.insert(i);
                }
                tracker
            },
            |mut tracker| tracker.flush().unwrap(),
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_len(c: &mut Criterion) {
    let mut tracker = SeenTracker::new(10000);
    for i in 0..1000 {
        tracker.insert(i);
    }

    c.bench_function("tracker_len", |b| b.iter(|| tracker.len()));
}

fn bench_is_empty(c: &mut Criterion) {
    let tracker = SeenTracker::new(10000);

    c.bench_function("tracker_is_empty", |b| b.iter(|| tracker.is_empty()));
}

criterion_group!(
    benches,
    bench_new,
    bench_with_config,
    bench_with_path,
    bench_contains_not_present,
    bench_contains_bloom_miss,
    bench_contains_bloom_hit_shard_miss,
    bench_contains_found,
    bench_insert_first,
    bench_insert_many,
    bench_insert_with_shard_eviction,
    bench_flush,
    bench_len,
    bench_is_empty,
);
criterion_main!(benches);
