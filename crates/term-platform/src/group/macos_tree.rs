//! Ticket I06 — macOS observed-tree backend (spec `03-resources.md` §7).
//! Only compiled on macOS targets.
//!
//! macOS R1 has no per-workload OS resource group, so ownership is
//! *observed*: the root process identity (PID + start time + boot id) plus a
//! PPID walk over sysinfo's process inventory, refreshed per sample. Coverage
//! is `observed_tree` (never `group`): descendants that detach into their own
//! session can escape observation (§7). Memory hard caps and CPU quotas are
//! reported unsupported — admission and memory-pressure observation are the
//! control mechanisms (§7). A root-only `setrlimit` is never presented as a
//! tree-wide memory cap.
//!
//! Termination targets *verified identities only* (§7): TERM → caller's grace
//! window → KILL, and a signal is never delivered to a PID whose recorded
//! identity no longer matches (PID reuse).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use term_contracts::ids::ProcessIdentity;
use term_contracts::metrics::{Metric, UsageCoverage, WorkloadUsage};
use term_contracts::snapshot::Capabilities;

use super::{
    rate_deltas, scheduling, GroupHandle, GroupInner, RateSample, SchedulingOutcome,
    SchedulingTier, StopPhase,
};
use crate::identity;

const SOURCE: &str = "sysinfo.tree";

/// Observed-tree backend for macOS. Holds the single daemon-wide sysinfo
/// `System` instance (spec `03-resources.md` §2: one instance, refreshed
/// incrementally).
pub struct MacosTreePlatform {
    system: Mutex<System>,
}

impl MacosTreePlatform {
    pub fn new() -> Self {
        Self {
            system: Mutex::new(System::new()),
        }
    }

    fn refresh(&self) -> io::Result<std::sync::MutexGuard<'_, System>> {
        // A panic inside the lock must not disable observation for the
        // daemon's lifetime (owned_alive → 0, cancel unable to signal).
        let mut system = self.system.lock().unwrap_or_else(|p| p.into_inner());
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu() // usage + accumulated time
                .with_memory()
                .with_disk_usage(),
        );
        Ok(system)
    }
}

impl Default for MacosTreePlatform {
    fn default() -> Self {
        Self::new()
    }
}

/// Backend payload carried by a [`GroupHandle`] on macOS. Clones SHARE the
/// observation state (one `Arc`): the orchestrator files a clone in the
/// workload registry *before* it attaches the waiting helper to the handle
/// it keeps on its own stack. A deep copy therefore left every registry-side
/// consumer — `workload.processes`, the telemetry loop's
/// `member_identities`, cancel's `terminate_owned`, `is_empty` — with
/// `root == None`, i.e. "no helper attached yet" for the workload's whole
/// life on macOS.
#[derive(Debug, Default, Clone)]
pub(crate) struct MacGroupInner {
    shared: Arc<MacGroupShared>,
}

#[derive(Debug, Default)]
pub(crate) struct MacGroupShared {
    /// Root identity; set when the waiting helper is attached.
    root: Mutex<Option<ProcessIdentity>>,
    /// Last verified member identities (pid → identity), refreshed on every
    /// observation; PID-reuse detection at termination reads this.
    recorded: Mutex<BTreeMap<u32, ProcessIdentity>>,
    prev: Mutex<Option<RateSample>>,
}

impl std::ops::Deref for MacGroupInner {
    type Target = MacGroupShared;

    fn deref(&self) -> &MacGroupShared {
        &self.shared
    }
}

fn tree_inner(group: &GroupHandle) -> io::Result<&MacGroupInner> {
    match &group.inner {
        GroupInner::Tree(t) => Ok(t),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a macos observed-tree group handle",
        )),
    }
}

fn root_of(inner: &MacGroupInner) -> io::Result<ProcessIdentity> {
    inner
        .root
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no helper attached yet"))
}

