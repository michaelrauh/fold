use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use fold::{
    disk_backed_queue::DiskBackedQueue, interner::Interner, ortho::Ortho, seen_tracker::SeenTracker,
};
use tempfile::TempDir;

// Full-book text to approximate real vocabulary/phrase size
const BOOK_TEXT: &str = include_str!("../e.txt");

fn build_interner() -> Interner {
    // Small vocabulary with overlapping phrases to exercise intersect logic
    let text = "\
        alpha beta gamma\n\
        alpha beta delta\n\
        alpha gamma delta\n\
        beta gamma epsilon";
    Interner::from_text(text)
}

/// End-to-end search pipeline slice: pop -> requirements -> intersect -> add -> dedup -> push.
/// Uses a large buffer to keep the queue in memory so we measure search CPU more than disk I/O.
fn bench_search_pipeline(c: &mut Criterion) {
    c.bench_function("search_pipeline_no_spill_10k", |b| {
        b.iter_batched(
            || {
                let temp_dir = TempDir::new().unwrap();
                let queue_path = temp_dir.path().join("queue");
                let tracker_path = temp_dir.path().join("tracker");

                // Large buffer to avoid spills; keep all shards in memory to avoid disk flushes.
                let mut queue =
                    DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 100_000).unwrap();
                let mut tracker =
                    SeenTracker::with_path(tracker_path.to_str().unwrap(), 1_000_000, 64, 64);

                let seed = Ortho::new();
                tracker.insert(seed.id());
                queue.push(seed).unwrap();

                (temp_dir, queue, tracker)
            },
            |(_temp_dir, mut queue, mut tracker)| {
                let interner = build_interner();
                let mut processed = 0usize;

                while let Some(ortho) = queue.pop().unwrap() {
                    processed += 1;
                    if processed >= 10_000 {
                        break;
                    }

                    let (forbidden, required) = ortho.get_requirements();
                    let completions = interner.intersect(&required, &forbidden);

                    for completion in completions.into_iter().take(4) {
                        let children = ortho.add(completion);

                        for child in children {
                            let id = child.id();
                            if !tracker.contains(&id) {
                                tracker.insert(id);
                                queue.push(child).unwrap();
                            }
                        }
                    }
                }
            },
            BatchSize::SmallInput,
        )
    });
}

/// Same pipeline but with a large interner built from the full book text and higher branching.
/// This better reflects the "one sentence at a time from a book" workload.
fn bench_search_pipeline_book(c: &mut Criterion) {
    let interner = Interner::from_text(BOOK_TEXT);
    c.bench_function("search_pipeline_book_50k_cap16", |b| {
        b.iter_batched(
            || {
                let temp_dir = TempDir::new().unwrap();
                let queue_path = temp_dir.path().join("queue");
                let tracker_path = temp_dir.path().join("tracker");

                // Large buffer to reduce spills but still realistic for book-scale vocab.
                let mut queue =
                    DiskBackedQueue::new_from_path(queue_path.to_str().unwrap(), 200_000).unwrap();
                let mut tracker =
                    SeenTracker::with_path(tracker_path.to_str().unwrap(), 2_000_000, 128, 128);

                let seed = Ortho::new();
                tracker.insert(seed.id());
                queue.push(seed).unwrap();

                (temp_dir, queue, tracker)
            },
            |(_temp_dir, mut queue, mut tracker)| {
                // Use the prebuilt book interner (shared across iterations, read-only)
                let mut processed = 0usize;

                while let Some(ortho) = queue.pop().unwrap() {
                    processed += 1;
                    if processed >= 50_000 {
                        break;
                    }

                    let (forbidden, required) = ortho.get_requirements();
                    let mut completions = interner.intersect(&required, &forbidden);
                    // Cap branching so the queue growth stays bounded during the bench run
                    completions.truncate(16);

                    for completion in completions {
                        let children = ortho.add(completion);

                        for child in children {
                            let id = child.id();
                            if !tracker.contains(&id) {
                                tracker.insert(id);
                                queue.push(child).unwrap();
                            }
                        }
                    }
                }
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, bench_search_pipeline, bench_search_pipeline_book);
criterion_main!(benches);
