//! Ticket I06 — `ResourcePlatform` trait + OS group backends
//! (spec `03-resources.md` §4–§7).
//!
//! Real OS calls run on platform worker threads owned by the caller; this
//! trait is blocking and must never be called on the actor/async executor.
//!
//! Backends (all fully implemented for their target OS, selected per build):
//! * [`windows_job`] — Job Object per workload (§6)
//! * [`linux_cgroup`] — cgroup v2 delegated subtree (§5)
//! * [`macos_tree`] — observed process tree (§7)
//! * [`mock`] — in-memory scripted backend for tests (B10 patterns)
//!
//! [`select_backend`] picks the backend for the compiling OS; other OSes get
//! an explicit unsupported backend instead of a silent stub.

pub mod mock;
pub mod scheduling;

#[cfg(target_os = "linux")]
pub mod linux_cgroup;
#[cfg(target_os = "linux")]
mod linux_recovery;
#[cfg(target_os = "macos")]
pub mod macos_guardian;
#[cfg(target_os = "macos")]
pub mod macos_tree;
#[cfg(target_os = "windows")]
pub mod windows_job;

#[cfg(target_os = "linux")]
pub use linux_cgroup::LinuxCgroupPlatform;
#[cfg(target_os = "macos")]
pub use macos_tree::MacosTreePlatform;
#[cfg(target_os = "windows")]
pub use windows_job::WindowsJobPlatform;

use std::io;

use term_contracts::ids::ProcessIdentity;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use term_contracts::ids::U64String;
use term_contracts::ids::WorkloadId;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use term_contracts::metrics::Metric;
use term_contracts::metrics::WorkloadUsage;
use term_contracts::snapshot::Capabilities;
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
use term_contracts::snapshot::{LimitCapability, LimitSupport};
use term_contracts::workload::{GroupKind, GroupRecoveryIdentity, WorkloadDescriptor};

/// Phase of a group termination sequence (02-runner §7: Unix TERM → 3 s →
/// KILL; Windows R1 is force-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopPhase {
    /// Cooperative signal window (SIGTERM on Unix). Windows R1 has no
    /// universal graceful CLI signal, so [`StopPhase::Grace`] is a documented
    /// no-op there and the caller labels the stop 강제 종료.
    Grace,
    /// Forced kill of all owned members (TerminateJobObject / SIGKILL /
    /// cgroup.kill).
    Force,
}

/// Scheduling policy applied to a workload's verified members
/// (spec `08-pressure-relief.md` §2). This is a *policy*, not a quota: it
/// only bites while the CPU is contended, and it must always be reversible
/// (§0-4) — platforms that cannot restore report `scheduling_yield`
/// unsupported and are never asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulingTier {
    /// The OS default the process was launched with (restore).
    Normal,
    /// Yield to everything else (macOS `PRIO_DARWIN_BG`, Windows
    /// `BELOW_NORMAL_PRIORITY_CLASS`, cgroup `cpu.weight = 10`).
    Background,
}

/// Per-member outcome of one [`ResourcePlatform::set_scheduling`] call.
/// `skipped_reused` counts identities whose pid now belongs to a different
/// process: those are never touched and never reported as an error
/// (01-contracts §1 pid-reuse defence).
///
/// The three counters do not have to add up to the member count: a process
/// that exited between the identity check and the OS call is neither applied
/// nor failed — it is no longer a member.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulingOutcome {
    pub applied: usize,
    pub failed: usize,
    pub skipped_reused: usize,
}

impl SchedulingOutcome {
    /// Some verified member could not be changed — the session state carries
    /// this as `partial` and the caller retries on the next tick (08 §2).
    pub fn is_partial(&self) -> bool {
        self.failed > 0
    }
}

/// Handle to one OS resource group (cgroup path / Job handle / observed tree
/// root). Cloning shares the underlying OS object (Arc semantics); dropping
/// the last clone tears it down — on Windows that kills every remaining
/// member (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`).
#[derive(Debug, Clone)]
pub struct GroupHandle {
    pub workload_id: WorkloadId,
    pub kind: GroupKind,
    /// Human-readable reference (job name / cgroup path / tree root pid).
    pub reference: String,
    pub(crate) inner: GroupInner,
}

