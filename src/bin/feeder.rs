use fold::{QueueConsumer, QueueConsumerLike, QueueProducer};
use fold::{OrthoFeeder, ortho_database::PostgresOrthoDatabase};

fn main() {
    fold::init_tracing("fold-feeder");
    let mut dbq = QueueConsumer::new("dbq");
    let mut workq = QueueProducer::new("workq").expect("Failed to create workq");
    let mut db = PostgresOrthoDatabase::new();
    dbq.consume_one_at_a_time_forever( |ortho| {
        OrthoFeeder::run_feeder_once(std::slice::from_ref(&ortho), &mut db, &mut workq)
    }).expect("Failed to consume batch from dbq");
}