//! Ticket I06 — Linux cgroup v2 backend (spec `03-resources.md` §5). Only the
//! whole module is compiled on Linux targets.
//!
//! Delegated-subtree discipline (§5): workload cgroups are created **only**
//! inside a subtree where controller delegation and write permission are
//! actually verified at runtime — never by chmod'ing `/sys/fs/cgroup` and
//! never via sudo. Discovery walks from the daemon's own cgroup (v2 path from
//! `/proc/self/cgroup`) outward to the unified root; the deepest ancestor
//! whose `cgroup.controllers` lists the needed controllers and whose
//! `cgroup.subtree_control` can provide them (already enabled, or enabling
//! succeeds) and that passes a create+remove probe directory becomes the
//! delegated root. The daemon's control processes stay outside the workload
//! subtree (`workloads/<uuid>` children only, §5 step 1).
//!
//! Creation follows §5 steps 2–4: write each requested limit file, **read it
//! back and verify**, attach the waiting helper PID via `cgroup.procs` and
//! verify membership — on failure the caller must not RELEASE. Measurement
//! uses `memory.current`, `memory.events`, `cpu.stat`, `cgroup.events`,
//! `pids.current`, `io.stat` and the member PID inventory. Telemetry quality:
//! `accounted_bytes` is the cgroup accounting value; `resident_bytes` is a
//! per-member RSS sum marked **estimated** (shared pages may be double
//! counted, §2) with coverage dropping to `partial` when some members are
//! unreadable.
//!
//! Cancel path (§5 step 6): TERM to current member PIDs (membership is the
//! ownership proof; the caller re-verifies identities per 02-runner §7),
//! after the grace window `cgroup.kill` when present, otherwise KILL to
//! remaining members; the directory is removed only after `populated 0`.
//!
//! Without a delegated subtree (cgroup v1/hybrid host, container, session
//! without delegation) the backend does NOT fail launches: `create_group`
//! returns an observed-process-tree handle (§5: observe/prefer fall back to
//! process-tree estimation; `require` is refused earlier by the orchestrator
//! because the limits are `PermissionRequired`/`Unsupported`). The tree
//! mirrors the macOS backend, with one Linux twist: proven descendants stay
//! recorded after the root exits (Linux reparents orphans to init, which
//! would drop them from a plain ppid walk while 02-runner §5 keeps the
//! workload RUNNING until its owned descendants leave).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::linux_recovery::{signal_member, CgroupAnchor};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use term_contracts::ids::ProcessIdentity;
use term_contracts::metrics::{Metric, UsageCoverage, WorkloadUsage};
use term_contracts::snapshot::{Capabilities, LimitCapability, LimitSupport};
use term_contracts::workload::{GroupKind, GroupRecoveryIdentity, WorkloadDescriptor};

use super::{
    bytes_metric, rate_deltas, GroupHandle, GroupInner, RateSample, SchedulingOutcome,
    SchedulingTier, StopPhase,
};
use crate::identity;

const CGROUP_FS: &str = "/sys/fs/cgroup";
const SOURCE: &str = "cgroup.v2";
/// Fixed period from §5: 100000 µs; quota = round(cores * period).
const CPU_PERIOD_US: u64 = 100_000;
/// Smallest quota the kernel accepts for a 100 ms period; lower values fail
/// at write time, so they are validated before any side effect.
const MIN_CPU_QUOTA_US: u64 = 1_000;
/// Bounded wait for `populated 0` after a force kill before reporting the
/// group as still-populated (the runner keeps polling `is_empty` anyway).
const POPULATED_TIMEOUT: Duration = Duration::from_secs(2);

/// cgroup v2 backend for Linux.
pub struct LinuxCgroupPlatform {
    /// Not probed yet until first use. Discovery is cached for the daemon
    /// lifetime; absence is a stable fact of the current process's
    /// permissions (and of the host's cgroup version).
    delegation: OnceLock<Result<Delegation, NoDelegation>>,
    /// Single daemon-wide sysinfo instance for the observed-tree fallback
    /// (03 §2: one instance, refreshed incrementally).
    system: Mutex<System>,
}

impl LinuxCgroupPlatform {
    pub fn new() -> Self {
        Self {
            delegation: OnceLock::new(),
            system: Mutex::new(System::new()),
        }
    }

    fn probe(&self) -> &Result<Delegation, NoDelegation> {
        self.delegation.get_or_init(discover)
    }

    #[cfg(test)]
    fn delegated(&self) -> Option<&Delegation> {
        self.probe().as_ref().ok()
    }

    fn refresh(&self) -> std::sync::MutexGuard<'_, System> {
        // A panic inside the lock must not disable observation for the
        // daemon's lifetime (owned_alive → 0, cancel unable to signal).
        let mut system = self.system.lock().unwrap_or_else(|p| p.into_inner());
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_disk_usage(),
        );
        system
    }
}

/// Why no delegated subtree exists — decides the capability wording
/// (`Unsupported` for a v1 host, `PermissionRequired` otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoDelegation {
    /// `/sys/fs/cgroup/cgroup.controllers` is absent: cgroup v1 or hybrid
    /// host (Ubuntu 20.04 default). No unified-hierarchy limits exist.
    V1Host,
    /// Unified hierarchy, but no ancestor of this process grants a writable
    /// subtree with any of memory/cpu/pids.
    NoWritableSubtree,
}

const NEEDED_CONTROLLERS: [&str; 3] = ["memory", "cpu", "pids"];

impl Default for LinuxCgroupPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
struct Delegation {
    root: PathBuf,
    /// Controllers available in `root/cgroup.controllers`.
    controllers: BTreeSet<String>,
}

/// Backend payload carried by a [`GroupHandle`] on Linux. Cloning shares the
/// cgroup path; the differential-sample base is duplicated per clone.
#[derive(Debug)]
pub(crate) struct LinuxGroupInner {
    path: PathBuf,
    anchor: Arc<CgroupAnchor>,
    recovered: bool,
    prev: Mutex<Option<RateSample>>,
}

impl Clone for LinuxGroupInner {
    fn clone(&self) -> Self {
        let prev = self.prev.lock().map(|p| *p).unwrap_or_default();
        Self {
            path: self.path.clone(),
            anchor: self.anchor.clone(),
            recovered: self.recovered,
            prev: Mutex::new(prev),
        }
    }
}

// ---- discovery ------------------------------------------------------------

fn read_tokens(path: &Path) -> io::Result<Vec<String>> {
    Ok(fs::read_to_string(path)?
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

fn try_delegated_root(dir: &Path) -> Option<Delegation> {
    // Controller availability for children of `dir`.
    let available: BTreeSet<String> = read_tokens(&dir.join("cgroup.controllers"))
        .ok()?
        .into_iter()
        .collect();
    // Partial delegation is accepted per controller (systemd < 247 hands
    // the user manager only `pids memory`): what is available becomes
    // Supported, the rest stays PermissionRequired and the orchestrator
    // reports the missing limits (03 §5).
    let needed: Vec<&str> = NEEDED_CONTROLLERS
        .iter()
        .copied()
        .filter(|c| available.contains(*c))
        .collect();
    if needed.is_empty() {
        return None;
    }
    // Controllers must be enabled in subtree_control for children to carry
    // the control files; enable the missing ones if the write succeeds
    // (EBUSY when the cgroup has member processes → walk outward).
    let enabled: BTreeSet<String> = read_tokens(&dir.join("cgroup.subtree_control"))
        .ok()?
        .into_iter()
        .map(|t| t.trim_start_matches('+').to_string())
        .collect();
    let missing: Vec<&str> = needed
        .iter()
        .filter(|c| !enabled.contains(**c))
        .copied()
        .collect();
    if !missing.is_empty() {
        let plus = missing
            .iter()
            .map(|c| format!("+{c}"))
            .collect::<Vec<_>>()
            .join(" ");
        if fs::write(dir.join("cgroup.subtree_control"), plus.as_bytes()).is_err() {
            return None;
        }
    }
    // Writability proof without touching the root mount's permissions: a
    // create+remove probe directory (§5 forbids chmod/sudo workarounds).
    let probe = dir.join(format!(".iyagi-probe-{}", uuid::Uuid::new_v4().simple()));
    match fs::create_dir(&probe) {
        Ok(()) => {
            let _ = fs::remove_dir(&probe);
        }
        Err(_) => return None,
    }
    Some(Delegation {
        root: dir.to_path_buf(),
        controllers: needed.into_iter().map(str::to_string).collect(),
    })
}

fn discover() -> Result<Delegation, NoDelegation> {
    // A v1/hybrid host has no unified `cgroup.controllers` at the mount
    // root (the unified tree, if any, sits under `unified/` without
    // controllers) — report it as such, not as a permission problem.
    if !Path::new(CGROUP_FS).join("cgroup.controllers").is_file() {
        return Err(NoDelegation::V1Host);
    }
    let raw =
        fs::read_to_string("/proc/self/cgroup").map_err(|_| NoDelegation::NoWritableSubtree)?;
    let v2 = raw
        .lines()
        .find(|l| l.starts_with("0::"))
        .ok_or(NoDelegation::V1Host)?;
    let rel = v2
        .strip_prefix("0::")
        .ok_or(NoDelegation::NoWritableSubtree)?
        .trim()
        .trim_start_matches('/');
    let mut ancestors: Vec<PathBuf> = Vec::new();
    let mut acc = PathBuf::new();
    for comp in Path::new(rel).components() {
        acc.push(comp);
        ancestors.push(PathBuf::from(CGROUP_FS).join(&acc));
    }
    // Deepest first (usually the delegated systemd user slice); the unified
    // root is the last resort and normally not writable for users.
    ancestors.reverse();
    ancestors.push(PathBuf::from(CGROUP_FS));
    match ancestors.iter().find_map(|cand| try_delegated_root(cand)) {
        Some(d) => {
            // Persisted mission groups must remain available for recovery.
            Ok(d)
        }
        None => Err(NoDelegation::NoWritableSubtree),
    }
}

/// `workloads/` must enable the controllers itself: cgroup v2 children only
/// get `memory.max`/`cpu.max`/`pids.max` (and the `*.current` accounting
/// files) when their *parent* lists the controller in
/// `cgroup.subtree_control` — enabling it on the delegated root reaches
/// `workloads/` but not the per-workload groups below it. Idempotent;
/// `workloads/` never holds processes, so EBUSY cannot occur.
fn enable_subtree_controllers(parent: &Path, controllers: &BTreeSet<String>) -> io::Result<()> {
    let enabled: BTreeSet<String> = read_tokens(&parent.join("cgroup.subtree_control"))?
        .into_iter()
        .map(|t| t.trim_start_matches('+').to_string())
        .collect();
    let missing: Vec<String> = controllers
        .iter()
        .filter(|c| !enabled.contains(*c))
        .map(|c| format!("+{c}"))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let plus = missing.join(" ");
    fs::write(parent.join("cgroup.subtree_control"), plus.as_bytes()).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("enable {plus} in {}: {e}", parent.display()),
        )
    })
}

