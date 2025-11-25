# Cloud Migration Plan: Coordination-Free Distributed Processing

## Executive Summary

This document outlines a plan to migrate the Fold text processing system to enable distributed processing across multiple Digital Ocean droplets. The key design principle is **no coordination between workers** - each droplet operates completely independently, pulling work from S3 and pushing results back. All intermediate processing uses local disk only, with S3 serving purely as input/output storage.

## Design Philosophy

### Core Principles

1. **No Coordination** - Workers never communicate with each other
2. **No Locking** - No distributed locks, no heartbeats, no job claiming
3. **No Database** - S3 only, no PostgreSQL or other coordination services
4. **Local Disk First** - All processing happens on local disk, S3 is only for input/output
5. **Idempotent Results** - Duplicate work is acceptable; results are merged later
6. **Simple Operations** - Download input → Process locally → Upload output

### Why No Coordination?

The original architecture uses file-move semantics for mutual exclusion. Rather than replicating this complexity in the cloud with databases and locks, we embrace a simpler model:

- **Duplicate work is cheaper than coordination overhead**
- **S3 eventual consistency is sufficient** for input/output
- **No single point of failure** (database)
- **Simpler deployment and operations**
- **Lower cost** (no managed database)

## Current Architecture

### File-Based Operations

The current system relies heavily on local filesystem operations:

1. **State Directory Structure** (`fold_state/`)
   - `input/` - Text files (.txt) and archive directories (.bin) awaiting processing
   - `in_process/` - Work folders for active processing with heartbeat files
   - `results_*/` - DiskBackedQueue directories for ortho results
   - `seen_shards/` - Disk-backed bloom filter shards for deduplication

2. **Processing Model**
   - Single machine, multiple process concurrency
   - File-move semantics for mutual exclusion (move file to `in_process/`)
   - Heartbeat files for crash detection
   - Recovery through file movement back to `input/`

3. **Data Structures Using Disk**
   - **DiskBackedQueue** - Spills orthos to disk files when memory buffer fills
   - **SeenTracker** - LRU sharded HashMap with disk backing

## Target Architecture: Coordination-Free Cloud

### Infrastructure Components

#### 1. Digital Ocean Spaces (S3-Compatible Object Storage)
- **Purpose**: Input/output storage only (not for intermediate state)
- **Buckets**:
  - `fold-input` - Text files to process
  - `fold-archives` - Completed archive results
- **Access**: S3-compatible API using Digital Ocean Spaces access keys

#### 2. Digital Ocean Droplets (Compute)
- **Purpose**: One-off processing tasks
- **Deployment**: Spawned with specific input file assignment
- **Configuration**: Environment variables for S3 credentials + assigned input
- **Lifecycle**: Start → Download input → Process on local disk → Upload output → Shutdown
- **Local Storage**: Use droplet's local SSD for all intermediate state

### Processing Model

```
┌─────────────────────────────────────────────────────────────────┐
│                         S3 (fold-input)                         │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐               │
│  │file1.txt│ │file2.txt│ │file3.txt│ │file4.txt│   ...         │
│  └────┬────┘ └────┬────┘ └────┬────┘ └────┬────┘               │
└───────┼──────────┼──────────┼──────────┼───────────────────────┘
        │          │          │          │
        ▼          ▼          ▼          ▼
   ┌─────────┐┌─────────┐┌─────────┐┌─────────┐
   │Droplet 1││Droplet 2││Droplet 3││Droplet 4│  (Independent)
   │         ││         ││         ││         │
   │ Local   ││ Local   ││ Local   ││ Local   │
   │ Disk    ││ Disk    ││ Disk    ││ Disk    │
   └────┬────┘└────┬────┘└────┬────┘└────┬────┘
        │          │          │          │
        ▼          ▼          ▼          ▼
┌─────────────────────────────────────────────────────────────────┐
│                       S3 (fold-archives)                        │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐            │
│  │archive1.tar.zst│ │archive2.tar.zst│ │archive3.tar.zst│  ...     │
│  └──────────────┘ └──────────────┘ └──────────────┘            │
└─────────────────────────────────────────────────────────────────┘
```

### Worker Lifecycle

Each droplet follows this simple lifecycle:

