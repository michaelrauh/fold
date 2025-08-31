use clap::{Parser, Subcommand};
use fold::interner::{BlobInternerHolder, InternerHolderLike};
use fold::{QueueProducer};

#[derive(Parser)]
#[command(name = "interner_util")]
#[command(about = "Interner utility for fold", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show interner versions
    InternerVersions,
    /// Feed text from S3 into interner
    FeedS3 {
        /// S3 path (e.g., s3://bucket/key)
        s3_path: String,
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
    rt: std::sync::Arc<tokio::runtime::Runtime>,
}

impl S3Client {
    fn new() -> Self {
        let bucket = std::env::var("FOLD_INTERNER_BLOB_BUCKET").unwrap();
        let endpoint_url = std::env::var("FOLD_INTERNER_BLOB_ENDPOINT").unwrap();
        let access_key = std::env::var("FOLD_INTERNER_BLOB_ACCESS_KEY").unwrap();
        let secret_key = std::env::var("FOLD_INTERNER_BLOB_SECRET_KEY").unwrap();
        let region = "us-east-1";
        let rt = std::sync::Arc::new(tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"));
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
    let mut holder = BlobInternerHolder::new().expect("Failed to create BlobInternerHolder");
    
    match cli.command {
        Commands::InternerVersions => {
            let versions = holder.versions();
            println!("Interner versions: {:?}", versions);
        }
        Commands::FeedS3 { s3_path } => {
            let s3_client = S3Client::new();
            let mut workq = QueueProducer::new("workq").expect("queue creation failed");
            
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
    }
}