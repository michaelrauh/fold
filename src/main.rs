use fold::Follower;
use fold::interner::{FileInternerHolder, BlobInternerHolder};
use fold::interner::InternerHolderLike;
use fold::ortho_database::{InMemoryOrthoDatabase, OrthoDatabaseLike};
use fold::queue::{Queue, MockQueue, QueueLike};
use fold::PostgresOrthoDatabase;
use tracing_subscriber::EnvFilter;
use std::fs;
use std::thread::sleep;
use dotenv::dotenv;
use std::time::Instant;
use opentelemetry::{KeyValue};
use opentelemetry_sdk::{Resource, trace as sdktrace};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use opentelemetry::trace::TracerProvider;

fn run<Q: QueueLike, D: OrthoDatabaseLike, H: fold::interner::InternerHolderLike>(
    mut dbq: Q,
    mut workq: Q,
    mut db: D,
    mut holder: H,
    _initial_file: &str,
) {
    use std::time::{Duration, Instant};
    let mut loop_count: usize = 0;
    let mut files_fed = 1; // 0.txt is already seeded
    let total_files = 29; // 0.txt to 28.txt
    let mut last_feed = Instant::now();
    let mut all_files_fed = false;
    let mut next_file_idx = 1;
    let printed_final_optimal = false;
    // Main processing loop
    loop {
        // Always process queues
        fold::OrthoFeeder::run_feeder_once(&mut dbq, &mut db, &mut workq);
        process_with_grace(&mut dbq, &mut workq, &mut db, &mut holder, files_fed, 0, &mut loop_count);
        // Feed logic
        if !all_files_fed {
            let queue_depth = workq.len() + dbq.len();
            let interner_count = holder.versions().len();
            let enough_time = last_feed.elapsed() >= Duration::from_secs(60);
            if queue_depth < 1000 && interner_count < 2 && enough_time && next_file_idx < total_files {
                let file = format!("{}.txt", next_file_idx);
                let s = fs::read_to_string(&file).expect(&format!("[main] Failed to read file {}", file));
                println!("[main] Feeding {}...", file);
                holder.add_text_with_seed(&s, &mut workq);
                println!("[main] Queues and InternerHolder updated");
                last_feed = Instant::now();
                files_fed += 1;
                next_file_idx += 1;
                if next_file_idx == total_files {
                    all_files_fed = true;
                }
            }
        } else {
            // After all files are fed, wait for all queues to empty, then print optimal and exit
            if workq.len() == 0 && dbq.len() == 0 {
                if !printed_final_optimal {
                    let ortho_opt = db.get_optimal();
                    if let Some(ortho) = ortho_opt {
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
                    println!("[main] Exiting.");
                }
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
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
        follower.run_follower_once(db, workq, holder);
        fold::run_worker_once(workq, dbq, holder);
        *loop_count += 1;
        let workq_depth = workq.len();
        let dbq_depth = dbq.len();
        let db_len = db.len();
        println!("[main] raw lens: workq_depth={}, dbq_depth={}, db_len={}", workq_depth, dbq_depth, db_len);
        let latest_version = holder.latest_version();
        println!("[main] LOOP_COUNT: {}", *loop_count);
        println!(
            "[main] workq depth: {}, dbq depth: {}, db len: {}, latest interner version: {}, files processed: {}",
            workq_depth, dbq_depth, db_len, latest_version, files_processed
        );
        // Periodically print optimal ortho
        if *loop_count % 1000 == 0 {
            let ortho_opt = db.get_optimal();
            if let Some(ortho) = ortho_opt {
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
        // If all queues are empty, exit early
        if workq.len() == 0 && dbq.len() == 0 {
            break;
        }
    }
}

fn main() {
    dotenv().ok();
    let mode = std::env::var("FOLD_MODE").unwrap_or_else(|_| "monolith".to_string());
    if mode == "distributed" {
        // Jaeger/OTLP tracing setup
        let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint("http://jaeger:4318/v1/traces")
            .build()
            .expect("Failed to build OTLP exporter");
        let resource = Resource::builder_empty()
            .with_attributes(vec![KeyValue::new("service.name", "fold-app")])
            .build();
        let tracer_provider = sdktrace::SdkTracerProvider::builder()
            .with_simple_exporter(otlp_exporter)
            .with_resource(resource)
            .build();
        let tracer = tracer_provider.tracer("fold-app");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        tracing_subscriber::registry()
            .with(otel_layer)
            .with(tracing_subscriber::fmt::layer())
            .init();
    } else {
        // Stdout tracing setup with full span events and env filter
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
            )
            .with(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("info,fold=trace"))
            )
            .init();
    }
    println!("[main] FOLD_MODE: {}", mode);
    let endpoint = std::env::var("FOLD_INTERNER_BLOB_ENDPOINT").unwrap_or_else(|_| "(unset)".to_string());
    println!("[main][debug] FOLD_INTERNER_BLOB_ENDPOINT: {}", endpoint);
    if mode == "monolith" {
        // Removed automatic cleaning of interner directory; use 'make clean' instead
    }
    let file = "0.txt";
    let initial_text = std::fs::read_to_string(&file).unwrap();
    if mode == "monolith" {
        // Monolith: always use FileInternerHolder, MockQueue, InMemoryOrthoDatabase
        let dbq = MockQueue::new();
        let mut workq = MockQueue::new();
        let db = InMemoryOrthoDatabase::new();
        let holder = FileInternerHolder::with_seed(&initial_text, &mut workq);
        run(dbq, workq, db, holder, file);
    } else {
        // Distributed: do NOT run feeder or follower or worker code
        println!("[main][debug] Using endpoint for BlobInternerHolder: {}", endpoint);
        let dbq = Queue::new("dbq");
        let mut workq = Queue::new("workq");
        let mut holder = BlobInternerHolder::with_seed(&initial_text, &mut workq);
        let mut loop_count: usize = 0;
        let mut files_fed = 1;
        let total_files = 29;
        let mut last_feed = Instant::now();
        let mut all_files_fed = false;
        let mut next_file_idx = 1;
        let mut db = PostgresOrthoDatabase::new();
        loop {
            // Do NOT run_worker_once, run_feeder_once, or run_follower_once in distributed mode
            // Feed logic only
            let workq_depth = workq.len();
            let dbq_depth = dbq.len();
            let db_len = db.len();
            println!("[main] raw lens: workq_depth={}, dbq_depth={}, db_len={}", workq_depth, dbq_depth, db_len);
            let latest_version = holder.latest_version();
            println!("[main] LOOP_COUNT: {}", loop_count);
            println!(
                "[main] workq depth: {}, dbq depth: {}, db len: {}, latest interner version: {}, files processed: {}",
                workq_depth, dbq_depth, db_len, latest_version, files_fed
            );
            sleep(std::time::Duration::from_millis(1000));
            if !all_files_fed {
                let queue_depth = workq.len() + dbq.len();
                let interner_count = holder.versions().len();
                let enough_time = last_feed.elapsed() >= std::time::Duration::from_secs(60);
                if queue_depth < 1000 && interner_count < 2 && enough_time && next_file_idx < total_files {
                    let file = format!("{}.txt", next_file_idx);
                    let s = std::fs::read_to_string(&file).expect(&format!("[main] Failed to read file {}", file));
                    println!("[main] Feeding {}...", file);
                    holder.add_text_with_seed(&s, &mut workq);
                    println!("[main] Queues and InternerHolder updated");
                    last_feed = Instant::now();
                    files_fed += 1;
                    next_file_idx += 1;
                    if next_file_idx == total_files {
                        all_files_fed = true;
                    }
                }
            } else {
                if workq.len() == 0 && dbq.len() == 0 {
                    let ortho_opt = db.get_optimal();
                    if let Some(ortho) = ortho_opt {
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
                    println!("[main] Exiting.");
                    break;
                }
            }
            loop_count += 1;
        }
    }
}