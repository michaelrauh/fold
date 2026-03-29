use crate::FoldError;
use crate::generation_store::Role;

const GIB: usize = 1024 * 1024 * 1024;
const MIB: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct MemoryBudget {
    pub process_claim_bytes: usize,
    pub system_reserve_bytes: usize,
    pub offload_headroom_bytes: usize,
    pub compaction_arena_bytes: usize,
    pub work_cache_bytes: usize,
    pub work_segment_max_bytes: usize,
    pub fan_in: usize,
    pub read_buf_bytes: usize,
    pub bufwriter_capacity: usize,
    pub history_cache_bytes: usize,
    pub landing_flush_threshold: usize,
}

impl MemoryBudget {
    pub fn for_role(role: Role, total_ram_bytes: usize) -> Result<Self, FoldError> {
        let leader_claim = env_or_default("FOLD_MEMORY_LEADER_MAX_BYTES", 8 * GIB);
        let follower_claim = env_or_default("FOLD_MEMORY_FOLLOWER_MAX_BYTES", 2 * GIB);
        let system_reserve = env_or_default("FOLD_MEMORY_SYSTEM_RESERVE_BYTES", 6 * GIB);
        let offload_headroom = env_or_default("FOLD_MEMORY_OFFLOAD_HEADROOM_BYTES", 512 * MIB);

        let process_claim_bytes = match role {
            Role::Leader => leader_claim,
            Role::Follower => follower_claim,
        };

        if total_ram_bytes <= system_reserve
            || process_claim_bytes > total_ram_bytes
            || process_claim_bytes.saturating_add(system_reserve) > total_ram_bytes
        {
            return Err(FoldError::Other(format!(
                "memory budget is not satisfiable on this host: total_ram={} reserve={} claim={}",
                total_ram_bytes, system_reserve, process_claim_bytes
            )));
        }

        let (
            compaction_arena_bytes,
            work_cache_bytes,
            work_segment_max_bytes,
            fan_in,
            read_buf_bytes,
            bufwriter_capacity,
            history_cache_bytes,
            landing_flush_threshold,
        ) = match role {
            Role::Leader => (
                384 * MIB,
                256 * MIB,
                64 * MIB,
                64,
                256 * 1024,
                1024 * 1024,
                128 * MIB,
                1024 * 1024,
            ),
            Role::Follower => (
                128 * MIB,
                64 * MIB,
                16 * MIB,
                32,
                128 * 1024,
                512 * 1024,
                32 * MIB,
                512 * 1024,
            ),
        };

        Ok(Self {
            process_claim_bytes,
            system_reserve_bytes: system_reserve,
            offload_headroom_bytes: offload_headroom,
            compaction_arena_bytes,
            work_cache_bytes,
            work_segment_max_bytes,
            fan_in,
            read_buf_bytes,
            bufwriter_capacity,
            history_cache_bytes,
            landing_flush_threshold,
        })
    }
}

fn env_or_default(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_budget_uses_expected_defaults() {
        let budget = MemoryBudget::for_role(Role::Leader, 16 * GIB).unwrap();
        assert_eq!(budget.process_claim_bytes, 8 * GIB);
        assert_eq!(budget.system_reserve_bytes, 6 * GIB);
        assert_eq!(budget.offload_headroom_bytes, 512 * MIB);
        assert_eq!(budget.compaction_arena_bytes, 384 * MIB);
        assert_eq!(budget.work_cache_bytes, 256 * MIB);
        assert_eq!(budget.work_segment_max_bytes, 64 * MIB);
        assert_eq!(budget.fan_in, 64);
        assert_eq!(budget.read_buf_bytes, 256 * 1024);
    }

    #[test]
    fn follower_budget_uses_expected_defaults() {
        let budget = MemoryBudget::for_role(Role::Follower, 16 * GIB).unwrap();
        assert_eq!(budget.process_claim_bytes, 2 * GIB);
        assert_eq!(budget.offload_headroom_bytes, 512 * MIB);
        assert_eq!(budget.compaction_arena_bytes, 128 * MIB);
        assert_eq!(budget.work_cache_bytes, 64 * MIB);
        assert_eq!(budget.work_segment_max_bytes, 16 * MIB);
        assert_eq!(budget.fan_in, 32);
        assert_eq!(budget.read_buf_bytes, 128 * 1024);
    }

    #[test]
    fn rejects_claim_plus_reserve_over_total_ram() {
        let err = MemoryBudget::for_role(Role::Leader, 13 * GIB).unwrap_err();
        assert!(err.to_string().contains("memory budget is not satisfiable"));
    }
}
