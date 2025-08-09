use fold::{QueueConsumer, QueueConsumerLike, QueueProducer};
use fold::{OrthoFeeder, ortho_database::PostgresOrthoDatabase};

fn main() {
    fold::init_tracing("fold-feeder");
    let mut dbq = QueueConsumer::new("dbq");
    let mut workq = QueueProducer::new("workq").expect("Failed to create workq");
    let mut db = PostgresOrthoDatabase::new();
    dbq.consume_batch_forever(100, |batch| {
        OrthoFeeder::run_feeder_once(batch, &mut db, &mut workq)
    }).expect("Failed to consume batch from dbq");
}