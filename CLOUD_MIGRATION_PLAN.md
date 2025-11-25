# Cloud Migration Plan: Coordination-Free Distributed Processing

## Executive Summary

This document outlines a plan to migrate the Fold text processing system to enable distributed processing across multiple Digital Ocean droplets. The key design principle is **no coordination between workers** - each droplet operates completely independently, pulling work from S3 and pushing results back. All intermediate processing uses local disk only, with S3 serving purely as input/output storage.

## Design Philosophy

### Core Principles

1. **S3 Move Semantics** - Claim work by moving from `fold-input/` to `fold-in-process/` (mirrors local file-move)
2. **Local Heartbeat** - Each worker maintains heartbeat for crash recovery (same as current)
3. **No Database** - S3 only, no PostgreSQL or other coordination services
4. **Local Disk First** - All processing happens on local disk, S3 is only for input/output/claiming
5. **Smallest/Largest Selection** - Workers pick smallest txt file or smallest+largest archives (like current)
6. **Simple Operations** - Move to in-process → Download → Process locally → Upload → Delete from in-process

### Work Claiming via S3 Move

The S3 API supports atomic move/rename operations via copy+delete. This mirrors the current local file-move semantics:

- **Claim work**: Move file from `s3://fold-input/` to `s3://fold-in-process/{worker_id}/`
- **On completion**: Upload result to `s3://fold-archives/`, delete from in-process
- **On crash recovery**: Stale heartbeats trigger move back to `fold-input/`
- **No duplicate work**: Only one worker can successfully move a file

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
- **Purpose**: Input, in-process tracking, and output storage
- **Buckets**:
  - `fold-input` - Text files and archives awaiting processing
  - `fold-in-process` - Files currently being processed (with worker_id prefix)
  - `fold-archives` - Completed archive results
- **Access**: S3-compatible API using Digital Ocean Spaces access keys
- **Work Claiming**: Atomic move from input to in-process prevents duplicate work

#### 2. Digital Ocean Droplets (Long-Running Worker Pools)
- **Purpose**: Processing tasks with manual scaling
- **Deployment**: Long-running worker pools (not one-off tasks)
- **Configuration**: Environment variables for S3 credentials
- **Lifecycle**: Continuous loop: claim work (S3 move) → process → upload → repeat
- **Local Storage**: Use droplet's local SSD for all intermediate state
- **Scaling**: Manual addition/removal/resizing of workers based on workload

### Processing Model

```
┌─────────────────────────────────────────────────────────────────┐
│                         S3 (fold-input)                         │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐               │
│  │file1.txt│ │file2.txt│ │arc_sm.bin│ │arc_lg.bin│   ...        │
│  └────┬────┘ └────┬────┘ └────┬────┘ └────┬────┘               │
└───────┼──────────┼──────────┼──────────┼───────────────────────┘
        │          │          │          │
        │    MOVE TO CLAIM    │          │
        ▼          ▼          ▼          ▼
┌─────────────────────────────────────────────────────────────────┐
│                     S3 (fold-in-process)                        │
│  ┌─────────────────────┐ ┌─────────────────────┐               │
│  │ worker-1/file1.txt  │ │ worker-2/arc_sm.bin │               │
│  │ worker-1/heartbeat  │ │ worker-2/arc_lg.bin │               │
│  └──────────┬──────────┘ │ worker-2/heartbeat  │               │
└─────────────┼────────────┴──────────┬──────────────────────────┘
              │                       │
              ▼                       ▼
   ┌────────────────┐     ┌────────────────┐
   │   Worker 1     │     │   Worker 2     │   (Long-Running Pool)
   │   2GB RAM      │     │   4GB RAM      │   (Mixed Sizes)
   │   Local Disk   │     │   Local Disk   │
   │   TUI ─────    │     │   TUI ─────    │   (Per-Worker View)
   └────────┬───────┘     └────────┬───────┘
            │                      │
            ▼                      ▼
┌─────────────────────────────────────────────────────────────────┐
│                       S3 (fold-archives)                        │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐            │
│  │archive1.tar.zst│ │archive2.tar.zst│ │archive3.tar.zst│  ...    │
│  └──────────────┘ └──────────────┘ └──────────────┘            │
└─────────────────────────────────────────────────────────────────┘
```

