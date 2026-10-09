//! Ticket I09 — sysinfo telemetry sampler (spec `03-resources.md` §2).
//!
//! One daemon-wide [`sysinfo::System`] instance is reused behind
//! [`sampler::SysinfoSource`] and refreshed *selectively* — never
//! `refresh_all`. Host metrics are polled at 1 s, disk capacity at 10 s,
//! interfaces on every host poll; per-workload samples refresh only the exact
//! pid list a workload tracks. The first differential sample for any counter,
//! counter resets, and permission gaps surface as `null` with a reason —
//! never a silent 0. No argv/env is ever read (see
//! [`sampler::workload_refresh_kind`], which requests cpu/memory/io only).
//!
//! Timing constants mirror `docs/implementation/defaults.json` `timing_ms`
//! and `limits.graph_samples`.

pub mod cadence;
pub mod pressure;
pub mod rates;
pub mod ring;
pub mod sampler;

pub use cadence::CadenceGate;
pub use pressure::classify_pressure;
pub use rates::{counter_delta_rate, Delta};
pub use ring::SampleRing;
pub use sampler::{
    default_aggregate, is_loopback_interface, Clock, CpuReading, DiskReading, InterfaceReading,
    MemoryReading, NetworkAggregate, PlatformSource, ProcessReading, SysinfoSource, SystemClock,
    TelemetrySampler,
};

/// `timing_ms.telemetry` — host sample cadence.
pub const HOST_TELEMETRY_INTERVAL_MS: u64 = 1_000;
/// `timing_ms.process_inventory` — process tree re-enumeration cadence
/// (driven by the workload tracker's caller, exposed here for schedulers).
pub const PROCESS_INVENTORY_INTERVAL_MS: u64 = 2_000;
/// `timing_ms.background_process_detail` — background workload detail cadence
/// (caller-driven; exposed for schedulers).
pub const BACKGROUND_PROCESS_DETAIL_INTERVAL_MS: u64 = 5_000;
/// `timing_ms.disk_capacity` — disk capacity/free refresh cadence.
pub const DISK_CAPACITY_INTERVAL_MS: u64 = 10_000;
/// `timing_ms.telemetry_stale` — samples older than this are stale; workload
/// baselines are dropped and upstream `M_i` is treated as 0 (spec §3).
pub const TELEMETRY_STALE_MS: u64 = 3_000;
/// `limits.graph_samples` — ring buffer capacity for UI graphs.
pub const GRAPH_SAMPLE_CAPACITY: usize = 300;
