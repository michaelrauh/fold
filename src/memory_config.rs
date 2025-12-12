use sysinfo::System;

const BYTES_PER_ORTHO: usize = 200;
const BYTES_PER_BLOOM_ITEM: usize = 2;
const BYTES_PER_SHARD_ITEM: usize = 12;
const TARGET_ITEMS_PER_SHARD: usize = 10_000;
const MIN_QUEUE_BUFFER: usize = 10_000;
const MIN_BLOOM_CAPACITY: usize = 1_000_000;
const MIN_SHARDS_IN_MEMORY: usize = 16;

/// Configuration for memory-intensive components
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    pub queue_buffer_size: usize,
    pub bloom_capacity: usize,
    pub num_shards: usize,
    pub max_shards_in_memory: usize,
}

impl MemoryConfig {
    /// Calculate optimal memory configuration targeting 75% of system RAM
    ///
    /// Minimum requirements:
    /// - Bloom filter: 1% false positive rate
    /// - Queue buffer: 100k orthos minimum
    /// - Shards in memory: 50% of total shards minimum
    ///
    /// Memory budget breakdown:
    /// - Interner: varies by vocabulary size (estimated from serialized size)
    /// - Queue buffers: 2 queues * buffer_size * ~200 bytes per ortho
    /// - Bloom filter: ~2 bytes per item (optimal for 1% FPR)
    /// - Shards in memory: max_shards * avg_items_per_shard * 12 bytes (usize key + ())
    /// - Runtime overhead: ~20% reserve for working memory
    ///
    /// Exits if minimum requirements cannot be met with available RAM.
    pub fn calculate(interner_bytes: usize, expected_results: usize) -> Self {
        let mut sys = System::new_all();
        sys.refresh_memory();

        let total_memory = sys.total_memory() as usize;
        let target_memory = (total_memory * 75) / 100;

        // println!("[memory_config] Total system RAM: {} MB", total_memory / 1_048_576);
        // println!("[memory_config] Target memory usage: {} MB (75%)", target_memory / 1_048_576);
        // println!("[memory_config] Interner size: {} MB", interner_bytes / 1_048_576);

        // Reserve 20% for runtime overhead (ortho processing, vectors, etc)
        let runtime_reserve = target_memory / 5;
        let available_for_caches = target_memory
            .saturating_sub(interner_bytes)
            .saturating_sub(runtime_reserve);

        // println!("[memory_config] Runtime reserve: {} MB", runtime_reserve / 1_048_576);
        // println!("[memory_config] Available for caches: {} MB", available_for_caches / 1_048_576);

        // Estimate ortho size (conservative estimate based on typical dimensions)
        // Small orthos: ~80 bytes, large orthos: ~900 bytes, use 200 as middle estimate
        let bytes_per_ortho = 200;

        // Bloom filter: ~2 bytes per item for 1% false positive rate
        let bytes_per_bloom_item = 2; // 1% FP rate

        // Shard item: 12 bytes (usize key + () value + hashmap overhead)
        let bytes_per_shard_item = 12;

        // Calculate bloom capacity (3x expected results for growth room)
        let bloom_capacity = if expected_results > 0 {
            (expected_results * 3).max(1_000_000)
        } else {
            1_000_000
        };

        let bloom_memory = bloom_capacity * bytes_per_bloom_item;

        // Calculate shard configuration
        // Target ~10K items per shard for good disk I/O granularity
        let target_items_per_shard = 10_000;
        let num_shards = if expected_results > 0 {
            (expected_results / target_items_per_shard)
                .max(64)
                .min(1024)
        } else {
            64
        };

        // MINIMUM REQUIREMENTS CHECK
        let min_queue_buffer = 100_000;
        let min_queue_memory = 2 * min_queue_buffer * bytes_per_ortho; // 2 queues
        let min_shards_in_memory = (num_shards + 1) / 2; // 50% of total shards
        let memory_per_shard = target_items_per_shard * bytes_per_shard_item;
        let min_shard_memory = min_shards_in_memory * memory_per_shard;

        let min_required_memory = bloom_memory + min_queue_memory + min_shard_memory;

        if available_for_caches < min_required_memory {
            eprintln!("\n[memory_config] ===== INSUFFICIENT MEMORY =====");
            eprintln!(
                "[memory_config] Available for caches: {} MB",
                available_for_caches / 1_048_576
            );
            eprintln!("[memory_config] Minimum required:");
            eprintln!(
                "[memory_config]   - Bloom (1% FPR): {} MB",
                bloom_memory / 1_048_576
            );
            eprintln!(
                "[memory_config]   - Queue (100k min): {} MB",
                min_queue_memory / 1_048_576
            );
            eprintln!(
                "[memory_config]   - Shards (50% in mem): {} MB",
                min_shard_memory / 1_048_576
            );
            eprintln!(
                "[memory_config]   - Total minimum: {} MB",
                min_required_memory / 1_048_576
            );
            eprintln!("[memory_config] ================================\n");
            std::process::exit(1);
        }

        // Calculate remaining memory after minimums
        let remaining_after_minimums =
            available_for_caches.saturating_sub(bloom_memory + min_queue_memory);

        // Start with minimum shards in memory (50%)
        let mut max_shards_in_memory = min_shards_in_memory;

        // Use remaining memory to tune up shards in memory if possible
        let available_for_extra_shards = remaining_after_minimums.saturating_sub(min_shard_memory);
        let extra_shards = available_for_extra_shards / memory_per_shard;
        max_shards_in_memory = (max_shards_in_memory + extra_shards).min(num_shards);

        // Queue buffer size is fixed at minimum for consistency
        let queue_buffer_size = min_queue_buffer;

        let config = Self {
            queue_buffer_size,
            bloom_capacity,
            num_shards,
            max_shards_in_memory,
        };

        config.print_summary(interner_bytes, runtime_reserve);

        config
    }

