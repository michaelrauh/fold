use fold::{Follower, PostgresOrthoDatabase, QueueProducer};
use fold::interner::{BlobInternerHolder, InternerHolderLike};
use dotenv::dotenv;

fn main() {
    dotenv().ok();
    fold::init_tracing("fold-follower");
    let mut producer = QueueProducer::new("workq").expect("Failed to create producer");
    let mut holder = BlobInternerHolder::new().expect("Failed to create BlobInternerHolder");
    let mut db = PostgresOrthoDatabase::new();
    let mut follower = Follower::new();

    loop {
        (&mut follower).run_follower_once(&mut db, &mut producer, &mut holder)
        .expect("Follower error");
    }
}
