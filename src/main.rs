use fold::feeder::OrthoFeeder;
use fold::follower::Follower;
use fold::interner::InternerHolder;
use fold::ortho_database::OrthoDatabase;
use fold::queue::Queue;
use fold::worker::Worker;
use std::fs;
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() {
    // Initialize with 1.txt instead of e.txt
    let filename = "1.txt";
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

    let dbq = Arc::new(Queue::new("dbq", 1000000));
    let db = Arc::new(OrthoDatabase::new());
    let workq = Arc::new(Queue::new("main", 800000));
    println!("[main] Queues created");

    let holder = Arc::new(Mutex::new(
        InternerHolder::with_seed(input, workq.clone()).await,
    ));
    println!("[main] InternerHolder created");

    // Spawn a task to add new seeds from 2.txt to 28.txt every 30 seconds
    let holder_clone = holder.clone();
    tokio::spawn(async move {
        for i in 2..=28 {
            let filename = format!("{}.txt", i);
            match fs::read_to_string(&filename) {
                Ok(text) => {
                    println!("[main] Adding seed from {}", filename);
                    let mut holder_guard = holder_clone.lock().await;
                    holder_guard.add_text_with_seed(text.trim()).await;
                },
                Err(e) => {
                    println!("[main] Failed to read {}: {}", filename, e);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });

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
    let worker_version = worker.interner.version();
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

    let mut loop_count = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        loop_count += 1;
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
        let latest_version = {
            let holder_guard = holder.lock().await;
            holder_guard.latest_version()
        };
        println!(
            "[main] workq depth: {}, dbq depth: {}, db len: {}, latest interner version: {}, worker version: {}",
            workq_depth, dbq_depth, db_len, latest_version, worker_version
        );
        if loop_count % 6 == 0 {
            let ortho_opt = db.get_optimal().await;
            if let Some(ortho) = ortho_opt {
                println!("[main] Optimal Ortho: {:?}", ortho);
                let payload_strings = {
                    let holder_guard = holder.lock().await;
                    let interner = holder_guard.get_latest();
                    ortho.payload().iter().map(|opt_idx| {
                        opt_idx.map(|idx| interner.string_for_index(idx).to_string())
                    }).collect::<Vec<_>>()
                };
                println!("[main] Optimal Ortho (strings): {:?}", payload_strings);
            } else {
                println!("[main] No optimal Ortho found.");
            }
        }
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
