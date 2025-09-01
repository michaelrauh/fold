use fold::{Follower, InMemoryOrthoDatabase, QueueProducer};
use fold::interner::{FileInternerHolder, InternerHolderLike};
use dotenv::dotenv;
use std::time::{Instant, Duration};

fn main() {
    dotenv().ok();
    let pid = std::process::id();
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let follower_id = format!("follower-{}-{}", pid, ts);
    println!("[follower] local follower_id={}", follower_id);
    fold::init_tracing("fold-follower");
    let mut dbq_producer = QueueProducer::new("dbq").expect("Failed to create dbq producer");
    let mut holder = FileInternerHolder::new().expect("Failed to create FileInternerHolder");
    let mut db = InMemoryOrthoDatabase::new();
    let mut follower = Follower::new();
    let mut last_reap = Instant::now();
    let reap_interval = Duration::from_secs(30);
    let mut _cumulative_bumped: usize = 0; // retained if needed elsewhere
    let mut cumulative_children: usize = 0;
    let mut window_children: usize = 0;
    let mut window_iters: usize = 0;
    let mut total_iters: usize = 0;
    // Perf aggregation (diff-only iterations where duration Some)
    let perf_window: usize = 200;
    let mut perf_durations_us: Vec<u128> = Vec::with_capacity(perf_window);
    const WINDOW: usize = 200; // iterations per reporting window

    loop {
        if last_reap.elapsed() >= reap_interval {
            // no-op for in-memory DB; retained timing for compatibility
            last_reap = Instant::now();
        }
        match (&mut follower).run_follower_once(&mut db, &mut dbq_producer, &mut holder) {
            Ok((bumped, produced_children, diff_duration)) => {
                if bumped + produced_children > 0 || bumped > 0 { // processed a parent ortho
                    _cumulative_bumped += bumped;
                    cumulative_children += produced_children;
                    window_children += produced_children;
                    window_iters += 1;
                    total_iters += 1;
                    let inst_children = produced_children;
                    let cum_avg = if total_iters > 0 { cumulative_children as f64 / total_iters as f64 } else { 0.0 };
                    println!(
                        "[follower][stats] children={} cum_avg_children_per_iter={:.4}",
                        inst_children,
                        cum_avg
                    );
                    if window_iters >= WINDOW && window_iters > 0 {
                        let window_avg = window_children as f64 / window_iters as f64;
                        println!(
                            "[follower][stats][window {} iters] window_children_total={} window_avg_children_per_iter={:.4}",
                            WINDOW, window_children, window_avg
                        );
                        window_children = 0;
                        window_iters = 0;
                    }
                }
                if let Some(dur) = diff_duration { // only diff (child production attempt) iterations timed
                    perf_durations_us.push(dur.as_micros());
                    println!("[follower][perf-iter] time_us={} children={}", dur.as_micros(), produced_children);
                    if perf_durations_us.len() >= perf_window {
                        let sum: u128 = perf_durations_us.iter().sum();
                        let count = perf_durations_us.len() as u128;
                        let avg = sum as f64 / count as f64;
                        let max = *perf_durations_us.iter().max().unwrap();
                        let min = *perf_durations_us.iter().min().unwrap();
                        println!("[follower][perf-window {}] avg_us={:.2} min_us={} max_us={}", perf_window, avg, min, max);
                        perf_durations_us.clear();
                    }
                }
            }
            Err(e) => {
                println!("[follower][error] {:?}", e);
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
