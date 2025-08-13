use fold::{QueueConsumer, QueueConsumerLike, QueueProducer};
use fold::{OrthoFeeder, ortho_database::PostgresOrthoDatabase};

fn main() {
    fold::init_tracing("fold-feeder");
    let mut dbq = QueueConsumer::new("dbq");
    let mut workq = QueueProducer::new("workq").expect("Failed to create workq");
    let mut db = PostgresOrthoDatabase::new();
    let mut cumulative_new: usize = 0;
    let mut cumulative_total: usize = 0;
    let mut window_new: usize = 0;
    let mut window_total: usize = 0;
    let mut window_batches: usize = 0;
    const WINDOW: usize = 50; // batches per reporting window
    dbq.consume_batch_forever(100, |batch| {
        match OrthoFeeder::run_feeder_once(batch, &mut db, &mut workq) {
            Ok((new_count, batch_total)) => {
                cumulative_new += new_count;
                cumulative_total += batch_total;
                window_new += new_count;
                window_total += batch_total;
                window_batches += 1;
                if batch_total > 0 {
                    let inst_rate = new_count as f64 / batch_total as f64;
                    let cum_rate = if cumulative_total > 0 { cumulative_new as f64 / cumulative_total as f64 } else { 0.0 };
                    println!("[feeder][stats] batch_new={} batch_total={} inst_rate={:.4} cum_rate={:.4}", new_count, batch_total, inst_rate, cum_rate);
                }
                if window_batches >= WINDOW && window_total > 0 {
                    let window_rate = window_new as f64 / window_total as f64;
                    println!("[feeder][stats][window {} batches] window_new={} window_total={} window_rate={:.4}", WINDOW, window_new, window_total, window_rate);
                    window_new = 0; window_total = 0; window_batches = 0;
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }).expect("Failed to consume batch from dbq");
}