/// Backend payload of a [`GroupHandle`]. Each real backend only accepts its
/// own variant; [`GroupInner::Mock`] exists on every OS so tests can script
/// behavior without touching the host.
#[derive(Debug, Clone)]
pub(crate) enum GroupInner {
    #[cfg(target_os = "windows")]
    Win(windows_job::WinGroupInner),
    #[cfg(target_os = "linux")]
    Cgroup(linux_cgroup::LinuxGroupInner),
    /// Linux without a delegated cgroup subtree: observed process tree
    /// (03 §5 — observe/prefer fall back to process-tree estimation).
    #[cfg(target_os = "linux")]
    ObservedTree(linux_cgroup::LinuxTreeInner),
    #[cfg(target_os = "macos")]
    Tree(macos_tree::MacGroupInner),
    #[cfg(target_os = "macos")]
    Guardian(macos_guardian::GuardianInner),
    Mock(mock::MockGroupInner),
}

/// OS resource-group contract (03-resources §4 — normative). Capabilities
/// reflect what the *current process* can actually use (permissions,
/// delegation, API success), never just the OS name.
pub trait ResourcePlatform: Send + Sync {
    /// Current-process reality probe; cheap enough to call per launch
    /// preflight, may cache delegation discovery internally.
    fn capabilities(&self) -> Capabilities;

    /// Create one group for the workload and apply the policy limits that the
    /// policy requests. Every applied limit is verified by read-back where the
    /// OS supports it (§5 step 2 discipline).
    fn create_group(&self, workload: &WorkloadDescriptor) -> io::Result<GroupHandle>;

    /// Persistent pipe executions provide an independent observer on macOS.
    fn needs_observer_guardian(&self) -> bool {
        false
    }

    /// Additional helper-to-group binding when a userspace observer supplies
    /// the recovery proof. Kernel cgroups carry their own immutable group ID.
    fn verify_recovered_root(
        &self,
        _group: &GroupHandle,
        _root: &ProcessIdentity,
    ) -> io::Result<()> {
        Ok(())
    }

    /// Capture before releasing the launch gate. None means this backend
    /// cannot prove ownership after a daemon restart.
    fn recovery_identity(&self, _group: &GroupHandle) -> io::Result<Option<GroupRecoveryIdentity>> {
        Ok(None)
    }

    /// Preserve the native object through termination until the caller's
    /// durable exit commit. Terminal workloads without persistence keep their
    /// usual immediate cleanup behavior.
    fn retain_group_until_exit(&self, _group: &mut GroupHandle) {}

