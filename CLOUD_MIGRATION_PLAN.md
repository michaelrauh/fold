# Cloud Migration Plan: File-Based to Cloud S3/DB Operations

## Executive Summary

This document outlines a detailed plan to migrate the Fold text processing system from local file-based operations to cloud-based operations using Digital Ocean services. The migration will enable multiple independent droplet runners to process work without requiring coordination or shared filesystem access.

## Current Architecture

### File-Based Operations

The current system relies heavily on local filesystem operations:

1. **State Directory Structure** (`fold_state/`)
   - `input/` - Text files (.txt) and archive directories (.bin) awaiting processing
   - `in_process/` - Work folders for active processing with heartbeat files
   - `results_*/` - DiskBackedQueue directories for ortho results
   - `checkpoint/` - Atomic checkpoints (no longer actively used in latest version)
   - `seen_shards/` - Disk-backed bloom filter shards for deduplication

2. **Processing Model**
   - Single machine, multiple process concurrency
   - File-move semantics for mutual exclusion (move file to `in_process/`)
   - Heartbeat files for crash detection (10 minute grace period)
   - Recovery through file movement back to `input/`

3. **Key File Operations** (from `file_handler.rs`)
   - `fs::read_dir()` - List files in directories
   - `fs::rename()` - Atomic move for mutual exclusion
   - `fs::create_dir_all()` - Create work folders
   - `fs::remove_dir_all()` - Cleanup after processing
   - `fs::read()` / `fs::write()` - Read/write binary data (interner, metadata, lineage)
   - `fs::metadata()` - Check heartbeat timestamps

4. **Data Structures Using Disk**
   - **DiskBackedQueue** - Spills orthos to disk files when memory buffer fills
   - **SeenTracker** - LRU sharded HashMap with disk backing
   - **Checkpoint System** - Three-queue strategy with atomic saves

## Target Architecture: Digital Ocean Cloud

### Infrastructure Components

#### 1. Digital Ocean Spaces (S3-Compatible Object Storage)
- **Purpose**: Replace filesystem for all persistent data
- **Buckets**:
  - `fold-input` - Input files and archives awaiting processing
  - `fold-results` - Completed archive results
  - `fold-temp` - Temporary work data (with lifecycle policies)
- **Access**: S3-compatible API using Digital Ocean Spaces access keys

#### 2. Digital Ocean Managed PostgreSQL Database
- **Purpose**: Job coordination, work queue, and metadata
- **Tables**:
  - `jobs` - Work items (text files or archive pairs to process)
  - `job_locks` - Distributed locking with heartbeat
  - `archives` - Archive metadata (ortho_count, lineage, size)
  - `processing_runs` - Audit log of processing history
- **Features**:
  - Connection pooling
  - Automated backups
  - High availability option

#### 3. Digital Ocean Droplets (Compute)
- **Purpose**: One-off processing tasks
- **Deployment**: Ad-hoc spawned droplets
- **Configuration**: Environment variables for DB/S3 credentials
- **Lifecycle**: Start → Process one job → Shutdown
- **OS**: Ubuntu 22.04 LTS with Rust toolchain

## Migration Strategy

### Phase 1: Add Cloud Abstractions (No Breaking Changes)

Create new storage abstraction layer alongside existing file operations:

**New Modules**:
- `src/storage/mod.rs` - Storage trait and factory
- `src/storage/local.rs` - LocalStorage (wraps existing file_handler)
- `src/storage/cloud.rs` - CloudStorage (S3 + PostgreSQL)
- `src/storage/types.rs` - Common types (StorageConfig, JobInfo, etc.)

**Key Trait**:
```rust
pub trait Storage {
    // Job queue operations
    fn claim_next_job(&mut self) -> Result<Option<JobInfo>, FoldError>;
    fn update_heartbeat(&mut self, job_id: &str) -> Result<(), FoldError>;
    fn complete_job(&mut self, job_id: &str) -> Result<(), FoldError>;
    fn abandon_job(&mut self, job_id: &str) -> Result<(), FoldError>;
    
    // File operations
    fn read_text_file(&self, path: &str) -> Result<String, FoldError>;
    fn write_archive(&mut self, archive: &Archive) -> Result<String, FoldError>;
    fn read_interner(&self, archive_id: &str) -> Result<Interner, FoldError>;
    fn list_archives(&self) -> Result<Vec<ArchiveMetadata>, FoldError>;
    
    // Queue operations
    fn create_queue(&self, name: &str) -> Result<Box<dyn Queue>, FoldError>;
    fn create_tracker(&self, name: &str) -> Result<Box<dyn Tracker>, FoldError>;
}
```