### Worker Lifecycle (Long-Running)

Each worker runs continuously in a loop:

```
while true:
    1. CHECK S3 AND CLAIM WORK
       - List available files in s3://fold-input/
       - For txt files: pick a random available file
       - For archives: find smallest and largest archives (like current merge logic)
       - CLAIM: Move selected file(s) to s3://fold-in-process/{worker_id}/
       - Update heartbeat timestamp in S3
    
    2. DOWNLOAD
       - Download claimed file(s) from s3://fold-in-process/{worker_id}/ to local disk
       - Initialize local fold_state/ directory
    
    3. PROCESS
       - Run standard fold processing (unchanged from current)
       - TUI displays real-time metrics for this worker
       - All intermediate state on local disk
       - Memory requirements checked at startup (may require resize)
       - Periodic heartbeat updates to S3
    
    4. UPLOAD AND COMPLETE
       - Compress result archive (tar.zst)
       - Upload to S3: s3://fold-archives/{input_name}_{worker_id}_{timestamp}.tar.zst
       - Delete claimed files from s3://fold-in-process/{worker_id}/
    
    5. CLEANUP
       - Clear local fold_state/ directory
       - Loop back to step 1
```

## Worker Pool Management

### Pool Architecture

Workers run as long-lived processes, not one-off tasks. This enables:
- **Per-worker TUI monitoring** - Each worker shows its own metrics dashboard
- **Dynamic memory adjustment** - Resize workers as needed based on runtime requirements
- **Cost efficiency** - No startup/shutdown overhead per job

### Manual Scaling Operations

**Add Worker**:
```bash
# Create new worker droplet
doctl compute droplet create "fold-worker-$(date +%s)" \
    --image fold-worker-snapshot \
    --size s-2vcpu-4gb \
    --region nyc3 \
    --user-data "#!/bin/bash
export DO_SPACES_ACCESS_KEY='...'
export DO_SPACES_SECRET_KEY='...'
cd /opt/fold && ./target/release/fold --mode cloud-worker"
```

**Remove Worker**:
```bash
# Graceful shutdown: wait for current job to complete
ssh worker-ip "pkill -SIGTERM fold"
# Then destroy droplet
doctl compute droplet delete <droplet-id>
```

**Resize Worker** (for memory-intensive jobs):
```bash
# Power off and resize
doctl compute droplet-action resize <droplet-id> --size s-4vcpu-8gb --wait
doctl compute droplet-action power-on <droplet-id>
```

### Size/Count Trade-off

As RAM requirements increase, trade off worker count for worker size:

| Workload | Worker Config | Monthly Cost |
|----------|--------------|--------------|
| Light (vocab <100K) | 4 × 2GB ($12/ea) | $48/month |
| Medium (vocab 100K-500K) | 2 × 4GB ($24/ea) | $48/month |
| Heavy (vocab >500K) | 1 × 8GB ($48/ea) | $48/month |

The total monthly budget can remain constant while adapting to workload requirements.

## Memory Requirements and Droplet Sizing

### Runtime Memory Detection

The `MemoryConfig::calculate()` function determines memory requirements at startup:

```rust
// From src/memory_config.rs
pub fn calculate(interner_bytes: usize, expected_results: usize) -> Self {
    let mut sys = System::new_all();
    sys.refresh_memory();
    
    let total_memory = sys.total_memory() as usize;
    let target_memory = (total_memory * 75) / 100;  // Target 75% of RAM
    
    // Minimum requirements:
    // - Bloom filter: ~2 bytes per item
    // - Queue buffer: 100k orthos minimum (~20MB per queue)
    // - Shards in memory: 50% of total shards
    
    // EXITS if minimum requirements cannot be met
    if available_for_caches < min_required_memory {
        eprintln!("INSUFFICIENT MEMORY");
        std::process::exit(1);
    }
}
```

### Memory Budget Breakdown

