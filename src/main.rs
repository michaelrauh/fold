use clap::{Parser, Subcommand};
use fold::interner::{FileInternerHolder, InternerHolderLike};
use fold::ortho_database::InMemoryOrthoDatabase;
use fold::{OrthoDatabaseLike, QueueLenLike};
use std::io::{self, Write};
use clap::CommandFactory;

#[derive(Parser)]
#[command(name = "ingestor")]
#[command(about = "Distributed ingestor CLI for fold", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Clone)]
enum Commands {
    /// Show queue sizes (workq, dbq)
    Queues,
    /// show database size
    Database,
    /// Add text from a file to the queue/interner
    /// Print latest optimal ortho and its strings
    PrintOptimal,
    /// Split a local file by a delimiter (writes parts and deletes original)
    SplitFile {
        /// File path (relative to interner data dir or absolute)
        file_path: String,
        /// Delimiter to split the file
        delimiter: String,
    },
    /// Feed a local file into the interner and delete it
    FeedFile {
        /// File path (relative to interner data dir or absolute)
        file_path: String,
    },
    /// Stage (upload) a local file into the interner data directory (put-s3 equivalent)
    /// File will be stored under its basename in the interner data dir (no rename)
    StageFile {
        /// Source file path on the local filesystem
        file_path: String,
    },
    /// Delete files under a given size threshold (in bytes)
    CleanFilesSmall {
        /// Size threshold in bytes
        size: usize,
    },
    /// Delete a specific interner version
    CleanInterner {
        /// Interner version to delete
        version: usize,
    },
    InternerVersions,
    /// Show counts of orthos per version
    VersionCounts,
    /// List objects in the interner data dir
    ListFiles,
    /// Print the contents of a staged file (relative to interner data dir)
    CatFile {
        /// File key/path in the interner data dir
        file_path: String,
    },
    /// Run the ingest loop for N iterations: worker -> feeder -> follower.
    /// Exits early if all queues are empty and there is only one interner version.
    Process {
        /// Number of iterations to run (positive integer)
        iterations: usize,
    },
    /// Save DB and queues to the configured state location (overwrites files)
    SaveState,
    /// Load DB and queues from the configured state location
    LoadState,
}


// No S3 anymore; all paths are plain file paths relative to the interner data dir.

// Disk-backed client that emulates the previous S3 behaviour but uses a local directory.
struct DiskClient {
    base: std::path::PathBuf,
}

impl DiskClient {
    fn new() -> Self {
        // Default directory can be overridden with FOLD_INTERNER_BLOB_DIR
        let base = std::env::var("FOLD_INTERNER_BLOB_DIR").map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("./internerdata"));
        let _ = std::fs::create_dir_all(&base);
        DiskClient { base }
    }

    fn get_object_blocking(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.base.join(key);
        std::fs::read(path).ok()
    }

    fn put_object_blocking(&self, key: &str, data: &[u8]) -> std::io::Result<()> {
        let path = self.base.join(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, data)
    }

    fn delete_object_blocking(&self, key: &str) -> std::io::Result<()> {
        let path = self.base.join(key);
        if path.exists() {
            std::fs::remove_file(path)
        } else {
            Ok(())
        }
    }

    fn list_objects(&self) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.base) {
            for de in entries.flatten() {
                if let Ok(meta) = de.metadata() {
                    if meta.is_file() {
                        if let Some(name) = de.file_name().to_str() {
                            out.push((name.to_string(), meta.len() as usize));
                        }
                    }
                }
            }
        }
        out
    }
}

