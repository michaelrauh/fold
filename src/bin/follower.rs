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

    loop {
        if last_reap.elapsed() >= reap_interval {
            db.reap_stale_claims();
            last_reap = Instant::now();
        }
        (&mut follower).run_follower_once(&mut db, &mut producer, &mut holder)
            .expect("Follower error");
    }
}
