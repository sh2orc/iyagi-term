//! Telemetry sampler: host sampling, workload usage tracking, and the
//! sysinfo boundary (spec `03-resources.md` §2).
//!
//! * One [`sysinfo::System`] is reused for the sampler's whole lifetime and
//!   refreshed selectively — never `refresh_all`.
//! * Host cadence 1 s (`timing_ms.telemetry`), disk capacity 10 s
//!   (`timing_ms.disk_capacity`), interfaces on every host poll. Polls
//!   between cadence points return the cached sample unchanged so the
//!   reported `monotonic_ms` keeps exposing true data age.
//! * The first differential sample, counter resets, and unreadable data are
//!   `null` + reason — never a silent 0.
//! * Workload sampling refreshes ONLY the pids the workload's provider
//!   returns, with cpu/memory/io refresh kinds and **without** cmd/env/exe/
//!   cwd/root/user/tasks — no argv/env ever enters telemetry.
//! * Workload baselines older than `timing_ms.telemetry_stale` (3 s) are
//!   dropped; [`TelemetrySampler::workload_sample_age_ms`] exposes sample
//!   age so the scheduler can treat `M_i` as 0 past that point (spec §3).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use term_contracts::ids::{ProcessIdentity, U64String, WorkloadId};
use term_contracts::metrics::{
    DiskSample, HostSample, InterfaceSample, Metric, MetricQuality, PressureLevel, UsageCoverage,
    WorkloadUsage,
};

use super::cadence::CadenceGate;
use super::pressure::classify_pressure;
use super::rates::{counter_delta_rate, Delta};
use super::ring::SampleRing;
use super::{
    DISK_CAPACITY_INTERVAL_MS, GRAPH_SAMPLE_CAPACITY, HOST_TELEMETRY_INTERVAL_MS,
    TELEMETRY_STALE_MS,
};

/// Collector identity stamped on every metric produced here.
const SRC: &str = "sysinfo";

// ---------------------------------------------------------------------------
// Clock — the daemon-wide injectable monotonic clock from `term-core`
// ---------------------------------------------------------------------------

pub use term_core::clock::Clock;
/// Production clock: [`term_core::MonotonicClock`] (milliseconds since
/// construction), re-exported under the telemetry-facing name.
pub use term_core::MonotonicClock as SystemClock;

// ---------------------------------------------------------------------------
// Platform source (the sysinfo boundary; fakes drive deterministic tests)
// ---------------------------------------------------------------------------

/// Instantaneous host memory counters (bytes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryReading {
    pub total_bytes: u64,
    pub available_bytes: u64,
    /// OS-reported used memory (`sysinfo::used_memory`); not `total - available`.
    pub used_bytes: u64,
    pub swap_used_bytes: u64,
}

/// Host CPU state. `cores_used` is `None` until a differential exists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuReading {
    pub logical_count: u32,
    /// Sum of per-core usage / 100 — delta(cpu_time)/delta(t) per core,
    /// aggregated across logical CPUs. `None` until the source has two
    /// refreshes to difference.
    pub cores_used: Option<f64>,
}

/// Disk capacity snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskReading {
    pub mount: String,
    pub capacity_bytes: u64,
    pub free_bytes: u64,
}

/// Cumulative interface counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InterfaceReading {
    pub name: String,
    pub rx_total_bytes: u64,
    pub tx_total_bytes: u64,
}

/// Per-pid counters used for workload usage. Only cpu/memory/io/pid —
/// deliberately no argv, env, exe, or cwd.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessReading {
    /// Platform start token recorded to notice pid reuse between polls.
    pub start_token: String,
    /// 시작 시각(epoch ms). 직전 폴 뒤에 생긴 프로세스의 CPU를 통째로 세는
    /// 판정에 쓴다. 모르면 `None`(그런 프로세스는 다음 폴부터 차분한다).
    pub start_epoch_ms: Option<u64>,
    /// 부모 pid(0 = 모름). 추적 중인 부모가 회수한 자식의 CPU를 두 번 세지
    /// 않는 데 쓴다.
    pub ppid: u32,
    /// Accumulated CPU time in CPU-milliseconds.
    pub cpu_ms: u64,
    /// 회수된(wait된) 자식들의 CPU 누계(ms). 짧게 살다 간 컴파일러·테스트
    /// 프로세스의 CPU는 폴 사이에 태어나 죽어 따로 보이지 않고 여기로만
    /// 들어온다. 0 = 모름·없음.
    pub child_cpu_ms: u64,
    pub rss_bytes: u64,
    /// 물리 footprint(macOS)·익명+공유+스왑(Linux). 있으면 RSS 대신 쓴다 —
    /// RSS 합은 공유 페이지를 이중으로 세고 압축된 페이지를 뺀다.
    pub footprint_bytes: Option<u64>,
    pub read_total_bytes: u64,
    pub write_total_bytes: u64,
}

/// 직전 폴보다 이만큼 먼저 시작한 프로세스까지 "새 프로세스"로 본다 — 멤버
/// 목록 재스캔(2 s)과 폴(1 s) 사이의 지연을 덮는다. 그보다 오래된 프로세스가
/// 늦게 멤버가 되면(부모가 바뀐 자손 재부착·태그 고아) 평생 누적 CPU를 한
/// 틱에 쏟지 않도록 다음 폴부터 차분한다.
const NEW_PROCESS_SLACK_MS: u64 = 3_000;

/// Selective-refresh boundary over one shared sysinfo instance. The
/// production implementation is [`SysinfoSource`]; tests inject fakes.
pub trait PlatformSource: Send {
    /// Refresh host memory and per-core CPU usage deltas.
    fn refresh_host(&mut self);
    /// Refresh disk capacity/free counters.
    fn refresh_disks(&mut self);
    /// Refresh network interface counters.
    fn refresh_networks(&mut self);
    /// Refresh only the exact pid list given (cpu/memory/io refresh kinds).
    fn refresh_processes(&mut self, pids: &[u32]);
    fn read_memory(&self) -> MemoryReading;
    fn read_cpu(&self) -> CpuReading;
    fn read_disks(&self) -> Vec<DiskReading>;
    fn read_interfaces(&self) -> Vec<InterfaceReading>;
    /// `None` when the pid is gone or no longer exists.
    fn read_process(&self, pid: u32) -> Option<ProcessReading>;
    /// 지금 시각(epoch ms) — 새 프로세스 판정의 기준. 시험이 바꾼다.
    fn now_epoch_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Refresh kinds used for workload pids: cpu + memory + io only. No
/// `cmd`/`environ`/`exe`/`cwd`/`root`/`user`/`tasks` — telemetry must never
/// capture argv/env, and skipping the extra /proc walks is cheaper.
pub fn workload_refresh_kind() -> sysinfo::ProcessRefreshKind {
    sysinfo::ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_disk_usage()
        .without_tasks()
}

/// Production source: ONE `sysinfo::System` (daemon-wide singleton usage
/// pattern), one `Disks`, one `Networks`, minimally seeded.
pub struct SysinfoSource {
    system: sysinfo::System,
    disks: sysinfo::Disks,
    networks: sysinfo::Networks,
    /// sysinfo reports 0% usage until it has two refreshes to difference.
    cpu_refresh_count: u64,
    /// 네이티브 계량값(macOS·Linux) — `refresh_processes`가 채운다.
    native: HashMap<u32, crate::native_proc::PidUsage>,
    /// 지난 갱신이 요청한 pid(sysinfo 경로) — 다음 갱신에 함께 넘겨 죽은
    /// pid의 레코드를 버리게 한다(`ProcessesToUpdate::Some`은 요청한 pid만
    /// 정리한다).
    last_pids: Vec<u32>,
}

impl SysinfoSource {
    /// Seeds totals and the CPU baseline with minimal refreshes; the process
    /// list stays untouched (no `refresh_all`, ever).
    pub fn new() -> Self {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        system.refresh_cpu_usage();
        Self {
            system,
            disks: sysinfo::Disks::new_with_refreshed_list(),
            networks: sysinfo::Networks::new_with_refreshed_list(),
            cpu_refresh_count: 1,
            native: HashMap::new(),
            last_pids: Vec::new(),
        }
    }
}

impl Default for SysinfoSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformSource for SysinfoSource {
    fn refresh_host(&mut self) {
        self.system.refresh_memory();
        self.system.refresh_cpu_usage();
        self.cpu_refresh_count += 1;
    }