| Component | Size Formula | Minimum | Notes |
|-----------|-------------|---------|-------|
| Interner | ~vocabulary size × 50 bytes | Varies | Serialized vocabulary + prefix maps |
| Queue buffers | 2 × 100K × 200 bytes | ~40MB | Work queue + results queue |
| Bloom filter | capacity × 2 bytes | ~2MB | 1M capacity default |
| Seen shards | shards × 10K × 12 bytes | ~7.7MB | 50% of shards in memory |
| Runtime overhead | 20% of total | Varies | Working memory for ortho processing |

### Droplet Size Recommendations

Based on memory requirements, select appropriate droplet size:

| Droplet Size | Total RAM | Available (75%) | Recommended For |
|--------------|-----------|-----------------|-----------------|
| s-1vcpu-1gb | 1GB | 750MB | Small vocab (<50K words) |
| s-1vcpu-2gb | 2GB | 1.5GB | Medium vocab (50K-200K words) |
| s-2vcpu-4gb | 4GB | 3GB | Large vocab (200K-500K words) |
| s-4vcpu-8gb | 8GB | 6GB | Very large vocab (>500K words) |

### Automatic Sizing Check

Workers check memory at startup and will exit if insufficient:

```bash
# Worker startup script checks memory
./target/release/fold --mode cloud-worker

# If output contains "INSUFFICIENT MEMORY":
#   1. Note the required memory from error message
#   2. Resize droplet to appropriate size
#   3. Restart worker
```

## Heartbeat and Recovery

### Local Heartbeat (Per-Worker)

Each worker maintains a local heartbeat file during processing:

```
/fold_state/in_process/{file}.txt.work/heartbeat
```

This heartbeat serves **local recovery only** (not coordination):
- Updated every 100,000 orthos processed
- Grace period: 10 minutes
- On worker restart: recover abandoned local jobs

### Worker Recovery Scenarios

**Scenario 1: Worker Crash During Processing**
```
1. Worker crashes while processing file1.txt
2. Local heartbeat becomes stale
3. On worker restart:
   - Detect stale heartbeat in local fold_state/
   - Move file1.txt back to local input/
   - Resume processing
4. S3 state unchanged (file1.txt still in fold-input/)
```

**Scenario 2: Worker Shutdown (Graceful)**
```
1. Operator sends SIGTERM
2. Worker completes current job
3. Uploads result to S3
4. Worker exits cleanly
```

**Scenario 3: Droplet Destroyed Mid-Job**
```
1. Droplet destroyed (power failure, etc.)
2. Local disk lost (ephemeral)
3. File remains in s3://fold-in-process/{worker_id}/
4. Heartbeat file becomes stale (no updates)
5. Recovery process moves file back to s3://fold-input/
6. Another worker claims and processes it
```

### Heartbeat-Based Recovery

Each worker maintains a heartbeat file in S3 at `s3://fold-in-process/{worker_id}/heartbeat`:
- Updated every 60 seconds during processing
- Contains timestamp of last update
- Grace period: 10 minutes (same as current local heartbeat)

**Recovery Process** (can be run by any worker or scheduled task):
```rust
// Check all worker directories in fold-in-process/
for worker_dir in list_worker_directories() {
    let heartbeat = read_heartbeat(worker_dir);
    
    if heartbeat.age() > GRACE_PERIOD {
        // Worker is stale - move files back to input
        for file in list_files(worker_dir) {
            if file != "heartbeat" {
                move_to_input(file);
            }
        }
        delete_worker_directory(worker_dir);
    }
}
```

### Work Claiming (No Duplicate Work)

Unlike simple "pick and download", workers **claim work** via S3 move:
- Move from `s3://fold-input/file.txt` to `s3://fold-in-process/{worker_id}/file.txt`
- S3 copy+delete is atomic per-object
- Only one worker can successfully claim a file
- Failed claims (file not found) mean another worker got it first

## TUI Dashboard (Per-Worker View)

### Dashboard Design

The TUI is designed to show **one worker's state**, not a global view:

```
┌─ FOLD Dashboard [Time: 1h 23m │ RAM: 1,234 MB / 75%] ─────────┐
│ Mode: Processing Text │ Interner: v3 │ Vocab: 45,678         │
│ Chunks: 10 │ Processed: 3 │ Remaining: 7                      │
│ QBuf: 100,000 │ Bloom: 3,000,000 │ Shards: 32/64 in mem       │
├───────────────────────────────────────────────────────────────┤
│ Current Operation                                              │
│ Processing: chapter_05.txt                                    │
│ Status: Processing orthos                                      │
│ 1,234,567 / 2,000,000 [████████████░░░░░░░░] 62%              │
├───────────────────────────────────────────────────────────────┤
│ Merge Progress │ Optimal Ortho │ Largest Archive              │
│ Completed: 2   │ Volume: 125   │ archive_ch01.bin             │
│ ...            │ Dims: [6,5,5] │ 45,678 orthos                │
└───────────────────────────────────────────────────────────────┘
```

### Key Metrics Displayed

| Metric | Description | Source |
|--------|-------------|--------|
| RAM Usage | Current memory consumption | `sysinfo::System` |
| Queue Buffer | Configured buffer size | `MemoryConfig` |
| Bloom Capacity | Bloom filter size | `MemoryConfig` |
| Shards in Memory | LRU shard count | `MemoryConfig` |
| Queue Depth | Current work queue size | Real-time |
| Seen Size | Total deduplicated IDs | Real-time |
| Optimal Volume | Best ortho found | Real-time |

### Monitoring Multiple Workers

To monitor multiple workers, use separate terminal sessions:

```bash
# Terminal 1 - Worker on droplet-1
ssh fold-worker-1 "cd /opt/fold && ./target/release/fold --mode cloud-worker"

# Terminal 2 - Worker on droplet-2
ssh fold-worker-2 "cd /opt/fold && ./target/release/fold --mode cloud-worker"

# Or use tmux/screen for multiple panes
tmux new-session \; split-window -h \; \
    send-keys 'ssh fold-worker-1 "cd /opt/fold && ./target/release/fold"' Enter \; \
    select-pane -L \; \
    send-keys 'ssh fold-worker-2 "cd /opt/fold && ./target/release/fold"' Enter
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

**Worker Setup Script** (`deploy/worker.sh`):

```bash
#!/bin/bash
set -e

# Long-running worker script
# Run as: ./worker.sh

DO_SPACES_ACCESS_KEY="${DO_SPACES_ACCESS_KEY:?Required}"
DO_SPACES_SECRET_KEY="${DO_SPACES_SECRET_KEY:?Required}"

# Install Rust (if not using pre-built image)
if ! command -v cargo &> /dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source $HOME/.cargo/env
fi

# Clone and build (if needed)
if [ ! -d "/opt/fold" ]; then
    git clone https://github.com/michaelrauh/fold.git /opt/fold
    cd /opt/fold
    cargo build --release
fi

cd /opt/fold

# Set environment
export DO_SPACES_REGION=nyc3
export DO_SPACES_ENDPOINT=https://nyc3.digitaloceanspaces.com
export DO_SPACES_ACCESS_KEY="$DO_SPACES_ACCESS_KEY"
export DO_SPACES_SECRET_KEY="$DO_SPACES_SECRET_KEY"

# Run in cloud worker mode (long-running loop)
./target/release/fold --mode cloud-worker
```

**Pool Management Script** (`deploy/manage_pool.sh`):

```bash
#!/bin/bash
# Manage worker pool: add, remove, resize, list workers

ACTION="$1"
WORKER_ID="$2"
SIZE="${3:-s-1vcpu-2gb}"

case "$ACTION" in
    add)
        echo "Adding worker with size $SIZE..."
        doctl compute droplet create "fold-worker-$(date +%s)" \
            --image fold-worker-snapshot \
            --size "$SIZE" \
            --region nyc3 \
            --ssh-keys "$(doctl compute ssh-key list --format ID --no-header | head -1)" \
            --user-data "#!/bin/bash
