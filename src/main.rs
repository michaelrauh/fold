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
    println!("[main] Using filename: {}", filename);
    let initial_text = match fs::read_to_string(filename) {
        Ok(s) => s,
        Err(e) => {
            println!("[main] Failed to read file: {}", e);
            return;
        }
    };
    let input = initial_text.trim();
    println!("[main] Read input from file");
    // println!("Using input: {}", input);

    let dbq = Arc::new(Queue::new("dbq", 1000000));
    let db = Arc::new(OrthoDatabase::new());
    let workq = Arc::new(Queue::new("main", 800000));
    println!("[main] Queues created");

    let holder = Arc::new(Mutex::new(
        InternerHolder::with_seed(input, workq.clone()).await,
    ));
    println!("[main] InternerHolder created");

    let shutdown = Arc::new(tokio::sync::Notify::new());
    let feeder_shutdown = shutdown.clone();
    let follower_shutdown = shutdown.clone();
    let worker_shutdown = shutdown.clone();

    let feeder_handle = {
        let dbq = dbq.clone();
        let db = db.clone();
        let workq = workq.clone();
        let shutdown = feeder_shutdown.clone();
        println!("[main] Spawning feeder");
        tokio::spawn(async move {
            OrthoFeeder::run(dbq, db, workq, shutdown).await;
        })
    };
    let follower_handle = {
        let db = db.clone();
        let workq = workq.clone();
        let container = holder.clone();
        let shutdown = follower_shutdown.clone();
        println!("[main] Spawning follower");
        tokio::spawn(async move {
            Follower::run(db, workq, container, shutdown).await;
        })
    };
    let mut worker = Worker::new(holder.clone()).await;
    let worker_handle = {
        let workq = workq.clone();
        let dbq = dbq.clone();
        let shutdown = worker_shutdown.clone();
        println!("[main] Spawning worker");
        tokio::spawn(async move {
            worker.run(workq, dbq, shutdown).await;
        })
    };
    println!("[main] Entering main loop");

    // Wait until both workq and dbq are empty
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let workq_depth = if let Ok(workq_guard) = workq.receiver.try_lock() {
            workq_guard.len()
        } else {
            continue;
        };
        let dbq_depth = if let Ok(dbq_guard) = dbq.receiver.try_lock() {
            dbq_guard.len()
        } else {
            continue;
        };
        let db_len = if let Ok(db_guard) = db.map.try_lock() {
            db_guard.len()
        } else {
            continue;
        };
        println!(
            "[main] workq depth: {}, dbq depth: {}, db len: {}",
            workq_depth, dbq_depth, db_len
        );
        if workq_depth == 0 && dbq_depth == 0 {
            break;
        }
    }
    println!("[main] Main loop exited");

    let ortho_opt = db.get_optimal().await;
    if let Some(ortho) = ortho_opt {
        println!("Optimal Ortho: {:?}", ortho);
        // Print non-interned version of payload
        let payload_strings = {
            let holder_guard = holder.lock().await;
            let interner = holder_guard.get_latest();
            ortho.payload().iter().map(|opt_idx| {
                opt_idx.map(|idx| interner.string_for_index(idx).to_string())
            }).collect::<Vec<_>>()
        };
        println!("Optimal Ortho (strings): {:?}", payload_strings);
    } else {
        println!("No optimal Ortho found.");
    }
    println!("[main] Notifying shutdown");
    shutdown.notify_waiters();
    feeder_handle.await.expect("feeder task panicked");
    follower_handle.await.expect("follower task panicked");
    worker_handle.await.expect("worker task panicked");
    println!("[main] All background tasks joined. Exiting.");
}