fn run_command(
    command: Commands,
    dbq: &mut fold::queue::MockQueue,
    workq: &mut fold::queue::MockQueue,
    holder: &mut FileInternerHolder,
    db: &mut InMemoryOrthoDatabase,
    disk_client: &DiskClient,
) {
    match command {
        Commands::Queues => {
            println!("workq depth: {}", workq.len().unwrap_or(0));
            println!("dbq depth: {}", dbq.len().unwrap_or(0));
        }
        Commands::PrintOptimal => {
            let ortho_opt = db.get_optimal();
            if let Ok(Some(ortho)) = ortho_opt {
                println!("Optimal Ortho: {:?}", ortho);
                if let Some(interner) = holder.get_latest() {
                    let payload_strings = ortho.payload().iter().map(|opt_idx| {
                        opt_idx.map(|idx| interner.string_for_index(idx).to_string())
                    }).collect::<Vec<_>>();
                    println!("Optimal Ortho (strings): {:?}", payload_strings);
                } else {
                    println!("No interner found for optimal ortho.");
                }
            } else {
                println!("No optimal Ortho found.");
            }
        }
        Commands::SplitFile { file_path, delimiter } => {
            let key = file_path;
            if let Some(data) = disk_client.get_object_blocking(&key) {
                let s = String::from_utf8_lossy(&data);
                let parts: Vec<&str> = s.split(&delimiter).collect();
                // Write each part to disk as a new file
                for (i, part) in parts.iter().enumerate() {
                    let new_key = format!("{}-part-{}", key, i);
                    disk_client.put_object_blocking(&new_key, part.as_bytes()).expect("Failed to write split part to disk");
                }
                // Delete the original file
                disk_client.delete_object_blocking(&key).ok();
                println!("Split file {} into {} parts, wrote to disk, deleted original.", key, parts.len());
            } else {
                panic!("Failed to split file from path {}", key);
            }
        }
        Commands::FeedFile { file_path } => {
            let key = file_path;
            if let Some(data) = disk_client.get_object_blocking(&key) {
                let s = String::from_utf8_lossy(&data);
                match holder.add_text_with_seed(&s, workq) {
                    Ok(()) => {
                        // After successful feed, save DB and queues before deleting the input file.
                        // State directory can be overridden with FOLD_STATE_DIR env var.
                        let state_dir = std::env::var("FOLD_STATE_DIR").unwrap_or_else(|_| "./state".to_string());
                        let path = std::path::Path::new(&state_dir);
                        if let Err(e) = std::fs::create_dir_all(path) {
                            eprintln!("Failed to create state dir '{}': {}. Not deleting input file.", state_dir, e);
                            println!("Fed file {} into interner but state save failed.", key);
                            return;
                        }
                        let db_path = path.join("db.bin");
                        let workq_path = path.join("workq.bin");
                        let dbq_path = path.join("dbq.bin");
                        let mut save_failed = false;
                        if let Err(e) = db.save_to_path(&db_path) { eprintln!("Failed to save db: {}", e); save_failed = true; }
                        if let Err(e) = workq.save_to_path(&workq_path) { eprintln!("Failed to save workq: {}", e); save_failed = true; }
                        if let Err(e) = dbq.save_to_path(&dbq_path) { eprintln!("Failed to save dbq: {}", e); save_failed = true; }
                        if save_failed {
                            eprintln!("One or more state components failed to save; not deleting input file {}", key);
                            println!("Fed file {} into interner but state save failed.", key);
                        } else {
                            // Only delete the file after successful state save
                            if let Err(e) = disk_client.delete_object_blocking(&key) {
                                eprintln!("Failed to delete input file '{}' after saving state: {}", key, e);
                            } else {
                                println!("Fed file {} into interner, saved state, and deleted original.", key);
                            }
                        }
                    }
                    Err(e) => {
                        panic!("Failed to feed file {} into interner: {}", key, e);
                    }
                }
            } else {
                panic!("Failed to feed from path {}", key);
            }
        }
    Commands::StageFile { file_path } => {
            // Read the source file from the real filesystem
            let src_path = std::path::PathBuf::from(&file_path);
            if !src_path.exists() {
                eprintln!("Source file '{}' does not exist.", file_path);
                return;
            }
            let data = match std::fs::read(&src_path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("Failed to read source file '{}': {}", file_path, e);
                    return;
                }
            };
            let dest_key = src_path.file_name().and_then(|n| n.to_str()).unwrap_or("staged").to_string();
            if let Err(e) = disk_client.put_object_blocking(&dest_key, &data) {
                eprintln!("Failed to stage file to '{}': {}", dest_key, e);
                return;
            }
            println!("Staged '{}' as '{}'", file_path, dest_key);
        }
        Commands::Process { iterations } => {
            println!("Starting process loop for {} iterations", iterations);
            let mut follower = fold::Follower::new();
            let mut cached_interner_opt = holder.get_latest();

            for it in 0..iterations {
                // Early exit: nothing to do and only one (or zero) interner version
                let work_len = workq.len().unwrap_or(0);
                let dbq_len = dbq.len().unwrap_or(0);
                let versions = holder.versions();
                if work_len == 0 && dbq_len == 0 && versions.len() <= 1 {
                    println!("iteration {}: idle (workq=0, dbq=0, interner_versions={}), exiting early", it, versions.len());
                    break;
                }

                let mut did_work = false;

                // Worker phase: single pass (process up to batch_size items)
                let mut processed_in_batch: usize = 0;
                let res = fold::queue::QueueConsumerLike::try_consume_batch_once(workq, 1, |batch| {
                    processed_in_batch = batch.len();
                    for ortho in batch {
                        if cached_interner_opt.is_none() {
                            if let Some(latest) = holder.get_latest() { cached_interner_opt = Some(latest); }
                            else { return Err(fold::FoldError::Interner("No interner available".to_string())); }
                        }
                        if ortho.version() > cached_interner_opt.as_ref().unwrap().version() {
                            if let Some(updated) = holder.get_latest() {
                                println!("[process] refreshing cached interner {} -> {} (ortho version {})", cached_interner_opt.as_ref().unwrap().version(), updated.version(), ortho.version());
                                cached_interner_opt = Some(updated);
                            } else {
                                return Err(fold::FoldError::Interner("No interner found during refresh".to_string()));
                            }
                        }
                        let interner_ref = cached_interner_opt.as_ref().unwrap();
                        fold::process_worker_item_with_cached(ortho, dbq, interner_ref)?;
                    }
                    Ok(())
                });
                if res.is_err() {
                    eprintln!("[process][worker] error: {:?}", res.err());
                } else if processed_in_batch > 0 {
                    did_work = true;
                }

                // Feeder phase: single pass (process up to batch_size DBQ items)
                let mut processed_dbq_batch: usize = 0;
                let res = fold::queue::QueueConsumerLike::try_consume_batch_once(dbq, 1, |batch| {
                    processed_dbq_batch = batch.len();
                    let _ = fold::OrthoFeeder::run_feeder_once(batch, db, workq)?;
                    Ok(())
                });
                if res.is_err() {
                    eprintln!("[process][feeder] error: {:?}", res.err());
                } else if processed_dbq_batch > 0 {
                    did_work = true;
                }

                // Follower phase: single pass
                match follower.run_follower_once(db, dbq, holder) {
                    Ok((bumped, produced_children, _)) => {
                        if bumped + produced_children > 0 { did_work = true; }
                        println!("[process][follower] iter={} bumped={} produced_children={}", it, bumped, produced_children);
                    }
                    Err(e) => { eprintln!("[process][follower] error: {:?}", e); }
                }

                if !did_work {
                    println!("iteration {}: no work produced this iteration, exiting early", it);
                    break;
                }
            }
            println!("Process loop complete.");
        }
        Commands::CleanInterner { version } => {
            let versions = holder.versions();
            if versions.contains(&version) {
                holder.delete(version);
                println!("Deleted interner version {}", version);
            } else {
                println!("Interner version {} not found (available: {:?})", version, versions);
            }
        }
        Commands::SaveState => {
            let state_dir = std::env::var("FOLD_STATE_DIR").unwrap_or_else(|_| "./state".to_string());
            let path = std::path::Path::new(&state_dir);
            let _ = std::fs::create_dir_all(path);
            let db_path = path.join("db.bin");
            let workq_path = path.join("workq.bin");
            let dbq_path = path.join("dbq.bin");
            if let Err(e) = db.save_to_path(&db_path) { eprintln!("Failed to save db: {}", e); } else { println!("Saved db to {:?}", db_path); }
            if let Err(e) = workq.save_to_path(&workq_path) { eprintln!("Failed to save workq: {}", e); } else { println!("Saved workq to {:?}", workq_path); }
            if let Err(e) = dbq.save_to_path(&dbq_path) { eprintln!("Failed to save dbq: {}", e); } else { println!("Saved dbq to {:?}", dbq_path); }
        }
        Commands::LoadState => {
            let state_dir = std::env::var("FOLD_STATE_DIR").unwrap_or_else(|_| "./state".to_string());
            let path = std::path::Path::new(&state_dir);
            let db_path = path.join("db.bin");
            let workq_path = path.join("workq.bin");
            let dbq_path = path.join("dbq.bin");
            if db_path.exists() {
                if let Err(e) = db.load_from_path(&db_path) { eprintln!("Failed to load db: {}", e); } else { println!("Loaded db from {:?}", db_path); }
            } else { println!("No db file at {:?}", db_path); }
            if workq_path.exists() {
                if let Err(e) = workq.load_from_path(&workq_path) { eprintln!("Failed to load workq: {}", e); } else { println!("Loaded workq from {:?}", workq_path); }
            } else { println!("No workq file at {:?}", workq_path); }
            if dbq_path.exists() {
                if let Err(e) = dbq.load_from_path(&dbq_path) { eprintln!("Failed to load dbq: {}", e); } else { println!("Loaded dbq from {:?}", dbq_path); }
            } else { println!("No dbq file at {:?}", dbq_path); }
        }
        Commands::ListFiles => {
            let mut objects = disk_client.list_objects();
            objects.sort_by_key(|(k, _)| k.clone());
            for (name, size) in objects {
                println!("{}\t{} bytes", name, size);
            }
        }
        Commands::CatFile { file_path } => {
            let key = file_path;
            match disk_client.get_object_blocking(&key) {
                Some(data) => {
                    let s = String::from_utf8_lossy(&data);
                    println!("{}", s);
                }
                None => {
                    eprintln!("File '{}' not found in interner data dir.", key);
                }
            }
        }
    Commands::CleanFilesSmall { size } => {
            let mut deleted = Vec::new();
            let objects = disk_client.list_objects();
            for (key, size_bytes) in objects {
                if size_bytes < size {
                    let _ = disk_client.delete_object_blocking(&key);
                    deleted.push((key, size_bytes));
                }
            }
            println!("Deleted objects under {} bytes:", size);
            for (key, sz) in deleted {
                println!("{} ({} bytes)", key, sz);
            }
        }
        Commands::Database => {
            let db_len = db.len().unwrap_or(0);
            println!("Database length: {}", db_len);
        }
        Commands::InternerVersions => {
            let versions = holder.versions();
            println!("Interner versions: {:?}", versions);
        }
        Commands::VersionCounts => {
            match db.version_counts() {
                Ok(pairs) => {
                    println!("version\tcount");
                    for (v,c) in pairs { println!("{}\t{}", v, c); }
                }
                Err(e) => eprintln!("Failed to get version counts: {}", e),
            }
        }
    }
}