export DO_SPACES_ACCESS_KEY='$DO_SPACES_ACCESS_KEY'
export DO_SPACES_SECRET_KEY='$DO_SPACES_SECRET_KEY'
/opt/fold/deploy/worker.sh" \
            --wait
        ;;
    
    remove)
        if [ -z "$WORKER_ID" ]; then
            echo "Usage: $0 remove <droplet-id>"
            exit 1
        fi
        echo "Removing worker $WORKER_ID..."
        # Signal graceful shutdown first
        WORKER_IP=$(doctl compute droplet get "$WORKER_ID" --format PublicIPv4 --no-header)
        ssh "root@$WORKER_IP" "pkill -SIGTERM fold" || true
        sleep 30  # Wait for graceful shutdown
        doctl compute droplet delete "$WORKER_ID" --force
        ;;
    
    resize)
        if [ -z "$WORKER_ID" ] || [ -z "$3" ]; then
            echo "Usage: $0 resize <droplet-id> <new-size>"
            exit 1
        fi
        NEW_SIZE="$3"
        echo "Resizing worker $WORKER_ID to $NEW_SIZE..."
        # Graceful shutdown
        WORKER_IP=$(doctl compute droplet get "$WORKER_ID" --format PublicIPv4 --no-header)
        ssh "root@$WORKER_IP" "pkill -SIGTERM fold" || true
        sleep 30
        # Power off
        doctl compute droplet-action power-off "$WORKER_ID" --wait
        # Resize
        doctl compute droplet-action resize "$WORKER_ID" --size "$NEW_SIZE" --wait
        # Power on
        doctl compute droplet-action power-on "$WORKER_ID" --wait
        # Restart worker
        sleep 10
        ssh "root@$WORKER_IP" "nohup /opt/fold/deploy/worker.sh > /var/log/fold.log 2>&1 &"
        ;;
    
    list)
        echo "Current worker pool:"
        doctl compute droplet list --tag-name fold-worker --format ID,Name,Memory,VCPUs,Status,PublicIPv4
        ;;
    
    *)
        echo "Usage: $0 {add|remove|resize|list} [worker-id] [size]"
        echo ""
        echo "Commands:"
        echo "  add [size]           - Add new worker (default: s-1vcpu-2gb)"
        echo "  remove <id>          - Remove worker (graceful shutdown)"
        echo "  resize <id> <size>   - Resize worker"
        echo "  list                 - List all workers"
        echo ""
        echo "Sizes:"
        echo "  s-1vcpu-1gb  - 1GB RAM  (\$6/month)"
        echo "  s-1vcpu-2gb  - 2GB RAM  (\$12/month)"
        echo "  s-2vcpu-4gb  - 4GB RAM  (\$24/month)"
        echo "  s-4vcpu-8gb  - 8GB RAM  (\$48/month)"
        exit 1
        ;;
esac
```

**Worker Image Creation** (`deploy/create_image.sh`):

```bash
#!/bin/bash
# Create a snapshot image with Rust and fold pre-installed
# This speeds up worker startup significantly

# Create base droplet
doctl compute droplet create "fold-base-$(date +%s)" \
    --image ubuntu-22-04-x64 \
    --size s-1vcpu-1gb \
    --region nyc3 \
    --wait

DROPLET_ID=$(doctl compute droplet list --format ID,Name --no-header | grep fold-base | awk '{print $1}')
DROPLET_IP=$(doctl compute droplet get "$DROPLET_ID" --format PublicIPv4 --no-header)

# Wait for SSH to be ready
sleep 30

# Install dependencies
ssh "root@$DROPLET_IP" << 'EOF'
apt-get update
apt-get install -y build-essential curl git

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# Clone and build fold
git clone https://github.com/michaelrauh/fold.git /opt/fold
cd /opt/fold
cargo build --release

# Create worker script
mkdir -p /opt/fold/deploy
EOF

scp deploy/worker.sh "root@$DROPLET_IP:/opt/fold/deploy/"

# Power off and snapshot
doctl compute droplet-action power-off "$DROPLET_ID" --wait
doctl compute droplet-action snapshot "$DROPLET_ID" --snapshot-name "fold-worker-snapshot" --wait