    /// Reopen an existing native group after validating its durable identity.
    /// Never create the group, attach a PID, or infer exit from a missing path.
    fn recover_group(
        &self,
        _workload_id: &WorkloadId,
        _reference: &str,
        _identity: &GroupRecoveryIdentity,
    ) -> io::Result<GroupHandle> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native group recovery unavailable",
        ))
    }

    /// Optional native directory cleanup after the recovered exit is durable.
    /// This must never signal or remove a replacement group with the same name.
    fn retire_recovered_group(&self, _group: &GroupHandle) -> io::Result<()> {
        Ok(())
    }

    /// Spec 02-runner §3 gate step: attach the **WAITING** helper to the group
    /// *before* RELEASE so the target and all its descendants inherit
    /// membership. Identity (pid + start_token + boot_id) is re-verified
    /// immediately before the OS attach call; on mismatch or attach failure
    /// the caller must NOT send RELEASE and must clean the helper up.
    ///
    /// Default: identical to [`ResourcePlatform::attach_pid`] (all R1
    /// backends attach the helper by verified PID).
    fn attach_waiting_helper(
        &self,
        group: &GroupHandle,
        identity: &ProcessIdentity,
    ) -> io::Result<()> {
        self.attach_pid(group, identity)
    }

    /// Attach a still-live process (verified against `identity` first) to the
    /// group. Windows opens the pid with
    /// `PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION`
    /// and calls `AssignProcessToJobObject`; nested-job / permission failures
    /// surface as `io::ErrorKind::PermissionDenied` so upstream require/prefer
    /// decisions can branch (§6). Linux writes `cgroup.procs` and verifies
    /// membership (§5 step 3). macOS records the tree root identity.
    fn attach_pid(&self, group: &GroupHandle, identity: &ProcessIdentity) -> io::Result<()>;

    /// Snapshot usage for the group. `now_ms` is a **monotonic** millisecond
    /// timestamp supplied by the caller (never wall clock — 01-contracts §1);
    /// rate and cpu_cores metrics are differentials against the previous
    /// sample, so the first sample reports them as unavailable (03 §2).
    fn sample_group(&self, group: &GroupHandle, now_ms: u64) -> io::Result<WorkloadUsage>;

    /// Verifiable member identities of the group right now. Used by the runner
    /// for the 02-runner §7 identity re-verification before/while stopping,
    /// and by `workload.processes`. Processes whose identity cannot be read
    /// are omitted (never guessed).
    fn member_identities(&self, group: &GroupHandle) -> io::Result<Vec<ProcessIdentity>>;

    /// Stop owned members. Identity re-verification discipline: the caller
    /// re-verifies via [`ResourcePlatform::member_identities`] per 02-runner
    /// §7; backends additionally never signal outside group membership.
    /// `Grace` on Windows R1 is a documented no-op (02-runner §7).
    fn terminate_owned(&self, group: &GroupHandle, phase: StopPhase) -> io::Result<()>;

    /// True when the group has no live members (job `ActiveProcesses == 0` /
    /// cgroup `populated 0` / verified tree empty).
    fn is_empty(&self, group: &GroupHandle) -> io::Result<bool>;

    /// Move the group to a scheduling tier (spec `08-pressure-relief.md` §2).
    /// Backends re-verify every member identity immediately before the OS
    /// call and never touch a pid outside the group's membership; a reused
    /// pid is counted in [`SchedulingOutcome::skipped_reused`], never
    /// signalled. Default: `Unsupported` — a backend that cannot restore the
    /// original tier must not apply the change at all (§0-4).
    fn set_scheduling(
        &self,
        _group: &GroupHandle,
        _tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "scheduling yield unavailable on this backend",
        ))
    }

    /// Suspend (pause) every verified member of the group — the resource
    /// guard's reversible stop (spec `08-pressure-relief.md` §5). Same
    /// discipline as [`ResourcePlatform::set_scheduling`]: membership is
    /// re-walked, every identity re-verified immediately before the OS call,
    /// a reused pid is skipped and counted. Default: `Unsupported`.
    fn suspend_owned(&self, _group: &GroupHandle) -> io::Result<SchedulingOutcome> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "suspend/resume unavailable on this backend",
        ))
    }

    /// Resume every verified member suspended by [`ResourcePlatform::suspend_owned`].
    fn resume_owned(&self, _group: &GroupHandle) -> io::Result<SchedulingOutcome> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "suspend/resume unavailable on this backend",
        ))
    }

    /// OOM kill counter for the group when the OS exposes one — Linux cgroup
    /// v2 `memory.events` → `oom_kill`. Read at teardown to classify the exit
    /// (`session.exited.reason = oom_kill`). `None` means "not observable on
    /// this platform" (Windows job objects / macOS observation-only trees);
    /// callers must not treat `None` as zero.
    fn oom_kill_count(&self, _group: &GroupHandle) -> Option<u64> {
        None
    }
}