// ---- helpers --------------------------------------------------------------

fn cgroup_inner(group: &GroupHandle) -> io::Result<&LinuxGroupInner> {
    match &group.inner {
        GroupInner::Cgroup(c) => Ok(c),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a linux cgroup group handle",
        )),
    }
}

/// `cpu.max` quota for the requested cores: `round(cores * 100000)` validated
/// against the kernel minimum (§5).
pub(crate) fn cpu_quota_for(cores: f64) -> io::Result<u64> {
    if !cores.is_finite() || cores <= 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cpu_max_cores must be finite and positive, got {cores}"),
        ));
    }
    let quota = (cores * CPU_PERIOD_US as f64).round() as u64;
    if quota < MIN_CPU_QUOTA_US {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cpu quota {quota}µs below kernel minimum {MIN_CPU_QUOTA_US}µs for period {CPU_PERIOD_US}µs"
            ),
        ));
    }
    Ok(quota)
}

/// Write a limit file and verify by read-back (§5 step 2).
fn write_verified(dir: &Path, file: &str, expected: &str) -> io::Result<()> {
    let path = dir.join(file);
    fs::write(&path, expected.as_bytes())
        .map_err(|e| io::Error::new(e.kind(), format!("write {file}: {e}")))?;
    let read = fs::read_to_string(&path)
        .map_err(|e| io::Error::new(e.kind(), format!("read back {file}: {e}")))?;
    let read = read.trim();
    if read != expected.trim() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{file} read-back {read:?} != requested {expected:?} (03 §5 step 2)"),
        ));
    }
    Ok(())
}

fn read_trimmed(path: &Path) -> io::Result<String> {
    fs::read_to_string(path).map(|s| s.trim().to_string())
}

fn read_number(path: &Path) -> Option<u64> {
    read_trimmed(path).ok()?.parse().ok()
}

pub(super) fn member_pids(dir: &Path) -> io::Result<Vec<u32>> {
    Ok(read_tokens(&dir.join("cgroup.procs"))?
        .into_iter()
        .filter_map(|t| t.parse().ok())
        .collect())
}

/// `populated` flag from `cgroup.events`.
pub(super) fn populated(dir: &Path) -> io::Result<bool> {
    let events = read_trimmed(&dir.join("cgroup.events"))?;
    for line in events.lines() {
        if let Some(value) = line.strip_prefix("populated") {
            return match value.trim() {
                "0" => Ok(false),
                "1" => Ok(true),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid populated flag",
                )),
            };
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "cgroup.events has no populated field",
    ))
}

fn page_size() -> u64 {
    static PAGE: OnceLock<u64> = OnceLock::new();
    *PAGE.get_or_init(|| unsafe { libc::sysconf(libc::_SC_PAGESIZE).max(1) as u64 })
}

/// Resident estimate for the member inventory: sum of `/proc/<pid>/statm`
/// resident pages. Marked estimated (shared-page duplication, §2). Unreadable
/// members are counted so the caller can downgrade coverage to `partial`.
fn resident_estimate(pids: &[u32]) -> (u64, usize) {
    let page = page_size();
    let mut total = 0u64;
    let mut unreadable = 0usize;
    for &pid in pids {
        match fs::read_to_string(format!("/proc/{pid}/statm")) {
            Ok(statm) => {
                // statm: size resident shared text lib data dt (pages)
                let resident_pages: u64 = statm
                    .split_whitespace()
                    .nth(1)
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0);
                total = total.saturating_add(resident_pages.saturating_mul(page));
            }
            Err(_) => unreadable += 1,
        }
    }
    (total, unreadable)
}

/// `usage_usec` from `cpu.stat`.
fn cpu_usage_usec(dir: &Path) -> Option<u64> {
    let stat = read_trimmed(&dir.join("cpu.stat")).ok()?;
    for line in stat.lines() {
        if let Some(rest) = line.strip_prefix("usage_usec") {
            return rest.trim().parse().ok();
        }
    }
    None
}

/// Summed `(rbytes, wbytes)` from `io.stat` (per-device counters).
fn io_bytes(dir: &Path) -> Option<(u64, u64)> {
    let stat = read_trimmed(&dir.join("io.stat")).ok()?;
    let (mut read, mut write) = (0u64, 0u64);
    for line in stat.lines() {
        for field in line.split_whitespace().skip(1) {
            if let Some(v) = field.strip_prefix("rbytes=") {
                read = read.saturating_add(v.parse().unwrap_or(0));
            } else if let Some(v) = field.strip_prefix("wbytes=") {
                write = write.saturating_add(v.parse().unwrap_or(0));
            }
        }
    }
    Some((read, write))
}

fn send_signal(pid: u32, sig: i32) {
    // ESRCH races with process exit are expected and not errors.
    let _ = unsafe { libc::kill(pid as libc::pid_t, sig) };
}

/// Pin `ident`'s pid with `pidfd_open`, re-verify the live identity still
/// matches, then signal through the pinned fd (§7): a member that exits
/// after the batch verification in `tree_terminate` cannot have its reused
/// pid signalled — signals go to the pinned process, never to the pid's
/// next holder. This is the observed-tree twin of the cgroup path's
/// `signal_member` discipline (linux_recovery.rs).
fn tree_signal_member(ident: &ProcessIdentity, signal: i32) {
    // SAFETY: pidfd_open has no pointer arguments and returns an owned FD.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, ident.pid as libc::pid_t, 0) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return; // exited since verification
        }
        // No pidfds (pre-5.3 kernel) or an unexpected open failure: fall
        // back to per-member re-verify-then-signal — still never a blind
        // kill on a batch-verified pid.
        tracing::debug!(pid = ident.pid, %error, "pidfd_open unavailable; re-verify-then-signal");
        if identity::process_identity(ident.pid).is_some_and(|live| live.same_process(ident)) {
            send_signal(ident.pid, signal);
        }
        return;
    }
    // SAFETY: the raw FD came from a successful pidfd_open; the File closes it.
    let pinned = unsafe { fs::File::from_raw_fd(fd as i32) };
    // The fd pins whoever held the pid at open time; the /proc stat compare
    // proves that holder is still `ident`. A reused pid always has a later
    // starttime, so a token match means the pid never changed hands since
    // the pin — and a signal through the fd cannot reach a later holder even
    // if the pid changes hands afterwards.
    if !identity::process_identity(ident.pid).is_some_and(|live| live.same_process(ident)) {
        return;
    }
    // SAFETY: pidfd_send_signal takes the owned fd, a signal number, an
    // absent siginfo pointer and no flags.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pinned.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        // Delivery failures (e.g. EPERM on a privilege-changed descendant)
        // must not block the rest of the tree's stop; the stop ladder re-runs
        // and `is_empty` eventually reports the unsignallable stray.
        tracing::debug!(pid = ident.pid, "pidfd_send_signal failed");
    }
}

