use fold::{Follower, PostgresOrthoDatabase, QueueProducer};
use fold::interner::{BlobInternerHolder, InternerHolderLike};
use dotenv::dotenv;
use std::time::{Instant, Duration};

fn main() {
    dotenv().ok();
    let pid = std::process::id();
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let follower_id = format!("follower-{}-{}", pid, ts);
    println!("[follower] local follower_id={}", follower_id);
    fold::init_tracing("fold-follower");
    let mut producer = QueueProducer::new("workq").expect("Failed to create producer");
    let mut holder = BlobInternerHolder::new().expect("Failed to create BlobInternerHolder");
    let mut db = PostgresOrthoDatabase::new_with_follower_id(follower_id);
    let mut follower = Follower::new();
    let mut last_reap = Instant::now();
    let reap_interval = Duration::from_secs(30);
    let mut cumulative_bumped: usize = 0;
    let mut cumulative_requeued: usize = 0;
    let mut window_bumped: usize = 0;
    let mut window_requeued: usize = 0;
    let mut window_iters: usize = 0;
    const WINDOW: usize = 200; // iterations per reporting window

    loop {
        if last_reap.elapsed() >= reap_interval {
            db.reap_stale_claims();
            last_reap = Instant::now();
        }
        match (&mut follower).run_follower_once(&mut db, &mut producer, &mut holder) {
            Ok((bumped, requeued)) => {
                if bumped + requeued > 0 {
                    cumulative_bumped += bumped;
                    cumulative_requeued += requeued;
                    window_bumped += bumped;
                    window_requeued += requeued;
                    window_iters += 1;
                    let total = cumulative_bumped + cumulative_requeued;
                    if bumped + requeued > 0 {
                        let inst_rate = if (bumped + requeued) > 0 {
                            bumped as f64 / (bumped + requeued) as f64
                        } else {
                            0.0
                        };
                        let cum_rate = if total > 0 {
                            cumulative_bumped as f64 / total as f64
                        } else {
                            0.0
                        };
                        println!(
                            "[follower][stats] bumped={} requeued={} inst_bump_rate={:.4} cum_bump_rate={:.4}",
                            bumped, requeued, inst_rate, cum_rate
                        );
                    }
                    if window_iters >= WINDOW && (window_bumped + window_requeued) > 0 {
                        let window_rate =
                            window_bumped as f64 / (window_bumped + window_requeued) as f64;
                        println!(
                            "[follower][stats][window {} iters] window_bumped={} window_requeued={} window_bump_rate={:.4}",
                            WINDOW, window_bumped, window_requeued, window_rate
                        );
                        window_bumped = 0;
                        window_requeued = 0;
                        window_iters = 0;
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