/// Select the resource-group backend for the compiling OS. Other OSes get an
/// explicit unsupported backend (capabilities all `unsupported`) instead of a
/// stub that pretends to work.
pub fn select_backend() -> Box<dyn ResourcePlatform> {
    #[cfg(target_os = "windows")]
    {
        Box::new(WindowsJobPlatform::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxCgroupPlatform::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(MacosTreePlatform::new())
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Box::new(UnsupportedPlatform)
    }
}

/// Explicit "no backend here" implementation for OSes outside the R1 set.
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
struct UnsupportedPlatform;

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
impl ResourcePlatform for UnsupportedPlatform {
    fn capabilities(&self) -> Capabilities {
        let unsupported = || LimitCapability {
            support: LimitSupport::Unsupported,
            reason: Some("unsupported OS for resource groups".into()),
        };
        Capabilities {
            memory_limit_kind: unsupported(),
            cpu_quota: unsupported(),
            process_count_limit: unsupported(),
            tree_accounting: unsupported(),
            reattach: unsupported(),
            resume: unsupported(),
            scheduling_yield: unsupported(),
            suspend_resume: unsupported(),
            platform: "unsupported".into(),
            notes: vec!["no resource group backend compiled for this OS".into()],
            mission_protocol: None,
            claude_provider_routing: false,
        }
    }

    fn create_group(&self, _workload: &WorkloadDescriptor) -> io::Result<GroupHandle> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no resource group backend on this OS",
        ))
    }

    fn attach_pid(&self, _group: &GroupHandle, _identity: &ProcessIdentity) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no resource group backend on this OS",
        ))
    }

    fn sample_group(&self, _group: &GroupHandle, _now_ms: u64) -> io::Result<WorkloadUsage> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no resource group backend on this OS",
        ))
    }

    fn member_identities(&self, _group: &GroupHandle) -> io::Result<Vec<ProcessIdentity>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no resource group backend on this OS",
        ))
    }

    fn terminate_owned(&self, _group: &GroupHandle, _phase: StopPhase) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no resource group backend on this OS",
        ))
    }

    fn is_empty(&self, _group: &GroupHandle) -> io::Result<bool> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no resource group backend on this OS",
        ))
    }
}

// ---- shared helpers -------------------------------------------------------

/// Monotonic counters retained between two samples for differential metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RateSample {
    /// Caller-supplied monotonic milliseconds.
    pub now_ms: u64,
    /// Cumulative CPU time (user+kernel) in microseconds.
    pub cpu_time_us: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// Differentials between two samples: `(cpu_cores, read_bytes_per_sec,
/// write_bytes_per_sec)`. `None` when the elapsed time is zero or the clock
/// went backwards (the caller must then report unavailable, never 0 — §2).
pub(crate) fn rate_deltas(prev: &RateSample, cur: &RateSample) -> Option<(f64, f64, f64)> {
    let dt_ms = cur.now_ms.checked_sub(prev.now_ms).filter(|dt| *dt > 0)?;
    let dt_s = dt_ms as f64 / 1_000.0;
    let cores = cur.cpu_time_us.saturating_sub(prev.cpu_time_us) as f64 / 1_000_000.0 / dt_s;
    let read = cur.read_bytes.saturating_sub(prev.read_bytes) as f64 / dt_s;
    let write = cur.write_bytes.saturating_sub(prev.write_bytes) as f64 / dt_s;
    Some((cores, read, write))
}

