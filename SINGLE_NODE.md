# Single Node Usage

This document describes how to use the simplified single-node version of fold, optimized for running on a single machine without Docker or distributed infrastructure.

## Building

Build the single-node binary without distributed dependencies:

```bash
cargo build --release --bin fold_single
```

The binary will be created at `target/release/fold_single`.

## Usage

The single-node binary has two main commands:

### Ingest Text

Add text to the interner from a file:

```bash
fold_single ingest <path-to-text-file>
```

This command:
- Reads the text file
- Updates the interner with new vocabulary
- Increments the interner version
- Saves the updated state to the resume file

### Run Worker

Execute the worker loop to generate orthos:

```bash
fold_single run
```

This command:
- Loads the frontier and interner from the resume file
- Adds a seed ortho to explore new vocabulary
- Processes all orthos in the work queue
- Generates child orthos based on vocabulary completions
- Deduplicates the frontier using the prefix rule
- Saves the updated frontier to the resume file

## Resume File

The system maintains state in a binary file called `fold_resume.bin` which contains:
- **Frontier**: A Vec of Ortho objects representing the current search frontier
- **Interner**: The vocabulary and phrase completion mappings

The resume file is automatically created on first run if it doesn't exist.

## Example Workflow

```bash
# First time setup - ingest initial text
fold_single ingest corpus.txt

# Run the worker to generate orthos
fold_single run

# Add more vocabulary
fold_single ingest more_text.txt

# Run again - will resume with expanded vocabulary
fold_single run
```

## How It Works

### Frontier Management

The frontier represents all discovered orthos in the branch-and-bound search tree, with non-lead nodes removed:
- **Lead nodes**: Orthos that are not prefixes of other orthos with the same shape
- **Non-lead nodes**: Orthos whose canonicalized payloads are prefixes of others (removed during deduplication)

### Work Queue

The system uses an in-memory VecDeque for the work queue:
1. Initialize queue with previous frontier
2. Add a seed ortho (empty ortho with current interner version)
3. Process each ortho by generating children for all valid completions
4. Deduplicate before saving

### Deduplication

The prefix rule identifies non-lead nodes:
- Group orthos by shape (dims)
- For each group, detect if an ortho's payload is a prefix of another
- Keep only lead nodes (those not prefixes of others)

## Performance

The single-node version eliminates communication overhead from:
- RabbitMQ message passing
- PostgreSQL database queries
- S3/MinIO blob storage access
- Network serialization/deserialization

All operations are in-memory except for:
- Initial resume file load
- Final resume file save
- Text file ingest

## Differences from Distributed Version

| Feature | Distributed | Single-Node |
|---------|------------|-------------|
| Docker | Required | Not used |
| RabbitMQ | Required | Not used |
| PostgreSQL | Required | Not used |
| MinIO/S3 | Required | Not used |
| Tracing/Jaeger | Enabled | Not used |
| Multiple workers | Yes | No |
| Follower process | Yes | No |
| Feeder process | Yes | No |
| State persistence | Database + Blob | Binary file |
| Queue | Distributed | In-memory |

## Building with Distributed Features

To build with distributed features enabled (for other binaries):

```bash
cargo build --release --features distributed
```

This enables:
- `fold_worker`
- `follower`
- `feeder`
- `ingestor`