### Phase 2: Implement Cloud Storage Layer

#### 2.1 S3 Integration (Digital Ocean Spaces)

**Dependencies** (add to `Cargo.toml`):
```toml
aws-sdk-s3 = "1.0"  # Works with DO Spaces via endpoint override
aws-config = "1.0"
tokio = { version = "1.0", features = ["full"] }
```

**Key Operations**:
- Upload/download using streaming to handle large files
- Multipart uploads for archives > 100MB
- Presigned URLs for temporary access
- Lifecycle rules for temp data cleanup (30 days)

**Implementation** (`src/storage/cloud.rs`):
```rust
pub struct CloudStorage {
    s3_client: aws_sdk_s3::Client,
    db_pool: sqlx::PgPool,
    bucket_input: String,
    bucket_results: String,
    bucket_temp: String,
    job_id: Option<String>,
}
```

#### 2.2 PostgreSQL Integration

**Dependencies**:
```toml
sqlx = { version = "0.7", features = ["runtime-tokio-rustls", "postgres", "uuid", "chrono"] }
uuid = { version = "1.0", features = ["v4"] }
chrono = "0.4"
```

**Schema** (`migrations/001_initial.sql`):
```sql
-- Jobs table: represents work items
CREATE TABLE jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_type VARCHAR(20) NOT NULL, -- 'txt_file' or 'archive_merge'
    status VARCHAR(20) NOT NULL,   -- 'pending', 'processing', 'completed', 'failed'
    input_path VARCHAR(500) NOT NULL, -- S3 key for input
    input_path_b VARCHAR(500),     -- For merges, second archive
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    heartbeat_at TIMESTAMPTZ,
    worker_id VARCHAR(100),        -- Droplet ID
    error_message TEXT,
    retry_count INT NOT NULL DEFAULT 0
);

CREATE INDEX idx_jobs_status ON jobs(status);
CREATE INDEX idx_jobs_heartbeat ON jobs(heartbeat_at) WHERE status = 'processing';

-- Job locks with distributed locking
CREATE TABLE job_locks (
    job_id UUID PRIMARY KEY REFERENCES jobs(id),
    worker_id VARCHAR(100) NOT NULL,
    locked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT fk_job FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE CASCADE
);

-- Archives metadata
CREATE TABLE archives (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    s3_key VARCHAR(500) NOT NULL UNIQUE,
    ortho_count BIGINT NOT NULL,
    lineage TEXT NOT NULL,
    size_bytes BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE INDEX idx_archives_ortho_count ON archives(ortho_count) WHERE NOT is_deleted;

-- Processing runs audit log
CREATE TABLE processing_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL REFERENCES jobs(id),
    worker_id VARCHAR(100) NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    orthos_processed BIGINT,
    result_archive_id UUID REFERENCES archives(id)
);
```

#### 2.3 Distributed Locking Strategy

**Heartbeat Protocol**:
1. Worker claims job: `UPDATE jobs SET status='processing', worker_id=?, heartbeat_at=NOW() WHERE id=? AND status='pending'`
2. Worker updates heartbeat every 60 seconds
3. Stale job detection query (run by all workers on startup):
   ```sql
   UPDATE jobs SET status='pending', worker_id=NULL 
   WHERE status='processing' 
   AND heartbeat_at < NOW() - INTERVAL '5 minutes'
   RETURNING id;
   ```
4. Job completion: `UPDATE jobs SET status='completed', completed_at=NOW()`

**Advantages over File-Based**:
- No shared filesystem required
- Proper transaction isolation
- Atomic updates via SQL transactions
- Built-in timestamp precision
- Database-level heartbeat monitoring

### Phase 3: Adapt Core Components

#### 3.1 DiskBackedQueue → CloudBackedQueue

