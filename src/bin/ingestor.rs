use clap::{Parser, Subcommand};
use fold::interner::{BlobInternerHolder, InternerHolderLike};
use fold::ortho_database::PostgresOrthoDatabase;
use fold::{OrthoDatabaseLike, QueueLenLike};
use std::sync::Arc;
use dotenv::dotenv;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ingestor")]
#[command(about = "Distributed ingestor CLI for fold", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
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
    /// Ingest from an S3 object, splitting by a delimiter
    IngestS3Split {
        /// S3 path (e.g., s3://bucket/key)
        s3_path: String,
        /// Delimiter to split the object
        delimiter: String,
    },
    /// Feed an S3 object into the interner and delete it
    FeedS3 {
        /// S3 path (e.g., s3://bucket/key)
        s3_path: String,
    },
    /// Delete S3 objects under a given size threshold (in bytes)
    CleanS3Small {
        /// Size threshold in bytes
        size: usize,
    },
    InternerVersions,
    /// Show counts of orthos per version
    VersionCounts,
}

// Helper to parse s3://bucket/key
fn parse_s3_path(s3_path: &str) -> Option<(String, String)> {
    let s = s3_path.strip_prefix("s3://")?;
    let mut parts = s.splitn(2, '/');
    let bucket = parts.next()?.to_string();
    let key = parts.next()?.to_string();
    Some((bucket, key))
}

struct S3Client {
    client: aws_sdk_s3::Client,
    bucket: String,
    rt: Arc<tokio::runtime::Runtime>,
}