    fn refresh_disks(&mut self) {
        // Storage only; io_usage stays unrefreshed (host disk throughput is
        // unavailable in R1 pending a validated native collector).
        self.disks
            .refresh_specifics(false, sysinfo::DiskRefreshKind::nothing().with_storage());
    }

    fn refresh_networks(&mut self) {
        self.networks.refresh(true);
    }

    fn refresh_processes(&mut self, pids: &[u32]) {
        // macOS·Linux: 네이티브 계량(rusage·/proc) — CPU에 회수된 자식 시간,
        // 메모리에 footprint가 함께 온다. sysinfo의 갱신은 거치지 않는다.
        if crate::native_proc::supported() {
            self.native = pids
                .iter()
                .filter_map(|&pid| crate::native_proc::pid_usage(pid).map(|usage| (pid, usage)))
                .collect();
            return;
        }
        // 지난번에 요청했지만 이번에 빠진 pid도 함께 넘겨 죽은 레코드를 버린다.
        let mut union: Vec<u32> = pids.to_vec();
        union.extend(
            self.last_pids
                .iter()
                .copied()
                .filter(|pid| !pids.contains(pid)),
        );
        self.last_pids = pids.to_vec();
        let list: Vec<sysinfo::Pid> = union.iter().map(|&p| sysinfo::Pid::from_u32(p)).collect();
        // `remove_dead_processes = true`: sysinfo keeps one `Process` record
        // per pid it was ever handed, so a long-lived daemon that sampled
        // thousands of short-lived workload pids would grow forever. With
        // `ProcessesToUpdate::Some` only the *requested* pids that failed to
        // refresh are dropped — other workloads' records stay put — and
        // `read_process` already treats a missing record as "gone".
        self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&list),
            true,
            workload_refresh_kind(),
        );
    }

    fn read_memory(&self) -> MemoryReading {
        MemoryReading {
            total_bytes: self.system.total_memory(),
            available_bytes: self.system.available_memory(),
            used_bytes: self.system.used_memory(),
            swap_used_bytes: self.system.used_swap(),
        }
    }

    fn read_cpu(&self) -> CpuReading {
        let logical = self.system.cpus().len() as u32;
        let cores_used = (self.cpu_refresh_count >= 2).then(|| {
            let sum: f64 = self
                .system
                .cpus()
                .iter()
                .map(|cpu| f64::from(cpu.cpu_usage()))
                .sum::<f64>()
                / 100.0;
            sum.clamp(0.0, logical as f64)
        });
        CpuReading {
            logical_count: logical,
            cores_used,
        }
    }

    fn read_disks(&self) -> Vec<DiskReading> {
        self.disks
            .list()
            .iter()
            .map(|disk| DiskReading {
                mount: disk.mount_point().to_string_lossy().into_owned(),
                capacity_bytes: disk.total_space(),
                free_bytes: disk.available_space(),
            })
            .collect()
    }

    fn read_interfaces(&self) -> Vec<InterfaceReading> {
        self.networks
            .list()
            .iter()
            .map(|(name, data)| InterfaceReading {
                name: name.clone(),
                rx_total_bytes: data.total_received(),
                tx_total_bytes: data.total_transmitted(),
            })
            .collect()
    }

    fn read_process(&self, pid: u32) -> Option<ProcessReading> {
        if crate::native_proc::supported() {
            let usage = self.native.get(&pid)?;
            return Some(ProcessReading {
                start_token: usage.start_token.clone(),
                start_epoch_ms: Some(usage.start_epoch_ms),
                ppid: usage.ppid,
                cpu_ms: usage.cpu_ms,
                child_cpu_ms: usage.child_cpu_ms,
                rss_bytes: usage.resident_bytes,
                footprint_bytes: usage.footprint_bytes,
                read_total_bytes: usage.read_bytes,
                write_total_bytes: usage.written_bytes,
            });
        }
        let process = self.system.process(sysinfo::Pid::from_u32(pid))?;
        if !process.exists() {
            return None;
        }
        let io = process.disk_usage();
        Some(ProcessReading {
            start_token: process.start_time().to_string(),
            start_epoch_ms: Some(process.start_time().saturating_mul(1_000)),
            ppid: process.parent().map(|parent| parent.as_u32()).unwrap_or(0),
            cpu_ms: process.accumulated_cpu_time(),
            child_cpu_ms: 0,
            rss_bytes: process.memory(),
            footprint_bytes: None,
            read_total_bytes: io.total_read_bytes,
            write_total_bytes: io.total_written_bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// Loopback detection and default aggregate
// ---------------------------------------------------------------------------

/// Best-effort loopback detection from interface names: Unix `lo`/`lo0…`,
/// Windows `Loopback Pseudo-Interface N`.
pub fn is_loopback_interface(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    if lower == "lo" || lower.contains("loopback") {
        return true;
    }
    match lower.strip_prefix("lo") {
        Some(rest) => rest.chars().next().is_some_and(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Default network aggregate (rx/tx). The spec's default sum excludes
/// loopback interfaces; each rate keeps its own quality semantics.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkAggregate {
    pub rx_bytes_per_sec: Metric<f64>,
    pub tx_bytes_per_sec: Metric<f64>,
}

/// Sums rates over non-loopback interfaces only. Never folds loopback
/// traffic into the default aggregate (spec §2).
pub fn default_aggregate(interfaces: &[InterfaceSample]) -> NetworkAggregate {
    let physical: Vec<&InterfaceSample> = interfaces
        .iter()
        .filter(|iface| !iface.is_loopback)
        .collect();
    NetworkAggregate {
        rx_bytes_per_sec: aggregate_component(&physical, |iface| &iface.rx_bytes_per_sec),
        tx_bytes_per_sec: aggregate_component(&physical, |iface| &iface.tx_bytes_per_sec),
    }
}

fn aggregate_component(
    interfaces: &[&InterfaceSample],
    component: impl Fn(&InterfaceSample) -> &Metric<f64>,
) -> Metric<f64> {
    if interfaces.is_empty() {
        return Metric::unavailable(SRC, "no non-loopback interfaces");
    }
    let mut sum = 0.0;
    let mut valued = 0usize;
    for iface in interfaces {
        if let Some(rate) = component(iface).value {
            sum += rate;
            valued += 1;
        }
    }
    if valued == 0 {
        return Metric::unavailable(SRC, "all non-loopback interfaces unavailable");
    }
    let degraded = interfaces
        .iter()
        .filter(|iface| {
            let metric = component(iface);
            metric.value.is_none() || metric.quality != MetricQuality::Measured
        })
        .count();
    if degraded == 0 {
        return Metric::measured(SRC, sum);
    }
    Metric {
        value: Some(sum),
        source: SRC.to_string(),
        quality: MetricQuality::Estimated,
        reason: Some(format!(
            "{degraded} of {} interfaces unavailable or estimated",
            interfaces.len()
        )),
    }
}

// ---------------------------------------------------------------------------
// Metric helpers
// ---------------------------------------------------------------------------

/// Byte-count metric with SQLite-bound clamping: values over `i64::MAX`
/// become `unavailable` with a reason instead of a wrong number.
fn bytes_metric(value: u128, quality: MetricQuality, reason: Option<&str>) -> Metric<U64String> {
    if value > i64::MAX as u128 {
        return Metric::unavailable(SRC, "value exceeds SQLite integer bound");
    }
    // Bounded by the check above (U64String::MAX == i64::MAX).
    let encoded = U64String::new(value as u64).expect("value bounded by i64::MAX check");
    Metric {
        value: Some(encoded),
        source: SRC.to_string(),
        quality,
        reason: reason.map(str::to_string),
    }
}

fn rate_metric(prev: Option<u64>, cur: u64, dt: Option<u64>) -> Metric<f64> {
    match (prev, dt) {
        (Some(prev), Some(dt)) => match counter_delta_rate(prev, cur, dt) {
            Delta::Rate(rate) => Metric::measured(SRC, rate),
            Delta::Reset => Metric::unavailable(SRC, "counter reset"),
            Delta::NoDelta => Metric::unavailable(SRC, "zero elapsed time between polls"),
        },
        _ => Metric::unavailable(SRC, "first sample"),
    }
}

// ---------------------------------------------------------------------------
// Workload state
// ---------------------------------------------------------------------------

/// Per-pid differential baseline from the previous workload poll.
#[derive(Debug, Clone)]
struct PidBaseline {
    /// Sysinfo-side start token recorded when the baseline was taken; a
    /// different token on a later poll means the pid was reused by a new
    /// process and the baseline is void.
    start_token: String,
    ppid: u32,
    cpu_ms: u64,
    child_cpu_ms: u64,
    read_total_bytes: u64,
    write_total_bytes: u64,
}

struct WorkloadState {
    provider: Arc<dyn Fn() -> Vec<ProcessIdentity> + Send + Sync>,
    per_pid: HashMap<u32, PidBaseline>,
    last_poll_ms: Option<u64>,
    /// 직전 폴의 epoch 시각(새 프로세스 판정 기준).
    last_poll_epoch_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Sampler
// ---------------------------------------------------------------------------

/// Host + workload sampler over a single shared sysinfo instance.
pub struct TelemetrySampler {
    clock: Arc<dyn Clock>,
    source: Box<dyn PlatformSource>,
    host_gate: CadenceGate,
    disk_gate: CadenceGate,
    last_host: Option<HostSample>,
    host_ring: SampleRing<HostSample>,
    host_prev_ms: Option<u64>,
    iface_prev: HashMap<String, (u64, u64)>,
    workloads: HashMap<WorkloadId, WorkloadState>,
}

impl TelemetrySampler {
    /// Production sampler: a shared [`SysinfoSource`] seeded with minimal
    /// refreshes.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_source(clock, Box::new(SysinfoSource::new()))
    }

    /// Sampler over an injected source (tests, alternative collectors).
    pub fn with_source(clock: Arc<dyn Clock>, source: Box<dyn PlatformSource>) -> Self {
        Self {
            clock,
            source,
            host_gate: CadenceGate::new(HOST_TELEMETRY_INTERVAL_MS),
            disk_gate: CadenceGate::new(DISK_CAPACITY_INTERVAL_MS),
            last_host: None,
            host_ring: SampleRing::new(GRAPH_SAMPLE_CAPACITY),
            host_prev_ms: None,
            iface_prev: HashMap::new(),
            workloads: HashMap::new(),
        }
    }

    /// Host sample honoring cadence gating. Polls before the 1 s cadence
    /// elapses return the cached sample (with its original `monotonic_ms`,
    /// so staleness stays visible); disks refresh only on the 10 s cadence;
    /// interfaces refresh on every committed host poll.
    pub fn poll_host(&mut self, now_ms: u64) -> HostSample {
        if !self.host_gate.due(now_ms) {
            return self
                .last_host
                .clone()
                .expect("cadence gate seeds on its first call, so the cache is always set");
        }
        self.source.refresh_host();
        self.source.refresh_networks();
        if self.disk_gate.due(now_ms) {
            self.source.refresh_disks();
        }
        let sample = self.build_host_sample(now_ms);
        self.last_host = Some(sample.clone());
        self.host_ring.push(sample.clone());
        sample
    }

    /// [`Self::poll_host`] using the injected clock's current time.
    pub fn poll_host_now(&mut self) -> HostSample {
        let now = self.clock.now_ms();
        self.poll_host(now)
    }

    /// Builds a fresh sample and commits the new differential baselines
    /// (host poll time + per-interface counters).
    fn build_host_sample(&mut self, now_ms: u64) -> HostSample {
        let memory = self.source.read_memory();
        let cpu = self.source.read_cpu();
        let dt = self.host_prev_ms.map(|prev| now_ms.saturating_sub(prev));

        let cpu_cores_used = match (dt, cpu.cores_used) {
            (None, _) | (_, None) => Metric::unavailable(SRC, "first sample"),
            (Some(0), _) => Metric::unavailable(SRC, "zero elapsed time between polls"),
            (Some(_), Some(cores)) => Metric::measured(SRC, cores),
        };

        let iface_readings = self.source.read_interfaces();
        let interfaces = iface_readings
            .iter()
            .map(|iface| {
                let prev = self.iface_prev.get(&iface.name).copied();
                InterfaceSample {
                    name: iface.name.clone(),
                    is_loopback: is_loopback_interface(&iface.name),
                    rx_bytes_per_sec: rate_metric(
                        prev.map(|pair| pair.0),
                        iface.rx_total_bytes,
                        dt,
                    ),
                    tx_bytes_per_sec: rate_metric(
                        prev.map(|pair| pair.1),
                        iface.tx_total_bytes,
                        dt,
                    ),
                }
            })
            .collect::<Vec<_>>();
        self.iface_prev = iface_readings
            .into_iter()
            .map(|iface| (iface.name, (iface.rx_total_bytes, iface.tx_total_bytes)))
            .collect();
        self.host_prev_ms = Some(now_ms);

        let disks = self
            .source
            .read_disks()
            .into_iter()
            .map(|disk| DiskSample {
                mount: disk.mount,
                capacity_bytes: bytes_metric(
                    disk.capacity_bytes as u128,
                    MetricQuality::Measured,
                    None,
                ),
                free_bytes: bytes_metric(disk.free_bytes as u128, MetricQuality::Measured, None),
            })
            .collect::<Vec<_>>();

        HostSample {
            monotonic_ms: now_ms,
            physical_total_bytes: bytes_metric(
                memory.total_bytes as u128,
                MetricQuality::Measured,
                None,
            ),
            physical_available_bytes: bytes_metric(
                memory.available_bytes as u128,
                MetricQuality::Measured,
                None,
            ),
            physical_used_bytes: Some(bytes_metric(
                memory.used_bytes as u128,
                MetricQuality::Measured,
                None,
            )),
            swap_used_bytes: bytes_metric(
                memory.swap_used_bytes as u128,
                MetricQuality::Measured,
                None,
            ),
            pressure: classify_pressure(memory.total_bytes, memory.available_bytes),
            // Hysteresis lives in the daemon (term-core `CpuPressureTracker`,
            // spec `08-pressure-relief.md` §1); the sampler has no history to
            // classify CPU saturation with, so it leaves NORMAL for the
            // daemon to overwrite after its tracker runs.
            cpu_pressure: PressureLevel::Normal,
            cpu_cores_used,
            logical_cpu_count: cpu.logical_count,
            disks,
            interfaces,
        }
    }

    /// Ring of the most recent host samples (oldest → newest), for UI graphs.
    pub fn host_history(&self) -> &SampleRing<HostSample> {
        &self.host_ring
    }

    /// Registers (or re-registers) a workload whose process set is resolved
    /// lazily by `pid_provider` at every poll — the provider owns the
    /// inventory cadence (2 s tree walk, 5 s background detail).
    pub fn track_workload(
        &mut self,
        workload_id: WorkloadId,
        pid_provider: Arc<dyn Fn() -> Vec<ProcessIdentity> + Send + Sync>,
    ) {
        self.workloads
            .entry(workload_id)
            .or_insert_with(|| WorkloadState {
                provider: pid_provider,
                per_pid: HashMap::new(),
                last_poll_ms: None,
                last_poll_epoch_ms: None,
            });
    }

    /// Removes a workload from tracking; its baselines are discarded.
    /// Returns whether a workload had been registered.
    pub fn untrack_workload(&mut self, workload_id: &WorkloadId) -> bool {
        self.workloads.remove(workload_id).is_some()
    }

    /// Refreshes ONLY this workload's pids and differences them against the
    /// previous poll. When any tracked pid is no longer visible the coverage
    /// becomes [`UsageCoverage::Partial`], `process_count` counts visible
    /// pids only, and metrics that cannot be computed surface `unavailable`
    /// with reasons — never a silent 0.
    pub fn poll_workload(
        &mut self,
        workload_id: &WorkloadId,
        now_ms: u64,
        coverage_hint: UsageCoverage,
    ) -> Option<WorkloadUsage> {
        let state = self.workloads.get_mut(workload_id)?;
        if let Some(last) = state.last_poll_ms {
            if now_ms.saturating_sub(last) > TELEMETRY_STALE_MS {
                tracing::debug!(
                    workload = %workload_id,
                    age_ms = now_ms.saturating_sub(last),
                    "dropping stale workload baselines"
                );
                state.per_pid.clear();
                state.last_poll_ms = None;
                state.last_poll_epoch_ms = None;
            }
        }
        let identities = (state.provider)();
        let pids: Vec<u32> = identities.iter().map(|identity| identity.pid).collect();
        self.source.refresh_processes(&pids);

        let mut visible: Vec<(u32, ProcessReading)> = Vec::with_capacity(identities.len());
        let mut missing = 0usize;
        for identity in &identities {
            match self.source.read_process(identity.pid) {
                Some(reading) => visible.push((identity.pid, reading)),
                None => missing += 1,
            }
        }

        let dt = state.last_poll_ms.map(|last| now_ms.saturating_sub(last));
        let now_epoch_ms = self.source.now_epoch_ms();
        let usage = build_workload_usage(
            workload_id,
            &visible,
            missing,
            dt,
            &state.per_pid,
            state.last_poll_epoch_ms,
            coverage_hint,
        );

        state.per_pid = visible
            .iter()
            .map(|(pid, reading)| {
                (
                    *pid,
                    PidBaseline {
                        start_token: reading.start_token.clone(),
                        ppid: reading.ppid,
                        cpu_ms: reading.cpu_ms,
                        child_cpu_ms: reading.child_cpu_ms,
                        read_total_bytes: reading.read_total_bytes,
                        write_total_bytes: reading.write_total_bytes,
                    },
                )
            })
            .collect();
        state.last_poll_ms = Some(now_ms);
        state.last_poll_epoch_ms = Some(now_epoch_ms);
        Some(usage)
    }

    /// [`Self::poll_workload`] using the injected clock's current time.
    pub fn poll_workload_now(
        &mut self,
        workload_id: &WorkloadId,
        coverage_hint: UsageCoverage,
    ) -> Option<WorkloadUsage> {
        let now = self.clock.now_ms();
        self.poll_workload(workload_id, now, coverage_hint)
    }

    /// Age of the workload's last sample in ms. `None` means the workload
    /// was never polled (or is not tracked) — callers treat both `None` and
    /// `Some(age > TELEMETRY_STALE_MS)` as `M_i = 0` (spec §3).
    pub fn workload_sample_age_ms(&self, workload_id: &WorkloadId, now_ms: u64) -> Option<u64> {
        self.workloads
            .get(workload_id)
            .and_then(|state| state.last_poll_ms.map(|last| now_ms.saturating_sub(last)))
    }
}

/// Differentials the visible readings against the previous poll's
/// baselines.
///
/// * 기존 pid: 자기 CPU 증가분 + 회수된 자식 CPU 증가분(폴 사이에 태어나
///   죽은 짧은 자식의 몫).
/// * 기준선이 없는 pid(새로 보였거나 pid 재사용): 직전 폴 뒤에 시작했으면
///   ([`NEW_PROCESS_SLACK_MS`] 여유) CPU·IO를 통째로 센다. 그보다 오래된
///   프로세스가 늦게 멤버가 된 것이면 다음 폴부터 차분한다.
/// * 사라진 pid의 부모가 아직 보이면 그 부모가 회수했다 — 부모의 자식 CPU에
///   이미 센 몫이 다시 들어오므로 빼서 두 번 세지 않는다.
#[allow(clippy::too_many_arguments)]
fn build_workload_usage(
    workload_id: &WorkloadId,
    visible: &[(u32, ProcessReading)],
    missing: usize,
    dt: Option<u64>,
    baselines: &HashMap<u32, PidBaseline>,
    prev_poll_epoch_ms: Option<u64>,
    coverage_hint: UsageCoverage,
) -> WorkloadUsage {
    let mut cpu_delta_ms: u128 = 0;
    let mut cpu_pairs = 0usize;
    let mut cpu_reset = false;
    let mut read_delta: u128 = 0;
    let mut read_pairs = 0usize;
    let mut read_reset = false;
    let mut write_delta: u128 = 0;
    let mut write_pairs = 0usize;
    let mut write_reset = false;
    for (pid, reading) in visible {
        let baseline = baselines
            .get(pid)
            .filter(|baseline| baseline.start_token == reading.start_token);
        let Some(baseline) = baseline else {
            // 새로 보인 프로세스(또는 재사용된 pid의 새 주인): 직전 폴 뒤에
            // 태어났으면 지금까지의 전부가 이번 구간의 몫이다.
            let born_since_last_poll = matches!(
                (prev_poll_epoch_ms, reading.start_epoch_ms),
                (Some(prev), Some(start)) if start.saturating_add(NEW_PROCESS_SLACK_MS) >= prev
            );
            if dt.is_some() && born_since_last_poll {
                cpu_delta_ms += u128::from(reading.cpu_ms) + u128::from(reading.child_cpu_ms);
                cpu_pairs += 1;
                read_delta += u128::from(reading.read_total_bytes);
                read_pairs += 1;
                write_delta += u128::from(reading.write_total_bytes);
                write_pairs += 1;
            }
            continue;
        };
        match (
            reading.cpu_ms.checked_sub(baseline.cpu_ms),
            reading.child_cpu_ms.checked_sub(baseline.child_cpu_ms),
        ) {
            (Some(own), Some(children)) => {
                cpu_delta_ms += u128::from(own) + u128::from(children);
                cpu_pairs += 1;
            }
            _ => cpu_reset = true,
        }
        match reading
            .read_total_bytes
            .checked_sub(baseline.read_total_bytes)
        {
            Some(delta) => {
                read_delta += u128::from(delta);
                read_pairs += 1;
            }
            None => read_reset = true,
        }
        match reading
            .write_total_bytes
            .checked_sub(baseline.write_total_bytes)
        {
            Some(delta) => {
                write_delta += u128::from(delta);
                write_pairs += 1;
            }
            None => write_reset = true,
        }
    }

    // 추적 중인 부모가 회수한 자식: 부모의 자식 CPU 증가분에 그 자식의 전부가
    // 들어왔다 — 이미 센 몫(직전 폴까지)을 뺀다.
    let visible_pids: HashSet<u32> = visible.iter().map(|(pid, _)| *pid).collect();
    let already_counted: u128 = baselines
        .iter()
        .filter(|(pid, baseline)| {
            !visible_pids.contains(pid)
                && baseline.ppid != 0
                && visible_pids.contains(&baseline.ppid)
        })
        .map(|(_, baseline)| u128::from(baseline.cpu_ms) + u128::from(baseline.child_cpu_ms))
        .sum();
    cpu_delta_ms = cpu_delta_ms.saturating_sub(already_counted);

    // Shared null-semantics for per-pid differentials; `scale` converts the
    // summed delta into the metric's unit (bytes/s multiply by 1000/dt;
    // cpu-ms divide by dt to yield cores).
    fn differential(
        sum: u128,
        pairs: usize,
        reset: bool,
        dt: Option<u64>,
        visible_is_empty: bool,
        scale: &dyn Fn(f64, u64) -> f64,
        reason: Option<&str>,
    ) -> Metric<f64> {
        match dt {
            None => Metric::unavailable(SRC, "first sample"),
            Some(0) => Metric::unavailable(SRC, "zero elapsed time between polls"),
            Some(_) if visible_is_empty => Metric::unavailable(SRC, "no visible processes"),
            Some(_) if reset => Metric::unavailable(SRC, "counter reset"),
            Some(_) if pairs == 0 => Metric::unavailable(SRC, "no overlapping process baseline"),
            Some(dt) => {
                let value = scale(sum as f64, dt);
                match reason {
                    // e.g. sysinfo on Windows counts all process I/O, not
                    // only disk I/O — name that honestly.
                    Some(reason) => Metric {
                        value: Some(value),
                        source: SRC.to_string(),
                        quality: MetricQuality::Measured,
                        reason: Some(reason.to_string()),
                    },
                    None => Metric::measured(SRC, value),
                }
            }
        }
    }

    // CPU-milliseconds per wall millisecond is dimensionless: 1.0 = one core.
    let cores_scale = |sum: f64, dt: u64| sum / dt as f64;
    let io_scale = |sum: f64, dt: u64| sum * 1_000.0 / dt as f64;
    let windows_io_reason =
        cfg!(windows).then_some("all process I/O; disk-only counters unavailable on Windows");
    let visible_is_empty = visible.is_empty();
    let cpu_cores = differential(
        cpu_delta_ms,
        cpu_pairs,
        cpu_reset,
        dt,
        visible_is_empty,
        &cores_scale,
        None,
    );
    let read_bytes_per_sec = differential(
        read_delta,
        read_pairs,
        read_reset,
        dt,
        visible_is_empty,
        &io_scale,
        windows_io_reason,
    );
    let write_bytes_per_sec = differential(
        write_delta,
        write_pairs,
        write_reset,
        dt,
        visible_is_empty,
        &io_scale,
        windows_io_reason,
    );

    let footprints: Option<u128> = visible
        .iter()
        .map(|(_, reading)| reading.footprint_bytes.map(u128::from))
        .sum();
    let resident_bytes = match (visible.is_empty(), footprints) {
        (true, _) => Metric::unavailable(SRC, "no visible processes"),
        // footprint: 공유 페이지를 이중으로 세지 않고 압축·스왑된 몫을 포함 —
        // Activity Monitor의 "메모리"와 같은 기준이다.
        (false, Some(sum)) => bytes_metric(sum, MetricQuality::Measured, None),
        (false, None) => {
            let sum: u128 = visible
                .iter()
                .map(|(_, reading)| u128::from(reading.rss_bytes))
                .sum();
            bytes_metric(
                sum,
                MetricQuality::Estimated,
                Some("shared pages double-counted"),
            )
        }
    };

    WorkloadUsage {
        workload_id: workload_id.clone(),
        cpu_cores,
        resident_bytes,
        accounted_bytes: Metric::unavailable(SRC, "cgroup accounting not collected in R1"),
        committed_bytes: Metric::unavailable(SRC, "job commit not collected in R1"),
        read_bytes_per_sec,
        write_bytes_per_sec,
        network_rx_bytes_per_sec: Metric::unavailable(SRC, "not supported in R1"),
        network_tx_bytes_per_sec: Metric::unavailable(SRC, "not supported in R1"),
        process_count: Metric::measured(SRC, visible.len() as u32),
        coverage: if missing == 0 {
            coverage_hint
        } else {
            UsageCoverage::Partial
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use super::*;
    use term_contracts::metrics::PressureLevel;

    // ------------------------------------------------------------------ fakes

    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn new(ms: u64) -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(ms)))
        }
        fn set(&self, ms: u64) {
            self.0.store(ms, Ordering::Relaxed);
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }

    #[derive(Default)]
    struct FakeCore {
        /// `now_epoch_ms`가 돌려줄 벽시계(새 프로세스 판정).
        epoch_ms: u64,
        host_refreshes: u64,
        disk_refreshes: u64,
        network_refreshes: u64,
        process_refresh_calls: Vec<Vec<u32>>,
        mem: MemoryReading,
        cpu: CpuReading,
        disks: Vec<DiskReading>,
        ifaces: Vec<InterfaceReading>,
        procs: HashMap<u32, ProcessReading>,
    }

    struct FakeSource {
        core: Arc<Mutex<FakeCore>>,
    }

    impl FakeSource {
        fn new() -> (Self, Arc<Mutex<FakeCore>>) {
            let core = Arc::new(Mutex::new(FakeCore::default()));
            (Self { core: core.clone() }, core)
        }
    }

    impl PlatformSource for FakeSource {
        fn refresh_host(&mut self) {
            self.core.lock().unwrap().host_refreshes += 1;
        }
        fn refresh_disks(&mut self) {
            self.core.lock().unwrap().disk_refreshes += 1;
        }
        fn refresh_networks(&mut self) {
            self.core.lock().unwrap().network_refreshes += 1;
        }
        fn refresh_processes(&mut self, pids: &[u32]) {
            self.core
                .lock()
                .unwrap()
                .process_refresh_calls
                .push(pids.to_vec());
        }
        fn read_memory(&self) -> MemoryReading {
            self.core.lock().unwrap().mem
        }
        fn read_cpu(&self) -> CpuReading {
            self.core.lock().unwrap().cpu.clone()
        }
        fn read_disks(&self) -> Vec<DiskReading> {
            self.core.lock().unwrap().disks.clone()
        }
        fn read_interfaces(&self) -> Vec<InterfaceReading> {
            self.core.lock().unwrap().ifaces.clone()
        }
        fn read_process(&self, pid: u32) -> Option<ProcessReading> {
            self.core.lock().unwrap().procs.get(&pid).cloned()
        }
        fn now_epoch_ms(&self) -> u64 {
            self.core.lock().unwrap().epoch_ms
        }
    }

    fn host_core() -> FakeCore {
        FakeCore {
            mem: MemoryReading {
                total_bytes: 16 << 30,
                available_bytes: 8 << 30,
                used_bytes: 7 << 30,
                swap_used_bytes: 1 << 30,
            },
            cpu: CpuReading {
                logical_count: 8,
                cores_used: Some(1.25),
            },
            disks: vec![DiskReading {
                mount: "C:\\".into(),
                capacity_bytes: 500 << 30,
                free_bytes: 100 << 30,
            }],
            ifaces: vec![
                InterfaceReading {
                    name: "Ethernet".into(),
                    rx_total_bytes: 1_000,
                    tx_total_bytes: 500,
                },
                InterfaceReading {
                    name: "Loopback Pseudo-Interface 1".into(),
                    rx_total_bytes: 9_000_000,
                    tx_total_bytes: 9_000_000,
                },
            ],
            ..FakeCore::default()
        }
    }

    fn sampler_with_host_core() -> (TelemetrySampler, Arc<Mutex<FakeCore>>, Arc<FakeClock>) {
        let (source, core) = FakeSource::new();
        *core.lock().unwrap() = host_core();
        let clock = FakeClock::new(0);
        (
            TelemetrySampler::with_source(clock.clone(), Box::new(source)),
            core,
            clock,
        )
    }

    fn reason_of<T>(metric: &Metric<T>) -> Option<&str> {
        metric.reason.as_deref()
    }

    // ------------------------------------------------------------- host tests

    #[test]
    fn first_host_sample_nulls_differentials_but_keeps_gauges() {
        let (mut sampler, _core, _clock) = sampler_with_host_core();
        let sample = sampler.poll_host(0);

        // Differentials: unavailable "first sample", never 0.
        assert_eq!(sample.cpu_cores_used.quality, MetricQuality::Unavailable);
        assert!(sample.cpu_cores_used.value.is_none());
        assert_eq!(reason_of(&sample.cpu_cores_used), Some("first sample"));
        for iface in &sample.interfaces {
            assert!(iface.rx_bytes_per_sec.value.is_none());
            assert_eq!(reason_of(&iface.rx_bytes_per_sec), Some("first sample"));
            assert!(iface.tx_bytes_per_sec.value.is_none());
        }
        // Gauges are measured immediately.
        assert_eq!(sample.physical_total_bytes.quality, MetricQuality::Measured);
        assert_eq!(
            sample.physical_available_bytes.value.unwrap().get(),
            8 << 30
        );
        assert_eq!(sample.swap_used_bytes.value.unwrap().get(), 1 << 30);
        assert_eq!(sample.pressure, PressureLevel::Normal);
        assert_eq!(sample.logical_cpu_count, 8);
        assert_eq!(sample.monotonic_ms, 0);
        assert_eq!(sample.disks.len(), 1);
        assert_eq!(
            sample.disks[0].free_bytes.value.as_ref().unwrap().get(),
            100 << 30
        );
    }

    #[test]
    fn second_sample_reports_host_cpu_cores() {
        let (mut sampler, core, _clock) = sampler_with_host_core();
        sampler.poll_host(0);
        core.lock().unwrap().cpu.cores_used = Some(2.5);
        let sample = sampler.poll_host(1_000);
        assert_eq!(sample.cpu_cores_used.quality, MetricQuality::Measured);
        assert!((sample.cpu_cores_used.value.unwrap() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn interface_rates_compute_from_counter_deltas() {
        let (mut sampler, core, _clock) = sampler_with_host_core();
        sampler.poll_host(0);
        {
            let mut core = core.lock().unwrap();
            core.ifaces[0].rx_total_bytes += 1_500; // 1500 B/s over 1 s
            core.ifaces[0].tx_total_bytes += 750; // 750 B/s
        }
        let sample = sampler.poll_host(1_000);
        let ethernet = sample
            .interfaces
            .iter()
            .find(|iface| iface.name == "Ethernet")
            .expect("ethernet present");
        assert_eq!(ethernet.rx_bytes_per_sec.quality, MetricQuality::Measured);
        assert!((ethernet.rx_bytes_per_sec.value.unwrap() - 1_500.0).abs() < 1e-9);
        assert!((ethernet.tx_bytes_per_sec.value.unwrap() - 750.0).abs() < 1e-9);
        // Loopback rates are still reported per interface…
        let loopback = sample
            .interfaces
            .iter()
            .find(|iface| iface.is_loopback)
            .expect("loopback present");
        assert!(loopback.rx_bytes_per_sec.value.is_some());
    }

    #[test]
    fn interface_counter_reset_yields_null_not_negative() {
        let (mut sampler, core, _clock) = sampler_with_host_core();
        sampler.poll_host(0);
        core.lock().unwrap().ifaces[0].rx_total_bytes = 400; // went backwards
        let sample = sampler.poll_host(1_000);
        let ethernet = &sample.interfaces[0];
        assert!(ethernet.rx_bytes_per_sec.value.is_none());
        assert_eq!(reason_of(&ethernet.rx_bytes_per_sec), Some("counter reset"));
        // The unaffected direction still reports a rate.
        assert!(ethernet.tx_bytes_per_sec.value.is_some());
    }

    #[test]
    fn new_interface_without_baseline_is_first_sample() {
        let (mut sampler, core, _clock) = sampler_with_host_core();
        sampler.poll_host(0);
        core.lock().unwrap().ifaces.push(InterfaceReading {
            name: "Wi-Fi".into(),
            rx_total_bytes: 10,
            tx_total_bytes: 10,
        });
        let sample = sampler.poll_host(1_000);
        let wifi = sample
            .interfaces
            .iter()
            .find(|iface| iface.name == "Wi-Fi")
            .expect("wifi present");
        assert_eq!(reason_of(&wifi.rx_bytes_per_sec), Some("first sample"));
    }

    #[test]
    fn cadence_gating_caches_between_polls() {
        let (mut sampler, core, _clock) = sampler_with_host_core();
        let first = sampler.poll_host(0);
        assert_eq!(core.lock().unwrap().host_refreshes, 1);
        assert_eq!(core.lock().unwrap().network_refreshes, 1);

        // 500 ms later: cached sample, no refresh, identical data (with the
        // original sample time, so age stays visible).
        let cached = sampler.poll_host(500);
        assert_eq!(core.lock().unwrap().host_refreshes, 1);
        assert_eq!(core.lock().unwrap().network_refreshes, 1);
        assert_eq!(cached, first);
        assert_eq!(cached.monotonic_ms, 0);

        // 1000 ms after the last firing: refresh happens.
        sampler.poll_host(1_000);
        assert_eq!(core.lock().unwrap().host_refreshes, 2);
        assert_eq!(core.lock().unwrap().network_refreshes, 2);
    }

    #[test]
    fn disks_refresh_on_ten_second_cadence_only() {
        let (mut sampler, core, _clock) = sampler_with_host_core();
        sampler.poll_host(0);
        assert_eq!(core.lock().unwrap().disk_refreshes, 1);
        for t in (1_000..9_000).step_by(1_000) {
            sampler.poll_host(t);
        }
        assert_eq!(core.lock().unwrap().disk_refreshes, 1, "no disk churn <10s");
        sampler.poll_host(10_000);
        assert_eq!(core.lock().unwrap().disk_refreshes, 2);
        // Disk data still surfaces on every host sample.
        assert_eq!(sampler.poll_host(11_000).disks.len(), 1);
    }

    #[test]
    fn clock_injection_drives_poll_host_now() {
        let (mut sampler, _core, clock) = sampler_with_host_core();
        clock.set(5_000);
        assert_eq!(sampler.poll_host_now().monotonic_ms, 5_000);
        clock.set(5_500);
        assert_eq!(sampler.poll_host_now().monotonic_ms, 5_000, "cached");
        clock.set(6_000);
        assert_eq!(sampler.poll_host_now().monotonic_ms, 6_000, "refreshed");
    }

    #[test]
    fn host_history_ring_keeps_committed_samples_only() {
        let (mut sampler, core, _clock) = sampler_with_host_core();
        sampler.poll_host(0);
        sampler.poll_host(500); // cached: not pushed again

        // 바이트 floor 한 칸 아래(1 GiB - 1) → critical.
        core.lock().unwrap().mem.available_bytes = (1 << 30) - 1;
        sampler.poll_host(1_000);
        let history = sampler.host_history();
        assert_eq!(history.len(), 2);
        assert_eq!(history.iter().next().unwrap().monotonic_ms, 0);
        assert_eq!(history.newest().unwrap().monotonic_ms, 1_000);
        assert_eq!(history.newest().unwrap().pressure, PressureLevel::Critical);
    }

    // ------------------------------------------------------- aggregate tests

    fn measured_rate(value: f64) -> Metric<f64> {
        Metric::measured(SRC, value)
    }

    fn iface(name: &str, rx: Metric<f64>, tx: Metric<f64>, is_loopback: bool) -> InterfaceSample {
        InterfaceSample {
            name: name.to_string(),
            rx_bytes_per_sec: rx,
            tx_bytes_per_sec: tx,
            is_loopback,
        }
    }

    #[test]
    fn default_aggregate_excludes_loopback() {
        let interfaces = vec![
            iface(
                "Ethernet",
                measured_rate(1_000.0),
                measured_rate(2_000.0),
                false,
            ),
            iface(
                "lo",
                measured_rate(1_000_000.0),
                measured_rate(1_000_000.0),
                true,
            ),
            iface("Wi-Fi", measured_rate(500.0), measured_rate(250.0), false),
        ];
        let aggregate = default_aggregate(&interfaces);
        assert_eq!(aggregate.rx_bytes_per_sec.quality, MetricQuality::Measured);
        assert!((aggregate.rx_bytes_per_sec.value.unwrap() - 1_500.0).abs() < 1e-9);
        assert!((aggregate.tx_bytes_per_sec.value.unwrap() - 2_250.0).abs() < 1e-9);
    }

    #[test]
    fn default_aggregate_degrades_gracefully() {
        // Only loopback present: unavailable, not loopback-summed.
        let only_loopback = vec![iface(
            "lo0",
            measured_rate(123.0),
            measured_rate(123.0),
            true,
        )];
        let aggregate = default_aggregate(&only_loopback);
        assert!(aggregate.rx_bytes_per_sec.value.is_none());
        assert_eq!(
            reason_of(&aggregate.rx_bytes_per_sec),
            Some("no non-loopback interfaces")
        );

        // One physical interface missing its rate: sum is estimated with a
        // reason, never presented as fully measured.
        let mixed = vec![
            iface(
                "Ethernet",
                measured_rate(100.0),
                measured_rate(100.0),
                false,
            ),
            iface(
                "Wi-Fi",
                Metric::unavailable(SRC, "first sample"),
                Metric::unavailable(SRC, "first sample"),
                false,
            ),
        ];
        let aggregate = default_aggregate(&mixed);
        assert_eq!(aggregate.rx_bytes_per_sec.quality, MetricQuality::Estimated);
        assert!((aggregate.rx_bytes_per_sec.value.unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(
            reason_of(&aggregate.rx_bytes_per_sec),
            Some("1 of 2 interfaces unavailable or estimated")
        );

        // All physical rates missing: unavailable.
        let dead = vec![iface(
            "Ethernet",
            Metric::unavailable(SRC, "counter reset"),
            Metric::unavailable(SRC, "counter reset"),
            false,
        )];
        let aggregate = default_aggregate(&dead);
        assert!(aggregate.rx_bytes_per_sec.value.is_none());
        assert_eq!(
            reason_of(&aggregate.rx_bytes_per_sec),
            Some("all non-loopback interfaces unavailable")
        );
    }

    #[test]
    fn loopback_detection_by_name() {
        assert!(is_loopback_interface("lo"));
        assert!(is_loopback_interface("lo0"));
        assert!(is_loopback_interface("lo9"));
        assert!(is_loopback_interface("Loopback Pseudo-Interface 1"));
        assert!(is_loopback_interface("  LO  "));
        assert!(!is_loopback_interface("eth0"));
        assert!(!is_loopback_interface("wlan0"));
        assert!(!is_loopback_interface("Ethernet"));
        assert!(!is_loopback_interface("en0"));
        assert!(!is_loopback_interface("localtelemetry"));
    }

    // ------------------------------------------------------ workload fakes

    fn identities(pids: &[u32]) -> Arc<dyn Fn() -> Vec<ProcessIdentity> + Send + Sync> {
        let list: Vec<ProcessIdentity> = pids
            .iter()
            .map(|&pid| ProcessIdentity {
                pid,
                start_token: format!("token-{pid}"),
                boot_id: "boot".into(),
            })
            .collect();
        Arc::new(move || list.clone())
    }

    fn proc_reading(pid: u32, cpu_ms: u64, rss: u64) -> ProcessReading {
        ProcessReading {
            start_token: format!("start-{pid}"),
            cpu_ms,
            rss_bytes: rss,
            read_total_bytes: 10_000 + u64::from(pid),
            write_total_bytes: 20_000 + u64::from(pid),
            ..ProcessReading::default()
        }
    }

    fn workload_sampler() -> (TelemetrySampler, Arc<Mutex<FakeCore>>) {
        let (source, core) = FakeSource::new();
        (
            TelemetrySampler::with_source(FakeClock::new(0), Box::new(source)),
            core,
        )
    }

    // ------------------------------------------------------ workload tests

    /// 부모(셸)의 회수된 자식 CPU: 폴 사이에 태어나 죽은 컴파일러의 몫이
    /// 부모의 자식 누계로만 보여도 워크로드 CPU에 들어간다.
    #[test]
    fn reaped_child_cpu_counts_toward_the_workload() {
        let (mut sampler, core) = workload_sampler();
        let shell = |child_ms: u64| ProcessReading {
            ppid: 1,
            child_cpu_ms: child_ms,
            ..proc_reading(100, 1_000, 1 << 20)
        };
        core.lock().unwrap().procs = HashMap::from([(100, shell(0))]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100]));
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);
        // 1초 사이에 자식이 1.5 코어·초를 쓰고 회수됐다.
        core.lock().unwrap().procs = HashMap::from([(100, shell(1_500))]);
        let usage = sampler
            .poll_workload(&id, 1_000, UsageCoverage::ObservedTree)
            .unwrap();
        assert!(
            (usage.cpu_cores.value.unwrap() - 1.5).abs() < 1e-9,
            "{usage:?}"
        );
    }

    /// 직전 폴 뒤에 태어난 프로세스는 첫 관측에서 통째로 세고, 늦게 멤버가 된
    /// 오래된 프로세스는 다음 폴부터 차분한다(평생 누적을 한 틱에 쏟지 않음).
    #[test]
    fn new_processes_count_in_full_but_old_late_joiners_do_not() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().epoch_ms = 10_000_000;
        core.lock().unwrap().procs = HashMap::from([(100, proc_reading(100, 1_000, 0))]);
        let id = WorkloadId::generate();
        let members = Arc::new(Mutex::new(vec![100u32]));
        let provider_members = Arc::clone(&members);
        sampler.track_workload(
            id.clone(),
            Arc::new(move || {
                provider_members
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|&pid| ProcessIdentity {
                        pid,
                        start_token: format!("token-{pid}"),
                        boot_id: "boot".into(),
                    })
                    .collect()
            }),
        );
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);
        {
            let mut core = core.lock().unwrap();
            core.epoch_ms = 10_001_000;
            // 101: 직전 폴(10_000_000) 뒤에 태어나 벌써 800 ms를 썼다.
            core.procs.insert(
                101,
                ProcessReading {
                    start_epoch_ms: Some(10_000_400),
                    ..proc_reading(101, 800, 0)
                },
            );
            // 102: 한 시간 전에 태어난 고아가 이제야 멤버가 됐다(누적 1시간).
            core.procs.insert(
                102,
                ProcessReading {
                    start_epoch_ms: Some(10_000_000 - 3_600_000),
                    ..proc_reading(102, 3_600_000, 0)
                },
            );
        }
        members.lock().unwrap().extend([101, 102]);
        let usage = sampler
            .poll_workload(&id, 1_000, UsageCoverage::ObservedTree)
            .unwrap();
        // 100은 그대로(0), 101은 통째로(0.8), 102는 세지 않는다.
        assert!(
            (usage.cpu_cores.value.unwrap() - 0.8).abs() < 1e-9,
            "{usage:?}"
        );
    }

    /// 추적 중인 부모가 추적 중이던 자식을 회수하면 그 자식의 몫이 부모의
    /// 자식 누계로 다시 들어온다 — 이미 센 몫은 빼서 두 번 세지 않는다.
    #[test]
    fn a_child_reaped_by_a_tracked_parent_is_not_counted_twice() {
        let (mut sampler, core) = workload_sampler();
        let parent = |child_ms: u64| ProcessReading {
            child_cpu_ms: child_ms,
            ..proc_reading(100, 1_000, 0)
        };
        let child = |cpu_ms: u64| ProcessReading {
            ppid: 100,
            ..proc_reading(101, cpu_ms, 0)
        };
        core.lock().unwrap().procs = HashMap::from([(100, parent(0)), (101, child(2_000))]);
        let id = WorkloadId::generate();
        let members = Arc::new(Mutex::new(vec![100u32, 101]));
        let provider_members = Arc::clone(&members);
        sampler.track_workload(
            id.clone(),
            Arc::new(move || {
                provider_members
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|&pid| ProcessIdentity {
                        pid,
                        start_token: format!("token-{pid}"),
                        boot_id: "boot".into(),
                    })
                    .collect()
            }),
        );
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);
        // 자식이 마지막 1초에 500 ms를 더 쓰고(총 2_500) 끝나 부모가 회수했다.
        core.lock().unwrap().procs = HashMap::from([(100, parent(2_500))]);
        members.lock().unwrap().retain(|pid| *pid == 100);
        let usage = sampler
            .poll_workload(&id, 1_000, UsageCoverage::ObservedTree)
            .unwrap();
        // 이번 구간의 몫은 자식이 마지막에 쓴 0.5 코어·초뿐이다.
        assert!(
            (usage.cpu_cores.value.unwrap() - 0.5).abs() < 1e-9,
            "{usage:?}"
        );
    }

    /// footprint가 있으면 RSS 합 대신 쓰고 측정값으로 보고한다.
    #[test]
    fn footprint_replaces_the_rss_sum_when_available() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().procs = HashMap::from([
            (
                100,
                ProcessReading {
                    footprint_bytes: Some(300 << 20),
                    ..proc_reading(100, 0, 646 << 20)
                },
            ),
            (
                101,
                ProcessReading {
                    footprint_bytes: Some(305 << 20),
                    ..proc_reading(101, 0, 235 << 20)
                },
            ),
        ]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100, 101]));
        let usage = sampler
            .poll_workload(&id, 0, UsageCoverage::ObservedTree)
            .unwrap();
        assert_eq!(usage.resident_bytes.quality, MetricQuality::Measured);
        assert_eq!(reason_of(&usage.resident_bytes), None);
        assert_eq!(usage.resident_bytes.value.unwrap().get(), 605 << 20);
    }

    #[test]
    fn workload_first_sample_is_null_differential() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().procs = HashMap::from([
            (100, proc_reading(100, 5_000, 1 << 20)),
            (101, proc_reading(101, 5_000, 2 << 20)),
        ]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100, 101]));
        let usage = sampler
            .poll_workload(&id, 0, UsageCoverage::ObservedTree)
            .expect("tracked");

        assert_eq!(usage.workload_id, id);
        assert_eq!(reason_of(&usage.cpu_cores), Some("first sample"));
        assert_eq!(reason_of(&usage.read_bytes_per_sec), Some("first sample"));
        assert_eq!(reason_of(&usage.write_bytes_per_sec), Some("first sample"));
        // Gauges work immediately.
        assert_eq!(usage.process_count.quality, MetricQuality::Measured);
        assert_eq!(usage.process_count.value, Some(2));
        assert_eq!(usage.resident_bytes.quality, MetricQuality::Estimated);
        assert_eq!(
            reason_of(&usage.resident_bytes),
            Some("shared pages double-counted")
        );
        assert_eq!(usage.resident_bytes.value.unwrap().get(), 3 << 20);
        // R1-unavailable fields carry reasons, not zeros.
        assert_eq!(
            reason_of(&usage.network_rx_bytes_per_sec),
            Some("not supported in R1")
        );
        assert_eq!(
            reason_of(&usage.accounted_bytes),
            Some("cgroup accounting not collected in R1")
        );
        assert_eq!(
            reason_of(&usage.committed_bytes),
            Some("job commit not collected in R1")
        );
        assert_eq!(usage.coverage, UsageCoverage::ObservedTree);
        // Only the tracked pids were refreshed — exactly that list.
        assert_eq!(
            core.lock().unwrap().process_refresh_calls,
            vec![vec![100, 101]]
        );
    }

    #[test]
    fn workload_cpu_and_io_sum_pid_deltas() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().procs = HashMap::from([
            (100, proc_reading(100, 5_000, 0)),
            (101, proc_reading(101, 5_000, 0)),
        ]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100, 101]));
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);

        {
            let mut core = core.lock().unwrap();
            let p100 = core.procs.get_mut(&100).unwrap();
            p100.cpu_ms += 750;
            p100.read_total_bytes += 2_000;
            let p101 = core.procs.get_mut(&101).unwrap();
            p101.cpu_ms += 250;
            p101.write_total_bytes += 4_000;
        }
        let usage = sampler
            .poll_workload(&id, 1_000, UsageCoverage::ObservedTree)
            .expect("tracked");
        // 1000 CPU-ms across pids over 1000 ms → 1.0 core.
        assert_eq!(usage.cpu_cores.quality, MetricQuality::Measured);
        assert!((usage.cpu_cores.value.unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(usage.read_bytes_per_sec.quality, MetricQuality::Measured);
        assert!((usage.read_bytes_per_sec.value.unwrap() - 2_000.0).abs() < 1e-9);
        assert!((usage.write_bytes_per_sec.value.unwrap() - 4_000.0).abs() < 1e-9);
    }

    #[test]
    fn workload_visibility_loss_is_partial_and_not_zero_filled() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().procs = HashMap::from([
            (100, proc_reading(100, 5_000, 1 << 20)),
            (101, proc_reading(101, 5_000, 4 << 20)),
        ]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100, 101]));
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);

        // pid 101 disappears; pid 100 keeps burning 500 CPU-ms/s.
        {
            let mut core = core.lock().unwrap();
            core.procs.remove(&101);
            core.procs.get_mut(&100).unwrap().cpu_ms += 500;
        }
        let usage = sampler
            .poll_workload(&id, 1_000, UsageCoverage::ObservedTree)
            .expect("tracked");

        assert_eq!(usage.coverage, UsageCoverage::Partial);
        // process_count counts visible pids only — measured, not zero-filled.
        assert_eq!(usage.process_count.value, Some(1));
        // Resident covers the visible subset (4 MiB gone, 1 MiB remains).
        assert_eq!(usage.resident_bytes.value.unwrap().get(), 1 << 20);
        // CPU comes from the surviving differential pair, not zero.
        assert_eq!(usage.cpu_cores.quality, MetricQuality::Measured);
        assert!((usage.cpu_cores.value.unwrap() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn workload_fully_lost_reports_unavailable_not_zero() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().procs = HashMap::from([(100, proc_reading(100, 5_000, 1 << 20))]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100]));
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);

        core.lock().unwrap().procs.clear();
        let usage = sampler
            .poll_workload(&id, 1_000, UsageCoverage::ObservedTree)
            .expect("tracked");
        assert_eq!(usage.coverage, UsageCoverage::Partial);
        assert_eq!(usage.process_count.value, Some(0)); // a real count of zero
        assert_eq!(reason_of(&usage.cpu_cores), Some("no visible processes"));
        assert_eq!(
            reason_of(&usage.resident_bytes),
            Some("no visible processes")
        );
        assert_eq!(
            reason_of(&usage.read_bytes_per_sec),
            Some("no visible processes")
        );
    }

    #[test]
    fn workload_counter_reset_nulls_the_rate() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().procs = HashMap::from([(100, proc_reading(100, 9_000, 0))]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100]));
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);

        // cpu_ms goes backwards (process restarted under the same pid+token).
        core.lock().unwrap().procs.get_mut(&100).unwrap().cpu_ms = 1_000;
        let usage = sampler
            .poll_workload(&id, 1_000, UsageCoverage::ObservedTree)
            .expect("tracked");
        assert_eq!(reason_of(&usage.cpu_cores), Some("counter reset"));
        assert!(usage.cpu_cores.value.is_none());
    }

    #[test]
    fn workload_pid_reuse_starts_a_fresh_baseline() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().procs = HashMap::from([(100, proc_reading(100, 5_000, 0))]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100]));
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);

        // Same pid, different start token: the old baseline is void.
        let mut reused = proc_reading(100, 50_000, 0);
        reused.start_token = "start-after-reuse".into();
        core.lock().unwrap().procs.insert(100, reused);
        let usage = sampler
            .poll_workload(&id, 1_000, UsageCoverage::ObservedTree)
            .expect("tracked");
        assert_eq!(
            reason_of(&usage.cpu_cores),
            Some("no overlapping process baseline")
        );
        assert!(usage.cpu_cores.value.is_none());
        // The next poll differences against the fresh baseline.
        core.lock().unwrap().procs.get_mut(&100).unwrap().cpu_ms += 1_000;
        let usage = sampler
            .poll_workload(&id, 2_000, UsageCoverage::ObservedTree)
            .expect("tracked");
        assert!((usage.cpu_cores.value.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn stale_workload_baselines_are_dropped_after_three_seconds() {
        let (mut sampler, core) = workload_sampler();
        core.lock().unwrap().procs = HashMap::from([(100, proc_reading(100, 5_000, 0))]);
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100]));
        sampler.poll_workload(&id, 0, UsageCoverage::ObservedTree);
        assert_eq!(sampler.workload_sample_age_ms(&id, 1_000), Some(1_000));

        // >3 s gap: baselines dropped → the next sample is "first sample"
        // again instead of differencing across a stale window.
        core.lock().unwrap().procs.get_mut(&100).unwrap().cpu_ms += 4_000;
        let usage = sampler
            .poll_workload(&id, 4_000, UsageCoverage::ObservedTree)
            .expect("tracked");
        assert_eq!(reason_of(&usage.cpu_cores), Some("first sample"));
        assert_eq!(sampler.workload_sample_age_ms(&id, 4_500), Some(500));

        // At exactly 3 s the baseline survives (stale means strictly older);
        // 3000 CPU-ms over 3000 ms → 1.0 core.
        sampler.poll_workload(&id, 7_000, UsageCoverage::ObservedTree);
        core.lock().unwrap().procs.get_mut(&100).unwrap().cpu_ms += 3_000;
        let usage = sampler
            .poll_workload(&id, 10_000, UsageCoverage::ObservedTree)
            .expect("tracked");
        assert!((usage.cpu_cores.value.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn untracked_workload_polls_return_none_and_age_is_none() {
        let (mut sampler, _core) = workload_sampler();
        let id = WorkloadId::generate();
        assert!(sampler
            .poll_workload(&id, 0, UsageCoverage::ObservedTree)
            .is_none());
        assert_eq!(sampler.workload_sample_age_ms(&id, 0), None);

        // Empty pid set: a real process_count of 0, coverage hint honored.
        sampler.track_workload(id.clone(), identities(&[]));
        let usage = sampler
            .poll_workload(&id, 0, UsageCoverage::Group)
            .expect("tracked");
        assert_eq!(usage.process_count.value, Some(0));
        assert_eq!(usage.coverage, UsageCoverage::Group);

        assert!(sampler.untrack_workload(&id));
        assert!(!sampler.untrack_workload(&id));
        assert!(sampler
            .poll_workload(&id, 1_000, UsageCoverage::Group)
            .is_none());
        assert_eq!(sampler.workload_sample_age_ms(&id, 1_000), None);
    }

    #[test]
    fn workload_poll_now_uses_injected_clock() {
        let (source, core) = FakeSource::new();
        core.lock().unwrap().procs = HashMap::from([(100, proc_reading(100, 0, 0))]);
        let clock = FakeClock::new(42);
        let mut sampler = TelemetrySampler::with_source(clock, Box::new(source));
        let id = WorkloadId::generate();
        sampler.track_workload(id.clone(), identities(&[100]));
        let usage = sampler
            .poll_workload_now(&id, UsageCoverage::ObservedTree)
            .expect("tracked");
        assert_eq!(reason_of(&usage.cpu_cores), Some("first sample"));
        assert_eq!(sampler.workload_sample_age_ms(&id, 142), Some(100));
    }

    // ---------------------------------------------------------- byte bounds

    #[test]
    fn byte_values_respect_u64string_boundaries() {
        let max = U64String::MAX;
        let ok = bytes_metric(u128::from(max), MetricQuality::Measured, None);
        assert_eq!(ok.quality, MetricQuality::Measured);
        assert_eq!(ok.value.unwrap().get(), max);
        let over = bytes_metric(u128::from(max) + 1, MetricQuality::Measured, None);
        assert_eq!(over.quality, MetricQuality::Unavailable);
        assert!(over.value.is_none());
        assert_eq!(
            over.reason.as_deref(),
            Some("value exceeds SQLite integer bound")
        );
        // Zero is a valid measurement.
        let zero = bytes_metric(0, MetricQuality::Measured, None);
        assert_eq!(zero.value.unwrap().get(), 0);
    }

    // ---------------------------------------------------------- no secrets

    #[test]
    fn workload_refresh_kind_never_requests_argv_env_or_metadata() {
        let kind = workload_refresh_kind();
        assert!(kind.cpu());
        assert!(kind.memory());
        assert!(kind.disk_usage());
        assert!(!kind.tasks());
        assert_eq!(kind.cmd(), sysinfo::UpdateKind::Never);
        assert_eq!(kind.environ(), sysinfo::UpdateKind::Never);
        assert_eq!(kind.exe(), sysinfo::UpdateKind::Never);
        assert_eq!(kind.cwd(), sysinfo::UpdateKind::Never);
        assert_eq!(kind.root(), sysinfo::UpdateKind::Never);
        assert_eq!(kind.user(), sysinfo::UpdateKind::Never);
    }
}
