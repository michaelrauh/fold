use clap::{Parser, Subcommand};
use fold::queue::{Queue, QueueLike};
use fold::interner::{BlobInternerHolder, InternerHolderLike};
use fold::ortho_database::PostgresOrthoDatabase;
use fold::{OrthoDatabaseLike};
use std::sync::Arc;

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
        let bucket = std::env::var("FOLD_INTERNER_BLOB_BUCKET").unwrap_or_else(|_| "internerdata".to_string());
        let endpoint_url = std::env::var("FOLD_INTERNER_BLOB_ENDPOINT").unwrap_or_else(|_| "http://minio:9000".to_string());
        let access_key = std::env::var("FOLD_INTERNER_BLOB_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());
        let secret_key = std::env::var("FOLD_INTERNER_BLOB_SECRET_KEY").unwrap_or_else(|_| "minioadmin".to_string());
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
    let cli = Cli::parse();
    let dbq = Queue::new("dbq").expect("Failed to create dbq");
    let mut workq = Queue::new("workq").expect("Failed to create workq");
    let mut holder = BlobInternerHolder::new().expect("Failed to create BlobInternerHolder");
    let mut db = PostgresOrthoDatabase::new();
    let s3_client = S3Client::new();
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
        Commands::IngestS3Split { s3_path, delimiter } => {
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
        Commands::FeedS3 { s3_path } => {
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
        Commands::CleanS3Small { size } => {
            let bucket = std::env::var("FOLD_INTERNER_BLOB_BUCKET").unwrap_or_else(|_| "internerdata".to_string());
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
        Commands::Database => {
            let db_len = db.len().unwrap_or(0);
            println!("Database length: {}", db_len);
        }
    }
}