**Current**: Spills to local filesystem
**Target**: Spills to S3 with local disk cache

```rust
pub struct CloudBackedQueue {
    buffer: Vec<Ortho>,
    buffer_size: usize,
    s3_client: aws_sdk_s3::Client,
    bucket: String,
    job_id: String,
    chunk_counter: usize,
    local_cache_dir: PathBuf, // /tmp/fold_cache/
}
```

**Strategy**:
- Keep in-memory buffer (10K items)
- Spill to local disk first (`/tmp/fold_cache/queue_{job_id}_{chunk}.bin`)
- Background upload to S3 (`s3://{bucket}/{job_id}/queue/chunk_{chunk}.bin`)
- Delete local file after successful upload
- On pop: check memory → check local cache → download from S3 if needed

**Benefits**:
- Lower latency than pure S3 (local cache)
- Survives network hiccups
- Same memory characteristics as current design

#### 3.2 SeenTracker → CloudBackedTracker

**Current**: Sharded HashMap with local disk LRU
**Target**: Sharded with S3 backing + PostgreSQL approximate counts

```rust
pub struct CloudBackedTracker {
    bloom: Bloom<usize>,
    loaded_shards: Vec<Shard>,
    s3_client: aws_sdk_s3::Client,
    bucket: String,
    job_id: String,
    num_shards: usize,
    max_shards_in_memory: usize,
}
```

**Strategy**:
- Keep bloom filter in memory (critical path)
- LRU shards in memory (32 shards)
- Cold shards upload to S3 (`s3://{bucket}/{job_id}/shards/shard_{id}.bin`)
- Use local temp directory as cache
- PostgreSQL tracks approximate counts for sizing

**Optimization**: For merge operations, previous shards can be discarded (fresh start with new interner version)

#### 3.3 Archive Format Changes

**Current**: Directory with multiple files
```
archive_name.bin/
  ├── interner.bin
  ├── results/
  │   ├── queue_*.bin
  ├── optimal.txt
  ├── lineage.txt
  ├── metadata.txt
  └── heartbeat
```

**Target**: Single compressed archive file + metadata in DB
```
S3 Key: archives/{uuid}.tar.zst
Contains:
  ├── interner.bin
  ├── results/queue_*.bin (files)
  ├── optimal.txt
  └── manifest.json (includes lineage, metadata)

PostgreSQL Record:
  - id: UUID
  - s3_key: archives/{uuid}.tar.zst
  - ortho_count: 12345
  - lineage: "(file1 file2)"
  - size_bytes: 1024000
```

**Benefits**:
- Single S3 object = atomic upload/download
- Compression reduces storage costs
- Metadata in DB enables fast queries
- No directory listing required

### Phase 4: Configuration and Deployment

#### 4.1 Environment Configuration

```bash
# Digital Ocean Spaces credentials
DO_SPACES_REGION=nyc3
DO_SPACES_ENDPOINT=https://nyc3.digitaloceanspaces.com
DO_SPACES_ACCESS_KEY=xxx
DO_SPACES_SECRET_KEY=xxx
DO_SPACES_BUCKET_INPUT=fold-input
DO_SPACES_BUCKET_RESULTS=fold-results
DO_SPACES_BUCKET_TEMP=fold-temp

# PostgreSQL connection
DATABASE_URL=postgresql://fold_user:password@db-host:25060/fold?sslmode=require

# Worker configuration
WORKER_ID=droplet-${DROPLET_ID}
STORAGE_BACKEND=cloud  # or 'local' for backwards compatibility
LOCAL_CACHE_DIR=/tmp/fold_cache
```

#### 4.2 Droplet Setup Script

```bash
#!/bin/bash
# deploy_droplet.sh

# Install dependencies
apt-get update
apt-get install -y build-essential curl

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# Clone repository
git clone https://github.com/michaelrauh/fold.git
cd fold

# Build release binary
cargo build --release

# Set environment variables from DO metadata service
export WORKER_ID=$(curl -s http://169.254.169.254/metadata/v1/id)
export DO_SPACES_ACCESS_KEY=$1
export DO_SPACES_SECRET_KEY=$2
export DATABASE_URL=$3
export STORAGE_BACKEND=cloud

# Run one job
./target/release/fold --mode one-shot

# Shutdown droplet (handled by DO API)
shutdown -h now
```