// ---- trait impl -----------------------------------------------------------

impl super::ResourcePlatform for LinuxCgroupPlatform {
    fn capabilities(&self) -> Capabilities {
        match self.probe() {
            Ok(d) => {
                let has = |c: &str| d.controllers.contains(c);
                let supported = |reason: String| LimitCapability {
                    support: LimitSupport::Supported,
                    reason: Some(reason),
                };
                let denied = |c: &str| LimitCapability {
                    support: LimitSupport::PermissionRequired,
                    reason: Some(format!(
                        "{c} controller not delegated to any writable ancestor"
                    )),
                };
                Capabilities {
                    memory_limit_kind: if has("memory") {
                        supported(format!(
                            "cgroup v2 memory.max/memory.high under {}",
                            d.root.display()
                        ))
                    } else {
                        denied("memory")
                    },
                    cpu_quota: if has("cpu") {
                        supported(format!("cgroup v2 cpu.max under {}", d.root.display()))
                    } else {
                        denied("cpu")
                    },
                    process_count_limit: if has("pids") {
                        supported(format!("cgroup v2 pids.max under {}", d.root.display()))
                    } else {
                        denied("pids")
                    },
                    tree_accounting: supported(
                        "cgroup membership covers the whole workload subtree".into(),
                    ),
                    // 08 §2: 비례 배분(cpu.weight)은 되돌릴 수 있고 경합이
                    // 없으면 영향이 없다. cpu 컨트롤러가 위임되지 않았으면
                    // 쓸 수 없다.
                    scheduling_yield: if has("cpu") {
                        supported(format!("cgroup v2 cpu.weight under {}", d.root.display()))
                    } else {
                        denied("cpu")
                    },
                    // 08 §5: freeze는 위임 여부와 무관하게 검증된 트리의
                    // SIGSTOP/SIGCONT로도 되돌릴 수 있다.
                    suspend_resume: supported("cgroup v2 freeze (per-pid fallback)"),
                    reattach: LimitCapability {
                        support: LimitSupport::Supported,
                        reason: None,
                    },
                    resume: LimitCapability {
                        support: LimitSupport::Unsupported,
                        reason: Some("daemon restart marks workloads INTERRUPTED (R1)".into()),
                    },
                    platform: "linux-cgroup.v2".into(),
                    notes: vec![format!("delegated root: {}", d.root.display())],
                    mission_protocol: None,
                    claude_provider_routing: false,
                }
            }
            Err(why) => {
                // v1/hybrid: the limits cannot exist on this host at all
                // (Unsupported); unified without delegation: they would
                // work with delegation (PermissionRequired). Both fall back
                // to the observed process tree.
                let (support, reason) = match why {
                    NoDelegation::V1Host => (
                        LimitSupport::Unsupported,
                        "cgroup v1/hybrid host: no unified cgroup.controllers (03 §5)",
                    ),
                    NoDelegation::NoWritableSubtree => (
                        LimitSupport::PermissionRequired,
                        "no writable cgroup v2 delegation for this process (03 §5)",
                    ),
                };
                let denied = |reason: &str| LimitCapability {
                    support,
                    reason: Some(reason.to_string()),
                };
                Capabilities {
                    memory_limit_kind: denied(reason),
                    cpu_quota: denied(reason),
                    process_count_limit: denied(reason),
                    // Without delegation the daemon falls back to observed
                    // process-tree estimation (§5: prefer keeps running with
                    // missing capabilities, require fails at preflight).
                    tree_accounting: LimitCapability {
                        support: LimitSupport::Supported,
                        reason: Some("observed process-tree estimation (no delegation)".into()),
                    },
                    // 08 §2: 비특권 프로세스는 nice를 올릴 수는 있어도
                    // RLIMIT_NICE 기본값에서 되돌릴 수 없다. 되돌릴 수 없는
                    // 완화는 적용하지 않는다(§0-4).
                    scheduling_yield: LimitCapability {
                        support: LimitSupport::Unsupported,
                        reason: Some(
                            "no cgroup delegation; nice cannot be restored without \
                             CAP_SYS_NICE (RLIMIT_NICE)"
                                .into(),
                        ),
                    },
                    suspend_resume: supported("SIGSTOP/SIGCONT on verified tree members"),
                    reattach: LimitCapability {
                        support: LimitSupport::Supported,
                        reason: None,
                    },
                    resume: LimitCapability {
                        support: LimitSupport::Unsupported,
                        reason: Some("daemon restart marks workloads INTERRUPTED (R1)".into()),
                    },
                    platform: "linux-cgroup.v2".into(),
                    notes: vec![
                        "no delegated subtree: observe/prefer fall back to process-tree estimation; require fails".into(),
                    ],
                    mission_protocol: None,
                    claude_provider_routing: false,
                }
            }
        }
    }

