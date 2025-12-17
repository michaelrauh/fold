use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fold::seen_tracker::SeenTracker;
use fold::seen_tracker_cached_runs::CachedRunSeenTracker;
use fold::seen_tracker_doubling_vec::DoublingVecTracker;
use fold::seen_tracker_doubling_vec_bloom::DoublingVecBloomTracker;
use fold::seen_tracker_hashset_doubling::HashSetDoublingTracker;
use fold::seen_tracker_dual_vec::DualVecSeenTracker;
use fold::seen_tracker_eytzinger_bloom::{
    EytzingerBloomTracker, EytzingerNoBloomTracker, SortedVecBloomTracker,
};
use fold::seen_tracker_hashset_vec::HashSetVecTracker;
use fold::seen_tracker_hashset_vec_bloom::HashSetVecBloomTracker;
use fold::seen_tracker_merge_dedup::MergeDedupTracker;
use fold::seen_tracker_linear_probe::LinearProbeDiskResizeTracker;
use fold::seen_tracker_segments::SegmentedRamSeenTracker;
use fold::seen_tracker_sharded::ShardedSeenTracker;
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn bench_new(c: &mut Criterion) {
    c.bench_function("tracker_new", |b| {
        b.iter(|| SeenTracker::new(black_box(10000)))
    });
}

fn bench_with_config(c: &mut Criterion) {
    c.bench_function("tracker_with_config", |b| {
        b.iter(|| SeenTracker::with_config(black_box(10000)))
    });
}

