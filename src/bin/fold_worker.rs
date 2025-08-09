use fold::interner::BlobInternerHolder;
use fold::interner::InternerHolderLike;
use fold::QueueConsumerLike;

fn main() {
    fold::init_tracing("fold-worker");
    let mut workq = fold::queue::QueueConsumer::new("workq");
    let mut dbq = fold::queue::QueueProducer::new("dbq").expect("dbq");
    let holder = BlobInternerHolder::new().expect("interner");

    while holder.get_latest().is_none() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Cache latest interner and refresh only when needed based on ortho version.
    let mut cached_interner = holder.get_latest().expect("interner should exist after wait loop");

    workq.consume_one_at_a_time_forever(|ortho| {
        if ortho.version() > cached_interner.version() {
            if let Some(updated) = holder.get_latest() {
                println!("[worker] Refreshing cached interner {} -> {} (ortho version {})", cached_interner.version(), updated.version(), ortho.version());
                cached_interner = updated;
            } else {
                return Err(fold::FoldError::Interner("No interner found during refresh".to_string()));
            }
        }
        fold::process_worker_item_with_cached(ortho, &mut dbq, &cached_interner)
    }).expect("worker loop error");
}
