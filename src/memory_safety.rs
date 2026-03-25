use crate::error::MEMORY_BUDGET_ERROR_PREFIX;
use crate::metrics::Metrics;
use std::cell::{Cell, RefCell};
use std::io;
use std::process::Command;
use std::thread_local;

const PHASE_HEADROOM_RESERVE_BYTES: usize = 256 * 1024 * 1024;

thread_local! {
    static PROCESS_CLAIM_BYTES: Cell<usize> = const { Cell::new(0) };
    static METRICS_HANDLE: RefCell<Option<Metrics>> = const { RefCell::new(None) };
}

pub struct ScopedProcessMemory {
    prev_claim_bytes: usize,
    prev_metrics: Option<Metrics>,
}

impl ScopedProcessMemory {
    pub fn new(claim_bytes: usize, metrics: Option<Metrics>) -> Self {
        let prev_claim_bytes = PROCESS_CLAIM_BYTES.with(|slot| {
            let prev = slot.get();
            slot.set(claim_bytes);
            prev
        });
        let prev_metrics = METRICS_HANDLE.with(|slot| {
            let mut slot = slot.borrow_mut();
            let prev = slot.clone();
            *slot = metrics;
            prev
        });
        Self {
            prev_claim_bytes,
            prev_metrics,
        }
    }
}

impl Drop for ScopedProcessMemory {
    fn drop(&mut self) {
        PROCESS_CLAIM_BYTES.with(|slot| slot.set(self.prev_claim_bytes));
        METRICS_HANDLE.with(|slot| *slot.borrow_mut() = self.prev_metrics.clone());
    }
}

pub fn current_process_claim_bytes() -> Option<usize> {
    PROCESS_CLAIM_BYTES.with(|slot| {
        let claim = slot.get();
        if claim == 0 { None } else { Some(claim) }
    })
}

pub fn phase_headroom_reserve_bytes() -> usize {
    PHASE_HEADROOM_RESERVE_BYTES
}

pub fn ensure_phase_headroom(bytes_needed: usize, reason: &str) -> io::Result<()> {
    let Some(claim_bytes) = current_process_claim_bytes() else {
        return Ok(());
    };
    let rss_bytes = current_process_rss_bytes();
    let effective_limit = claim_bytes.saturating_sub(PHASE_HEADROOM_RESERVE_BYTES);
    if rss_bytes.saturating_add(bytes_needed) <= effective_limit {
        return Ok(());
    }

    let message = format!(
        "{} reason={} rss_bytes={} bytes_needed={} claim_bytes={} reserve_bytes={}",
        MEMORY_BUDGET_ERROR_PREFIX,
        reason,
        rss_bytes,
        bytes_needed,
        claim_bytes,
        PHASE_HEADROOM_RESERVE_BYTES
    );

    METRICS_HANDLE.with(|slot| {
        if let Some(metrics) = slot.borrow().as_ref() {
            metrics.add_log(format!("Memory guard denied phase: {}", message));
        }
    });

    Err(io::Error::other(message))
}

fn parse_status_field_kib(contents: &str, field: &str) -> Option<u64> {
    contents
        .lines()
        .find(|line| line.starts_with(field))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u64>().ok())
}

pub fn parse_linux_status_rss_bytes(contents: &str) -> Option<usize> {
    parse_status_field_kib(contents, "VmRSS:")
        .and_then(|kib| kib.checked_mul(1024))
        .and_then(|bytes| usize::try_from(bytes).ok())
}

#[cfg(target_os = "linux")]
fn read_linux_process_rss_bytes() -> Option<usize> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|contents| parse_linux_status_rss_bytes(&contents))
}

#[cfg(target_os = "linux")]
fn read_linux_total_ram_bytes() -> Option<usize> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|contents| parse_status_field_kib(&contents, "MemTotal:"))
        .and_then(|kib| kib.checked_mul(1024))
        .and_then(|bytes| usize::try_from(bytes).ok())
}

pub fn current_process_rss_bytes() -> usize {
    #[cfg(target_os = "linux")]
    if let Some(bytes) = read_linux_process_rss_bytes() {
        return bytes;
    }

    if let Some(bytes) = read_ps_process_rss_bytes() {
        return bytes;
    }

    let mut sys = sysinfo::System::new();
    if let Ok(pid) = sysinfo::get_current_pid() {
        let _ = sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), false);
        if let Some(proc_) = sys.process(pid) {
            let raw = proc_.memory();
            if let Some(total_bytes) = total_system_ram_bytes().checked_div(1024) {
                if (raw as usize) <= total_bytes.saturating_mul(2) {
                    return (raw as usize).saturating_mul(1024);
                }
            }
            return raw as usize;
        }
    }
    0
}

fn read_ps_process_rss_bytes() -> Option<usize> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rss_kib = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    rss_kib
        .checked_mul(1024)
        .and_then(|bytes| usize::try_from(bytes).ok())
}

pub fn total_system_ram_bytes() -> usize {
    #[cfg(target_os = "linux")]
    if let Some(bytes) = read_linux_total_ram_bytes() {
        return bytes;
    }

    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let raw = sys.total_memory() as usize;
    raw.saturating_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_linux_status_rss_bytes_reads_kib_value() {
        let fixture = "Name:\tfold\nVmRSS:\t  123456 kB\nThreads:\t1\n";
        assert_eq!(parse_linux_status_rss_bytes(fixture), Some(123_456 * 1024));
    }
}