/// Byte-count metric with the SQLite `i64` bound enforced (never a silent 0).
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub(crate) fn bytes_metric(source: &str, value: u64) -> Metric<U64String> {
    match U64String::new(value) {
        Ok(v) => Metric::measured(source, v),
        Err(e) => Metric::unavailable(source, format!("value out of persisted range: {e}")),
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::time::{Duration, Instant};

    /// Monotonic milliseconds since process start (test clock source; mirrors
    /// the daemon's monotonic `now_ms` contract).
    pub fn mono_ms() -> u64 {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(Instant::now);
        start.elapsed().as_millis() as u64
    }

    /// Poll `f` every 25 ms until it returns true or `deadline` elapses.
    pub fn poll_until(deadline: Duration, mut f: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    fn workload(
        policy: term_contracts::launch::LaunchPolicy,
    ) -> term_contracts::workload::WorkloadDescriptor {
        use std::collections::BTreeMap;
        use term_contracts::ids::SessionId;
        use term_contracts::workload::WorkloadDescriptor;
        WorkloadDescriptor {
            workload_id: term_contracts::ids::WorkloadId::generate(),
            session_id: SessionId::generate(),
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            program: if cfg!(windows) {
                "C:/Windows/System32/ping.exe".into()
            } else {
                "/bin/sleep".into()
            },
            argv: vec![],
            env_overrides: BTreeMap::new(),
            cols: 80,
            rows: 24,
            policy,
        }
    }

    fn policy(
        memory_max: Option<u64>,
        cpu_max_cores: Option<f64>,
        pids_max: Option<u32>,
    ) -> term_contracts::launch::LaunchPolicy {
        use term_contracts::ids::U64String;
        use term_contracts::launch::{Enforcement, LaunchPolicy};
        LaunchPolicy {
            reservation_bytes: U64String::new(1 << 31).expect("2 GiB fits"),
            cpu_slots: 1,
            enforcement: Enforcement::Prefer,
            memory_max_bytes: memory_max.map(|m| U64String::new(m).expect("fits i64")),
            cpu_max_cores,
            pids_max,
        }
    }

    /// Managed workload descriptor with an explicit policy for tests.
    pub fn test_workload(
        memory_max: Option<u64>,
        cpu_max_cores: Option<f64>,
        pids_max: Option<u32>,
    ) -> term_contracts::workload::WorkloadDescriptor {
        workload(policy(memory_max, cpu_max_cores, pids_max))
    }

    /// Long-lived own child (never an unrelated process).
    pub fn spawn_lived_child() -> std::process::Child {
        if cfg!(windows) {
            std::process::Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .spawn()
                .expect("spawn ping")
        } else {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn sleep")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_deltas_requires_positive_elapsed_time() {
        let a = RateSample {
            now_ms: 1_000,
            cpu_time_us: 500_000,
            read_bytes: 100,
            write_bytes: 100,
        };
        let same = RateSample { now_ms: 1_000, ..a };
        assert!(rate_deltas(&a, &same).is_none(), "zero elapsed");
        let backwards = RateSample { now_ms: 999, ..a };
        assert!(rate_deltas(&a, &backwards).is_none(), "clock went back");

        // 1 core for 100 ms → 1.0 cores; 1000 B / 0.1 s → 10 KB/s.
        let next = RateSample {
            now_ms: 1_100,
            cpu_time_us: 600_000,
            read_bytes: 1_100,
            write_bytes: 100,
        };
        let (cores, read, write) = rate_deltas(&a, &next).expect("valid deltas");
        assert!((cores - 1.0).abs() < 1e-9);
        assert!((read - 10_000.0).abs() < 1e-6);
        assert_eq!(write, 0.0);

        // Counter reset (cur < prev) must saturate, not wrap.
        let reset = RateSample {
            now_ms: 1_200,
            cpu_time_us: 0,
            read_bytes: 0,
            write_bytes: 0,
        };
        let (cores, read, _) = rate_deltas(&next, &reset).expect("valid deltas");
        assert_eq!(cores, 0.0);
        assert_eq!(read, 0.0);
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn bytes_metric_bounds() {
        assert!(bytes_metric("x", 123).value.is_some());
        assert!(bytes_metric("x", u64::MAX).value.is_none());
    }

    #[test]
    fn select_backend_matches_os() {
        let backend = select_backend();
        let caps = backend.capabilities();
        if cfg!(target_os = "windows") {
            assert_eq!(caps.platform, "windows-job");
        } else if cfg!(target_os = "linux") {
            assert_eq!(caps.platform, "linux-cgroup.v2");
        } else if cfg!(target_os = "macos") {
            assert_eq!(caps.platform, "macos-observed-tree");
        } else {
            assert_eq!(caps.platform, "unsupported");
        }
    }
}