# Cleanup base droplet
doctl compute droplet delete "$DROPLET_ID" --force

echo "Created snapshot: fold-worker-snapshot"
```

## Work Claiming and Selection

### File Selection Strategy

Workers use the same selection logic as the current local implementation:

**For text files**:
- List all `.txt` files in `s3://fold-input/`
- Pick a random available file
- Claim by moving to `s3://fold-in-process/{worker_id}/`

**For archive merges**:
- List all `.bin` archives in `s3://fold-input/`
- Find the **smallest** archive by ortho count (metadata in filename or manifest)
- Find the **largest** archive by ortho count
- Claim both by moving to `s3://fold-in-process/{worker_id}/`

```rust
// Mirror of current get_smallest_and_largest_archives logic
fn select_archives_for_merge(archives: Vec<ArchiveMetadata>) -> Option<(String, String)> {
    if archives.len() < 2 {
        return None;
    }
    
    // Sort by ortho count
    let mut sorted = archives.clone();
    sorted.sort_by_key(|a| a.ortho_count);
    
    let smallest = sorted.first()?.s3_key.clone();
    let largest = sorted.last()?.s3_key.clone();
    
    Some((smallest, largest))
}
```

### No Duplicate Work

The S3 move-to-claim model prevents duplicate work:
- Worker attempts to move file from `fold-input/` to `fold-in-process/{worker_id}/`
- If file is already gone (moved by another worker), the move fails
- Worker retries with another file
- Only one worker can successfully claim each file

## Local Disk Usage

### All Intermediate State is Local

```
/fold_state/                    # Local SSD on droplet
├── input/                      # Downloaded from S3
│   └── assigned_file.txt
├── in_process/                 # Standard processing
│   └── assigned_file.txt.work/
│       ├── source.txt
│       ├── heartbeat          # Local heartbeat for crash recovery
│       ├── queue/             # DiskBackedQueue spill
│       └── seen_shards/       # SeenTracker shards
└── results_*/                  # Final results before upload
```

### Why Local Disk?

1. **Performance**: Local SSD is 100x faster than S3 for random access
2. **Cost**: No S3 API charges for intermediate operations
3. **Simplicity**: Existing DiskBackedQueue and SeenTracker work unchanged
4. **Reliability**: No network failures during processing
5. **Memory Extension**: Disk-backed structures allow processing larger datasets than RAM alone

### Disk Space Requirements

| Component | Typical Size | Notes |
|-----------|-------------|-------|
| Input file | 1-100MB | Downloaded from S3 |
| Queue spill | 10-500MB | Depends on work queue depth |
| Seen shards | 50-500MB | Depends on total orthos generated |
| Results | 100MB-5GB | All generated orthos |
| **Total** | **200MB-6GB** | Per job |

Recommended local disk: 25-80GB depending on expected job sizes.

## Configuration

### Environment Variables

```bash
# S3 Configuration (required)
DO_SPACES_REGION=nyc3
DO_SPACES_ENDPOINT=https://nyc3.digitaloceanspaces.com
DO_SPACES_ACCESS_KEY=xxx
DO_SPACES_SECRET_KEY=xxx
DO_SPACES_INPUT_BUCKET=fold-input
DO_SPACES_IN_PROCESS_BUCKET=fold-in-process
DO_SPACES_ARCHIVE_BUCKET=fold-archives

# Worker Configuration
WORKER_ID=${HOSTNAME}          # Unique worker identifier
LOCAL_STATE_DIR=/fold_state    # Local processing directory
HEARTBEAT_INTERVAL=60          # Seconds between heartbeat updates
HEARTBEAT_GRACE_PERIOD=600     # 10 minutes (same as local)
```

### Command-Line Arguments

```bash
./fold --mode local           # Default: existing behavior
./fold --mode cloud-worker    # Cloud: long-running worker loop
```

## Change Points Summary

### Files Requiring Changes

1. **`src/main.rs`**
   - Add `--mode` argument parsing
   - Add `run_cloud_worker()` function with continuous loop
   - Keep existing `run_local()` as default