fn bench_with_path(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path().join("tracker");
    let path_str = path.to_str().unwrap();

    c.bench_function("tracker_with_path", |b| {
        b.iter(|| SeenTracker::with_path(black_box(path_str), black_box(10000)))
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
    let mut tracker = SeenTracker::with_path(path_str, 10000);

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

    let mut tracker = SeenTracker::with_path(&path_str, 10000);
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
                let mut tracker = SeenTracker::with_path(path_str, 10000);
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

/// Measures amortized cost of repeated insert+flush cycles that exercise run writes.
/// Each iteration builds 5 batches of 50k IDs (chunked to simulate streaming),
/// flushes after each batch, and includes the flush time in the measurement.
fn bench_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 5;
    const CHUNK: usize = 2048;
    c.bench_function("tracker_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let temp_dir = TempDir::new().unwrap();
                let path = temp_dir.path().join("tracker");
                let path_str = path.to_str().unwrap();
                let mut tracker = SeenTracker::with_path(path_str, 2_000_000);

                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for _ in 0..take {
                            buf.push(next_id);
                            next_id += 1;
                        }
                        tracker.check_batch(&buf, false).unwrap();
                        remaining = remaining.saturating_sub(take);
                    }
                    tracker.flush_pending().unwrap();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Same pattern as above, but using the legacy sharded tracker (all shards resident).
fn bench_sharded_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 5;
    const CHUNK: usize = 2048;
    c.bench_function("sharded_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let temp_dir = TempDir::new().unwrap();
                let path = temp_dir.path().join("tracker_sharded");
                let path_str = path.to_str().unwrap();
                // Keep all shards in memory to mirror the RAM-heavy configuration.
                let mut tracker = ShardedSeenTracker::with_path(path_str, 2_000_000, 64, 64);

                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for _ in 0..take {
                            buf.push(next_id);
                            next_id += 1;
                        }
                        for id in &buf {
                            tracker.insert(*id);
                        }
                        remaining = remaining.saturating_sub(take);
                    }
                    tracker.flush().unwrap();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Sharded tracker with partial residency (60/64 shards resident) to simulate light evictions.
fn bench_sharded_amortized_insert_flush_60_of_64(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 5;
    const CHUNK: usize = 2048;
    c.bench_function("sharded_amortized_insert_flush_5x50k_60of64", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let temp_dir = TempDir::new().unwrap();
                let path = temp_dir.path().join("tracker_sharded");
                let path_str = path.to_str().unwrap();
                let mut tracker = ShardedSeenTracker::with_path(path_str, 2_000_000, 64, 60);

                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for _ in 0..take {
                            buf.push(next_id);
                            next_id += 1;
                        }
                        for id in &buf {
                            tracker.insert(*id);
                        }
                        remaining = remaining.saturating_sub(take);
                    }
                    tracker.flush().unwrap();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Sharded tracker with half residency (32/64 shards resident) to force more evictions.
fn bench_sharded_amortized_insert_flush_32_of_64(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 5;
    const CHUNK: usize = 2048;
    c.bench_function("sharded_amortized_insert_flush_5x50k_32of64", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let temp_dir = TempDir::new().unwrap();
                let path = temp_dir.path().join("tracker_sharded");
                let path_str = path.to_str().unwrap();
                let mut tracker = ShardedSeenTracker::with_path(path_str, 2_000_000, 64, 32);

                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for _ in 0..take {
                            buf.push(next_id);
                            next_id += 1;
                        }
                        for id in &buf {
                            tracker.insert(*id);
                        }
                        remaining = remaining.saturating_sub(take);
                    }
                    tracker.flush().unwrap();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Two-vec tracker: single large in-memory vec + single disk run merged each flush.
fn bench_dual_vec_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 5;
    const CHUNK: usize = 2048;
    c.bench_function("dual_vec_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let temp_dir = TempDir::new().unwrap();
                let path = temp_dir.path().join("dual_vec.bin");
                let path_str = path.to_str().unwrap();
                let flush_limit = 200_000; // allow big in-memory growth
                let mut tracker = DualVecSeenTracker::with_path(path_str, 2_000_000, flush_limit);

                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for _ in 0..take {
                            buf.push(next_id);
                            next_id += 1;
                        }
                        for id in &buf {
                            tracker.insert(*id);
                        }
                        remaining = remaining.saturating_sub(take);
                    }
                    tracker.flush().unwrap();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Run-based tracker with many cached runs in RAM (~high residency).
fn bench_cached_runs_amortized_insert_flush_high(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 5;
    const CHUNK: usize = 2048;
    c.bench_function("cached_runs_amortized_insert_flush_5x50k_high", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let temp_dir = TempDir::new().unwrap();
                let path = temp_dir.path().join("cached_runs_high");
                let path_str = path.to_str().unwrap();
                let mut tracker =
                    CachedRunSeenTracker::with_path(path_str, 2_000_000, 100_000, 60);

                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for _ in 0..take {
                            buf.push(next_id);
                            next_id += 1;
                        }
                        for id in &buf {
                            tracker.insert(*id);
                        }
                        remaining = remaining.saturating_sub(take);
                    }
                    tracker.flush().unwrap();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Run-based tracker with moderate cached runs in RAM (~medium residency).
fn bench_cached_runs_amortized_insert_flush_med(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 20; // 1M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5; // ~20% dups
    const BUFFER_LIMIT: usize = 16_384;
    const MAX_CACHED_RUNS: usize = 32;
    c.bench_function("cached_runs_amortized_insert_flush_5x50k_med", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let temp_dir = TempDir::new().unwrap();
                let path = temp_dir.path().join("cached_runs_med");
                let path_str = path.to_str().unwrap();
                let mut tracker =
                    CachedRunSeenTracker::with_path(path_str, 2_000_000, BUFFER_LIMIT, MAX_CACHED_RUNS);

                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// HashSet front with sorted backing Vec to cap memory; merges on threshold.
fn bench_hashset_vec_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 20; // 1M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5; // ~20% dups
    const FLUSH_LIMIT: usize = 16_384;
    c.bench_function("hashset_vec_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = HashSetVecTracker::new(FLUSH_LIMIT);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// HashSet+Vec with Bloom front, amortized insert+flush under mixed dup workload.
fn bench_hashset_vec_bloom_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 20; // 1M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5; // ~20% dups
    const FLUSH_LIMIT: usize = 16_384;
    const BLOOM_CAP: usize = 2_000_000;
    c.bench_function("hashset_vec_bloom_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = HashSetVecBloomTracker::new(BLOOM_CAP, FLUSH_LIMIT);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Doubling vector levels (1k, 2k, 4k, ...) with cascading merges.
fn bench_doubling_vec_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 20; // 1M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5; // ~20% dups
    const BASE_CAPACITY: usize = 1_024;
    c.bench_function("doubling_vec_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = DoublingVecTracker::new(BASE_CAPACITY);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Larger head-to-head: HashSet+Vec without Bloom, 10M ids, same dup pattern.
fn bench_hashset_vec_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const FLUSH_LIMIT: usize = 16_384;
    c.bench_function("hashset_vec_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = HashSetVecTracker::new(FLUSH_LIMIT);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Larger head-to-head: HashSet+Vec with Bloom, 10M ids, same dup pattern.
fn bench_hashset_vec_bloom_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const FLUSH_LIMIT: usize = 16_384;
    const BLOOM_CAP: usize = 20_000_000;
    c.bench_function("hashset_vec_bloom_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = HashSetVecBloomTracker::new(BLOOM_CAP, FLUSH_LIMIT);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Larger head-to-head: Doubling vec without Bloom, 10M ids, same dup pattern.
fn bench_doubling_vec_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const BASE_CAPACITY: usize = 1_024;
    c.bench_function("doubling_vec_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = DoublingVecTracker::new(BASE_CAPACITY);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Larger head-to-head: Doubling vec with Bloom, 10M ids, same dup pattern.
fn bench_doubling_vec_bloom_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const BASE_CAPACITY: usize = 1_024;
    const BLOOM_CAP: usize = 20_000_000;
    c.bench_function("doubling_vec_bloom_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = DoublingVecBloomTracker::new(BLOOM_CAP, BASE_CAPACITY);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// HashSet front + doubling levels, 10M ids.
fn bench_hashset_doubling_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const BASE_CAPACITY: usize = 1_024;
    c.bench_function("hashset_doubling_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = HashSetDoublingTracker::new(BASE_CAPACITY);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

fn bench_hashset_doubling_head_to_head_10m_base_1k(c: &mut Criterion) {
    run_hashset_doubling_head_to_head(c, 1_024, "hashset_doubling_head_to_head_10m_base1k");
}

fn bench_hashset_doubling_head_to_head_10m_base_2k(c: &mut Criterion) {
    run_hashset_doubling_head_to_head(c, 2_048, "hashset_doubling_head_to_head_10m_base2k");
}

fn bench_hashset_doubling_head_to_head_10m_base_4k(c: &mut Criterion) {
    run_hashset_doubling_head_to_head(c, 4_096, "hashset_doubling_head_to_head_10m_base4k");
}

fn bench_hashset_doubling_head_to_head_10m_base_8k(c: &mut Criterion) {
    run_hashset_doubling_head_to_head(c, 8_192, "hashset_doubling_head_to_head_10m_base8k");
}

fn bench_hashset_doubling_head_to_head_10m_base_16k(c: &mut Criterion) {
    run_hashset_doubling_head_to_head(
        c,
        16_384,
        "hashset_doubling_head_to_head_10m_base16k",
    );
}

fn run_hashset_doubling_head_to_head(c: &mut Criterion, base: usize, name: &'static str) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    c.bench_function(name, |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = HashSetDoublingTracker::new(base);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Plain HashSet (no tiers), 10M ids, ~20% dups.
fn bench_plain_hashset_head_to_head_10m(c: &mut Criterion) {
    use nohash_hasher::BuildNoHashHasher;
    use std::collections::HashSet;

    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    c.bench_function("plain_hashset_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut set: HashSet<usize, BuildNoHashHasher<usize>> =
                    HashSet::with_hasher(BuildNoHashHasher::default());
                set.reserve(IDS_PER_BATCH * BATCHES);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        for id in &buf {
                            set.insert(*id);
                        }
                        remaining = remaining.saturating_sub(take);
                    }
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Linear probe nohash table with disk-assisted resize, 10M ids.
fn bench_linear_probe_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const INITIAL_CAP: usize = 16_384;
    c.bench_function("linear_probe_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = LinearProbeDiskResizeTracker::new(INITIAL_CAP, 0.8);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Eytzinger tiers with per-tier Bloom (0.2% FP), base16k, 10M ids.
fn bench_eytzinger_bloom_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const BASE_CAPACITY: usize = 16_384;
    c.bench_function("eytzinger_bloom_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = EytzingerBloomTracker::new(BASE_CAPACITY);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Eytzinger tiers without Bloom.
fn bench_eytzinger_no_bloom_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const BASE_CAPACITY: usize = 16_384;
    c.bench_function("eytzinger_no_bloom_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = EytzingerNoBloomTracker::new(BASE_CAPACITY);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Sorted vec tiers with Bloom (no Eytzinger).
fn bench_sorted_vec_bloom_head_to_head_10m(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 200; // 10M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5;
    const BASE_CAPACITY: usize = 16_384;
    c.bench_function("sorted_vec_bloom_head_to_head_10m", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = SortedVecBloomTracker::new(BASE_CAPACITY);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Doubling vector levels with Bloom front.
fn bench_doubling_vec_bloom_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 20; // 1M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5; // ~20% dups
    const BASE_CAPACITY: usize = 1_024;
    const BLOOM_CAP: usize = 2_000_000;
    c.bench_function("doubling_vec_bloom_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = DoublingVecBloomTracker::new(BLOOM_CAP, BASE_CAPACITY);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.insert_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Pure in-memory segmented tracker (no disk), to show a RAM-only ceiling.
fn bench_segmented_ram_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 5;
    const CHUNK: usize = 2048;
    c.bench_function("segmented_ram_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = SegmentedRamSeenTracker::new(2_000_000, 100_000);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for _ in 0..take {
                            buf.push(next_id);
                            next_id += 1;
                        }
                        for id in &buf {
                            tracker.insert(*id);
                        }
                        remaining = remaining.saturating_sub(take);
                    }
                    tracker.flush().unwrap();
                }
                total += start.elapsed();
            }
            total
        })
    });
}

/// Deferred-dedup tracker benchmark: push batches, dedup on flush.
fn bench_merge_dedup_amortized_insert_flush(c: &mut Criterion) {
    const IDS_PER_BATCH: usize = 50_000;
    const BATCHES: usize = 20; // 1M ids per iter
    const CHUNK: usize = 1024;
    const DUP_STRIDE: usize = 5; // ~20% dups
    c.bench_function("merge_dedup_amortized_insert_flush_5x50k", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut tracker = MergeDedupTracker::new(2_000_000, 16_384);
                let mut next_id = 0usize;
                let mut buf = Vec::with_capacity(CHUNK);
                let start = Instant::now();
                for _ in 0..BATCHES {
                    let mut remaining = IDS_PER_BATCH;
                    while remaining > 0 {
                        let take = remaining.min(CHUNK);
                        buf.clear();
                        for j in 0..take {
                            if next_id > 0 && j % DUP_STRIDE == 0 {
                                buf.push(next_id - 1);
                            } else {
                                buf.push(next_id);
                                next_id += 1;
                            }
                        }
                        tracker.stage_batch(&buf);
                        remaining = remaining.saturating_sub(take);
                    }
                    let _ = tracker.flush_with_result().unwrap();
                }
                total += start.elapsed();
            }
            total
        })
    });
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
    bench_amortized_insert_flush,
    bench_sharded_amortized_insert_flush,
    bench_sharded_amortized_insert_flush_60_of_64,
    bench_sharded_amortized_insert_flush_32_of_64,
    bench_dual_vec_amortized_insert_flush,
    bench_cached_runs_amortized_insert_flush_high,
    bench_cached_runs_amortized_insert_flush_med,
    bench_hashset_vec_amortized_insert_flush,
    bench_hashset_vec_bloom_amortized_insert_flush,
    bench_doubling_vec_amortized_insert_flush,
    bench_doubling_vec_bloom_amortized_insert_flush,
    bench_hashset_vec_head_to_head_10m,
    bench_hashset_vec_bloom_head_to_head_10m,
    bench_doubling_vec_head_to_head_10m,
    bench_doubling_vec_bloom_head_to_head_10m,
    bench_hashset_doubling_head_to_head_10m_base_1k,
    bench_hashset_doubling_head_to_head_10m,
    bench_hashset_doubling_head_to_head_10m_base_2k,
    bench_hashset_doubling_head_to_head_10m_base_4k,
    bench_hashset_doubling_head_to_head_10m_base_8k,
    bench_hashset_doubling_head_to_head_10m_base_16k,
    bench_plain_hashset_head_to_head_10m,
    bench_linear_probe_head_to_head_10m,
    bench_eytzinger_bloom_head_to_head_10m,
    bench_eytzinger_no_bloom_head_to_head_10m,
    bench_sorted_vec_bloom_head_to_head_10m,
    bench_segmented_ram_amortized_insert_flush,
    bench_merge_dedup_amortized_insert_flush,
);
criterion_main!(benches);
