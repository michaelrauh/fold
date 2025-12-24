# Dynamic RAM Policy

## Overview

Fold uses **continuous, dynamic RAM allocation** with leader/follower roles. RAM budgets adjust based on global system memory pressure, ensuring efficient resource usage without manual tuning.

## Leader/Follower Roles

### Leader
- **Primary processing node** or single-instance deployment
- **Aggressive memory usage**: targets 65-85% of available RAM
- **Large run budget**: 2-6 GB for external sort arenas
- Suitable for dedicated processing machines

### Follower
- **Resource-constrained node** or multi-instance deployment
- **Conservative memory usage**: targets 50-70% of available RAM
- **Smaller run budget**: 256 MB - 1 GB for external sort arenas
- Minimum viable: 128 MB
- Bail if budget < 128 MB and memory pressure persists

## Continuous RAM Dials

RAM configuration is **not static**—it adjusts continuously based on system state:

### Inputs
- **Global RSS %**: Current memory usage as percentage of total system RAM
- **Headroom bytes**: Available free memory

### Targets

| Role     | Aggressive Below | Conservative Above |
|----------|------------------|--------------------|
| Leader   | 65%              | 85%                |
| Follower | 50%              | 70%                |

### Adjustment Strategy

```rust
fn compute_config(role: Role, global_rss_pct: f64, headroom: usize) -> Config {
    let target_pct = match role {
        Role::Leader => {
            if global_rss_pct < 0.65 { 0.85 }
            else { 0.65 }
        }
        Role::Follower => {
            if global_rss_pct < 0.50 { 0.70 }
            else { 0.50 }
        }
    };
    
    let budget = (total_ram * target_pct) - current_usage;
    let run_budget = 0.7 * budget;
    let fan_in = clamp(budget / read_buf_bytes, 8, 128);
    
    Config { run_budget_bytes: run_budget, fan_in, ... }
}
```

**Key behavior**:
- Below target: Increase allocation (aggressive)
- Above target: Decrease allocation (conservative)
- Smooth transitions, not abrupt changes

## RAM Budget Breakdown

### External Sort Components

```
run_budget = 0.7 * budget    # Arena for sort
read_buf = budget / fan_in   # Per-run buffer for merge
fan_in = clamp(budget / read_buf_bytes, 8, 128)
```

**Example** (Leader, 8 GB available):
```
budget = 8 GB
run_budget = 5.6 GB          # Arena capacity
read_buf = 64 MB             # Per-file read buffer
fan_in = 128                 # Max concurrent runs
```

**Example** (Follower, 1 GB available):
```
budget = 1 GB
run_budget = 700 MB          # Arena capacity
read_buf = 8 MB              # Per-file read buffer
fan_in = 128                 # Max concurrent runs
```

### Minimum Viable Configuration

```
run_budget >= 128 MB
fan_in >= 8
read_buf >= 8 MB
```

**Follower bail condition**:
```rust
if run_budget < 128 MB && global_rss_pct > 0.70 {
    // Insufficient RAM, cannot proceed safely
    bail!("Insufficient memory for follower role");
}
```

## Integration with Compaction

### Arena-Based Run Generation

```rust
let config = compute_config(role);
let mut arena = Vec::with_capacity(config.run_budget_bytes / item_size);

while let Some(item) = input.next() {
    if arena.len() * item_size > config.run_budget_bytes {
        // Arena full: sort and flush
        arena.sort();
        write_run(&arena)?;
        arena.clear();
        
        // Re-check config for next run (RAM may have changed)
        config = compute_config(role);
        arena.reserve(config.run_budget_bytes / item_size);
    }
    arena.push(item);
}
```

### K-Way Merge

```rust
let config = compute_config(role);
let fan_in = config.fan_in;
let read_buf_bytes = config.read_buf_bytes;

let mut heap = BinaryHeap::new();
for run in runs.chunks(fan_in) {
    // Open up to fan_in runs, each with read_buf_bytes buffer
    let merged = merge_runs(run, read_buf_bytes)?;
    heap.push(merged);
}
```