2. **`Cargo.toml`**
   - Add `aws-sdk-s3`, `aws-config`, `tokio`, `zstd`, `tar`
   - Feature flag for cloud support (optional)

### New Files

1. **`src/s3_io.rs`** - S3 download/upload operations
2. **`src/compression.rs`** - Archive compression/decompression
3. **`deploy/worker.sh`** - Long-running worker script
4. **`deploy/manage_pool.sh`** - Pool management (add/remove/resize)
5. **`deploy/create_image.sh`** - Create worker snapshot image

### Unchanged Files

- `src/file_handler.rs` - No changes needed
- `src/disk_backed_queue.rs` - No changes needed
- `src/seen_tracker.rs` - No changes needed
- `src/memory_config.rs` - No changes needed (already handles sizing)
- `src/tui.rs` - No changes needed (already shows per-worker metrics)
- `src/metrics.rs` - No changes needed
- `src/interner.rs` - No changes needed
- `src/ortho.rs` - No changes needed

## Cost Estimation

### Monthly Costs (Worker Pool Model)

**Assumptions**:
- 1000 jobs/month
- Average 100MB input per job
- Average 500MB output per job
- Worker pool running 50% of the month (on-demand scaling)

**Spaces (S3 Storage)**:
- Storage: 500GB × $0.02/GB = $10/month
- Transfer: 600GB out × $0.01/GB = $6/month
- **Spaces Total**: ~$16/month

**Droplets (Worker Pool)**:

| Pool Configuration | Hourly Cost | Monthly (50% utilization) |
|-------------------|-------------|---------------------------|
| 2 × 2GB workers | $0.024/hr | ~$18/month |
| 1 × 4GB worker | $0.036/hr | ~$13/month |
| Mixed (1×2GB + 1×4GB) | $0.048/hr | ~$18/month |

**Total Estimated Cost**: $30-35/month

**Cost Optimization Strategies**:
1. Scale down worker pool during off-peak hours
2. Use smaller workers for light workloads
3. Temporarily add large workers only for heavy jobs
4. Monitor memory requirements and right-size workers

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
   - ✓ Workers run as long-lived processes in a pool
   - ✓ Workers claim files via S3 move (no duplicate work)
   - ✓ Archives selected using smallest/largest logic (like current)
   - ✓ Processing uses local disk only (no network during processing)
   - ✓ Archive uploaded to S3 on completion
   - ✓ TUI shows per-worker metrics correctly
   - ✓ Memory requirements enforced at startup

2. **Recovery**:
   - ✓ Heartbeat updated during processing (to S3)
   - ✓ Stale heartbeats detected (10 minute grace period)
   - ✓ Abandoned files moved back to input for re-processing
   - ✓ Crashed workers' work is not lost

3. **Operations**:
   - ✓ Workers can be added/removed manually
   - ✓ Workers can be resized for memory requirements
   - ✓ Per-worker TUI monitoring available

4. **Cost**:
   - ✓ No database fees (S3 only)
   - ✓ Pool can be scaled down when idle
   - ✓ Right-size workers to actual memory needs

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

This migration plan enables distributed processing with a **worker pool model** that provides:

- **Per-worker monitoring**: TUI dashboard shows each worker's metrics (RAM, queue depth, progress)
- **Dynamic sizing**: Workers can be resized based on runtime memory requirements from `MemoryConfig`
- **Local recovery**: Heartbeat-based crash recovery within each worker (not cross-worker coordination)
- **Manual scaling**: Add, remove, or resize workers based on workload needs
- **Size/count trade-off**: Run fewer large workers or more small workers based on memory requirements

The key principles:
- **S3 move semantics** - Claim work by moving files, same as current local file-move
- **Heartbeat-based recovery** - Stale workers' files recovered back to input
- **Smallest/largest selection** - Archive merges use same logic as current
- **Local disk first** - All processing happens on local SSD
- **No duplicate work** - Move-to-claim ensures each file processed once

This approach preserves the existing codebase (file_handler, disk_backed_queue, seen_tracker, memory_config, tui) while enabling horizontal scaling across multiple machines. The S3 move semantics mirror the local `input/` → `in_process/` pattern.