#### 4.3 Job Submission Tool

New utility: `fold-submit` for uploading work to cloud queue

```rust
// src/bin/fold_submit.rs
// Upload text file to S3 and create job in PostgreSQL

fn main() -> Result<(), FoldError> {
    let args = parse_args();
    let storage = CloudStorage::new_from_env()?;
    
    // Upload file to S3
    let s3_key = storage.upload_file(&args.input_file).await?;
    
    // Create job in database
    storage.create_job("txt_file", &s3_key, None).await?;
    
    println!("Job created: {}", s3_key);
    Ok(())
}
```

### Phase 5: Migration Path

#### Option A: Big Bang Migration
- Deploy all changes at once
- Migrate existing local state to cloud
- Shutdown local processing, start cloud processing

**Pros**: Clean cut, simpler code
**Cons**: Risky, requires downtime, all-or-nothing

#### Option B: Gradual Migration (Recommended)
- Keep both storage backends
- Use feature flag to control which backend
- Run hybrid mode: some workers local, some cloud
- Gradually migrate data and workload

**Pros**: Lower risk, allows testing, no downtime
**Cons**: More complex code temporarily

**Implementation**:
```rust
pub fn create_storage(config: &StorageConfig) -> Box<dyn Storage> {
    match config.backend {
        StorageBackend::Local => Box::new(LocalStorage::new(config)),
        StorageBackend::Cloud => Box::new(CloudStorage::new(config)),
    }
}
```

## Key Decisions and Tradeoffs

### Decision 1: S3 vs Database for Queue Storage

**Choice**: S3 for queue data, PostgreSQL for metadata and coordination

**Rationale**:
- S3: Better for large binary blobs (queue files, interners)
- PostgreSQL: Better for transactional coordination and queries
- Hybrid approach plays to each service's strengths

**Tradeoff**: More complex architecture, but better scalability

### Decision 2: Compression Format

**Choice**: Zstandard (.zst) compression for archives

**Rationale**:
- 40-60% smaller than uncompressed
- Fast decompression (critical for processing)
- Good compression ratio vs speed balance
- Better than gzip for binary data

**Tradeoff**: Adds CPU overhead, but saves 40-60% on storage and transfer

### Decision 3: Heartbeat Frequency

**Choice**: 60-second heartbeat, 5-minute grace period

**Rationale**:
- More frequent than local (was 100K orthos = variable time)
- Shorter grace period enabled by reliable network
- Faster failure detection
- Lower chance of duplicate work

**Tradeoff**: More database writes, but negligible with connection pooling

### Decision 4: Local Cache Strategy

**Choice**: Mandatory local disk cache at `/tmp/fold_cache`

**Rationale**:
- Reduces S3 API calls (cost)
- Improves latency for hot data
- Survives temporary network issues
- Droplets have fast local SSD

**Tradeoff**: Requires disk space, but droplets have ample /tmp space

### Decision 5: Archive Format

**Choice**: Single compressed tar archive per result

**Rationale**:
- Atomic upload/download
- No directory listing overhead
- Simpler S3 lifecycle management
- Better compression ratio

**Tradeoff**: Must compress/decompress entire archive (but fast with zstd)

## Change Points Summary

### Files Requiring Major Changes

1. **`src/file_handler.rs`** → **`src/storage/local.rs`**
   - Extract existing logic into LocalStorage trait implementation
   - Keep file-based operations for backward compatibility

2. **`src/main.rs`**
   - Add storage factory at startup
   - Replace direct file_handler calls with storage trait calls
   - Add `--mode one-shot` flag for droplet workers

3. **`src/disk_backed_queue.rs`** → **`src/storage/cloud_queue.rs`**
   - Add S3 upload/download logic
   - Implement local cache layer
   - Keep same API surface

4. **`src/seen_tracker.rs`** → **`src/storage/cloud_tracker.rs`**
   - Add S3 shard persistence
   - Keep bloom filter in memory (critical path)

5. **`Cargo.toml`**
   - Add aws-sdk-s3, sqlx, tokio, zstd
   - Make async runtime available

### New Files

