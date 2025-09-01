use clap::{Parser, Subcommand};
use fold::interner::{FileInternerHolder, InternerHolderLike};
use fold::ortho_database::InMemoryOrthoDatabase;
use fold::{OrthoDatabaseLike, QueueLenLike};

#[derive(Parser)]
#[command(name = "ingestor")]
#[command(about = "Distributed ingestor CLI for fold", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show queue sizes (workq, dbq)
    Queues,
    /// show database size
    Database,
    /// Add text from a file to the queue/interner
    /// Print latest optimal ortho and its strings
    PrintOptimal,
    /// Split from a local file by a delimiter (writes parts and deletes original)
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
    /// Delete files under a given size threshold (in bytes)
    CleanFilesSmall {
        /// Size threshold in bytes
        size: usize,
    },
    InternerVersions,
    /// Show counts of orthos per version
    VersionCounts,
}

// All paths are plain file paths now.

// Disk-backed client for ingestor: stores blobs under ./internerdata by default
struct DiskClient {
    base: std::path::PathBuf,
}

impl DiskClient {
    fn new() -> Self {
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

fn main() {
    let cli = Cli::parse();
    let mut dbq = fold::queue::QueueConsumer::new("dbq");
    let mut workq = fold::queue::QueueProducer::new("workq").expect("queue creation failed");
    let mut holder = FileInternerHolder::new().expect("Failed to create FileInternerHolder");
    let mut db = InMemoryOrthoDatabase::new();
    let disk_client = DiskClient::new();
    match cli.command {
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
        if let Some(data) = disk_client.get_object_blocking(&file_path) {
                    let s = String::from_utf8_lossy(&data);
                    let parts: Vec<&str> = s.split(&delimiter).collect();
                    // Write each part back to disk as new objects
                    for (i, part) in parts.iter().enumerate() {
                        let new_key = format!("{}-part-{}", file_path, i);
                        disk_client.put_object_blocking(&new_key, part.as_bytes()).expect("Failed to write split part to disk");
                    }
                    // Delete the original object
                    disk_client.delete_object_blocking(&file_path).ok();
                    println!("Split file {} into {} parts, wrote to disk, deleted original.", file_path, parts.len());
                } else {
                    panic!("Failed to split file from path {}", file_path);
                }
            }
        Commands::FeedFile { file_path } => {
                if let Some(data) = disk_client.get_object_blocking(&file_path) {
                    let s = String::from_utf8_lossy(&data);
                    match holder.add_text_with_seed(&s, &mut workq) {
                        Ok(()) => {
                            // Only delete the object after successful feed
                            disk_client.delete_object_blocking(&file_path).ok();
                            println!("Fed file {} into interner and deleted original.", file_path);
                        }
                        Err(e) => {
                            panic!("Failed to feed file {} into interner: {}", file_path, e);
                        }
                    }
                } else {
                    panic!("Failed to feed from path {}", file_path);
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
                println!("Deleted files under {} bytes:", size);
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