/// PIDs of the observed tree (root included) under the refreshed inventory.
fn observed_tree(system: &System, root_pid: u32) -> Vec<u32> {
    let root = Pid::from_u32(root_pid);
    if system.process(root).is_none() {
        return Vec::new(); // root gone
    }
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut seen = std::collections::HashSet::from([root]);
    let mut out = vec![root_pid];
    let mut queue: VecDeque<Pid> = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        if let Some(kids) = children.get(&pid) {
            for kid in kids {
                if !seen.insert(*kid) {
                    continue;
                }
                queue.push_back(*kid);
                out.push(kid.as_u32());
            }
        }
    }
    out
}

/// Observe the root and previously verified descendants whose identities
/// still match. A reused PID anchors no tree; a surviving known descendant
/// remains owned even after reparenting. A member whose identity is
/// unreadable (EPERM on a setuid-both descendant) stays as an unverifiable
/// anchor — it never anchors a subtree and never aborts the scan. Unobserved
/// escapes remain outside this backend's coverage (§7).
fn anchored_tree(
    system: &System,
    inner: &MacGroupInner,
    root: &ProcessIdentity,
) -> io::Result<Vec<u32>> {
    // Proven descendants can outlive or detach from their parent. Keep
    // observing them (and their children) while the original identity
    // matches; a reused root PID must never anchor an unrelated tree.
    let recorded = inner.recorded.lock().unwrap_or_else(|p| p.into_inner());
    let mut anchors: Vec<_> = recorded.values().cloned().collect();
    if !anchors.iter().any(|ident| ident.pid == root.pid) {
        anchors.push(root.clone());
    }
    drop(recorded);
    let mut observed = std::collections::BTreeSet::new();
    for anchor in anchors {
        match identity::process_identity_checked(anchor.pid) {
            Ok(Some(live)) if live.same_process(&anchor) => {
                // a sysinfo omission cannot erase a verified live anchor
                observed.insert(anchor.pid);
                observed.extend(observed_tree(system, anchor.pid));
            }
            Ok(_) => {} // gone (ESRCH/zombie): anchors nothing
            Err(_) => {
                // Unreadable ownership — proc_pidinfo(PROC_PIDTBSDINFO)
                // returns EPERM when a member's ruid AND euid both differ
                // (sudo/setuid-both descendant). Propagating the error here
                // used to fail `stop`/`is_empty` for the ENTIRE workload.
                // Mirror the Linux observed-tree convention (identity ->
                // Option, linux_cgroup tree_verify): the member stays in the
                // tree as an unverifiable anchor — remembered, never
                // signalled, coverage downgraded to partial — and one
                // unreadable member never aborts the group scan.
                observed.insert(anchor.pid);
                // No subtree walk: the pid cannot be re-verified, so any
                // observed children could belong to a reuse successor.
            }
        }
    }
    Ok(observed.into_iter().collect())
}

/// Verify observed pids against identity probes and the recorded map.
/// Returns `(verified, reused, gone, unverifiable)`; new (unrecorded) pids
/// are verified by tree membership alone and get recorded. `unverifiable`
/// counts observed members whose identity is currently unreadable (EPERM on
/// a setuid-both descendant): they stay recorded, are never verified or
/// signalled, and one of them must not fail the whole scan (Linux mirror).
/// They are not `gone` either: an unreadable probe is not evidence of exit
/// (`identity::process_identity_checked`), so they keep the group non-empty.
///
/// The recorded map is pruned to the observed set on every call: this runs
/// twice a second per running managed workload, and a long build that forks
/// thousands of short-lived children would otherwise grow the map forever.
/// The root row is never pruned: after the root exits it is the only anchor
/// that lets `anchored_tree` recognise a later reuse of the root pid.
fn verify_members(
    inner: &MacGroupInner,
    root_pid: u32,
    observed: &[u32],
) -> io::Result<(Vec<ProcessIdentity>, Vec<u32>, usize, usize)> {
    let mut recorded = inner.recorded.lock().unwrap_or_else(|p| p.into_inner());
    let mut verified = Vec::new();
    let mut reused = Vec::new();
    let mut gone = 0usize;
    let mut unverifiable = 0usize;
    for &pid in observed {
        match identity::process_identity_checked(pid) {
            Ok(Some(current)) => {
                let matches_record = recorded
                    .get(&pid)
                    .is_none_or(|prev| prev.same_process(&current));
                if matches_record {
                    recorded.insert(pid, current.clone());
                    verified.push(current);
                } else {
                    // PID now belongs to a different process: never signal.
                    reused.push(pid);
                }
            }
            Ok(None) => gone += 1,
            Err(_) => {
                // Unreadable identity (non-ESRCH): non-fatal, mirroring the
                // Linux observed tree — count as unverifiable (downgrades
                // coverage to partial) instead of failing `stop` for the
                // whole workload. The recorded row (pid is observed)
                // survives so the member stays an anchor, and the member
                // still counts as present: it may be alive, so emptiness
                // must never be inferred from it.
                unverifiable += 1;
            }
        }
    }
    // Exited members leave the map with them (reused pids stay recorded so
    // the mismatch keeps being detected while they are still observed); the
    // root row stays as the PID-reuse anchor.
    let live: std::collections::BTreeSet<u32> = observed.iter().copied().collect();
    recorded.retain(|pid, _| live.contains(pid) || *pid == root_pid);
    Ok((verified, reused, gone, unverifiable))
}

