use sysinfo::System;

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
    /// Memory budget breakdown:
    /// - Interner: varies by vocabulary size (estimated from serialized size)
    /// - Queue buffers: 2 queues * buffer_size * ~200 bytes per ortho
    /// - Bloom filter: ~1.44 bytes per item (optimal for 1% FPR)
    /// - Shards in memory: max_shards * avg_items_per_shard * 12 bytes (usize key + ())
    /// - Runtime overhead: ~20% reserve for working memory
    pub fn calculate(interner_bytes: usize, expected_results: usize) -> Self {
        let mut sys = System::new_all();
        sys.refresh_memory();
        
        let total_memory = sys.total_memory() as usize;
        let target_memory = (total_memory * 75) / 100;
        
        println!("[memory_config] Total system RAM: {} MB", total_memory / 1_048_576);
        println!("[memory_config] Target memory usage: {} MB (75%)", target_memory / 1_048_576);
        println!("[memory_config] Interner size: {} MB", interner_bytes / 1_048_576);
        
        // Reserve 20% for runtime overhead (ortho processing, vectors, etc)
        let runtime_reserve = target_memory / 5;
        let available_for_caches = target_memory.saturating_sub(interner_bytes).saturating_sub(runtime_reserve);
        
        println!("[memory_config] Runtime reserve: {} MB", runtime_reserve / 1_048_576);
        println!("[memory_config] Available for caches: {} MB", available_for_caches / 1_048_576);
        
        // Estimate ortho size (conservative estimate based on typical dimensions)
        // Small orthos: ~80 bytes, large orthos: ~900 bytes, use 200 as middle estimate
        let bytes_per_ortho = 200;
        
        // Bloom filter: ~1.44 bytes per item for 1% false positive rate
        let bytes_per_bloom_item = 2; // Round up for safety
        
        // Shard item: 12 bytes (usize key + () value + hashmap overhead)
        let bytes_per_shard_item = 12;
        
        // Calculate bloom capacity (3x expected results for growth room)
        let bloom_capacity = if expected_results > 0 {
            (expected_results * 3).max(1_000_000)
        } else {
            1_000_000
        };
        
        let bloom_memory = bloom_capacity * bytes_per_bloom_item;
        
        // Calculate remaining memory after bloom filter
        let remaining_after_bloom = available_for_caches.saturating_sub(bloom_memory);
        
        // Split remaining memory between queues (30%) and shards (70%)
        let queue_memory = (remaining_after_bloom * 30) / 100;
        let shard_memory = (remaining_after_bloom * 70) / 100;
        
        // Calculate queue buffer size (2 queues: work_queue and results_queue)
        // Each queue has one buffer in memory
        let queue_buffer_size = (queue_memory / (2 * bytes_per_ortho)).max(1000).min(100_000);
        
        // Calculate shard configuration
        // Target ~10K items per shard for good disk I/O granularity
        let target_items_per_shard = 10_000;
        let num_shards = if expected_results > 0 {
            (expected_results / target_items_per_shard).max(64).min(1024)
        } else {
            64
        };
        
        // Calculate how many shards we can keep in memory
        let memory_per_shard = target_items_per_shard * bytes_per_shard_item;
        let max_shards_in_memory = (shard_memory / memory_per_shard).max(16).min(num_shards);
        
        let config = Self {
            queue_buffer_size,
            bloom_capacity,
            num_shards,
            max_shards_in_memory,
        };
        
        config.print_summary(interner_bytes, runtime_reserve);
        
        config
    }
    
    fn print_summary(&self, interner_bytes: usize, runtime_reserve: usize) {
        let bytes_per_ortho = 200;
        let bytes_per_bloom_item = 2;
        let bytes_per_shard_item = 12;
        let target_items_per_shard = 10_000;
        
        let queue_memory = 2 * self.queue_buffer_size * bytes_per_ortho;
        let bloom_memory = self.bloom_capacity * bytes_per_bloom_item;
        let shard_memory = self.max_shards_in_memory * target_items_per_shard * bytes_per_shard_item;
        let total_estimated = interner_bytes + queue_memory + bloom_memory + shard_memory + runtime_reserve;
        
        println!("\n[memory_config] ===== MEMORY CONFIGURATION =====");
        println!("[memory_config] Queue buffer size: {} orthos (~{} MB per queue)", 
                 self.queue_buffer_size, (self.queue_buffer_size * bytes_per_ortho) / 1_048_576);
        println!("[memory_config] Bloom capacity: {} items (~{} MB)", 
                 self.bloom_capacity, bloom_memory / 1_048_576);
        println!("[memory_config] Shards: {} total, {} in memory (~{} MB)", 
                 self.num_shards, self.max_shards_in_memory, shard_memory / 1_048_576);
        println!("[memory_config] Estimated total: {} MB", total_estimated / 1_048_576);
        println!("[memory_config] ================================\n");
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
    fn test_rebalancing_with_growing_results() {
        // Test that bloom and shards scale appropriately with result count
        
        // Small result set (below 1M minimum for bloom)
        let config_small = MemoryConfig::calculate(10 * 1_048_576, 10_000);
        assert_eq!(config_small.bloom_capacity, 1_000_000, "Bloom should use minimum: {}", config_small.bloom_capacity);
        assert_eq!(config_small.num_shards, 64, "Should have minimum shards");
        
        // Medium result set (still below 1M, but more shards)
        let config_medium = MemoryConfig::calculate(10 * 1_048_576, 100_000);
        assert_eq!(config_medium.bloom_capacity, 1_000_000, "Bloom should use minimum: {}", config_medium.bloom_capacity);
        assert_eq!(config_medium.num_shards, 64, "10 shards by formula, but clamped to 64 minimum: {}", config_medium.num_shards);
        
        // Large result set (1M results = 3M bloom, ~100 shards)
        let config_large = MemoryConfig::calculate(10 * 1_048_576, 1_000_000);
        assert_eq!(config_large.bloom_capacity, 3_000_000, "Bloom should be 3x results: {}", config_large.bloom_capacity);
        assert_eq!(config_large.num_shards, 100, "Should be 100 shards (1M/10K): {}", config_large.num_shards);
        
        // Very large result set (10M results = 30M bloom, 1000 shards)
        let config_xlarge = MemoryConfig::calculate(10 * 1_048_576, 10_000_000);
        assert_eq!(config_xlarge.bloom_capacity, 30_000_000, "Bloom should be 3x results: {}", config_xlarge.bloom_capacity);
        assert_eq!(config_xlarge.num_shards, 1000, "Should be 1000 shards (10M/10K): {}", config_xlarge.num_shards);
        
        // Verify scaling relationship: larger datasets get more resources
        assert!(config_large.bloom_capacity > config_medium.bloom_capacity, "Large should exceed medium");
        assert!(config_xlarge.bloom_capacity > config_large.bloom_capacity, "XLarge should exceed large");
        
        assert!(config_large.num_shards > config_medium.num_shards, "Large should have more shards");
        assert!(config_xlarge.num_shards > config_large.num_shards, "XLarge should have more shards");
        
        // Verify that max_shards_in_memory is constrained by available memory and num_shards
        assert!(config_small.max_shards_in_memory <= config_small.num_shards);
        assert!(config_large.max_shards_in_memory <= config_large.num_shards);
        assert!(config_xlarge.max_shards_in_memory <= config_xlarge.num_shards);
    }
}