    fn create_group(&self, workload: &WorkloadDescriptor) -> io::Result<GroupHandle> {
        let d = match self.probe() {
            Ok(d) => d,
            // §5: observe/prefer keep running on process-tree estimation;
            // `require` never reaches here (the orchestrator refuses
            // PermissionRequired/Unsupported limits at preflight).
            Err(why) => {
                tracing::debug!(workload = %workload.workload_id, ?why, "no cgroup delegation; observed-tree fallback");
                return Ok(observed_tree_handle(workload));
            }
        };
        // Daemon/control processes stay outside; only workload children live
        // below `workloads/` (§5 step 1).
        let parent = d.root.join("workloads");
        fs::create_dir_all(&parent).map_err(|e| {
            io::Error::new(e.kind(), format!("cannot create {}: {e}", parent.display()))
        })?;
        enable_subtree_controllers(&parent, &d.controllers)?;
        let dir = parent.join(workload.workload_id.as_str());
        fs::create_dir(&dir).map_err(|e| {
            io::Error::new(e.kind(), format!("cannot create {}: {e}", dir.display()))
        })?;

        let policy = &workload.policy;
        let has = |c: &str| d.controllers.contains(c);
        let skip = |c: &str, file: &str| {
            // Partial delegation: the orchestrator already listed this limit
            // as a missing capability (prefer keeps running, 03 §5).
            tracing::debug!(workload = %workload.workload_id, controller = c, "{file} skipped: controller not delegated");
        };
        let apply = || -> io::Result<()> {
            if let Some(mem) = &policy.memory_max_bytes {
                if !has("memory") {
                    skip("memory", "memory.max");
                } else {
                    let m = mem.get();
                    if m == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "memory_max_bytes must be positive when set",
                        ));
                    }
                    // memory.max = M (hard limit, OOM at the edge);
                    // memory.high = floor(M*0.9) (reclaim/throttle path) — §5.
                    // The kernel stores both rounded down to whole pages, so an
                    // unaligned request would fail its own read-back (§5 step
                    // 2); align first, the same way the kernel would.
                    let page = page_size();
                    let max = m / page * page;
                    if max == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("memory_max_bytes {m} is below one page ({page})"),
                        ));
                    }
                    write_verified(&dir, "memory.max", &max.to_string())?;
                    let high = (max / 10 * 9) / page * page;
                    write_verified(&dir, "memory.high", &high.to_string())?;
                }
            }
            if let Some(cores) = policy.cpu_max_cores {
                if has("cpu") {
                    let quota = cpu_quota_for(cores)?;
                    write_verified(&dir, "cpu.max", &format!("{quota} {CPU_PERIOD_US}"))?;
                } else {
                    skip("cpu", "cpu.max");
                }
            }
            if let Some(pids) = policy.pids_max {
                if !has("pids") {
                    skip("pids", "pids.max");
                } else {
                    if pids == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "pids_max must be positive when set",
                        ));
                    }
                    // Only set when explicitly requested (§5).
                    write_verified(&dir, "pids.max", &pids.to_string())?;
                }
            }
            Ok(())
        };
        if let Err(e) = apply() {
            let _ = fs::remove_dir(&dir);
            return Err(e);
        }

        Ok(GroupHandle {
            workload_id: workload.workload_id.clone(),
            kind: GroupKind::Cgroup,
            reference: dir.to_string_lossy().into_owned(),
            inner: GroupInner::Cgroup(LinuxGroupInner {
                anchor: Arc::new(CgroupAnchor::open(&dir)?),
                path: dir,
                recovered: false,
                prev: Mutex::new(None),
            }),
        })
    }

    fn recovery_identity(&self, group: &GroupHandle) -> io::Result<Option<GroupRecoveryIdentity>> {
        match &group.inner {
            GroupInner::Cgroup(inner) => inner.anchor.identity().map(Some),
            GroupInner::ObservedTree(_) => Ok(None),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a Linux group",
            )),
        }
    }

    fn retain_group_until_exit(&self, group: &mut GroupHandle) {
        if let GroupInner::Cgroup(inner) = &mut group.inner {
            inner.recovered = true;
        }
    }

    fn recover_group(
        &self,
        workload_id: &term_contracts::ids::WorkloadId,
        reference: &str,
        expected: &GroupRecoveryIdentity,
    ) -> io::Result<GroupHandle> {
        let path = PathBuf::from(reference);
        if !path.is_absolute()
            || path.file_name().and_then(|n| n.to_str()) != Some(workload_id.as_str())
            || path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                != Some("workloads")
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid recovered cgroup path",
            ));
        }
        let anchor = Arc::new(CgroupAnchor::open(&path)?);
        if &anchor.identity()? != expected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recovered cgroup identity changed",
            ));
        }
        Ok(GroupHandle {
            workload_id: workload_id.clone(),
            kind: GroupKind::Cgroup,
            reference: reference.into(),
            inner: GroupInner::Cgroup(LinuxGroupInner {
                path,
                anchor,
                recovered: true,
                prev: Mutex::new(None),
            }),
        })
    }

    fn attach_pid(&self, group: &GroupHandle, identity: &ProcessIdentity) -> io::Result<()> {
        if let GroupInner::ObservedTree(tree) = &group.inner {
            return tree_attach(tree, identity);
        }
        let inner = cgroup_inner(group)?;
        // Full triple check immediately before the move (01 §1).
        identity::process_identity(identity.pid)
            .filter(|live| live.same_process(identity))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "identity mismatch: process gone or pid reused",
                )
            })?;
        let procs = inner.anchor.path().join("cgroup.procs");
        fs::write(&procs, identity.pid.to_string().as_bytes())
            .map_err(|e| io::Error::new(e.kind(), format!("write cgroup.procs: {e}")))?;
        // Membership verification — failure must NOT release the helper
        // (§5 step 3).
        let members = member_pids(&inner.anchor.path())?;
        if !members.contains(&identity.pid) {
            return Err(io::Error::other(
                "membership verification failed after cgroup.procs write (03 §5 step 3)",
            ));
        }
        Ok(())
    }

    fn retire_recovered_group(&self, group: &GroupHandle) -> io::Result<()> {
        let inner = cgroup_inner(group)?;
        if !inner.recovered || !self.is_empty(group)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "recovered group is not empty",
            ));
        }
        if inner.anchor.removed()? {
            return Ok(());
        }
        if CgroupAnchor::open(&inner.path)?.identity()? != inner.anchor.identity()? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cgroup path was replaced",
            ));
        }
        super::linux_recovery::remove_empty_children(&inner.anchor, &mut 1024, 0)?;
        fs::remove_dir(&inner.path)?;
        inner.anchor.mark_removed();
        Ok(())
    }

    fn sample_group(&self, group: &GroupHandle, now_ms: u64) -> io::Result<WorkloadUsage> {
        if let GroupInner::ObservedTree(tree) = &group.inner {
            return self.tree_sample(group, tree, now_ms);
        }
        let inner = cgroup_inner(group)?;
        let path = inner.anchor.path();
        let dir = &path;

        let accounted = read_number(&dir.join("memory.current"))
            .map(|v| bytes_metric(SOURCE, v))
            .unwrap_or_else(|| Metric::unavailable(SOURCE, "memory.current unreadable"));

        let pids = member_pids(dir)?;
        let (resident, unreadable_members) = resident_estimate(&pids);
        // Sum of member RSS: estimated, shared pages may double-count (§2).
        let resident_metric = match term_contracts::ids::U64String::new(resident) {
            Ok(v) => Metric::estimated("proc.rss", v),
            Err(_) => Metric::unavailable("proc.rss", "value out of persisted range"),
        };

        let process_count = read_number(&dir.join("pids.current"))
            .map(|v| Metric::measured(SOURCE, v as u32))
            .unwrap_or_else(|| Metric::unavailable(SOURCE, "pids.current unreadable"));

        let cpu_us = cpu_usage_usec(dir);
        let io = io_bytes(dir);
        let cur = RateSample {
            now_ms,
            cpu_time_us: cpu_us.unwrap_or(0),
            read_bytes: io.map(|i| i.0).unwrap_or(0),
            write_bytes: io.map(|i| i.1).unwrap_or(0),
        };
        let mut prev = inner
            .prev
            .lock()
            .map_err(|_| io::Error::other("sample lock poisoned"))?;
        let missing_counters = |what: &str| {
            Metric::unavailable(
                SOURCE,
                format!("{what} unreadable (controller not delegated?)"),
            )
        };
        let first = |reason: &'static str| Metric::<f64>::unavailable(SOURCE, reason);
        let (cpu_cores, read_rate, write_rate) = match prev.as_ref() {
            Some(p) => {
                let deltas = rate_deltas(p, &cur);
                let cpu = match (cpu_us, deltas) {
                    (Some(_), Some((cores, _, _))) => Metric::measured(SOURCE, cores),
                    (Some(_), None) => first("zero elapsed time since previous sample"),
                    (None, _) => missing_counters("cpu.stat"),
                };
                let (read, write) = match (io, deltas) {
                    (Some(_), Some((_, r, w))) => {
                        (Metric::measured(SOURCE, r), Metric::measured(SOURCE, w))
                    }
                    (Some(_), None) => (
                        first("zero elapsed time since previous sample"),
                        first("zero elapsed time since previous sample"),
                    ),
                    (None, _) => (missing_counters("io.stat"), missing_counters("io.stat")),
                };
                // Advance the differential base only when the pair produced
                // usable deltas.
                if deltas.is_some() {
                    *prev = Some(cur);
                }
                (cpu, read, write)
            }
            None => {
                *prev = Some(cur);
                (
                    first("first differential sample (03 §2)"),
                    first("first differential sample (03 §2)"),
                    first("first differential sample (03 §2)"),
                )
            }
        };
        drop(prev);

        let coverage = if unreadable_members > 0 {
            UsageCoverage::Partial
        } else {
            UsageCoverage::Group
        };

        Ok(WorkloadUsage {
            workload_id: group.workload_id.clone(),
            cpu_cores,
            resident_bytes: resident_metric,
            accounted_bytes: accounted,
            // cgroup v2 reports anon+file accounting as memory.current; a
            // distinct commit figure is not exposed.
            committed_bytes: Metric::unavailable(
                SOURCE,
                "commit accounting not exposed by cgroup v2; memory.current reported as accounted",
            ),
            read_bytes_per_sec: read_rate,
            write_bytes_per_sec: write_rate,
            network_rx_bytes_per_sec: Metric::unavailable(
                SOURCE,
                "per-cgroup networking requires bpf-based accounting (R1: none)",
            ),
            network_tx_bytes_per_sec: Metric::unavailable(
                SOURCE,
                "per-cgroup networking requires bpf-based accounting (R1: none)",
            ),
            process_count,
            coverage,
        })
    }

    fn member_identities(&self, group: &GroupHandle) -> io::Result<Vec<ProcessIdentity>> {
        if let GroupInner::ObservedTree(tree) = &group.inner {
            return Ok(self.tree_members(tree)?.0);
        }
        let inner = cgroup_inner(group)?;
        // A cgroup removed by an earlier Force (populated 0 → rmdir) has no
        // members; the runner's cancel and the actor's finalize both stop
        // the group and the loser of that race must not see an I/O error.
        if inner.anchor.removed()? {
            return Ok(Vec::new());
        }
        Ok(member_pids(&inner.anchor.path())?
            .iter()
            .filter_map(|&pid| identity::process_identity(pid))
            .collect())
    }

    fn terminate_owned(&self, group: &GroupHandle, phase: StopPhase) -> io::Result<()> {
        if let GroupInner::ObservedTree(tree) = &group.inner {
            return self.tree_terminate(tree, phase);
        }
        let inner = cgroup_inner(group)?;
        let path = inner.anchor.path();
        let dir = &path;
        match phase {
            // TERM to current members. Membership in this cgroup is the
            // ownership proof; the runner re-verifies identities separately
            // (02-runner §7).
            StopPhase::Grace => {
                if inner.anchor.removed()? {
                    return Ok(());
                }
                for pid in member_pids(dir)? {
                    signal_member(dir, pid, libc::SIGTERM)?;
                }
                Ok(())
            }
            StopPhase::Force => {
                if inner.anchor.removed()? {
                    return Ok(()); // already torn down by the other stopper
                }
                let kill_file = dir.join("cgroup.kill");
                if kill_file.exists() {
                    // Kernel 5.14+: atomically signal every member.
                    fs::write(&kill_file, b"1")
                        .map_err(|e| io::Error::new(e.kind(), format!("write cgroup.kill: {e}")))?;
                } else {
                    for pid in member_pids(dir)? {
                        signal_member(dir, pid, libc::SIGKILL)?;
                    }
                }
                // §5 step 6: remove the directory only after populated=0.
                let deadline = Instant::now() + POPULATED_TIMEOUT;
                while Instant::now() < deadline {
                    let still_populated = match populated(dir) {
                        Ok(p) => p,
                        Err(_) if inner.anchor.removed()? => return Ok(()),
                        Err(e) => return Err(e),
                    };
                    if !still_populated {
                        // Keep restart evidence until the recovered Exec exit commits.
                        if inner.recovered {
                            return Ok(());
                        }
                        // The pinned FD remains the signal/read target even if the
                        // original name is replaced. Never remove its replacement.
                        let Ok(expected) = inner.anchor.identity() else {
                            return Ok(());
                        };
                        if CgroupAnchor::open(&inner.path)
                            .and_then(|a| a.identity())
                            .ok()
                            != Some(expected)
                        {
                            return Ok(());
                        }
                        let mut last_err = None;
                        for _ in 0..3 {
                            match fs::remove_dir(&inner.path) {
                                Ok(()) => {
                                    inner.anchor.mark_removed();
                                    return Ok(());
                                }
                                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                                Err(e) => {
                                    last_err = Some(e);
                                    std::thread::sleep(Duration::from_millis(50));
                                }
                            }
                        }
                        return Err(io::Error::other(format!(
                            "cgroup {} not removable: {}",
                            dir.display(),
                            last_err.map(|e| e.to_string()).unwrap_or_default()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("cgroup {} still populated after force kill", dir.display()),
                ))
            }
        }
    }

    /// 08 §2 양보: 위임 cgroup은 `cpu.weight` 100 → 10(비례 배분이므로
    /// 경합이 없으면 영향이 없다). 위임이 없는 관측 트리 fallback은
    /// `unsupported`다 — per-pid nice는 되돌릴 수 없다(§0-4).
    fn set_scheduling(
        &self,
        group: &GroupHandle,
        tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        if matches!(&group.inner, GroupInner::ObservedTree(_)) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no cgroup delegation; nice cannot be restored without \
                 CAP_SYS_NICE (RLIMIT_NICE)",
            ));
        }
        let inner = cgroup_inner(group)?;
        if inner.anchor.removed()? {
            return Ok(SchedulingOutcome::default());
        }
        let weight = match tier {
            SchedulingTier::Background => "10",
            SchedulingTier::Normal => "100",
        };
        write_verified(&inner.anchor.path(), "cpu.weight", weight)?;
        Ok(SchedulingOutcome {
            applied: 1,
            failed: 0,
            skipped_reused: 0,
        })
    }

    /// 08 §5: cgroup 위임이 있으면 `cgroup.freeze`로, 없으면 관측 트리의
    /// 검증된 멤버에 per-pid SIGSTOP/SIGCONT로 일시정지/재개한다. nice와
    /// 달리 SIGSTOP 되돌리기에 특권이 필요 없어 비위임 경로도 안전하다.
    fn suspend_owned(&self, group: &GroupHandle) -> io::Result<SchedulingOutcome> {
        self.set_suspended(group, true)
    }

    fn resume_owned(&self, group: &GroupHandle) -> io::Result<SchedulingOutcome> {
        self.set_suspended(group, false)
    }

    fn set_suspended(&self, group: &GroupHandle, suspend: bool) -> io::Result<SchedulingOutcome> {
        if let GroupInner::ObservedTree(tree) = &group.inner {
            let (verified, reused, _unverifiable) = self.tree_members(tree)?;
            if !reused.is_empty() {
                tracing::debug!(?reused, "skipped reused pids during suspend");
            }
            return scheduling::set_process_suspend(&verified, suspend);
        }
        let inner = cgroup_inner(group)?;
        if inner.anchor.removed()? {
            return Ok(SchedulingOutcome::default());
        }
        write_verified(
            &inner.anchor.path(),
            "cgroup.freeze",
            if suspend { "1" } else { "0" },
        )?;
        Ok(SchedulingOutcome {
            applied: 1,
            failed: 0,
            skipped_reused: 0,
        })
    }

    fn is_empty(&self, group: &GroupHandle) -> io::Result<bool> {
        if let GroupInner::ObservedTree(tree) = &group.inner {
            return Ok(self.tree_members(tree)?.0.is_empty());
        }
        let inner = cgroup_inner(group)?;
        if inner.anchor.removed()? {
            return Ok(true);
        }
        match populated(&inner.anchor.path()) {
            Ok(populated) => Ok(!populated),
            // Removed after `populated 0` by the Force path: empty by
            // construction, not an error for the second observer.
            Err(_) if inner.anchor.removed()? => Ok(true),
            Err(e) => Err(e),
        }
    }

    /// `memory.events`의 `oom_kill` 누적 카운터(이 워크로드 전용 cgroup
    /// 기준이므로 0보다 크면 이 그룹에서 OOM kill이 일어났다). 파일을
    /// 읽을 수 없는 상황(cgroup already removed 등)은 None — 0이 아니다.
    fn oom_kill_count(&self, group: &GroupHandle) -> Option<u64> {
        let inner = match cgroup_inner(group) {
            Ok(inner) => inner,
            Err(_) => return None,
        };
        let events = std::fs::read_to_string(inner.anchor.path().join("memory.events")).ok()?;
        parse_oom_kill_count(&events)
    }
}