/// Signal `ident`'s pid only when its identity still matches *immediately
/// before each individual kill* (§7). macOS has no pidfd, so a batch
/// verify-then-loop-kill leaves the whole loop's duration as a PID-reuse
/// window; a per-member check narrows it to the signal call itself. A pid
/// that is gone or now unreadable is skipped — never signalled on the bare
/// pid alone.
fn signal_verified(ident: &ProcessIdentity, sig: i32) {
    if identity::process_identity_checked(ident.pid)
        .is_ok_and(|probe| probe.is_some_and(|live| live.same_process(ident)))
    {
        // Verified a moment ago; ESRCH races with exit are not errors.
        let _ = unsafe { libc::kill(ident.pid as libc::pid_t, sig) };
    }
}

impl super::ResourcePlatform for MacosTreePlatform {
    fn needs_observer_guardian(&self) -> bool {
        true
    }
    fn verify_recovered_root(&self, group: &GroupHandle, root: &ProcessIdentity) -> io::Result<()> {
        super::macos_guardian::verify_root(group, root)
    }
    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::observe_only("macos-observed-tree");
        caps.tree_accounting.reason = Some(
            "observed tree; detached descendants may escape (coverage observed_tree/partial)"
                .into(),
        );
        caps.reattach.reason =
            Some("session persists in the daemon; UI re-attach supported".into());
        caps.scheduling_yield.reason =
            Some("PRIO_DARWIN_BG per verified observed member; restore re-walks the tree".into());
        caps
    }

    fn create_group(
        &self,
        workload: &term_contracts::workload::WorkloadDescriptor,
    ) -> io::Result<GroupHandle> {
        Ok(GroupHandle {
            workload_id: workload.workload_id.clone(),
            kind: term_contracts::workload::GroupKind::ObservedTree,
            reference: format!("observed-tree:{}", workload.workload_id),
            inner: GroupInner::Tree(MacGroupInner::default()),
        })
    }

    fn recovery_identity(
        &self,
        group: &GroupHandle,
    ) -> io::Result<Option<term_contracts::workload::GroupRecoveryIdentity>> {
        Ok(super::macos_guardian::recovery_identity(group))
    }
    fn recover_group(
        &self,
        workload: &term_contracts::ids::WorkloadId,
        reference: &str,
        expected: &term_contracts::workload::GroupRecoveryIdentity,
    ) -> io::Result<GroupHandle> {
        match expected {
            term_contracts::workload::GroupRecoveryIdentity::MacosGuardian {
                guardian,
                endpoint,
            } if reference == endpoint => {
                super::macos_guardian::connect_group(workload, endpoint, guardian)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "not a matching macOS guardian identity",
            )),
        }
    }
    fn retire_recovered_group(&self, group: &GroupHandle) -> io::Result<()> {
        if matches!(&group.inner, GroupInner::Guardian(_)) {
            return super::macos_guardian::retire(group);
        }
        Ok(())
    }

    fn attach_pid(&self, group: &GroupHandle, ident: &ProcessIdentity) -> io::Result<()> {
        if matches!(&group.inner, GroupInner::Guardian(_)) {
            return super::macos_guardian::attach(group, ident);
        }
        let inner = tree_inner(group)?;
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

    fn sample_group(&self, group: &GroupHandle, now_ms: u64) -> io::Result<WorkloadUsage> {
        if matches!(&group.inner, GroupInner::Guardian(_)) {
            return super::macos_guardian::sample(group, now_ms);
        }
        let inner = tree_inner(group)?;
        let root = root_of(inner)?;
        let system = self.refresh()?;
        let observed = anchored_tree(&system, inner, &root)?;

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
        let (verified, reused, gone, unverifiable) = verify_members(inner, root.pid, &observed)?;
        drop(system);

        let cur = RateSample {
            now_ms,
            cpu_time_us: cpu_ms.saturating_mul(1_000),
            read_bytes,
            write_bytes,
        };
        let mut prev = inner.prev.lock().unwrap_or_else(|p| p.into_inner());
        let first = |reason: &'static str| Metric::<f64>::unavailable(SOURCE, reason);
        let (cpu_cores, read_rate, write_rate) = match prev.as_ref() {
            Some(p) => match rate_deltas(p, &cur) {
                Some((cores, read, write)) => {
                    *prev = Some(cur);
                    (
                        Metric::measured(SOURCE, cores),
                        Metric::measured(SOURCE, read),
                        Metric::measured(SOURCE, write),
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

        // Observation gaps downgrade coverage below the honest ceiling of
        // observed_tree (§7); it is never reported as `group`.
        let coverage = if !reused.is_empty() || gone > 0 || unverifiable > 0 {
            UsageCoverage::Partial
        } else {
            UsageCoverage::ObservedTree
        };
        let unavail_f = |reason: &str| Metric::<f64>::unavailable(SOURCE, reason);
        let unavail_b =
            |reason: &str| Metric::<term_contracts::ids::U64String>::unavailable(SOURCE, reason);

        Ok(WorkloadUsage {
            workload_id: group.workload_id.clone(),
            cpu_cores,
            // Sum of member RSS via sysinfo: estimated (shared pages, §2).
            resident_bytes: match term_contracts::ids::U64String::new(resident) {
                Ok(v) => Metric::estimated("sysinfo.rss", v),
                Err(_) => Metric::unavailable("sysinfo.rss", "value out of persisted range"),
            },
            accounted_bytes: unavail_b("cgroup-style accounting is linux-only"),
            committed_bytes: unavail_b("commit accounting is windows job-only"),
            read_bytes_per_sec: read_rate,
            write_bytes_per_sec: write_rate,
            network_rx_bytes_per_sec: unavail_f("per-process networking unavailable in R1"),
            network_tx_bytes_per_sec: unavail_f("per-process networking unavailable in R1"),
            process_count: Metric::measured(SOURCE, verified.len() as u32),
            coverage,
        })
    }

    fn member_identities(&self, group: &GroupHandle) -> io::Result<Vec<ProcessIdentity>> {
        if matches!(&group.inner, GroupInner::Guardian(_)) {
            return super::macos_guardian::members(group);
        }
        let inner = tree_inner(group)?;
        let root = root_of(inner)?;
        let system = self.refresh()?;
        let observed = anchored_tree(&system, inner, &root)?;
        let (verified, _reused, _gone, unverifiable) = verify_members(inner, root.pid, &observed)?;
        if verified.is_empty() && unverifiable > 0 {
            // Unreadable members are omitted (never guessed), but an empty
            // list would read as "no members left" — the guardian observer
            // takes it as proof of exit and settles/retires the workload
            // while they may still run. Report the gap instead.
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "observed group members remain, but none has a readable identity",
            ));
        }
        Ok(verified)
    }

    fn terminate_owned(&self, group: &GroupHandle, phase: StopPhase) -> io::Result<()> {
        if matches!(&group.inner, GroupInner::Guardian(_)) {
            return super::macos_guardian::stop(group, phase);
        }
        let inner = tree_inner(group)?;
        let root = root_of(inner)?;
        let system = self.refresh()?;
        let observed = anchored_tree(&system, inner, &root)?;
        // Unverifiable members are never signalled on the bare pid.
        let (verified, reused, _gone, _unverifiable) = verify_members(inner, root.pid, &observed)?;
        drop(system);
        let sig = match phase {
            StopPhase::Grace => libc::SIGTERM,
            StopPhase::Force => libc::SIGKILL,
        };
        for ident in &verified {
            // Per-member re-verification immediately before each signal: a
            // member that exited since the batch pass may have had its pid
            // reused (§7).
            signal_verified(ident, sig);
        }
        if !reused.is_empty() {
            // Surfaced for diagnostics: PIDs that escaped signaling.
            tracing::debug!(?reused, "skipped reused pids during terminate");
        }
        Ok(())
    }

    /// 08 §2: 검증된 멤버 전체에 darwin background 정책을 건다. 나중에 생긴
    /// 자손은 정책을 상속하므로 해제도 "지금"의 멤버 전체를 다시 훑는다 —
    /// 그래서 `terminate_owned`과 똑같이 매번 트리를 새로 관측한다.
    fn set_scheduling(
        &self,
        group: &GroupHandle,
        tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        let members = self.verified_members(group, "set_scheduling")?;
        scheduling::set_process_scheduling(&members, tier)
    }

    /// 08 §5: 일시정지/재개. 양보와 같은 멤버십·신원 규율을 쓴다.
    fn suspend_owned(&self, group: &GroupHandle) -> io::Result<SchedulingOutcome> {
        let members = self.verified_members(group, "suspend")?;
        scheduling::set_process_suspend(&members, true)
    }

    fn resume_owned(&self, group: &GroupHandle) -> io::Result<SchedulingOutcome> {
        let members = self.verified_members(group, "resume")?;
        scheduling::set_process_suspend(&members, false)
    }

    fn is_empty(&self, group: &GroupHandle) -> io::Result<bool> {
        if matches!(&group.inner, GroupInner::Guardian(_)) {
            return super::macos_guardian::is_empty(group);
        }
        let inner = tree_inner(group)?;
        let root = root_of(inner)?;
        let system = self.refresh()?;
        let observed = anchored_tree(&system, inner, &root)?;
        if observed.is_empty() {
            return Ok(true);
        }
        let (verified, _reused, _gone, unverifiable) = verify_members(inner, root.pid, &observed)?;
        // An unreadable member may still be alive: it is an unverifiable
        // anchor, never proof of exit, so the group is not empty while one
        // is observed.
        Ok(verified.is_empty() && unverifiable == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::testutil::{mono_ms, poll_until, spawn_lived_child, test_workload};
    use crate::group::ResourcePlatform;
    use std::time::Duration;
    use term_contracts::snapshot::LimitSupport;

    #[test]
    fn capabilities_observe_only_profile() {
        let caps = MacosTreePlatform::new().capabilities();
        assert_eq!(caps.platform, "macos-observed-tree");
        assert_eq!(caps.memory_limit_kind.support, LimitSupport::Unsupported);
        assert_eq!(caps.cpu_quota.support, LimitSupport::Unsupported);
        assert_eq!(caps.process_count_limit.support, LimitSupport::Unsupported);
        assert!(caps
            .tree_accounting
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("observed")));
    }

    /// The orchestrator stores a clone of the handle in the registry BEFORE
    /// attaching the helper through its own copy, so clones must share the
    /// root. A deep copy made every registry-side lookup on macOS fail with
    /// "no helper attached yet" (gated_launch's `workload.processes`).
    #[test]
    fn handle_clones_share_the_attached_root() {
        let platform = MacosTreePlatform::new();
        let wl = test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("create");
        let registry_copy = group.clone();
        let me = identity::process_identity(std::process::id()).expect("self identity");
        platform
            .attach_pid(&group, &me)
            .expect("attach through the original handle");
        let members = platform
            .member_identities(&registry_copy)
            .expect("the clone sees the attached root");
        assert!(members.iter().any(|m| m.pid == std::process::id()));
        // Same state both ways: a second attach through the clone is refused.
        assert!(platform.attach_pid(&registry_copy, &me).is_err());
    }

    #[test]
    fn attach_rejects_identity_mismatch() {
        let platform = MacosTreePlatform::new();
        let wl = test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("create");
        let ghost = ProcessIdentity {
            pid: u32::MAX - 4,
            start_token: "1".into(),
            boot_id: identity::boot_id(),
        };
        assert!(platform.attach_pid(&group, &ghost).is_err());
        // Sampling before attach is an explicit error, not a fake empty tree.
        assert!(platform.sample_group(&group, mono_ms()).is_err());
    }

    /// L6: `recorded`는 매 관측마다 살아 있는 pid 집합으로 정리된다 —
    /// 떠난 자식이 맵에 영원히 남지 않는다(관리 워크로드당 초당 2회 호출).
    #[test]
    fn verify_members_prunes_recorded_pids_that_left_the_tree() {
        let inner = MacGroupInner::default();
        let stale = ProcessIdentity {
            pid: u32::MAX - 7,
            start_token: "1".into(),
            boot_id: identity::boot_id(),
        };
        inner
            .recorded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(stale.pid, stale.clone());

        let live = std::process::id();
        let (verified, reused, _gone, _unverifiable) =
            verify_members(&inner, live, &[live]).unwrap();
        assert!(verified.iter().any(|v| v.pid == live), "self is verified");
        assert!(reused.is_empty());

        let recorded = inner.recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            !recorded.contains_key(&stale.pid),
            "관측되지 않은 pid는 정리된다"
        );
        assert!(recorded.contains_key(&live), "관측된 pid는 남는다");
        assert_eq!(recorded.len(), 1);
    }

    /// PID 재사용 봉인(§7): 루트가 떠난 뒤 같은 pid를 얻은 무관한 프로세스(와
    /// 그 자손)는 "트리 소속만으로" 검증되어선 안 된다 — 관측은 비어 있고
    /// terminate는 아무것도 신호하지 않는다.
    #[test]
    fn reused_root_pid_is_never_observed_or_signaled() {
        let platform = MacosTreePlatform::new();
        let wl = test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("create");
        // The stranger now owning the pid our (exited) root used to have.
        let mut stranger = spawn_lived_child();
        let live = identity::process_identity(stranger.id()).expect("stranger identity");
        let stale_root = ProcessIdentity {
            pid: live.pid,
            start_token: format!("{}-stale", live.start_token),
            boot_id: live.boot_id.clone(),
        };
        assert!(!stale_root.same_process(&live));
        let inner = tree_inner(&group).expect("tree inner");
        *inner.root.lock().unwrap_or_else(|p| p.into_inner()) = Some(stale_root.clone());
        inner
            .recorded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(stale_root.pid, stale_root);

        assert!(
            platform.is_empty(&group).expect("is_empty"),
            "reused root pid → empty"
        );
        assert!(platform
            .member_identities(&group)
            .expect("members")
            .is_empty());
        let usage = platform.sample_group(&group, mono_ms()).expect("sample");
        assert_eq!(usage.process_count.value, Some(0));
        platform
            .terminate_owned(&group, StopPhase::Grace)
            .expect("grace");
        platform
            .terminate_owned(&group, StopPhase::Force)
            .expect("force");
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            stranger.try_wait().expect("try_wait").is_none(),
            "an unrelated process holding the old root pid must never be signaled"
        );
        let _ = stranger.kill();
        let _ = stranger.wait();
    }

    /// 루트가 떠나 관측 집합이 비어도 루트 행은 남는다 — 재사용 판정의 닻.
    #[test]
    fn verify_members_keeps_the_root_row_after_root_exit() {
        let inner = MacGroupInner::default();
        let root = identity::process_identity(std::process::id()).expect("self identity");
        inner
            .recorded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(root.pid, root.clone());
        let (verified, reused, gone, unverifiable) = verify_members(&inner, root.pid, &[]).unwrap();
        assert!(verified.is_empty() && reused.is_empty() && gone == 0 && unverifiable == 0);
        let recorded = inner.recorded.lock().unwrap_or_else(|p| p.into_inner());
        assert!(recorded
            .get(&root.pid)
            .is_some_and(|r| r.same_process(&root)));
    }

    /// One unreadable member must not poison the whole scan. A non-ESRCH
    /// identity failure (EPERM on a setuid-both descendant; pid 0 yields the
    /// same InvalidInput error shape) counts as unverifiable, never as gone:
    /// the rest of the tree stays verifiable, so stop/is_empty keep working.
    #[test]
    fn unreadable_member_is_unverifiable_not_fatal() {
        let inner = MacGroupInner::default();
        let live = std::process::id();
        let (verified, reused, gone, unverifiable) =
            verify_members(&inner, live, &[0, live]).unwrap();
        assert!(verified.iter().any(|v| v.pid == live), "self is verified");
        assert!(reused.is_empty());
        assert_eq!(gone, 0, "an unreadable probe is not evidence of exit");
        assert_eq!(unverifiable, 1, "unreadable member counts as unverifiable");
    }

    /// A group whose only observed member is unreadable is never reported
    /// empty: neither `is_empty` nor the guardian's `member_identities`
    /// evidence may claim an exit that was not observed (a premature
    /// "empty" would release the reservation and retire the guardian while
    /// the member still runs). Coverage is downgraded to partial.
    #[test]
    fn unreadable_only_member_never_reports_an_empty_group() {
        let platform = MacosTreePlatform::new();
        let wl = test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("create");
        // pid 0 fails the identity probe with the same non-ESRCH error
        // shape as EPERM on a setuid-both member. terminate_owned is not
        // exercised here: nothing may ever signal pid 0.
        let unreadable = ProcessIdentity {
            pid: 0,
            start_token: "1".into(),
            boot_id: identity::boot_id(),
        };
        let inner = tree_inner(&group).expect("tree inner");
        *inner.root.lock().unwrap_or_else(|p| p.into_inner()) = Some(unreadable.clone());
        inner
            .recorded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(unreadable.pid, unreadable);

        assert!(
            !platform.is_empty(&group).expect("is_empty"),
            "an unreadable member keeps the group non-empty"
        );
        assert!(
            platform.member_identities(&group).is_err(),
            "no verifiable member is reported as a gap, not as an empty group"
        );
        let usage = platform.sample_group(&group, mono_ms()).expect("sample");
        assert_eq!(usage.process_count.value, Some(0));
        assert_eq!(usage.coverage, UsageCoverage::Partial);
    }

    /// An unreadable anchor stays observed (unverifiable, never signalled)
    /// instead of aborting the scan, and never anchors a subtree it can no
    /// longer prove.
    #[test]
    fn unreadable_anchor_stays_observed_without_a_subtree() {
        let system = System::new();
        let inner = MacGroupInner::default();
        let root = identity::process_identity(std::process::id()).expect("self identity");
        inner
            .recorded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                0,
                ProcessIdentity {
                    pid: 0,
                    start_token: "1".into(),
                    boot_id: identity::boot_id(),
                },
            );
        let observed = anchored_tree(&system, &inner, &root).expect("anchored tree");
        assert!(observed.contains(&0), "unreadable member stays an anchor");
        assert!(
            !observed.iter().any(|&pid| pid != 0 && pid != root.pid),
            "no subtree is anchored from an unverifiable pid"
        );
    }

    /// Per-member re-verification before each signal: a pid whose identity
    /// no longer matches the recorded one is never signalled.
    #[test]
    fn signal_verified_skips_a_reused_pid() {
        let mut stranger = spawn_lived_child();
        let live = identity::process_identity(stranger.id()).expect("stranger identity");
        let stale = ProcessIdentity {
            pid: live.pid,
            start_token: format!("{}-stale", live.start_token),
            boot_id: live.boot_id.clone(),
        };
        signal_verified(&stale, libc::SIGKILL);
        signal_verified(&stale, libc::SIGTERM);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            stranger.try_wait().expect("try_wait").is_none(),
            "a pid whose identity does not match is never signalled"
        );
        let _ = stranger.kill();
        let _ = stranger.wait();
    }

    /// 08 §2: 그룹 경로도 검증된 멤버에만 정책을 걸고, 해제는 원상 복구다.
    /// 루트가 붙지 않은 그룹은 "빈 트리"가 아니라 오류다 — 추측한 루트로
    /// 남의 프로세스를 건드리지 않는다.
    #[test]
    fn set_scheduling_applies_and_restores_the_verified_tree() {
        use crate::group::scheduling::observed_tier;
        use crate::group::SchedulingTier;

        let platform = MacosTreePlatform::new();
        let wl = test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("create");
        assert!(
            platform
                .set_scheduling(&group, SchedulingTier::Background)
                .is_err(),
            "no attached root → error, never a silent no-op"
        );

        let mut child = spawn_lived_child();
        let ident = identity::process_identity(child.id()).expect("child identity");
        platform.attach_pid(&group, &ident).expect("attach");
        assert_eq!(observed_tier(ident.pid), Some(SchedulingTier::Normal));

        let out = platform
            .set_scheduling(&group, SchedulingTier::Background)
            .expect("background");
        assert!(out.applied >= 1 && !out.is_partial(), "{out:?}");
        assert_eq!(observed_tier(ident.pid), Some(SchedulingTier::Background));

        let out = platform
            .set_scheduling(&group, SchedulingTier::Normal)
            .expect("restore");
        assert!(out.applied >= 1 && !out.is_partial(), "{out:?}");
        assert_eq!(
            observed_tier(ident.pid),
            Some(SchedulingTier::Normal),
            "해제를 확인하지 못한 수단은 출시하지 않는다(08 §9)"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn observed_tree_lifecycle() {
        let platform = MacosTreePlatform::new();
        let wl = test_workload(None, None, None);
        let group = platform.create_group(&wl).expect("create");
        let mut child = spawn_lived_child();
        let ident = identity::process_identity(child.id()).expect("child identity");
        platform.attach_pid(&group, &ident).expect("attach");
        platform
            .attach_pid(&group, &ident)
            .expect_err("second root rejected");

        let usage = platform.sample_group(&group, mono_ms()).expect("sample 1");
        assert!(usage.process_count.value >= Some(1));
        assert_eq!(usage.coverage, UsageCoverage::ObservedTree);
        assert!(usage.cpu_cores.value.is_none(), "first differential");
        std::thread::sleep(Duration::from_millis(150));
        let usage2 = platform.sample_group(&group, mono_ms()).expect("sample 2");
        assert!(usage2.cpu_cores.value.is_some());

        let members = platform.member_identities(&group).expect("members");
        assert!(members.iter().any(|m| m.pid == child.id()));

        assert!(!platform.is_empty(&group).expect("not empty"));
        // sleep(1) exits on SIGTERM; force covers stragglers.
        platform
            .terminate_owned(&group, StopPhase::Grace)
            .expect("grace");
        if !poll_until(Duration::from_secs(5), || {
            platform.is_empty(&group).unwrap_or(false)
        }) {
            platform
                .terminate_owned(&group, StopPhase::Force)
                .expect("force");
            assert!(poll_until(Duration::from_secs(5), || {
                platform.is_empty(&group).unwrap_or(false)
            }));
        }
        let _ = child.wait();
    }
}

impl MacosTreePlatform {
    /// 지금 검증된 멤버 전체(가디언 그룹 포함). 정지된 트리는 새 자손을
    /// 만들 수 없지만, 재개 뒤 새로 생긴 자손은 다음 정지 패스에서 붙잡힌다.
    fn verified_members(
        &self,
        group: &GroupHandle,
        what: &str,
    ) -> io::Result<Vec<ProcessIdentity>> {
        if matches!(&group.inner, GroupInner::Guardian(_)) {
            return super::macos_guardian::members(group);
        }
        let inner = tree_inner(group)?;
        let root = root_of(inner)?;
        let system = self.refresh()?;
        let observed = anchored_tree(&system, inner, &root)?;
        let (verified, reused, _gone, _unverifiable) = verify_members(inner, root.pid, &observed)?;
        drop(system);
        if !reused.is_empty() {
            tracing::debug!(?reused, "skipped reused pids during {what}");
        }
        Ok(verified)
    }
}