    /// Estimate bytes used by the configuration, including a 20% runtime reserve.
    pub fn estimate_bytes(&self, interner_bytes: usize) -> usize {
        let queue_memory = 2usize
            .saturating_mul(self.queue_buffer_size)
            .saturating_mul(BYTES_PER_ORTHO);
        let bloom_memory = self.bloom_capacity.saturating_mul(BYTES_PER_BLOOM_ITEM);
        let shard_memory = self
            .max_shards_in_memory
            .saturating_mul(TARGET_ITEMS_PER_SHARD)
            .saturating_mul(BYTES_PER_SHARD_ITEM);
        let runtime_reserve = (queue_memory + bloom_memory + shard_memory + interner_bytes) / 5;

        interner_bytes
            .saturating_add(queue_memory)
            .saturating_add(bloom_memory)
            .saturating_add(shard_memory)
            .saturating_add(runtime_reserve)
    }

    /// Attempt to scale the configuration down to fit within the available bytes.
    /// Returns None if even the minimum viable configuration cannot fit.
    pub fn scale_to_budget(&self, available_bytes: usize, interner_bytes: usize) -> Option<Self> {
        if available_bytes == 0 {
            return None;
        }

        let mut scaled = self.clone();
        let current_estimate = scaled.estimate_bytes(interner_bytes);
        if current_estimate <= available_bytes {
            return Some(scaled);
        }

        let scale = available_bytes as f64 / current_estimate as f64;

        scaled.queue_buffer_size =
            (((scaled.queue_buffer_size as f64) * scale).round() as usize).max(MIN_QUEUE_BUFFER);
        scaled.bloom_capacity =
            (((scaled.bloom_capacity as f64) * scale).round() as usize).max(MIN_BLOOM_CAPACITY);
        scaled.max_shards_in_memory = (((scaled.max_shards_in_memory as f64) * scale).round()
            as usize)
            .max(MIN_SHARDS_IN_MEMORY)
            .min(scaled.num_shards);

        let scaled_estimate = scaled.estimate_bytes(interner_bytes);
        if scaled_estimate <= available_bytes {
            Some(scaled)
        } else {
            None
        }
    }

    fn print_summary(&self, interner_bytes: usize, runtime_reserve: usize) {
        let bytes_per_ortho = 200;
        let bytes_per_bloom_item = 5; // 0.1% FPR
        let bytes_per_shard_item = 12;
        let target_items_per_shard = 10_000;

        let queue_memory = 2 * self.queue_buffer_size * bytes_per_ortho;
        let bloom_memory = self.bloom_capacity * bytes_per_bloom_item;
        let shard_memory =
            self.max_shards_in_memory * target_items_per_shard * bytes_per_shard_item;
        let _total_estimated =
            interner_bytes + queue_memory + bloom_memory + shard_memory + runtime_reserve;

        // println!("\n[memory_config] ===== MEMORY CONFIGURATION =====");
        // println!("[memory_config] Queue buffer size: {} orthos (~{} MB per queue)",
        //          self.queue_buffer_size, (self.queue_buffer_size * bytes_per_ortho) / 1_048_576);
        // println!("[memory_config] Bloom capacity: {} items (~{} MB, 0.1% FPR)",
        //          self.bloom_capacity, bloom_memory / 1_048_576);
        // println!("[memory_config] Shards: {} total, {} in memory ({:.1}%, ~{} MB)",
        //          self.num_shards, self.max_shards_in_memory,
        //          (self.max_shards_in_memory as f64 / self.num_shards as f64) * 100.0,
        //          shard_memory / 1_048_576);
        // println!("[memory_config] Estimated total: {} MB", total_estimated / 1_048_576);
        // println!("[memory_config] ================================\n");
    }