1. **`src/storage/mod.rs`** - Storage trait and factory
2. **`src/storage/local.rs`** - LocalStorage implementation
3. **`src/storage/cloud.rs`** - CloudStorage implementation
4. **`src/storage/types.rs`** - Common types and helpers
5. **`src/storage/cloud_queue.rs`** - CloudBackedQueue
6. **`src/storage/cloud_tracker.rs`** - CloudBackedTracker
7. **`src/bin/fold_submit.rs`** - Job submission CLI tool
8. **`migrations/*.sql`** - Database schema migrations
9. **`deploy/droplet_setup.sh`** - Droplet initialization script
10. **`deploy/spawn_worker.sh`** - Spawn droplet for job

### Configuration Changes

1. **Environment Variables**
   - Add DO Spaces credentials
   - Add PostgreSQL connection string
   - Add storage backend selector

2. **Command-Line Flags**
   - `--storage-backend <local|cloud>` - Select storage
   - `--mode <continuous|one-shot>` - Worker mode
   - `--job-id <uuid>` - Process specific job (cloud mode)

## Testing Strategy

### Unit Tests
- Mock Storage trait for existing tests
- Test LocalStorage against temp directories
- Test CloudStorage with LocalStack (S3 emulator) + test PostgreSQL

### Integration Tests
- End-to-end test with local backend (existing tests)
- End-to-end test with cloud backend + mocks
- Test job claiming race conditions
- Test heartbeat timeout scenarios
- Test archive upload/download integrity

### Load Tests
- Spawn 10 droplets concurrently
- Process 100 text files simultaneously
- Verify no duplicate work
- Measure latency vs local filesystem

## Performance Considerations

### Expected Latencies

| Operation | Local | Cloud (no cache) | Cloud (cached) |
|-----------|-------|------------------|----------------|
| Read text file | 1-5ms | 50-100ms | 5-10ms |
| Write archive | 10-50ms | 200-500ms | 50-100ms + async upload |
| Queue spill | 5-20ms | 100-200ms | 10-30ms + async upload |
| Heartbeat update | N/A | 10-20ms | 10-20ms |
| Job claim | N/A | 20-50ms | 20-50ms |

### Optimization Strategies

1. **Aggressive Local Caching**
   - Cache all S3 reads in `/tmp` for session duration
   - Only upload to S3, never delete until cleanup

2. **Async Uploads**
   - Upload to S3 in background while continuing processing
   - Only block on critical path (job completion)

3. **Connection Pooling**
   - Reuse PostgreSQL connections
   - Keep S3 client alive across operations

4. **Batch Operations**
   - Batch multiple heartbeat updates if processing is fast
   - Upload multiple queue chunks in parallel

## Cost Estimation (Digital Ocean)

### Monthly Costs (example workload)

**Assumptions**:
- 1000 jobs/month
- Average 100MB input per job
- Average 500MB output per job
- 10 concurrent workers peak
- Each job takes 2 hours average

**Spaces (S3 Storage)**:
- Storage: 500GB × $0.02/GB = $10/month
- Transfer: 600GB out × $0.01/GB = $6/month
- API calls: ~100K operations = negligible
- **Spaces Total**: ~$16/month

**Managed PostgreSQL**:
- Basic tier (1GB RAM, 10GB storage) = $15/month
- No backup tier sufficient for this use case
- **Database Total**: $15/month

**Droplets (Compute)**:
- $0.007/hour (1GB RAM, 1 vCPU)
- 1000 jobs × 2 hours × $0.007 = $14/month
- With parallelism: ~$14-50/month depending on concurrency
- **Compute Total**: ~$15-50/month

**Total Estimated Cost**: $46-81/month

**Comparison to Single Server**:
- Single droplet (8GB RAM, 4 vCPU) = $48/month
- Cloud approach scales up/down automatically
- No idle costs when no work to process

## Rollback Plan

If migration fails or issues arise:

1. **Immediate Rollback**:
   - Set `STORAGE_BACKEND=local` for all workers
   - Stop all cloud workers
   - Restart local workers

2. **Data Recovery**:
   - Download all S3 data back to local filesystem
   - Reconstruct `fold_state/` directory structure
   - Run local workers to complete jobs

