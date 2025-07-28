use fold::feeder::OrthoFeeder;
use fold::follower::Follower;
use fold::interner::InternerHolder;
use fold::ortho_database::{InMemoryOrthoDatabase, PostgresOrthoDatabase, OrthoDatabaseLike};
use fold::queue::{Queue, MockQueue, QueueLike};
use fold::worker::Worker;
use std::fs;
use dotenv::dotenv;
use std::time::Instant;

fn run<Q: QueueLike, D: OrthoDatabaseLike>(mut dbq: Q, mut workq: Q, mut db: D) {
    // Prepare list of files 0.txt to 28.txt
    let files: Vec<String> = (0..=28).map(|i| format!("{}.txt", i)).collect();
    let mut current_file_idx = 0;
    // Read the first file (0.txt)
    let initial_text = match fs::read_to_string(&files[0]) {
        Ok(s) => s,
        Err(e) => {
            println!("[main] Failed to read file {}: {}", files[0], e);
            return;
        }
    };
    let mut holder = InternerHolder::with_seed(initial_text.trim(), &mut workq);
    println!("[main] Queues and InternerHolder created");
    let mut loop_count = 0;
    let mut files_processed = 1; // 0.txt is already processed
    let start_time = Instant::now(); // Start grace period timer
    loop {
        OrthoFeeder::run(&mut dbq, &mut db, &mut workq);
        Follower::run(&mut db, &mut workq, &mut holder);
        Worker::run(&mut Worker::new(&mut holder), &mut workq, &mut dbq, &mut holder);
        loop_count += 1;
        let workq_depth = workq.len();
        let dbq_depth = dbq.len();
        let db_len = db.len();
        let latest_version = holder.latest_version();
        let num_interners = holder.num_interners();
        println!(
            "[main] workq depth: {}, dbq depth: {}, db len: {}, latest interner version: {}, files processed: {}",
            workq_depth, dbq_depth, db_len, latest_version, files_processed
        );
        // Periodically print optimal ortho
        if loop_count % 1000 == 0 {
            let ortho_opt = db.get_optimal();
            if let Some(ortho) = ortho_opt {
                println!("[main] Optimal Ortho: {:?}", ortho);
                let interner = holder.get_latest();
                let payload_strings = ortho.payload().iter().map(|opt_idx| {
                    opt_idx.map(|idx| interner.string_for_index(idx).to_string())
                }).collect::<Vec<_>>();
                println!("[main] Optimal Ortho (strings): {:?}", payload_strings);
            } else {
                println!("[main] No optimal Ortho found.");
            }
        }
        let GRACE_PERIOD_SECS: u64 = 60;
        // Feed next file if queues are small and only one interner, but only after 30s grace period
        if start_time.elapsed().as_secs() >= GRACE_PERIOD_SECS {
            if (workq_depth + dbq_depth) < 1000 && num_interners == 1 && files_processed < files.len() {
                current_file_idx += 1;
                let next_file = &files[current_file_idx];
                match fs::read_to_string(next_file) {
                    Ok(s) => {
                        println!("[main] Feeding {}...", next_file);
                        holder.add_text_with_seed(s.trim(), &mut workq);
                        files_processed += 1;
                    },
                    Err(e) => {
                        println!("[main] Failed to read file {}: {}", next_file, e);
                    }
                }
            }
        } else {
            if files_processed < files.len() {
                let remaining = GRACE_PERIOD_SECS - start_time.elapsed().as_secs();
                println!("[main] Grace period active ({}s remaining), not feeding new files.", remaining);
            }
        }
        // Only exit when all files are processed and queues are empty
        if files_processed == files.len() && workq_depth == 0 && dbq_depth == 0 {
            break;
        }
    }
    println!("[main] Main loop exited");
    let ortho_opt = db.get_optimal();
    if let Some(ortho) = ortho_opt {
        println!("Optimal Ortho: {:?}", ortho);
        let interner = holder.get_latest();
        let payload_strings = ortho.payload().iter().map(|opt_idx| {
            opt_idx.map(|idx| interner.string_for_index(idx).to_string())
        }).collect::<Vec<_>>();
        println!("Optimal Ortho (strings): {:?}", payload_strings);
    } else {
        println!("No optimal Ortho found.");
    }
    println!("[main] Exiting.");
}

fn main() {
    dotenv().ok();
    let amqp_url = std::env::var("FOLD_AMQP_URL")
        .expect("FOLD_AMQP_URL environment variable must be set");
    let pg_url = std::env::var("FOLD_PG_URL")
        .expect("FOLD_PG_URL environment variable must be set");
    println!("[main] FOLD_AMQP_URL: {}", amqp_url);
    println!("[main] FOLD_PG_URL: {}", pg_url);
    let use_local = amqp_url == "local" || pg_url == "local";
    if use_local {
        run(MockQueue::new(), MockQueue::new(), InMemoryOrthoDatabase::new());
    } else {
        run(Queue::new("dbq"), Queue::new("main"), PostgresOrthoDatabase::new());
    }
}