    /// Get default configuration for testing or when system info unavailable
    pub fn default_config() -> Self {
        Self {
            queue_buffer_size: 10_000,
            bloom_capacity: 10_000_000,
            num_shards: 64,
            max_shards_in_memory: 64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_with_small_interner() {
        let config = MemoryConfig::calculate(10 * 1_048_576, 100_000);

        // Should have reasonable values
        assert!(config.queue_buffer_size >= 1000);
        assert!(config.bloom_capacity >= 100_000);
        assert!(config.num_shards >= 64);
        assert!(config.max_shards_in_memory >= 16);
        assert!(config.max_shards_in_memory <= config.num_shards);
    }

    #[test]
    fn test_calculate_with_large_interner() {
        let config = MemoryConfig::calculate(500 * 1_048_576, 1_000_000);

        assert!(config.queue_buffer_size >= 1000);
        assert!(config.bloom_capacity >= 1_000_000);
        assert!(config.num_shards >= 64);
        assert!(config.max_shards_in_memory <= config.num_shards);
    }

    #[test]
    fn test_calculate_with_zero_results() {
        let config = MemoryConfig::calculate(10 * 1_048_576, 0);

        // Should use defaults for zero results
        assert!(config.bloom_capacity >= 1_000_000);
        assert_eq!(config.num_shards, 64);
    }

    #[test]
    fn test_default_config() {
        let config = MemoryConfig::default_config();

        assert_eq!(config.queue_buffer_size, 10_000);
        assert_eq!(config.bloom_capacity, 10_000_000);
        assert_eq!(config.num_shards, 64);
        assert_eq!(config.max_shards_in_memory, 64);
    }

    #[test]
    fn estimate_and_scale_down_to_budget() {
        let config = MemoryConfig::default_config();
        let interner_bytes = 1_000_000;
        let estimate = config.estimate_bytes(interner_bytes);
        assert!(estimate > 0);

        // Budget equal to estimate should succeed (no scaling needed).
        let budget_ok = estimate;
        let scaled_same = config
            .scale_to_budget(budget_ok, interner_bytes)
            .expect("should accept budget equal to estimate");
        assert_eq!(scaled_same.queue_buffer_size, config.queue_buffer_size);

        // Budget far below minimum should fail to scale.
        let budget_too_small = 1;
        assert!(
            config
                .scale_to_budget(budget_too_small, interner_bytes)
                .is_none(),
            "scaling should fail when budget is below any viable configuration"
        );
    }

    #[test]
    fn test_rebalancing_with_growing_results() {
        // Test that bloom and shards scale appropriately with result count

        // Small result set (below 1M minimum for bloom)
        let config_small = MemoryConfig::calculate(10 * 1_048_576, 10_000);
        assert_eq!(
            config_small.bloom_capacity, 1_000_000,
            "Bloom should use minimum: {}",
            config_small.bloom_capacity
        );
        assert_eq!(config_small.num_shards, 64, "Should have minimum shards");

        // Medium result set (still below 1M, but more shards)
        let config_medium = MemoryConfig::calculate(10 * 1_048_576, 100_000);
        assert_eq!(
            config_medium.bloom_capacity, 1_000_000,
            "Bloom should use minimum: {}",
            config_medium.bloom_capacity
        );
        assert_eq!(
            config_medium.num_shards, 64,
            "10 shards by formula, but clamped to 64 minimum: {}",
            config_medium.num_shards
        );

        // Large result set (1M results = 3M bloom, ~100 shards)
        let config_large = MemoryConfig::calculate(10 * 1_048_576, 1_000_000);
        assert_eq!(
            config_large.bloom_capacity, 3_000_000,
            "Bloom should be 3x results: {}",
            config_large.bloom_capacity
        );
        assert_eq!(
            config_large.num_shards, 100,
            "Should be 100 shards (1M/10K): {}",
            config_large.num_shards
        );

        // Very large result set (10M results = 30M bloom, 1000 shards)
        let config_xlarge = MemoryConfig::calculate(10 * 1_048_576, 10_000_000);
        assert_eq!(
            config_xlarge.bloom_capacity, 30_000_000,
            "Bloom should be 3x results: {}",
            config_xlarge.bloom_capacity
        );
        assert_eq!(
            config_xlarge.num_shards, 1000,
            "Should be 1000 shards (10M/10K): {}",
            config_xlarge.num_shards
        );

        // Verify scaling relationship: larger datasets get more resources
        assert!(
            config_large.bloom_capacity > config_medium.bloom_capacity,
            "Large should exceed medium"
        );
        assert!(
            config_xlarge.bloom_capacity > config_large.bloom_capacity,
            "XLarge should exceed large"
        );

        assert!(
            config_large.num_shards > config_medium.num_shards,
            "Large should have more shards"
        );
        assert!(
            config_xlarge.num_shards > config_large.num_shards,
            "XLarge should have more shards"
        );

        // Verify that max_shards_in_memory is constrained by available memory and num_shards
        assert!(config_small.max_shards_in_memory <= config_small.num_shards);
        assert!(config_large.max_shards_in_memory <= config_large.num_shards);
        assert!(config_xlarge.max_shards_in_memory <= config_xlarge.num_shards);
    }
}