fn main() {
    // initialize persistent state once
    // NOTE: Do not set process environment variables here; rely on the caller or defaults inside constructors.

    let mut dbq = fold::queue::MockQueue::new();
    let mut workq = fold::queue::MockQueue::new();
    // Use an explicit file location for the interner so we don't rely on environment variables here.
    let mut holder = FileInternerHolder::new_with_path("./interner").expect("Failed to create FileInternerHolder");
    let mut db = InMemoryOrthoDatabase::new();
    let disk_client = DiskClient::new();

    // If the process was invoked with arguments, handle two cases:
    // - If the args include a help flag, print clap help and continue into the REPL.
    // - Otherwise, parse/run a single command and exit.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        if args.iter().any(|a| a == "-h" || a == "--help" || a == "help") {
            // Print help to stdout but do not exit the process; allow the interactive loop to follow.
            Cli::command().print_help().ok();
            println!();
        } else {
            // No help requested: parse and run one-time command and exit.
            let cli = Cli::parse();
            run_command(cli.command, &mut dbq, &mut workq, &mut holder, &mut db, &disk_client);
            println!("Done.");
            return;
        }
    }

    println!("Entering interactive command loop. Type 'exit' or 'quit' to leave.");
    let stdin = io::stdin();
    loop {
        print!("> ");
        io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.read_line(&mut line).is_err() { break; }
        let line = line.trim();
        if line.is_empty() { continue; }
        if line == "exit" || line == "quit" { break; }

        // Allow printing help from inside the REPL at any time.
        if line == "help" || line == "-h" || line == "--help" {
            Cli::command().print_help().ok();
            println!();
            continue;
        }
        if line.starts_with("help ") {
            if let Some(sub) = line.split_whitespace().nth(1) {
                // Try to find and print the subcommand help
                let mut found = false;
                for sc in Cli::command().get_subcommands() {
                    if sc.get_name() == sub {
                        sc.clone().print_help().ok();
                        println!();
                        found = true;
                        break;
                    }
                }
                if !found {
                    println!("Unknown subcommand '{}'.", sub);
                }
                continue;
            }
        }

        // Simple whitespace split for arguments. Quoting not supported in this minimal loop.
        let mut args: Vec<String> = vec!["fold".to_string()];
        args.extend(line.split_whitespace().map(|s| s.to_string()));

        match Cli::try_parse_from(args) {
            Ok(cli) => {
                run_command(cli.command, &mut dbq, &mut workq, &mut holder, &mut db, &disk_client);
                println!("Done.");
            }
            Err(e) => {
                println!("Error parsing command: {}", e);
            }
        }
    }
    println!("Exiting.");
}
