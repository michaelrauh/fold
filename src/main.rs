use fold::feeder::OrthoFeeder;
use fold::follower::Follower;
use fold::interner::InternerHolder;
use fold::ortho_database::OrthoDatabase;
use fold::queue::Queue;
use fold::worker::Worker;
use std::fs;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() {
    // Prompt for initial filename
    print!("Enter filename for initial text for InternerHolder: ");
    std::io::stdout().flush().unwrap();
    let filename = "e.txt";
    let initial_text = match fs::read_to_string(filename) {
        Ok(s) => s,
        Err(e) => {
            return;
        }
    };
    let input = initial_text.trim();
    println!("Using input: {}", input);

    let dbq = Arc::new(Queue::new("dbq", 1000000));
    let db = Arc::new(OrthoDatabase::new());
    let workq = Arc::new(Queue::new("main", 800000));

    let holder = Arc::new(Mutex::new(
        InternerHolder::with_seed(input, workq.clone()).await,
    ));

    let shutdown = Arc::new(tokio::sync::Notify::new());
    let feeder_shutdown = shutdown.clone();
    let follower_shutdown = shutdown.clone();
    let worker_shutdown = shutdown.clone();

    let feeder_handle = {
        let dbq = dbq.clone();
        let db = db.clone();
        let workq = workq.clone();
        let shutdown = feeder_shutdown.clone();
        tokio::spawn(async move {
            OrthoFeeder::run(dbq, db, workq, shutdown).await;
        })
    };
    let follower_handle = {
        let db = db.clone();
        let workq = workq.clone();
        let container = holder.clone();
        let shutdown = follower_shutdown.clone();
        tokio::spawn(async move {
            Follower::run(db, workq, container, shutdown).await;
        })
    };
    let mut worker = Worker::new(holder.clone()).await;
    let worker_handle = {
        let workq = workq.clone();
        let dbq = dbq.clone();
        let shutdown = worker_shutdown.clone();
        tokio::spawn(async move {
            worker.run(workq, dbq, shutdown).await;
        })
    };

    // Wait until both workq and dbq are empty
    loop {
        let workq_empty = workq.is_empty().await;
        let dbq_empty = dbq.is_empty().await;
        let workq_depth = { workq.receiver.lock().await.len() };
        let dbq_depth = { dbq.receiver.lock().await.len() };
        let db_len = { db.map.lock().await.len() };
        println!(
            "[main] workq depth: {}, dbq depth: {}, db len: {}",
            workq_depth, dbq_depth, db_len
        );
        if workq_empty && dbq_empty {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let ortho_opt = db.get_optimal().await;
    if let Some(ortho) = ortho_opt {
        println!("Optimal Ortho: {:?}", ortho);
    } else {
        println!("No optimal Ortho found.");
    }
    shutdown.notify_waiters();
    feeder_handle.await.expect("feeder task panicked");
    follower_handle.await.expect("follower task panicked");
    worker_handle.await.expect("worker task panicked");
}
