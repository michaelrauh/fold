use fold::interner::FileInternerHolder;
use fold::interner::InternerHolderLike;
use fold::QueueConsumerLike;

fn main() {
    fold::init_tracing("fold-worker");
    let mut workq = fold::queue::QueueConsumer::new("workq");
    let mut dbq = fold::queue::QueueProducer::new("dbq").expect("dbq");
    let holder = FileInternerHolder::new().expect("interner");

    while holder.get_latest().is_none() {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Cache latest interner and refresh only when needed based on ortho version.
    let mut cached_interner = holder.get_latest().expect("interner should exist after wait loop");
    let perf_window: usize = 200; // fixed window, matches follower default
    let mut perf_durations_us: Vec<u128> = Vec::with_capacity(perf_window);

    workq.consume_one_at_a_time_forever(|ortho| {
        if ortho.version() > cached_interner.version() {
            if let Some(updated) = holder.get_latest() {
                println!("[worker] Refreshing cached interner {} -> {} (ortho version {})", cached_interner.version(), updated.version(), ortho.version());
                cached_interner = updated;
            } else {
                return Err(fold::FoldError::Interner("No interner found during refresh".to_string()));
            }
        }
        let start = std::time::Instant::now();
        let res = fold::process_worker_item_with_cached(ortho, &mut dbq, &cached_interner);
        let dur = start.elapsed();
        perf_durations_us.push(dur.as_micros());
        println!("[worker][perf-iter] time_us={}", dur.as_micros());
        if perf_durations_us.len() >= perf_window {
            let sum: u128 = perf_durations_us.iter().sum();
            let count = perf_durations_us.len() as u128;
            let avg = sum as f64 / count as f64;
            let max = *perf_durations_us.iter().max().unwrap();
            let min = *perf_durations_us.iter().min().unwrap();
            println!("[worker][perf-window {}] avg_us={:.2} min_us={} max_us={}", perf_window, avg, min, max);
            perf_durations_us.clear();
        }
        res
    }).expect("worker loop error");
}