```
1. STARTUP
   - Droplet spawned with environment: INPUT_FILE=s3://fold-input/file1.txt
   - Initialize local fold_state/ directory on local SSD

2. DOWNLOAD
   - Download assigned input file from S3 to local disk
   - Place in local fold_state/input/

3. PROCESS
   - Run standard fold processing (unchanged from current)
   - All intermediate state (queues, shards, etc.) on local disk
   - No network calls during processing

4. UPLOAD
   - Compress result archive (tar.zst)
   - Upload to S3: s3://fold-archives/{input_name}_{timestamp}.tar.zst

5. SHUTDOWN
   - Droplet terminates
   - Local disk is discarded
```

## Migration Strategy

### Phase 1: Add S3 Input/Output Layer

Minimal changes to existing code - just add S3 download/upload at boundaries:

**New Module**: `src/s3_io.rs`

```rust
use aws_sdk_s3::Client;

pub struct S3IO {
    client: Client,
    input_bucket: String,
    archive_bucket: String,
}

impl S3IO {
    /// Download a file from S3 to local path
    pub async fn download_input(&self, s3_key: &str, local_path: &Path) -> Result<(), FoldError> {
        let resp = self.client.get_object()
            .bucket(&self.input_bucket)
            .key(s3_key)
            .send()
            .await?;
        
        let mut file = File::create(local_path)?;
        let bytes = resp.body.collect().await?.into_bytes();
        file.write_all(&bytes)?;
        Ok(())
    }
    
    /// Upload archive to S3
    pub async fn upload_archive(&self, local_path: &Path, s3_key: &str) -> Result<(), FoldError> {
        let body = ByteStream::from_path(local_path).await?;
        self.client.put_object()
            .bucket(&self.archive_bucket)
            .key(s3_key)
            .body(body)
            .send()
            .await?;
        Ok(())
    }
    
    /// List available input files
    pub async fn list_inputs(&self) -> Result<Vec<String>, FoldError> {
        let resp = self.client.list_objects_v2()
            .bucket(&self.input_bucket)
            .send()
            .await?;
        
        Ok(resp.contents()
            .iter()
            .filter_map(|obj| obj.key().map(String::from))
            .collect())
    }
}
```

**Dependencies** (add to `Cargo.toml`):
```toml
aws-sdk-s3 = "1.0"
aws-config = "1.0"
tokio = { version = "1.0", features = ["rt-multi-thread"] }
zstd = "0.13"
```

### Phase 2: Modify Main Entry Point

Add cloud mode to main.rs:

```rust
fn main() -> Result<(), FoldError> {
    let args = parse_args();
    
    match args.mode {
        Mode::Local => run_local(args)?,      // Existing behavior
        Mode::CloudWorker => run_cloud_worker(args)?,  // New
    }
    
    Ok(())
}

fn run_cloud_worker(args: Args) -> Result<(), FoldError> {
    let runtime = tokio::runtime::Runtime::new()?;
    
    runtime.block_on(async {
        let s3 = S3IO::from_env()?;
        
        // 1. Download assigned input
        let input_file = std::env::var("INPUT_FILE")?;
        let local_input = PathBuf::from("./fold_state/input/").join(&input_file);
        s3.download_input(&input_file, &local_input).await?;
        
        // 2. Process using existing local logic (synchronous)
        // This uses all existing DiskBackedQueue, SeenTracker, etc.
        let config = StateConfig::default();
        file_handler::initialize_with_config(&config)?;
        
        // Process the single file
        process_single_file(&local_input, &config)?;
        
        // 3. Find and upload result archive
        let archives = find_archives(&config)?;
        for archive in archives {
            let compressed = compress_archive(&archive)?;
            let s3_key = format!("{}_{}.tar.zst", 
                input_file.trim_end_matches(".txt"),
                chrono::Utc::now().timestamp());
            s3.upload_archive(&compressed, &s3_key).await?;
        }
        
        Ok(())
    })
}
```

### Phase 3: Archive Compression

Add compression for efficient S3 storage:

```rust
fn compress_archive(archive_dir: &Path) -> Result<PathBuf, FoldError> {
    let output_path = archive_dir.with_extension("tar.zst");
    
    // Create tar archive
    let tar_file = File::create(&output_path)?;
    let zstd_encoder = zstd::Encoder::new(tar_file, 3)?;
    let mut tar = tar::Builder::new(zstd_encoder);
    
    tar.append_dir_all(".", archive_dir)?;
    
    let zstd_encoder = tar.into_inner()?;
    zstd_encoder.finish()?;
    
    Ok(output_path)
}

fn decompress_archive(archive_path: &Path, output_dir: &Path) -> Result<(), FoldError> {
    let file = File::open(archive_path)?;
    let zstd_decoder = zstd::Decoder::new(file)?;
    let mut tar = tar::Archive::new(zstd_decoder);
    
    tar.unpack(output_dir)?;
    Ok(())
}
```