// ---- observed-tree fallback (no delegation) ------------------------------

/// Backend payload for the no-delegation fallback: an observed process tree
/// anchored on the attached helper identity. Clones SHARE the observation
/// state (one `Arc`): the orchestrator files a clone in the workload
/// registry before it attaches the helper to the handle on its own stack.
#[derive(Debug, Default, Clone)]
pub(crate) struct LinuxTreeInner {
    shared: Arc<LinuxTreeShared>,
}

#[derive(Debug, Default)]
pub(crate) struct LinuxTreeShared {
    /// `KEY=VALUE` of the env override whose value is this workload's id
    /// (the daemon stamps `IYAGI_*_ID` into every managed target). A process
    /// carrying it inherited it from our helper's exec chain, so it is ours
    /// even after the root exited and init adopted it — the ppid walk alone
    /// loses orphans that were never recorded while the root lived (root
    /// exits 800 ms in, telemetry samples once a second: B08 turned into a
    /// false SUCCEEDED). Processes that scrub their environment fall back
    /// to the recorded ppid walk (coverage observed_tree/partial, §7).
    env_tag: Option<Vec<u8>>,
    /// Root identity; set when the waiting helper is attached.
    root: Mutex<Option<ProcessIdentity>>,
    /// Verified members (pid → identity). Proven descendants stay recorded
    /// after the root exits: Linux reparents orphans to init, so a plain
    /// ppid walk would lose them while 02-runner §5 keeps the workload
    /// RUNNING until its owned descendants leave (B08).
    recorded: Mutex<BTreeMap<u32, ProcessIdentity>>,
    prev: Mutex<Option<RateSample>>,
}

impl std::ops::Deref for LinuxTreeInner {
    type Target = LinuxTreeShared;

    fn deref(&self) -> &LinuxTreeShared {
        &self.shared
    }
}

fn observed_tree_handle(workload: &WorkloadDescriptor) -> GroupHandle {
    let env_tag = workload
        .env_overrides
        .iter()
        .find(|(_, v)| v.as_str() == workload.workload_id.as_str())
        .map(|(k, v)| format!("{k}={v}").into_bytes());
    GroupHandle {
        workload_id: workload.workload_id.clone(),
        kind: GroupKind::ObservedTree,
        reference: format!("observed-tree:{}", workload.workload_id),
        inner: GroupInner::ObservedTree(LinuxTreeInner {
            shared: Arc::new(LinuxTreeShared {
                env_tag,
                ..LinuxTreeShared::default()
            }),
        }),
    }
}

/// Upper bound on how much of `/proc/<pid>/environ` is inspected.
const ENVIRON_SCAN_CAP: u64 = 256 * 1024;

/// Live pids (from the refreshed inventory) whose environment carries
/// `tag`, skipping pids already observed. Unreadable environs (other users,
/// kernel threads, exited) are simply not ours.
fn env_tagged_pids(system: &System, tag: &[u8], skip: &BTreeSet<u32>) -> Vec<u32> {
    use std::io::Read;
    let mut found = Vec::new();
    let me = std::process::id();
    for pid in system.processes().keys().map(|p| p.as_u32()) {
        if pid == me || skip.contains(&pid) {
            continue;
        }
        let Ok(file) = fs::File::open(format!("/proc/{pid}/environ")) else {
            continue;
        };
        let mut buf = Vec::new();
        if file.take(ENVIRON_SCAN_CAP).read_to_end(&mut buf).is_err() {
            continue;
        }
        if buf.split(|b| *b == 0).any(|entry| entry == tag) {
            found.push(pid);
        }
    }
    found
}

