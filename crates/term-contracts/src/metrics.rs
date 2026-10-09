//! Measurement contract: quality-tagged metrics and host/workload samples
//! (spec `03-resources.md` §2).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{U64String, WorkloadId};

/// Every externally shown number is either measured, estimated, or
/// unavailable with a reason — never a silent 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum MetricQuality {
    Measured,
    Estimated,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Metric<T> {
    pub value: Option<T>,
    /// Collector identity, e.g. `sysinfo`, `cgroup.v2`, `win32.job`.
    pub source: String,
    pub quality: MetricQuality,
    pub reason: Option<String>,
}

impl<T> Metric<T> {
    pub fn measured(source: &str, value: T) -> Self {
        Self {
            value: Some(value),
            source: source.to_string(),
            quality: MetricQuality::Measured,
            reason: None,
        }
    }

    pub fn estimated(source: &str, value: T) -> Self {
        Self {
            value: Some(value),
            source: source.to_string(),
            quality: MetricQuality::Estimated,
            reason: None,
        }
    }

    pub fn unavailable(source: &str, reason: impl Into<String>) -> Self {
        Self {
            value: None,
            source: source.to_string(),
            quality: MetricQuality::Unavailable,
            reason: Some(reason.into()),
        }
    }
}

/// Host memory pressure classification with hysteresis handled daemon-side.
/// The same three-level vocabulary also carries CPU saturation
/// (spec `08-pressure-relief.md` §1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "UPPERCASE")]
pub enum PressureLevel {
    #[default]
    Normal,
    Warning,
    Critical,
}

/// How completely a workload's usage numbers cover its process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UsageCoverage {
    Group,
    ObservedTree,
    Partial,
}

/// Per-workload usage snapshot. Field kinds are NOT interchangeable and are
/// never summed with each other (resident vs accounted vs committed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkloadUsage {
    pub workload_id: WorkloadId,
    /// Logical cores; 1.0 = one core fully busy.
    pub cpu_cores: Metric<f64>,
    pub resident_bytes: Metric<U64String>,
    /// Linux cgroup accounting value when available.
    pub accounted_bytes: Metric<U64String>,
    /// Windows job commit charge when available.
    pub committed_bytes: Metric<U64String>,
    pub read_bytes_per_sec: Metric<f64>,
    pub write_bytes_per_sec: Metric<f64>,
    pub network_rx_bytes_per_sec: Metric<f64>,
    pub network_tx_bytes_per_sec: Metric<f64>,
    pub process_count: Metric<u32>,
    pub coverage: UsageCoverage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiskSample {
    pub mount: String,
    pub capacity_bytes: Metric<U64String>,
    pub free_bytes: Metric<U64String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InterfaceSample {
    pub name: String,
    pub rx_bytes_per_sec: Metric<f64>,
    pub tx_bytes_per_sec: Metric<f64>,
    /// Loopback and virtual interfaces are flagged; the default aggregate
    /// excludes them.
    pub is_loopback: bool,
}

/// Host-level sample. All rates are computed from monotonic deltas; the first
/// differential sample after start is `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HostSample {
    /// Monotonic milliseconds at sample time (never wall clock).
    pub monotonic_ms: u64,
    pub physical_total_bytes: Metric<U64String>,
    pub physical_available_bytes: Metric<U64String>,
    /// OS-reported used physical memory (app + wired + compressed, minus
    /// purgeable) — the same figure Activity Monitor / `sysinfo::used_memory`
    /// shows. Distinct from `total - available`: on macOS `available` counts
    /// reclaimable cache (inactive pages), so `total - available` reads far
    /// lower than the real footprint. Older daemons omit this → `None`, and the
    /// UI falls back to `total - available`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub physical_used_bytes: Option<Metric<U64String>>,
    pub swap_used_bytes: Metric<U64String>,
    pub pressure: PressureLevel,
    /// CPU saturation level with hysteresis (spec `08-pressure-relief.md`
    /// §1); older daemons omit it → NORMAL.
    #[serde(default)]
    pub cpu_pressure: PressureLevel,
    pub cpu_cores_used: Metric<f64>,
    pub logical_cpu_count: u32,
    pub disks: Vec<DiskSample>,
    pub interfaces: Vec<InterfaceSample>,
}

impl HostSample {
    /// Effective availability for admission math: `None` when stale/unavailable
    /// (the caller must then treat telemetry as stale, not as zero).
    pub fn available_bytes(&self) -> Option<u64> {
        match (
            &self.physical_available_bytes.value,
            &self.physical_available_bytes.quality,
        ) {
            (Some(v), MetricQuality::Measured) | (Some(v), MetricQuality::Estimated) => {
                Some(v.get())
            }
            _ => None,
        }
    }

    pub fn total_bytes(&self) -> Option<u64> {
        self.physical_total_bytes.value.as_ref().map(|v| v.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_is_not_zero() {
        let m: Metric<u32> = Metric::unavailable("sysinfo", "permission denied");
        assert_eq!(m.value, None);
        assert_eq!(m.quality, MetricQuality::Unavailable);
        assert!(m.reason.is_some());
    }

    /// 옛 데몬 페이로드에는 `cpu_pressure`가 없다 — NORMAL로 읽힌다.
    #[test]
    fn legacy_host_sample_without_cpu_pressure_reads_as_normal() {
        let legacy = serde_json::json!({
            "monotonic_ms": 1_000,
            "physical_total_bytes": {"value": null, "source": "sysinfo", "quality": "unavailable", "reason": "n/a"},
            "physical_available_bytes": {"value": null, "source": "sysinfo", "quality": "unavailable", "reason": "n/a"},
            "swap_used_bytes": {"value": null, "source": "sysinfo", "quality": "unavailable", "reason": "n/a"},
            "pressure": "WARNING",
            "cpu_cores_used": {"value": null, "source": "sysinfo", "quality": "unavailable", "reason": "n/a"},
            "logical_cpu_count": 8,
            "disks": [],
            "interfaces": []
        });
        let back: HostSample = serde_json::from_value(legacy).expect("legacy sample parses");
        assert_eq!(back.pressure, PressureLevel::Warning);
        assert_eq!(back.cpu_pressure, PressureLevel::Normal);
    }

    #[test]
    fn host_availability_requires_a_value() {
        let mut host = HostSample {
            monotonic_ms: 1_000,
            physical_total_bytes: Metric::measured("sysinfo", U64String::new(16 << 30).unwrap()),
            physical_available_bytes: Metric::measured("sysinfo", U64String::new(8 << 30).unwrap()),
            physical_used_bytes: None,
            swap_used_bytes: Metric::unavailable("sysinfo", "n/a"),
            pressure: PressureLevel::Normal,
            cpu_pressure: PressureLevel::Normal,
            cpu_cores_used: Metric::estimated("sysinfo", 1.2),
            logical_cpu_count: 8,
            disks: vec![],
            interfaces: vec![],
        };
        assert_eq!(host.available_bytes(), Some(8 << 30));
        assert_eq!(host.total_bytes(), Some(16 << 30));
        host.physical_available_bytes = Metric::unavailable("sysinfo", "first sample");
        assert_eq!(host.available_bytes(), None);
    }
}
