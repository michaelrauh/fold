use criterion::{Criterion, black_box, criterion_group, criterion_main, BenchmarkId};
use fold::seen_tracker::SeenTracker;
use std::collections::HashSet;

fn bench_seen_tracker_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("tracker_insert");
    
    for size in [1000, 10000, 100000] {
        group.bench_with_input(
            BenchmarkId::new("size", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut tracker = SeenTracker::new(size);
                    for i in 0..size {
                        tracker.insert(black_box(i));
                    }
                    black_box(tracker.len())
                });
            },
        );
    }
    
    group.finish();
}

fn bench_seen_tracker_contains(c: &mut Criterion) {
    let mut group = c.benchmark_group("tracker_contains");
    
    for size in [1000, 10000, 100000] {
        group.bench_with_input(
            BenchmarkId::new("size", size),
            &size,
            |b, &size| {
                let mut tracker = SeenTracker::new(size);
                for i in 0..size {
                    tracker.insert(i);
                }
                
                b.iter(|| {
                    let mut found = 0;
                    for i in 0..size {
                        if tracker.contains(&black_box(i)) {
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

fn bench_seen_tracker_bloom_effectiveness(c: &mut Criterion) {
    let size = 100000;
    let mut tracker = SeenTracker::new(size);
    
    for i in 0..size / 2 {
        tracker.insert(i);
    }
    
    c.bench_function("tracker_bloom_hits", |b| {
        b.iter(|| {
            let mut hits = 0;
            for i in 0..size / 2 {
                if tracker.contains(&black_box(i)) {
                    hits += 1;
                }
            }
            black_box(hits)
        });
    });
    
    c.bench_function("tracker_bloom_misses", |b| {
        b.iter(|| {
            let mut misses = 0;
            for i in size / 2..size {
                if !tracker.contains(&black_box(i)) {
                    misses += 1;
                }
            }
            black_box(misses)
        });
    });
}

fn bench_seen_tracker_vs_hashset(c: &mut Criterion) {
    let mut group = c.benchmark_group("tracker_vs_hashset");
    
    for size in [1000, 10000, 50000] {
        group.bench_with_input(
            BenchmarkId::new("tracker", size),
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
        
        group.bench_with_input(
            BenchmarkId::new("hashset", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut set = HashSet::new();
                    for i in 0..size {
                        set.insert(i);
                    }
                    let mut found = 0;
                    for i in 0..size {
                        if set.contains(&i) {
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

fn bench_seen_tracker_sharding(c: &mut Criterion) {
    let size = 100000;
    
    c.bench_function("tracker_shard_distribution", |b| {
        b.iter(|| {
            let mut tracker = SeenTracker::new(size);
            for i in 0..size {
                tracker.insert(black_box(i));
            }
            black_box(tracker.len())
        });
    });
}

fn bench_seen_tracker_disk_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("tracker_disk");
    
    for size in [10000, 50000, 100000] {
        group.bench_with_input(
            BenchmarkId::new("size", size),
            &size,
            |b, &size| {
                b.iter(|| {
                    let mut tracker = SeenTracker::new(size);
                    
                    for i in 0..size {
                        tracker.insert(i);
                    }
                    
                    black_box(tracker.len())
                });
            },
        );
    }
    
    group.finish();
}

fn bench_seen_tracker_len(c: &mut Criterion) {
    let size = 100000;
    let mut tracker = SeenTracker::new(size);
    
    for i in 0..size {
        tracker.insert(i);
    }
    
    c.bench_function("tracker_len", |b| {
        b.iter(|| {
            black_box(tracker.len())
        });
    });
}

criterion_group!(
    benches,
    bench_seen_tracker_insert,
    bench_seen_tracker_contains,
    bench_seen_tracker_bloom_effectiveness,
    bench_seen_tracker_vs_hashset,
    bench_seen_tracker_sharding,
    bench_seen_tracker_disk_ops,
    bench_seen_tracker_len,
);
criterion_main!(benches);