fn tree_root(inner: &LinuxTreeInner) -> io::Result<ProcessIdentity> {
    inner
        .root
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no helper attached yet"))
}

fn tree_attach(inner: &LinuxTreeInner, ident: &ProcessIdentity) -> io::Result<()> {
    // Full triple check immediately before claiming ownership (01 §1).
    let live = identity::process_identity(ident.pid)
        .filter(|live| live.same_process(ident))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity mismatch: process gone or pid reused",
            )
        })?;
    let mut root = inner.root.lock().unwrap_or_else(|p| p.into_inner());
    if root.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tree root already attached",
        ));
    }
    *root = Some(live.clone());
    drop(root);
    inner
        .recorded
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(live.pid, live);
    Ok(())
}

/// PIDs of the observed tree under `root_pid` (root included) in the
/// refreshed inventory; empty when the root is gone.
fn observed_subtree(system: &System, root_pid: u32, out: &mut BTreeSet<u32>) {
    let root = Pid::from_u32(root_pid);
    if system.process(root).is_none() {
        return;
    }
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    out.insert(root_pid);
    let mut queue: VecDeque<Pid> = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for kid in kids {
                if out.insert(kid.as_u32()) {
                    queue.push_back(*kid);
                }
            }
        }
    }
}

/// Anchors of the observed tree: the root while its identity still matches,
/// plus every recorded member whose identity still matches (orphans that
/// outlived the root and whatever they spawned since). A pid that now
/// belongs to another process anchors nothing and is never signalled (§7).
fn tree_anchors(inner: &LinuxTreeInner, root: &ProcessIdentity) -> Vec<u32> {
    let recorded = inner.recorded.lock().unwrap_or_else(|p| p.into_inner());
    let mut anchors = Vec::new();
    for (pid, ident) in recorded.iter() {
        let still_ours =
            identity::process_identity(*pid).is_some_and(|live| live.same_process(ident));
        if still_ours {
            anchors.push(*pid);
        }
    }
    if !anchors.contains(&root.pid)
        && identity::process_identity(root.pid).is_some_and(|live| live.same_process(root))
    {
        anchors.push(root.pid);
    }
    anchors
}

/// Verify observed pids against identity probes and the recorded map.
/// Returns `(verified, reused, gone)`; new (unrecorded) pids are verified by
/// tree membership alone and get recorded. Exited members leave the map
/// (bounded growth); the root row stays as the PID-reuse anchor.
fn tree_verify(
    inner: &LinuxTreeInner,
    root_pid: u32,
    observed: &BTreeSet<u32>,
) -> (Vec<ProcessIdentity>, Vec<u32>, usize) {
    let mut recorded = inner.recorded.lock().unwrap_or_else(|p| p.into_inner());
    let mut verified = Vec::new();
    let mut reused = Vec::new();
    let mut gone = 0usize;
    for &pid in observed {
        match identity::process_identity(pid) {
            Some(current) => {
                let matches_record = recorded
                    .get(&pid)
                    .is_none_or(|prev| prev.same_process(&current));
                if matches_record {
                    recorded.insert(pid, current.clone());
                    verified.push(current);
                } else {
                    reused.push(pid);
                }
            }
            None => gone += 1,
        }
    }
    recorded.retain(|pid, _| observed.contains(pid) || *pid == root_pid);
    (verified, reused, gone)
}

/// Observed pids of the workload: ppid walks from every identity anchor,
/// plus env-tagged processes (and their subtrees).
fn observe_workload(
    system: &System,
    inner: &LinuxTreeInner,
    root: &ProcessIdentity,
) -> BTreeSet<u32> {
    let mut observed = BTreeSet::new();
    for anchor in tree_anchors(inner, root) {
        observed_subtree(system, anchor, &mut observed);
    }
    if let Some(tag) = inner.env_tag.as_deref() {
        for pid in env_tagged_pids(system, tag, &observed) {
            observed_subtree(system, pid, &mut observed);
        }
    }
    observed
}

impl LinuxCgroupPlatform {
    /// Refresh + walk + verify in one pass: `(verified, reused, gone)`.
    fn tree_members(
        &self,
        inner: &LinuxTreeInner,
    ) -> io::Result<(Vec<ProcessIdentity>, Vec<u32>, usize)> {
        let root = tree_root(inner)?;
        let system = self.refresh();
        let observed = observe_workload(&system, inner, &root);
        drop(system);
        Ok(tree_verify(inner, root.pid, &observed))
    }

    fn tree_sample(
        &self,
        group: &GroupHandle,
        inner: &LinuxTreeInner,
        now_ms: u64,
    ) -> io::Result<WorkloadUsage> {
        let root = tree_root(inner)?;
        let system = self.refresh();
        let observed = observe_workload(&system, inner, &root);
        let mut resident = 0u64;
        let mut cpu_ms = 0u64;
        let mut read_bytes = 0u64;
        let mut write_bytes = 0u64;
        for &pid in &observed {
            if let Some(process) = system.process(Pid::from_u32(pid)) {
                resident = resident.saturating_add(process.memory());
                cpu_ms = cpu_ms.saturating_add(process.accumulated_cpu_time());
                let disk = process.disk_usage();
                read_bytes = read_bytes.saturating_add(disk.total_read_bytes);
                write_bytes = write_bytes.saturating_add(disk.total_written_bytes);
            }
        }
        drop(system);
        let (verified, reused, gone) = tree_verify(inner, root.pid, &observed);

        let cur = RateSample {
            now_ms,
            cpu_time_us: cpu_ms.saturating_mul(1_000),
            read_bytes,
            write_bytes,
        };
        let mut prev = inner.prev.lock().unwrap_or_else(|p| p.into_inner());
        let first = |reason: &'static str| Metric::<f64>::unavailable(TREE_SOURCE, reason);
        let (cpu_cores, read_rate, write_rate) = match prev.as_ref() {
            Some(p) => match rate_deltas(p, &cur) {
                Some((cores, read, write)) => {
                    *prev = Some(cur);
                    (
                        Metric::measured(TREE_SOURCE, cores),
                        Metric::measured(TREE_SOURCE, read),
                        Metric::measured(TREE_SOURCE, write),
                    )
                }
                None => (
                    first("zero elapsed time since previous sample"),
                    first("zero elapsed time since previous sample"),
                    first("zero elapsed time since previous sample"),
                ),
            },
            None => {
                *prev = Some(cur);
                (
                    first("first differential sample (03 §2)"),
                    first("first differential sample (03 §2)"),
                    first("first differential sample (03 §2)"),
                )
            }
        };
        drop(prev);

        // Observation gaps downgrade coverage below observed_tree (§7); it
        // is never reported as `group`.
        let coverage = if !reused.is_empty() || gone > 0 {
            UsageCoverage::Partial
        } else {
            UsageCoverage::ObservedTree
        };
        let unavail_f = |reason: &str| Metric::<f64>::unavailable(TREE_SOURCE, reason);
        let unavail_b = |reason: &str| {
            Metric::<term_contracts::ids::U64String>::unavailable(TREE_SOURCE, reason)
        };
        Ok(WorkloadUsage {
            workload_id: group.workload_id.clone(),
            cpu_cores,
            resident_bytes: match term_contracts::ids::U64String::new(resident) {
                Ok(v) => Metric::estimated("sysinfo.rss", v),
                Err(_) => Metric::unavailable("sysinfo.rss", "value out of persisted range"),
            },
            accounted_bytes: unavail_b("cgroup accounting needs a delegated subtree (03 §5)"),
            committed_bytes: unavail_b("commit accounting is windows job-only"),
            read_bytes_per_sec: read_rate,
            write_bytes_per_sec: write_rate,
            network_rx_bytes_per_sec: unavail_f("per-process networking unavailable in R1"),
            network_tx_bytes_per_sec: unavail_f("per-process networking unavailable in R1"),
            process_count: Metric::measured(TREE_SOURCE, verified.len() as u32),
            coverage,
        })
    }

    fn tree_terminate(&self, inner: &LinuxTreeInner, phase: StopPhase) -> io::Result<()> {
        let (verified, reused, _gone) = self.tree_members(inner)?;
        let sig = match phase {
            StopPhase::Grace => libc::SIGTERM,
            StopPhase::Force => libc::SIGKILL,
        };
        for ident in &verified {
            // The batch verification happened a moment ago; each signal is
            // pinned to the verified identity (pidfd) so a member that
            // exited in between can never redirect it to a pid-reuse
            // successor (§7).
            tree_signal_member(ident, sig);
        }
        if !reused.is_empty() {
            tracing::debug!(?reused, "skipped reused pids during terminate");
        }
        Ok(())
    }
}