### Phase 4: Deployment Scripts

**Droplet Setup Script** (`deploy/worker.sh`):

```bash
#!/bin/bash
set -e

INPUT_FILE="$1"
DO_SPACES_ACCESS_KEY="$2"
DO_SPACES_SECRET_KEY="$3"

# Install Rust (if not using pre-built image)
if ! command -v cargo &> /dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source $HOME/.cargo/env
fi

# Clone and build
git clone https://github.com/michaelrauh/fold.git
cd fold
cargo build --release

# Set environment
export DO_SPACES_REGION=nyc3
export DO_SPACES_ENDPOINT=https://nyc3.digitaloceanspaces.com
export DO_SPACES_ACCESS_KEY="$DO_SPACES_ACCESS_KEY"
export DO_SPACES_SECRET_KEY="$DO_SPACES_SECRET_KEY"
export INPUT_FILE="$INPUT_FILE"

# Run in cloud worker mode
./target/release/fold --mode cloud-worker

# Shutdown
shutdown -h now
```

**Orchestrator Script** (`deploy/spawn_workers.sh`):

```bash
#!/bin/bash
# Spawns droplets for each input file in S3

# List input files
INPUT_FILES=$(aws s3 ls s3://fold-input/ --endpoint-url https://nyc3.digitaloceanspaces.com | awk '{print $4}')

for file in $INPUT_FILES; do
    echo "Spawning worker for: $file"
    
    # Create droplet via DO API
    doctl compute droplet create "fold-worker-$(date +%s)" \
        --image ubuntu-22-04-x64 \
        --size s-1vcpu-1gb \
        --region nyc3 \
        --user-data "#!/bin/bash
curl -s https://raw.githubusercontent.com/michaelrauh/fold/main/deploy/worker.sh | bash -s '$file' '$DO_SPACES_ACCESS_KEY' '$DO_SPACES_SECRET_KEY'" \
        --wait
done
```

## Handling Duplicate Work

Since workers don't coordinate, the same input might be processed multiple times. This is handled at the merge phase:

### Merge Strategy

```rust
// Archives from the same input file can be identified by name prefix
// e.g., file1_1699900000.tar.zst, file1_1699900001.tar.zst

fn merge_duplicate_archives(archives: Vec<Archive>) -> Archive {
    // Group by input file
    let grouped = group_by_input_file(archives);
    
    // For each group, keep the one with highest ortho count
    // (or merge if they have different results)
    grouped.into_iter()
        .map(|(input, archives)| {
            archives.into_iter()
                .max_by_key(|a| a.ortho_count)
                .unwrap()
        })
        .collect()
}
```

### Eventual Consistency

- Multiple workers may process the same file
- All results are valid (deterministic processing)
- Merge phase picks the best or combines results
- **Duplicate work cost < coordination overhead**

## Local Disk Usage

### All Intermediate State is Local

```
/fold_state/                    # Local SSD on droplet
├── input/                      # Downloaded from S3
│   └── assigned_file.txt
├── in_process/                 # Standard processing
│   └── assigned_file.txt.work/
│       ├── source.txt
│       ├── heartbeat          # Local heartbeat (not for coordination)
│       ├── queue/             # DiskBackedQueue spill
│       └── seen_shards/       # SeenTracker shards
└── results_*/                  # Final results before upload
```

### Why Local Disk?

1. **Performance**: Local SSD is 100x faster than S3 for random access
2. **Cost**: No S3 API charges for intermediate operations
3. **Simplicity**: Existing DiskBackedQueue and SeenTracker work unchanged
4. **Reliability**: No network failures during processing

### Droplet Sizing

- **Minimum**: 1GB RAM, 25GB SSD ($6/month or $0.009/hour)
- **Recommended**: 2GB RAM, 50GB SSD ($12/month or $0.018/hour)
- **Large jobs**: 4GB RAM, 80GB SSD ($24/month or $0.036/hour)

Local SSD is ephemeral (lost on shutdown) which is perfect for this model.

## Configuration

### Environment Variables

```bash
# S3 Configuration (required)
DO_SPACES_REGION=nyc3
DO_SPACES_ENDPOINT=https://nyc3.digitaloceanspaces.com
DO_SPACES_ACCESS_KEY=xxx
DO_SPACES_SECRET_KEY=xxx
DO_SPACES_INPUT_BUCKET=fold-input
DO_SPACES_ARCHIVE_BUCKET=fold-archives

# Worker Configuration
INPUT_FILE=file1.txt           # Assigned input file
LOCAL_STATE_DIR=/fold_state    # Local processing directory
```