3. **Database Cleanup**:
   - Mark all cloud jobs as 'failed'
   - Export job metadata for debugging
   - Optionally drop cloud tables

## Timeline Estimate

- **Phase 1** (Abstractions): 1-2 weeks
- **Phase 2** (Cloud Storage): 2-3 weeks
- **Phase 3** (Core Components): 2-3 weeks
- **Phase 4** (Deployment): 1 week
- **Phase 5** (Testing & Migration): 2-3 weeks

**Total Estimated Time**: 8-12 weeks

## Success Criteria

1. **Functional**:
   - ✓ Multiple droplets process jobs concurrently without coordination
   - ✓ No duplicate work processed
   - ✓ Archives correctly uploaded and downloadable
   - ✓ Heartbeat system detects and recovers failed jobs

2. **Performance**:
   - ✓ Throughput >= 90% of local filesystem approach
   - ✓ End-to-end latency < 2x local filesystem approach
   - ✓ Memory usage same as current (no regression)

3. **Reliability**:
   - ✓ Zero data loss during normal operations
   - ✓ Graceful handling of network failures
   - ✓ Automatic recovery from worker crashes
   - ✓ Database transactions prevent race conditions

4. **Cost**:
   - ✓ Total monthly cost < single large server for same workload
   - ✓ Cost scales linearly with work volume
   - ✓ Zero cost when idle

## Appendix: Alternative Approaches Considered

### A1: Pure S3 (No PostgreSQL)

**Approach**: Use S3 object metadata for coordination

**Rejected Because**:
- S3 lacks transaction support
- No atomic compare-and-swap
- Race conditions inevitable with multiple writers
- Heartbeat monitoring difficult

### A2: Redis for Coordination

**Approach**: Use Redis instead of PostgreSQL for job queue

**Rejected Because**:
- Digital Ocean doesn't offer managed Redis
- Self-managed Redis adds operational complexity
- PostgreSQL sufficient for this workload
- Database provides better audit trail

### A3: Kubernetes StatefulSet

**Approach**: Use K8s with persistent volumes

**Rejected Because**:
- Overkill for simple one-shot jobs
- Persistent volumes defeat purpose (still tied to location)
- Added complexity of K8s management
- Higher costs than simple droplets

### A4: Message Queue (RabbitMQ/SQS)

**Approach**: Use message queue for work distribution

**Rejected Because**:
- Digital Ocean doesn't offer managed queues
- PostgreSQL sufficient for this workload
- Message queue doesn't provide metadata storage
- Still need database for archives table

## Appendix: Security Considerations

### Credentials Management

1. **Digital Ocean Spaces**:
   - Use API keys with minimum necessary permissions
   - Rotate keys every 90 days
   - Separate keys per environment (dev/prod)

2. **PostgreSQL**:
   - Use SSL/TLS for all connections
   - Separate user per worker type
   - Restrict IP access to droplet VPC

3. **Droplet Access**:
   - Inject credentials via environment variables only
   - Never commit credentials to repository
   - Use DO metadata service for worker identification

### Data Encryption

1. **At Rest**:
   - DO Spaces: Encryption by default
   - PostgreSQL: Encryption at rest enabled
   - Local cache: Encrypted filesystem for /tmp

2. **In Transit**:
   - HTTPS for all S3 operations
   - SSL/TLS for all PostgreSQL connections
   - No unencrypted data transfer

### Access Control

1. **Least Privilege**:
   - Each worker can only access its own job data
   - Shared archives in results bucket readable by all
   - Temp bucket has 30-day auto-cleanup

2. **Audit Logging**:
   - All job state changes logged to `processing_runs` table
   - S3 access logs enabled
   - PostgreSQL query logs for debugging

## Conclusion

This migration plan provides a comprehensive path to convert Fold from a single-machine file-based system to a distributed cloud-based system running on Digital Ocean infrastructure. The approach maintains backward compatibility through a storage abstraction layer, minimizes risk through gradual migration, and enables true horizontal scaling through stateless droplet workers.

The key insight is that the existing architecture already uses disk-backed data structures for memory management, making the transition to cloud storage a natural extension. By replacing local disk with S3 and file-based mutual exclusion with PostgreSQL transactions, we achieve the same correctness guarantees with better scalability.
