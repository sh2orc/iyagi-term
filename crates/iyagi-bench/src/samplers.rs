//! Process-level CPU/RSS sampling of the daemon via sysinfo, plus host
//! metadata. Mirrors the daemon telemetry sampler's selective refresh
//! (never `refresh_all`, never cmd/env reads).

use sysinfo::{ProcessesToUpdate, System};

use crate::report::HostInfo;

/// Sampling state for one target process.
pub struct ProcSampler {
    system: System,
    pid: sysinfo::Pid,
}

#[derive(Debug, Clone, Copy)]
pub struct ProcReading {
    /// CPU usage percent since the previous refresh (100 % == one core).
    pub cpu_percent: f32,
    /// Resident set size in bytes.
    pub rss_bytes: u64,
}

impl ProcSampler {
    pub fn new(pid: u32) -> ProcSampler {
        let mut system = System::new();
        // Establish the CPU-time baseline: the first differential is only
        // valid from the second refresh on (sysinfo semantics).
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]),
            false,
            sysinfo::ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory(),
        );
        ProcSampler {
            system,
            pid: sysinfo::Pid::from_u32(pid),
        }
    }

    /// Refresh and read. `None` when the process vanished.
    pub fn refresh(&mut self) -> Option<ProcReading> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            false,
            sysinfo::ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory(),
        );
        let process = self.system.process(self.pid)?;
        Some(ProcReading {
            cpu_percent: process.cpu_usage(),
            rss_bytes: process.memory(),
        })
    }
}

/// One-shot host metadata for the report header.
pub fn host_info() -> HostInfo {
    let mut system = System::new();
    system.refresh_memory();
    system.refresh_cpu_all();
    let cpu_brand = system
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "unknown".into());
    HostInfo {
        os: System::name().unwrap_or_else(|| std::env::consts::OS.into()),
        os_version: System::long_os_version()
            .unwrap_or_else(|| System::os_version().unwrap_or_else(|| "unknown".into())),
        arch: std::env::consts::ARCH.into(),
        cpu_count: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or_else(|_| system.cpus().len().max(1)),
        cpu_brand,
        memory_total_bytes: system.total_memory(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_info_is_populated() {
        let info = host_info();
        assert!(!info.os.is_empty());
        assert!(info.cpu_count >= 1);
        assert!(info.memory_total_bytes > 0);
    }

    #[test]
    fn proc_sampler_reports_self() {
        let pid = std::process::id();
        let mut sampler = ProcSampler::new(pid);
        // Second refresh carries a valid differential.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let reading = sampler.refresh().expect("self process");
        assert!(reading.cpu_percent >= 0.0);
        assert!(reading.rss_bytes > 0);
    }

    #[test]
    fn proc_sampler_missing_process_is_none() {
        let mut sampler = ProcSampler::new(u32::MAX - 7);
        assert!(sampler.refresh().is_none());
    }
}
