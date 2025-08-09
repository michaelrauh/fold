use fold::interner::BlobInternerHolder;
use fold::interner::InternerHolderLike;
use fold::QueueConsumerLike;

fn main() {
    fold::init_tracing("fold-worker");
    let mut workq = fold::queue::QueueConsumer::new("workq");
    let mut dbq = fold::queue::QueueProducer::new("dbq").expect("dbq");
    let mut holder = BlobInternerHolder::new().expect("interner");

    while holder.get_latest().is_none() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    workq.consume_one_at_a_time_forever(|ortho| {
        fold::process_worker_item(ortho, &mut dbq, &mut holder)
    }).expect("worker loop error");
}
