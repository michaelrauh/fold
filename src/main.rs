use fold::Follower;
use fold::interner::FileInternerHolder;
use fold::interner::InternerHolderLike;
use fold::ortho_database::{InMemoryOrthoDatabase, OrthoDatabaseLike};
use fold::queue::{MockQueue, QueueLike};
use dotenv::dotenv;
use std::time::Instant;

fn run<Q: QueueLike, D: OrthoDatabaseLike, H: fold::interner::InternerHolderLike>(
    dbq: &mut Q,
    workq: &mut Q,
    db: &mut D,
    holder: &mut H,
    chapters: Vec<&str>,
) {
    println!("[main] Found {} chapters in e.txt", chapters.len());
    for (i, chapter) in chapters.iter().enumerate() {
        println!("[main] Feeding chapter {}...", i);
        if let Err(e) = holder.add_text_with_seed(chapter, workq) {
            eprintln!("Failed to add text with seed: {:?}", e);
        }
        let mut loop_count: usize = 0;
        let files_fed = 1;
        let printed_final_optimal = false;
        loop {
            if let Err(e) = fold::OrthoFeeder::run_feeder_once(dbq, db, workq) {
                eprintln!("Feeder error: {:?}", e);
            }
            process_with_grace(dbq, workq, db, holder, files_fed, 0, &mut loop_count);
            if workq.len().unwrap_or(0) == 0 && dbq.len().unwrap_or(0) == 0 {
                if !printed_final_optimal {
                    let ortho_opt = db.get_optimal();
                    if let Ok(Some(ortho)) = ortho_opt {
                        println!("[main] Final Optimal Ortho: {:?}", ortho);
                        if let Some(interner) = holder.get_latest() {
                            let payload_strings = ortho.payload().iter().map(|opt_idx| {
                                opt_idx.map(|idx| interner.string_for_index(idx).to_string())
                            }).collect::<Vec<_>>();
                            println!("[main] Final Optimal Ortho (strings): {:?}", payload_strings);
                        } else {
                            println!("[main] No interner found for final optimal ortho.");
                        }
                    } else {
                        println!("[main] No final optimal Ortho found.");
                    }
                    println!("[main] Exiting chapter loop.");
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

fn process_with_grace<Q: QueueLike, D: OrthoDatabaseLike, H: fold::interner::InternerHolderLike>(
    dbq: &mut Q,
    workq: &mut Q,
    db: &mut D,
    holder: &mut H,
    files_processed: usize,
    grace_period_secs: u64,
    loop_count: &mut usize,
) {
    let mut follower = Follower::new();
    let grace_start = Instant::now();
    loop {
        if let Err(e) = follower.run_follower_once(db, workq, holder) {
            eprintln!("Follower error: {:?}", e);
        }
        if let Err(e) = fold::run_worker_once(workq, dbq, holder) {
            eprintln!("Worker error: {:?}", e);
        }
        *loop_count += 1;
        let workq_depth = workq.len().unwrap_or(0);
        let dbq_depth = dbq.len().unwrap_or(0);
        let db_len = db.len().unwrap_or(0);
        println!("[main] raw lens: workq_depth={}, dbq_depth={}, db_len={}", workq_depth, dbq_depth, db_len);
        let latest_version = holder.latest_version();
        println!("[main] LOOP_COUNT: {}", *loop_count);
        println!(
            "[main] workq depth: {}, dbq depth: {}, db len: {}, latest interner version: {}, files processed: {}",
            workq_depth, dbq_depth, db_len, latest_version, files_processed
        );
        if *loop_count % 1000 == 0 {
            let ortho_opt = db.get_optimal();
            if let Ok(Some(ortho)) = ortho_opt {
                println!("[main] (file idx: {}) Optimal Ortho: {:?}", files_processed, ortho);
                if let Some(interner) = holder.get_latest() {
                    let payload_strings = ortho.payload().iter().map(|opt_idx| {
                        opt_idx.map(|idx| interner.string_for_index(idx).to_string())
                    }).collect::<Vec<_>>();
                    println!("[main] Optimal Ortho (strings): {:?}", payload_strings);
                } else {
                    println!("[main] No interner found for optimal ortho.");
                }
            } else {
                println!("[main] No optimal Ortho found.");
            }
        }
        let elapsed = grace_start.elapsed().as_secs();
        if elapsed >= grace_period_secs {
            break;
        } else {
            let remaining = grace_period_secs - elapsed;
            println!("[main] Grace period active ({}s remaining) before next feed.", remaining);
        }
        let workq_len = workq.len().expect("Failed to get workq length");
        let dbq_len = dbq.len().expect("Failed to get dbq length");
        if workq_len == 0 && dbq_len == 0 {
            break;
        }
    }
}

fn main() {
    dotenv().ok();
    let mode = std::env::var("FOLD_MODE").unwrap_or_else(|_| "monolith".to_string());
    if mode != "monolith" {
        println!("[main] Skipping main.rs: FOLD_MODE is not monolith (got '{}')", mode);
        return;
    }
    println!("[main] FOLD_MODE: monolith");
    let endpoint = std::env::var("FOLD_INTERNER_BLOB_ENDPOINT").unwrap_or_else(|_| "(unset)".to_string());
    println!("[main][debug] FOLD_INTERNER_BLOB_ENDPOINT: {}", endpoint);
    let file = "e.txt";
    let initial_text = std::fs::read_to_string(&file).unwrap();
    let chapters: Vec<&str> = initial_text.split("CHAPTER").collect();
    let mut dbq = MockQueue::new();
    let mut workq = MockQueue::new();
    let mut db = InMemoryOrthoDatabase::new();
    let mut holder = FileInternerHolder::new().unwrap_or_else(|e| {
        eprintln!("Failed to create holder: {:?}", e);
        std::process::exit(1);
    });
    // Initialize the holder if needed 
    if let Err(e) = holder.add_text_with_seed("", &mut workq) {
        eprintln!("Failed to initialize holder: {:?}", e);
    }
    run(&mut dbq, &mut workq, &mut db, &mut holder, chapters);
}