## Memory Pressure Handling

### Leader Response
1. **RSS < 65%**: Increase `run_budget` toward 85% target
2. **RSS 65-85%**: Maintain current allocation
3. **RSS > 85%**: Decrease `run_budget` toward 65% target
4. Never drop below 128 MB minimum

### Follower Response
1. **RSS < 50%**: Increase `run_budget` toward 70% target
2. **RSS 50-70%**: Maintain current allocation
3. **RSS > 70%**: Decrease `run_budget` toward 50% target
4. If < 128 MB and RSS > 70%: bail (insufficient resources)

## Superseded Features

This RAM policy **replaces** the following old mechanisms:

### Old: Static Memory Config
- `MemoryConfig::calculate(interner_bytes, result_count)`
- Fixed bloom filter capacity
- Fixed shard counts
- Static queue buffer sizes

### Old: Spill-Half Disk Queue
- Hybrid memory/disk queue with 10K item threshold
- Manual capacity tuning
- Fixed spill trigger

### Old: Bloom Filter + Sharded Seen Tracker
- Bloom filter for fast negative lookup
- Disk-backed hash shards with LRU cache
- Complex memory management

**All replaced by**: History store with sorted runs + dynamic RAM dials.

## Configuration API

```rust
pub enum Role { Leader, Follower }

pub struct Config {
    pub run_budget_bytes: usize,  // Arena capacity
    pub fan_in: usize,             // Max concurrent runs
    pub read_buf_bytes: usize,     // Per-run read buffer
    pub allow_compaction: bool,    // Enable optional history compaction
}

impl Config {
    pub fn compute(role: Role) -> Self {
        // Queries system memory, computes dynamic config
    }
}
```

## Examples

### Leader on 16 GB Machine

```
Total RAM: 16 GB
Current usage: 8 GB (50%)
Target: 85% (below aggressive threshold)

Budget: (16 GB × 0.85) - 8 GB = 5.6 GB
run_budget: 5.6 GB × 0.7 = 3.92 GB
fan_in: clamp(5.6 GB / 8 MB, 8, 128) = 128
read_buf: 8 MB
```

### Follower on 4 GB Machine

```
Total RAM: 4 GB
Current usage: 2.5 GB (62.5%)
Target: 50% (above conservative threshold)

Budget: (4 GB × 0.50) - 2.5 GB = -0.5 GB (negative!)
Adjust: Use minimum viable config
run_budget: 128 MB (minimum)
fan_in: 8 (minimum)
read_buf: 8 MB

If RSS stays > 70%: bail (insufficient resources)
```

## Design Rationale

### Why Continuous Adjustment?

- Memory pressure changes during execution
- Other processes may start/stop
- Adaptive policy prevents OOM
- No manual tuning required

### Why Leader/Follower Split?

- Single deployment: use Leader for maximum throughput
- Multi-instance: use Follower to coexist safely
- Cloud/container: Follower respects resource limits
- Dedicated server: Leader maximizes hardware utilization

### Why These Thresholds?

- **Leader 65-85%**: Aggressive but safe on dedicated machines
- **Follower 50-70%**: Conservative for shared environments
- **128 MB minimum**: Smallest viable arena for external sort
- **8 fan_in minimum**: Reasonable merge performance

### Why 70/30 Split?

- **70% to arena**: Sort is the primary memory consumer
- **30% to read buffers**: Fan-in of 8-128 provides good merge performance
- Balance between arena size and merge parallelism

## Migration Notes

**This policy replaces**:
- `MemoryConfig::calculate()` from old code
- Bloom filter sizing logic
- Shard count calculations
- Queue buffer size tuning

**Remove these when implementing**:
- `memory_config.rs` (old static config)
- Bloom filter allocation code
- Seen tracker shard management
- Fixed spill thresholds