/// Metric source label for the observed-tree fallback.
const TREE_SOURCE: &str = "linux-observed-tree";

/// `memory.events` 내용에서 `oom_kill <n>` 값 추출(단위 시험 가능한 순수 함수).
fn parse_oom_kill_count(events: &str) -> Option<u64> {
    events
        .lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            match (tokens.next(), tokens.next()) {
                (Some("oom_kill"), Some(value)) => value.parse::<u64>().ok(),
                _ => None,
            }
        })
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::ResourcePlatform;

    #[test]
    fn parses_oom_kill_counter_from_memory_events() {
        let events = "low 0\nhigh 12\nmax 0\noom 3\noom_kill 1\n";
        assert_eq!(parse_oom_kill_count(events), Some(1));
        assert_eq!(parse_oom_kill_count("oom 7\n"), None);
        assert_eq!(parse_oom_kill_count(""), None);
        // oom(시도)과 oom_kill(실제 kill)은 다른 키다.
        assert_eq!(parse_oom_kill_count("oom 9\n"), None);
    }

    #[test]
    fn cpu_quota_for_math_and_minimum() {
        assert_eq!(cpu_quota_for(1.0).unwrap(), 100_000);
        assert_eq!(cpu_quota_for(2.5).unwrap(), 250_000);
        assert_eq!(cpu_quota_for(0.5).unwrap(), 50_000);
        assert_eq!(cpu_quota_for(0.01).unwrap(), 1_000);
        // Below the kernel minimum for a 100 ms period.
        assert!(cpu_quota_for(0.009).is_err());
        assert!(cpu_quota_for(0.0).is_err());
        assert!(cpu_quota_for(-1.0).is_err());
        assert!(cpu_quota_for(f64::NAN).is_err());
    }

    #[test]
    fn populated_parsing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(populated(dir.path()).is_err(), "no cgroup.events");
        std::fs::write(dir.path().join("cgroup.events"), "populated 0\nfrozen 0\n").unwrap();
        assert!(!populated(dir.path()).unwrap());
        std::fs::write(dir.path().join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
        assert!(populated(dir.path()).unwrap());
    }

    #[test]
    fn cpu_and_io_stat_parsing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("cpu.stat"),
            "usage_usec 123456\nnr_periods 1\n",
        )
        .unwrap();
        assert_eq!(cpu_usage_usec(dir.path()), Some(123_456));
        std::fs::write(
            dir.path().join("io.stat"),
            "8:0 rbytes=100 wbytes=40 dbytes=0\n8:16 rbytes=22 wbytes=2\n",
        )
        .unwrap();
        assert_eq!(io_bytes(dir.path()), Some((122, 42)));
        std::fs::remove_file(dir.path().join("io.stat")).unwrap();
        assert_eq!(io_bytes(dir.path()), None);
    }

    #[test]
    fn statm_resident_estimate_handles_missing_procs() {
        let (total, unreadable) = resident_estimate(&[]);
        assert_eq!((total, unreadable), (0, 0));
        // A certainly-nonexistent pid counts as unreadable, never as 0-valued.
        let (total, unreadable) = resident_estimate(&[u32::MAX - 4]);
        assert_eq!(total, 0);
        assert_eq!(unreadable, 1);
    }

    /// Full delegation path; self-skips when the host grants no delegation
    /// (e.g. CI containers with a read-only /sys/fs/cgroup) unless
    /// `IYAGI_CGROUP_REQUIRE_DELEGATION=1` (the delegated CI runner sets it so
    /// a silently skipping job fails instead of passing vacuously).
    #[test]
    fn cgroup_lifecycle_when_delegated() {
        let platform = LinuxCgroupPlatform::new();
        let Some(_d) = platform.delegated().cloned() else {
            if std::env::var_os("IYAGI_CGROUP_REQUIRE_DELEGATION").is_some_and(|v| v == "1") {
                panic!(
                    "IYAGI_CGROUP_REQUIRE_DELEGATION=1 but no writable cgroup v2 delegation: {:?}",
                    platform.probe().as_ref().err()
                );
            }
            eprintln!("no cgroup v2 delegation on this host — skipping integration part");
            let caps = platform.capabilities();
            assert_ne!(caps.memory_limit_kind.support, LimitSupport::Supported);
            // No delegation is not a launch failure: observe/prefer fall
            // back to the observed process tree (03 §5).
            let wl = crate::group::testutil::test_workload(Some(1 << 30), None, None);
            let group = platform.create_group(&wl).expect("observed-tree fallback");
            assert_eq!(group.kind, GroupKind::ObservedTree);
            return;
        };
        let caps = platform.capabilities();
        assert_eq!(caps.platform, "linux-cgroup.v2");
        assert_eq!(caps.memory_limit_kind.support, LimitSupport::Supported);

        let wl = crate::group::testutil::test_workload(Some(512 << 20), Some(1.0), Some(64));
        let group = platform.create_group(&wl).expect("create group");
        assert!(group.reference.contains("workloads"));
        // The limit files only exist when `workloads/` enabled the
        // controllers for its children; read them back (§5 step 2).
        let dir = Path::new(&group.reference);
        assert_eq!(
            read_trimmed(&dir.join("memory.max")).unwrap(),
            (512u64 << 20).to_string()
        );
        assert_eq!(read_trimmed(&dir.join("cpu.max")).unwrap(), "100000 100000");
        assert_eq!(read_trimmed(&dir.join("pids.max")).unwrap(), "64");

        let mut child = crate::group::testutil::spawn_lived_child();
        let ident = identity::process_identity(child.id()).expect("child identity");
        platform.attach_pid(&group, &ident).expect("attach");
        assert!(!platform.is_empty(&group).expect("not empty"));
        assert!(platform
            .member_identities(&group)
            .expect("members")
            .iter()
            .any(|m| m.pid == child.id()));

        let usage = platform
            .sample_group(&group, crate::group::testutil::mono_ms())
            .unwrap();
        assert!(usage.process_count.value >= Some(1));
        assert!(usage.accounted_bytes.value.is_some());

        platform
            .terminate_owned(&group, StopPhase::Grace)
            .expect("term");
        platform
            .terminate_owned(&group, StopPhase::Force)
            .expect("force");
        assert!(crate::group::testutil::poll_until(
            Duration::from_secs(5),
            || platform.is_empty(&group).unwrap_or(false)
        ));
        let _ = child.wait();
    }

    fn tree_only_platform() -> LinuxCgroupPlatform {
        LinuxCgroupPlatform {
            delegation: OnceLock::from(Err(NoDelegation::NoWritableSubtree)),
            system: Mutex::new(System::new()),
        }
    }

    fn recovery_platform() -> Option<LinuxCgroupPlatform> {
        let platform = LinuxCgroupPlatform::new();
        if platform.delegated().is_none() {
            assert_ne!(
                std::env::var("IYAGI_CGROUP_REQUIRE_DELEGATION").as_deref(),
                Ok("1"),
                "native recovery tests require real delegated cgroups"
            );
            return None;
        }
        Some(platform)
    }

    #[test]
    fn recovered_cgroup_stops_nested_members_and_retains_evidence_until_retired() {
        let Some(platform) = recovery_platform() else {
            return;
        };
        let workload = crate::group::testutil::test_workload(None, None, None);
        let group = platform.create_group(&workload).unwrap();
        let token = platform.recovery_identity(&group).unwrap().unwrap();
        let nested = Path::new(&group.reference).join("nested");
        fs::create_dir(&nested).unwrap();
        let mut member = crate::group::testutil::spawn_lived_child();
        let mut unrelated = crate::group::testutil::spawn_lived_child();
        fs::write(nested.join("cgroup.procs"), member.id().to_string()).unwrap();
        let reference = group.reference.clone();
        drop(group);
        drop(platform);
        let replacement = LinuxCgroupPlatform::new();
        let recovered = replacement
            .recover_group(&workload.workload_id, &reference, &token)
            .unwrap();
        assert!(!replacement.is_empty(&recovered).unwrap());
        replacement
            .terminate_owned(&recovered, StopPhase::Force)
            .unwrap();
        assert!(replacement.is_empty(&recovered).unwrap());
        member.wait().unwrap();
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "unrelated process was signalled"
        );
        assert!(
            Path::new(&reference).is_dir(),
            "exit evidence must survive DB failure"
        );
        replacement.retire_recovered_group(&recovered).unwrap();
        assert!(!nested.exists());
        assert!(replacement.is_empty(&recovered).unwrap());
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
    }

    #[test]
    fn recovery_rejects_reused_names_wrong_boot_and_wrong_kernel_generation() {
        let Some(platform) = recovery_platform() else {
            return;
        };
        let workload = crate::group::testutil::test_workload(None, None, None);
        let original = platform.create_group(&workload).unwrap();
        let token = platform.recovery_identity(&original).unwrap().unwrap();
        let pinned = platform
            .recover_group(&workload.workload_id, &original.reference, &token)
            .unwrap();
        for changed in [
            GroupRecoveryIdentity::CgroupV2 {
                boot_id: "previous-boot".into(),
                kernel_id: match &token {
                    GroupRecoveryIdentity::CgroupV2 { kernel_id, .. } => kernel_id.clone(),
                    _ => unreachable!(),
                },
            },
            GroupRecoveryIdentity::CgroupV2 {
                boot_id: identity::boot_id(),
                kernel_id: "0000000000000000".into(),
            },
        ] {
            assert!(platform
                .recover_group(&workload.workload_id, &original.reference, &changed)
                .is_err());
        }
        platform
            .terminate_owned(&original, StopPhase::Force)
            .unwrap();
        assert!(platform
            .recover_group(&workload.workload_id, &original.reference, &token)
            .is_err());
        let replacement = platform.create_group(&workload).unwrap();
        let mut child = crate::group::testutil::spawn_lived_child();
        platform
            .attach_pid(
                &replacement,
                &identity::process_identity(child.id()).unwrap(),
            )
            .unwrap();
        assert_ne!(
            platform.recovery_identity(&replacement).unwrap().as_ref(),
            Some(&token)
        );
        assert!(platform
            .recover_group(&workload.workload_id, &replacement.reference, &token)
            .is_err());
        // A previously opened handle still addresses the deleted group. It
        // cannot kill the process occupying the reused pathname.
        let _ = platform.terminate_owned(&pinned, StopPhase::Force);
        assert!(child.try_wait().unwrap().is_none());
        assert!(!platform.is_empty(&replacement).unwrap());
        platform
            .terminate_owned(&replacement, StopPhase::Force)
            .unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn recovery_rejects_regular_directories_and_symlinks() {
        let Some(platform) = recovery_platform() else {
            return;
        };
        let workload = crate::group::testutil::test_workload(None, None, None);
        let group = platform.create_group(&workload).unwrap();
        let token = platform.recovery_identity(&group).unwrap().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let name = tmp
            .path()
            .join("workloads")
            .join(workload.workload_id.as_str());
        fs::create_dir_all(&name).unwrap();
        assert!(platform
            .recover_group(&workload.workload_id, name.to_str().unwrap(), &token)
            .is_err());
        fs::remove_dir(&name).unwrap();
        std::os::unix::fs::symlink(&group.reference, &name).unwrap();
        assert!(platform
            .recover_group(&workload.workload_id, name.to_str().unwrap(), &token)
            .is_err());
        platform.terminate_owned(&group, StopPhase::Force).unwrap();
    }

    /// No delegation: launches get an observed-tree group whose members are
    /// found by identity-verified ppid walks and signalled on stop.
    #[test]
    fn observed_tree_fallback_lifecycle() {
        let platform = tree_only_platform();
        let caps = platform.capabilities();
        assert_eq!(
            caps.memory_limit_kind.support,
            LimitSupport::PermissionRequired
        );
        assert_eq!(caps.tree_accounting.support, LimitSupport::Supported);
        let wl = crate::group::testutil::test_workload(Some(1 << 30), Some(1.0), None);
        let group = platform.create_group(&wl).expect("fallback group");
        assert_eq!(group.kind, GroupKind::ObservedTree);
        assert!(group.reference.starts_with("observed-tree:"));

        let mut child = crate::group::testutil::spawn_lived_child();
        let ident = identity::process_identity(child.id()).expect("child identity");
        platform.attach_pid(&group, &ident).expect("attach");
        assert!(
            platform.attach_pid(&group, &ident).is_err(),
            "root attaches once"
        );
        assert!(!platform.is_empty(&group).expect("not empty"));
        assert!(platform
            .member_identities(&group)
            .expect("members")
            .iter()
            .any(|m| m.pid == child.id()));
        let usage = platform
            .sample_group(&group, crate::group::testutil::mono_ms())
            .expect("sample");
        assert_eq!(usage.coverage, UsageCoverage::ObservedTree);
        assert_eq!(usage.process_count.value, Some(1));
        assert!(
            usage.accounted_bytes.value.is_none(),
            "no cgroup accounting without delegation"
        );

        platform
            .terminate_owned(&group, StopPhase::Force)
            .expect("force");
        assert!(crate::group::testutil::poll_until(
            Duration::from_secs(5),
            || {
                let _ = child.try_wait();
                platform.is_empty(&group).unwrap_or(false)
            }
        ));
        let _ = child.wait();
    }

    /// B08 on the fallback: the root exits while a proven descendant lives
    /// on (reparented to init); the group stays populated until stop.
    #[test]
    fn observed_tree_keeps_proven_descendants_after_root_exit() {
        let platform = tree_only_platform();
        let wl = crate::group::testutil::test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("fallback group");
        let mut sh = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30 & sleep 0.5"])
            .spawn()
            .expect("spawn sh");
        let ident = identity::process_identity(sh.id()).expect("sh identity");
        platform.attach_pid(&group, &ident).expect("attach");
        // Record the background sleep while the root is still alive.
        assert!(crate::group::testutil::poll_until(
            Duration::from_secs(3),
            || {
                platform
                    .member_identities(&group)
                    .map(|m| m.len() >= 2)
                    .unwrap_or(false)
            }
        ));
        let _ = sh.wait(); // root gone, orphan reparented away from us
        let members = platform.member_identities(&group).expect("members");
        assert_eq!(members.len(), 1, "orphan stays owned: {members:?}");
        assert_ne!(members[0].pid, sh.id());
        assert!(!platform.is_empty(&group).expect("populated"));
        // A recycled root pid is never trusted: the anchor is the identity.
        platform
            .terminate_owned(&group, StopPhase::Force)
            .expect("force");
        assert!(crate::group::testutil::poll_until(
            Duration::from_secs(5),
            || { platform.is_empty(&group).unwrap_or(false) }
        ));
    }

    /// Orphans that were never recorded while the root lived are still
    /// found through the workload id the daemon stamps into the target's
    /// environment (B08 on the fallback: root exits early, children hold).
    #[test]
    fn observed_tree_finds_env_tagged_orphans_never_seen_while_root_lived() {
        let platform = tree_only_platform();
        let mut wl = crate::group::testutil::test_workload(None, None, None);
        wl.env_overrides.insert(
            "IYAGI_TEST_WORKLOAD_ID".into(),
            wl.workload_id.as_str().to_string(),
        );
        let group = platform.create_group(&wl).expect("fallback group");
        let mut sh = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 30 & exit 0"])
            .env("IYAGI_TEST_WORKLOAD_ID", wl.workload_id.as_str())
            .spawn()
            .expect("spawn sh");
        let ident = identity::process_identity(sh.id()).expect("sh identity");
        platform.attach_pid(&group, &ident).expect("attach");
        // No observation pass while the root is alive.
        let _ = sh.wait();
        let members = platform.member_identities(&group).expect("members");
        assert_eq!(
            members.len(),
            1,
            "env-tagged orphan must be owned: {members:?}"
        );
        assert_ne!(members[0].pid, sh.id());
        assert!(!platform.is_empty(&group).expect("populated"));
        platform
            .terminate_owned(&group, StopPhase::Force)
            .expect("force");
        assert!(crate::group::testutil::poll_until(
            Duration::from_secs(5),
            || { platform.is_empty(&group).unwrap_or(false) }
        ));
    }

    /// The observed-tree stop never signals a bare batch-verified pid: a pid
    /// that now belongs to a different process fails the pinned re-verify
    /// and is skipped, and a dead pid is a quiet no-op.
    #[test]
    fn tree_signal_member_never_signals_a_reused_pid() {
        let mut stranger = crate::group::testutil::spawn_lived_child();
        let live = identity::process_identity(stranger.id()).expect("stranger identity");
        let stale = ProcessIdentity {
            pid: live.pid,
            start_token: format!("{}-stale", live.start_token),
            boot_id: live.boot_id.clone(),
        };
        tree_signal_member(&stale, libc::SIGKILL);
        // Certainly-nonexistent pid: quiet no-op on every path.
        tree_signal_member(
            &ProcessIdentity {
                pid: u32::MAX - 4,
                start_token: "1".into(),
                boot_id: identity::boot_id(),
            },
            libc::SIGKILL,
        );
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            stranger.try_wait().expect("try_wait").is_none(),
            "a pid whose identity does not match is never signalled"
        );
        let _ = stranger.kill();
        let _ = stranger.wait();
    }
}