### Command-Line Arguments

```bash
./fold --mode local           # Default: existing behavior
./fold --mode cloud-worker    # Cloud: download → process → upload
```

## Change Points Summary

### Files Requiring Changes

1. **`src/main.rs`**
   - Add `--mode` argument parsing
   - Add `run_cloud_worker()` function
   - Keep existing `run_local()` as default

2. **`Cargo.toml`**
   - Add `aws-sdk-s3`, `aws-config`, `tokio`, `zstd`, `tar`
   - Feature flag for cloud support (optional)

### New Files

1. **`src/s3_io.rs`** - S3 download/upload operations
2. **`src/compression.rs`** - Archive compression/decompression
3. **`deploy/worker.sh`** - Droplet worker script
4. **`deploy/spawn_workers.sh`** - Orchestrator script

### Unchanged Files

- `src/file_handler.rs` - No changes needed
- `src/disk_backed_queue.rs` - No changes needed
- `src/seen_tracker.rs` - No changes needed
- `src/interner.rs` - No changes needed
- `src/ortho.rs` - No changes needed

## Cost Estimation

### Monthly Costs (1000 jobs/month)

**Assumptions**:
- 1000 jobs/month
- Average 100MB input per job
- Average 500MB output per job
- Each job takes 2 hours average

**Spaces (S3 Storage)**:
- Storage: 500GB × $0.02/GB = $10/month
- Transfer: 600GB out × $0.01/GB = $6/month
- **Spaces Total**: ~$16/month

**Droplets (Compute)**:
- $0.009/hour (1GB RAM droplet)
- 1000 jobs × 2 hours × $0.009 = $18/month
- **Compute Total**: ~$18/month

**Total Estimated Cost**: ~$34/month

**Comparison to Coordinated Approach**:
- No PostgreSQL: saves $15/month
- Simpler architecture: lower maintenance cost
- Some duplicate work: adds ~10-20% compute cost

## Testing Strategy

### Unit Tests
- Test S3IO with mocked S3 client
- Test compression round-trip
- Test archive listing/filtering

### Integration Tests
- End-to-end cloud worker test (LocalStack for S3)
- Test with various input sizes
- Test failure scenarios (S3 unavailable, etc.)

### Acceptance Tests
- Deploy test droplet with sample input
- Verify archive uploaded correctly
- Download and verify archive contents

## Success Criteria

1. **Functional**:
   - ✓ Worker downloads assigned input from S3
   - ✓ Processing uses local disk only (no network during processing)
   - ✓ Archive uploaded to S3 on completion
   - ✓ Droplet shuts down after completion

2. **Performance**:
   - ✓ No latency penalty vs local processing (network only at boundaries)
   - ✓ Local disk performance same as current

3. **Simplicity**:
   - ✓ No database to manage
   - ✓ No distributed locking
   - ✓ No heartbeat coordination
   - ✓ Workers are completely stateless

4. **Cost**:
   - ✓ Lower than coordinated approach
   - ✓ Pay only for active processing

## Timeline Estimate

- **Phase 1** (S3 I/O): 3-5 days
- **Phase 2** (Main entry point): 2-3 days
- **Phase 3** (Compression): 1-2 days
- **Phase 4** (Deployment): 2-3 days

**Total Estimated Time**: 2-3 weeks

## Security Considerations

### Credentials Management

1. **S3 Access Keys**:
   - Use minimum permissions (read input, write archives)
   - Inject via environment variables only
   - Never commit to repository

2. **Droplet Security**:
   - Use private VPC if available
   - Firewall: block all inbound traffic
   - Auto-terminate after timeout

### Data Protection

1. **At Rest**: S3 encryption by default
2. **In Transit**: HTTPS for all S3 operations
3. **Local Disk**: Ephemeral (lost on shutdown)

## Conclusion

This simplified migration plan eliminates coordination complexity by embracing independent worker processing. Each droplet operates in isolation, downloading its assigned input, processing entirely on local disk, and uploading results. No databases, no locks, no heartbeats.

The key insight is that **duplicate work is acceptable** in exchange for simpler architecture. When processing is deterministic, running the same job twice produces identical results - one can simply be discarded at merge time.

This approach:
- **Reduces operational complexity** (no database to manage)
- **Lowers cost** (no managed database fees)
- **Improves reliability** (no coordination failures)
- **Simplifies debugging** (each worker is independent)
- **Preserves existing code** (minimal changes to core processing)