impl S3Client {
    fn new() -> Self {
        let bucket = std::env::var("FOLD_INTERNER_BLOB_BUCKET").unwrap();
        let endpoint_url = std::env::var("FOLD_INTERNER_BLOB_ENDPOINT").unwrap();
        let access_key = std::env::var("FOLD_INTERNER_BLOB_ACCESS_KEY").unwrap();
        let secret_key = std::env::var("FOLD_INTERNER_BLOB_SECRET_KEY").unwrap();
        let region = "us-east-1";
        let rt = Arc::new(tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"));
        let config = rt.block_on(async {
            aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(region)
                .endpoint_url(&endpoint_url)
                .credentials_provider(aws_sdk_s3::config::Credentials::new(
                    &access_key,
                    &secret_key,
                    None,
                    None,
                    "minio",
                ))
                .load()
                .await
        });
        let s3_config = aws_sdk_s3::config::Builder::from(&config)
            .force_path_style(true)
            .build();
        let client = aws_sdk_s3::Client::from_conf(s3_config);
        Self { client, bucket, rt }
    }
    fn get_object_blocking(&self, key: &str) -> Option<Vec<u8>> {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        self.rt.block_on(async move {
            match client.get_object().bucket(&bucket).key(key).send().await {
                Ok(resp) => {
                    let data = resp.body.collect().await.ok()?;
                    let bytes = data.into_bytes();
                    Some(bytes.to_vec())
                },
                Err(_e) => None,
            }
        })
    }
}

fn main() {
    dotenv().ok();
    fold::init_tracing("fold-ingestor");
    let cli = Cli::parse();
    // shared resources
    let mut dbq = fold::queue::QueueConsumer::new("dbq");
    let mut workq = fold::queue::QueueProducer::new("workq").expect("queue creation failed");
    let mut holder = BlobInternerHolder::new().expect("Failed to create BlobInternerHolder");
    let mut db = PostgresOrthoDatabase::new();
    let s3_client = S3Client::new();

    match cli.command {
        Some(Commands::Queues) => {
            println!("workq depth: {}", workq.len().unwrap_or(0));
            println!("dbq depth: {}", dbq.len().unwrap_or(0));
        }
    Some(Commands::PrintOptimal) => {
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
    Some(Commands::IngestS3Split { s3_path, delimiter }) => {
                if let Some((bucket, key)) = parse_s3_path(&s3_path) {
                    if let Some(data) = s3_client.get_object_blocking(&key) {
                        let s = String::from_utf8_lossy(&data);
                        let parts: Vec<&str> = s.split(&delimiter).collect();
                        // Write each part to S3 as a new object
                        for (i, part) in parts.iter().enumerate() {
                            let new_key = format!("{}-part-{}", key, i);
                            s3_client.rt.block_on(s3_client.client.put_object()
                                .bucket(&bucket)
                                .key(&new_key)
                                .body(aws_sdk_s3::primitives::ByteStream::from(part.as_bytes().to_vec()))
                                .send())
                                .expect("Failed to write split part to S3");
                        }
                        // Delete the original object
                        s3_client.rt.block_on(s3_client.client.delete_object()
                            .bucket(&bucket)
                            .key(&key)
                            .send())
                            .expect("Failed to delete original S3 object after split");
                        println!("Split S3 object {} into {} parts, wrote to bucket {}, deleted original.", s3_path, parts.len(), bucket);
                    } else {
                        panic!("Failed to ingest from S3 path {}", s3_path);
                    }
                } else {
                    panic!("Invalid S3 path: {}", s3_path);
                }
            }
    Some(Commands::FeedS3 { s3_path }) => {
                if let Some((bucket, key)) = parse_s3_path(&s3_path) {
                    if let Some(data) = s3_client.get_object_blocking(&key) {
                        let s = String::from_utf8_lossy(&data);
                        match holder.add_text_with_seed(&s, &mut workq) {
                            Ok(()) => {
                                // Only delete the object after successful feed
                                s3_client.rt.block_on(s3_client.client.delete_object()
                                    .bucket(&bucket)
                                    .key(&key)
                                    .send())
                                    .expect("Failed to delete S3 object after feed");
                                println!("Fed S3 object {} into interner and deleted original.", s3_path);
                            }
                            Err(e) => {
                                panic!("Failed to feed S3 object {} into interner: {}", s3_path, e);
                            }
                        }
                    } else {
                        panic!("Failed to feed from S3 path {}", s3_path);
                    }
                } else {
                    panic!("Invalid S3 path: {}", s3_path);
                }
            }
    Some(Commands::CleanS3Small { size }) => {
                let bucket = std::env::var("FOLD_INTERNER_BLOB_BUCKET").unwrap();
                let client = &s3_client.client;
                let rt = &s3_client.rt;
                let mut deleted = Vec::new();
                let objects = rt.block_on(async {
                    client.list_objects_v2().bucket(&bucket).send().await
                });
                if let Ok(resp) = objects {
                    for obj in resp.contents() {
                        let key = obj.key().unwrap_or("");
                        let size_bytes = obj.size().unwrap_or(0) as usize;
                        if size_bytes < size {
                            let _ = rt.block_on(async {
                                client.delete_object().bucket(&bucket).key(key).send().await
                            });
                            deleted.push((key.to_string(), size_bytes));
                        }
                    }
                }
                println!("Deleted objects under {} bytes:", size);
                for (key, sz) in deleted {
                    println!("{} ({} bytes)", key, sz);
                }
            }
    Some(Commands::Database) => {
                let db_len = db.len().unwrap_or(0);
                println!("Database length: {}", db_len);
                match db.total_bytes() {
                    Ok(b) => println!("Database total bytes: {}", b),
                    Err(e) => eprintln!("Failed to compute total bytes: {}", e),
                }
                match db.version_byte_sizes() {
                    Ok(pairs) => {
                        println!("version\tbytes");
                        for (v, b) in pairs { println!("{}\t{}", v, b); }
                    }
                    Err(e) => eprintln!("Failed to compute per-version bytes: {}", e),
                }
            }
    Some(Commands::InternerVersions) => {
                let versions = holder.versions();
                println!("Interner versions: {:?}", versions);
            }
        Some(Commands::VersionCounts) => {
            match db.version_counts() {
                Ok(pairs) => {
                    println!("version\tcount");
                    for (v,c) in pairs { println!("{}\t{}", v, c); }
                }
                Err(e) => eprintln!("Failed to get version counts: {}", e),
            }
        }
        None => {
            // Default daemon mode: periodically print queue/db sizes and wait
            println!("[ingestor] starting daemon mode (no subcommand provided)");
            while holder.get_latest().is_none() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            loop {
                let workq_len = workq.len().unwrap_or(0);
                let dbq_len = dbq.len().unwrap_or(0);
                let db_len = db.len().unwrap_or(0);
                // Emit three concise, one-line summaries to match the behavior of
                // `make queue-count`, `make db-count`, and `make version-counts`.
                // All values are counts; no byte metrics are logged.
                let version_pairs = match db.version_counts() {
                    Ok(v) => v,
                    Err(_) => Vec::new(),
                };
                // Compact into comma-separated `version:count` pairs.
                let vc_line = if version_pairs.is_empty() {
                    "-".to_string()
                } else {
                    version_pairs.into_iter().map(|(v,c)| format!("{}:{}", v, c)).collect::<Vec<_>>().join(",")
                };
                // Single-line status combining queues, db length, and version counts.
                println!(
                    "[ingestor][status] workq={} dbq={} db_len={} versions={}",
                    workq_len, dbq_len, db_len, vc_line
                );
                std::thread::sleep(Duration::from_secs(10));
            }
        }
    }
}
