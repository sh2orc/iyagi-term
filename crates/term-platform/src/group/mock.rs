//! Ticket I06 — deterministic in-memory backend for tests (runner/scheduler
//! tickets + B10 patterns: "terminate is never called on an identity-changed
//! process").
//!
//! Scripting surface:
//! * preset [`Capabilities`]
//! * scripted attach failures per PID (attach fails, helper must be cleaned
//!   up, no RELEASE semantics — 02-runner §3)
//! * scripted PID reuse: members whose recorded identity no longer matches
//!   are skipped by [`ResourcePlatform::terminate_owned`] and recorded in
//!   [`MockTerminateCall::skipped_reuse`] instead of being signalled
//! * scripted `is_empty` sequences (last value sticks)
//! * scripted `set_scheduling` behaviour (ok / failure / unsupported) with a
//!   `(workload_id, tier)` call log (08 §2)
//! * logs: created groups, attach attempts, terminate calls
//!
//! All state lives in the platform (shared by reference); group handles only
//! carry the reference key.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::Mutex;

use term_contracts::ids::{ProcessIdentity, SessionId, U64String, WorkloadId};
use term_contracts::metrics::{Metric, UsageCoverage, WorkloadUsage};
use term_contracts::snapshot::Capabilities;
use term_contracts::workload::{GroupKind, WorkloadDescriptor};

use super::{
    GroupHandle, GroupInner, ResourcePlatform, SchedulingOutcome, SchedulingTier, StopPhase,
};

/// Marker payload for mock handles (state lives in [`MockPlatform`]).
#[derive(Debug, Clone, Default)]
pub struct MockGroupInner;

/// One scripted group member: the identity recorded when it was attached and
/// what that PID looks like *now* (`reused_as = Some(_)` simulates PID reuse;
/// `alive = false` simulates a dead process).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockMember {
    pub recorded: ProcessIdentity,
    pub reused_as: Option<ProcessIdentity>,
    pub alive: bool,
}

impl MockMember {
    pub fn alive(identity: ProcessIdentity) -> Self {
        Self {
            recorded: identity,
            reused_as: None,
            alive: true,
        }
    }
}

/// What a scripted [`ResourcePlatform::set_scheduling`] call answers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MockScheduling {
    /// Every verified member changed tier.
    #[default]
    Ok,
    /// The call reached the OS but some member could not be changed —
    /// the caller must surface `partial` and retry (08 §2).
    Partial,
    /// The backend cannot yield reversibly here (08 §0-4).
    Unsupported,
}

/// Recorded `set_scheduling` call: which workload and which tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockSchedulingCall {
    pub workload_id: WorkloadId,
    pub group: String,
    pub tier: SchedulingTier,
}

/// Recorded outcome of one `terminate_owned` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockTerminateCall {
    pub group: String,
    pub phase: StopPhase,
    /// Identities that were actually signalled (recorded identity verified).
    pub signalled: Vec<ProcessIdentity>,
    /// PIDs skipped because the recorded identity no longer matches (reuse).
    pub skipped_reuse: Vec<u32>,
}

#[derive(Debug, Default)]
struct MockInner {
    created: Vec<String>,
    attach_errors: BTreeMap<u32, String>,
    members: BTreeMap<String, Vec<MockMember>>,
    empty_script: BTreeMap<String, VecDeque<bool>>,
    attach_log: Vec<(String, u32)>,
    terminate_log: Vec<MockTerminateCall>,
    scheduling: MockScheduling,
    scheduling_log: Vec<MockSchedulingCall>,
}

/// In-memory scripted [`super::ResourcePlatform`].
pub struct MockPlatform {
    caps: Mutex<Capabilities>,
    inner: Mutex<MockInner>,
}

impl MockPlatform {
    pub fn new(caps: Capabilities) -> Self {
        Self {
            caps: Mutex::new(caps),
            inner: Mutex::new(MockInner::default()),
        }
    }

    /// Replace the reported capabilities (e.g. to script preflight changes).
    pub fn set_capabilities(&self, caps: Capabilities) {
        *self.caps.lock().expect("mock lock poisoned") = caps;
    }

    /// Script: `attach_pid` fails for this PID with `msg` (no attach recorded,
    /// no release semantics).
    pub fn script_attach_error(&self, pid: u32, msg: &str) {
        self.inner
            .lock()
            .expect("mock lock poisoned")
            .attach_errors
            .insert(pid, msg.to_string());
    }

    /// Add/replace a scripted member list for a group reference.
    pub fn set_members(&self, group_ref: &str, members: Vec<MockMember>) {
        self.inner
            .lock()
            .expect("mock lock poisoned")
            .members
            .insert(group_ref.to_string(), members);
    }

    /// Simulate PID reuse: the PID of the member recorded as `pid` in
    /// `group_ref` now belongs to `new_identity`.
    pub fn simulate_pid_reuse(&self, group_ref: &str, pid: u32, new_identity: ProcessIdentity) {
        let mut inner = self.inner.lock().expect("mock lock poisoned");
        if let Some(members) = inner.members.get_mut(group_ref) {
            for m in members.iter_mut().filter(|m| m.recorded.pid == pid) {
                m.reused_as = Some(new_identity.clone());
            }
        }
    }

    /// Script consecutive `is_empty` answers; the last one sticks once the
    /// queue is exhausted. Without a script, emptiness derives from members.
    pub fn script_is_empty(&self, group_ref: &str, answers: Vec<bool>) {
        self.inner
            .lock()
            .expect("mock lock poisoned")
            .empty_script
            .insert(group_ref.to_string(), VecDeque::from(answers));
    }

    /// Group references in creation order.
    pub fn created_groups(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("mock lock poisoned")
            .created
            .clone()
    }

    /// `(group_ref, pid)` of every successful attach.
    pub fn attach_log(&self) -> Vec<(String, u32)> {
        self.inner
            .lock()
            .expect("mock lock poisoned")
            .attach_log
            .clone()
    }

    pub fn terminate_log(&self) -> Vec<MockTerminateCall> {
        self.inner
            .lock()
            .expect("mock lock poisoned")
            .terminate_log
            .clone()
    }

    /// Script what `set_scheduling` answers from now on (default `Ok`).
    pub fn script_scheduling(&self, behaviour: MockScheduling) {
        self.inner.lock().expect("mock lock poisoned").scheduling = behaviour;
    }

    /// Every `set_scheduling` call in order — including the ones that
    /// answered `Unsupported` (the attempt itself is observable).
    pub fn scheduling_log(&self) -> Vec<MockSchedulingCall> {
        self.inner
            .lock()
            .expect("mock lock poisoned")
            .scheduling_log
            .clone()
    }

    fn mock_of(group: &GroupHandle) -> io::Result<()> {
        match &group.inner {
            GroupInner::Mock(_) => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a mock group handle",
            )),
        }
    }
}

impl Default for MockPlatform {
    fn default() -> Self {
        let caps = Capabilities::observe_only("mock");
        Self::new(caps)
    }
}

impl super::ResourcePlatform for MockPlatform {
    fn capabilities(&self) -> Capabilities {
        self.caps.lock().expect("mock lock poisoned").clone()
    }

    fn create_group(&self, workload: &WorkloadDescriptor) -> io::Result<GroupHandle> {
        let reference = workload.workload_id.to_string();
        self.inner
            .lock()
            .expect("mock lock poisoned")
            .created
            .push(reference.clone());
        Ok(GroupHandle {
            workload_id: workload.workload_id.clone(),
            kind: GroupKind::ObservedTree,
            reference,
            inner: GroupInner::Mock(MockGroupInner),
        })
    }

    fn attach_pid(&self, group: &GroupHandle, identity: &ProcessIdentity) -> io::Result<()> {
        Self::mock_of(group)?;
        let mut inner = self.inner.lock().expect("mock lock poisoned");
        if let Some(msg) = inner.attach_errors.get(&identity.pid) {
            // Scripted failure: no attach recorded → the caller must clean
            // the helper up and must NOT send RELEASE (02-runner §3).
            return Err(io::Error::other(format!(
                "scripted attach failure for pid {}: {msg}",
                identity.pid
            )));
        }
        let reference = group.reference.clone();
        inner
            .members
            .entry(reference.clone())
            .or_default()
            .push(MockMember::alive(identity.clone()));
        inner.attach_log.push((reference, identity.pid));
        Ok(())
    }

    fn sample_group(&self, group: &GroupHandle, _now_ms: u64) -> io::Result<WorkloadUsage> {
        Self::mock_of(group)?;
        let inner = self.inner.lock().expect("mock lock poisoned");
        let members = inner
            .members
            .get(&group.reference)
            .cloned()
            .unwrap_or_default();
        let alive = members.iter().filter(|m| m.alive).count() as u32;
        let unavail_f = |reason: &'static str| Metric::<f64>::unavailable("mock", reason);
        let unavail_b = |reason: &'static str| {
            Metric::<term_contracts::ids::U64String>::unavailable("mock", reason)
        };
        Ok(WorkloadUsage {
            workload_id: group.workload_id.clone(),
            cpu_cores: unavail_f("mock backend does not produce cpu deltas"),
            resident_bytes: unavail_b("mock backend does not produce resident"),
            accounted_bytes: unavail_b("mock backend does not produce accounting"),
            committed_bytes: unavail_b("mock backend does not produce commit"),
            read_bytes_per_sec: unavail_f("mock backend does not produce io rates"),
            write_bytes_per_sec: unavail_f("mock backend does not produce io rates"),
            network_rx_bytes_per_sec: unavail_f("mock backend does not produce network"),
            network_tx_bytes_per_sec: unavail_f("mock backend does not produce network"),
            process_count: Metric::measured("mock", alive),
            coverage: UsageCoverage::Group,
        })
    }

    fn member_identities(&self, group: &GroupHandle) -> io::Result<Vec<ProcessIdentity>> {
        Self::mock_of(group)?;
        let inner = self.inner.lock().expect("mock lock poisoned");
        Ok(inner
            .members
            .get(&group.reference)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.alive && m.reused_as.is_none())
            .map(|m| m.recorded)
            .collect())
    }

    fn terminate_owned(&self, group: &GroupHandle, phase: StopPhase) -> io::Result<()> {
        Self::mock_of(group)?;
        let mut inner = self.inner.lock().expect("mock lock poisoned");
        let members = inner.members.get_mut(&group.reference);
        let mut call = MockTerminateCall {
            group: group.reference.clone(),
            phase,
            signalled: Vec::new(),
            skipped_reuse: Vec::new(),
        };
        if let Some(members) = members {
            for m in members.iter_mut() {
                if m.reused_as.is_some() {
                    // B10: the PID now belongs to another process — never
                    // signal it, record the skip (02-runner §7).
                    call.skipped_reuse.push(m.recorded.pid);
                    continue;
                }
                if m.alive {
                    call.signalled.push(m.recorded.clone());
                    if phase == StopPhase::Force {
                        m.alive = false;
                    }
                }
            }
        }
        inner.terminate_log.push(call);
        Ok(())
    }

    fn set_scheduling(
        &self,
        group: &GroupHandle,
        tier: SchedulingTier,
    ) -> io::Result<SchedulingOutcome> {
        Self::mock_of(group)?;
        let mut inner = self.inner.lock().expect("mock lock poisoned");
        inner.scheduling_log.push(MockSchedulingCall {
            workload_id: group.workload_id.clone(),
            group: group.reference.clone(),
            tier,
        });
        let members = inner
            .members
            .get(&group.reference)
            .cloned()
            .unwrap_or_default();
        // A reused pid is never touched — exactly like terminate_owned.
        let skipped_reused = members.iter().filter(|m| m.reused_as.is_some()).count();
        let targets = members
            .iter()
            .filter(|m| m.alive && m.reused_as.is_none())
            .count();
        match inner.scheduling {
            MockScheduling::Unsupported => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "scripted: scheduling yield unsupported",
            )),
            MockScheduling::Partial => Ok(SchedulingOutcome {
                applied: targets.saturating_sub(1),
                failed: targets.min(1),
                skipped_reused,
            }),
            MockScheduling::Ok => Ok(SchedulingOutcome {
                applied: targets,
                failed: 0,
                skipped_reused,
            }),
        }
    }

    fn is_empty(&self, group: &GroupHandle) -> io::Result<bool> {
        Self::mock_of(group)?;
        let mut inner = self.inner.lock().expect("mock lock poisoned");
        if let Some(queue) = inner.empty_script.get_mut(&group.reference) {
            if let Some(next) = queue.pop_front() {
                return Ok(next);
            }
            if let Some(last) = queue.back() {
                return Ok(*last);
            }
        }
        let members = inner
            .members
            .get(&group.reference)
            .cloned()
            .unwrap_or_default();
        // A reused PID is no longer our member: it was never signalled and
        // must not keep the group "non-empty" forever.
        Ok(members.iter().all(|m| !m.alive || m.reused_as.is_some()))
    }
}

impl MockPlatform {
    /// Convenience for tests that do not care about the descriptor: create a
    /// group for a fresh random workload id with observe-only defaults.
    pub fn create_anonymous_group(&self) -> io::Result<GroupHandle> {
        let workload = WorkloadDescriptor {
            workload_id: WorkloadId::generate(),
            session_id: SessionId::generate(),
            cwd: "/mock".into(),
            program: "/mock/program".into(),
            argv: vec![],
            env_overrides: std::collections::BTreeMap::new(),
            cols: 80,
            rows: 24,
            policy: term_contracts::launch::LaunchPolicy {
                reservation_bytes: U64String::new(1 << 31).expect("2 GiB fits"),
                cpu_slots: 1,
                enforcement: term_contracts::launch::Enforcement::Observe,
                memory_max_bytes: None,
                cpu_max_cores: None,
                pids_max: None,
            },
        };
        self.create_group(&workload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::ResourcePlatform;

    fn identity(pid: u32, start: &str) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_token: start.into(),
            boot_id: "mock-boot".into(),
        }
    }

    #[test]
    fn capabilities_are_preset_and_replaceable() {
        let platform = MockPlatform::default();
        assert_eq!(platform.capabilities().platform, "mock");
        let mut caps = Capabilities::observe_only("mock-limited");
        caps.platform = "mock".into();
        platform.set_capabilities(caps);
        assert!(platform.capabilities().notes.is_empty());
    }

    #[test]
    fn attach_failure_records_nothing_and_never_terminates() {
        let platform = MockPlatform::default();
        let group = platform.create_anonymous_group().expect("create");
        platform.script_attach_error(4242, "GROUP_ATTACH_FAILED");
        let err = platform
            .attach_pid(&group, &identity(4242, "7"))
            .expect_err("scripted failure");
        assert!(err.to_string().contains("GROUP_ATTACH_FAILED"));
        // No attach recorded; no release/terminate semantics happened.
        assert!(!platform.attach_log().iter().any(|(_, pid)| *pid == 4242));
        assert!(platform.terminate_log().is_empty());
        assert!(platform.is_empty(&group).expect("no members"));
    }

    #[test]
    fn attach_waiting_helper_delegates_to_attach_pid() {
        let platform = MockPlatform::default();
        let group = platform.create_anonymous_group().expect("create");
        platform
            .attach_waiting_helper(&group, &identity(10, "1"))
            .expect("delegates");
        assert_eq!(platform.attach_log(), vec![(group.reference.clone(), 10)]);
    }

    #[test]
    fn terminate_never_signals_identity_changed_pids() {
        let platform = MockPlatform::default();
        let group = platform.create_anonymous_group().expect("create");
        let root = identity(100, "11");
        let other = identity(200, "22");
        platform.set_members(
            &group.reference,
            vec![MockMember::alive(root), MockMember::alive(other.clone())],
        );
        // PID 100 was recycled into an unrelated process.
        platform.simulate_pid_reuse(&group.reference, 100, identity(100, "9999"));

        platform
            .terminate_owned(&group, StopPhase::Force)
            .expect("force");
        let log = platform.terminate_log();
        assert_eq!(log.len(), 1);
        assert_eq!(
            log[0].signalled,
            vec![other.clone()],
            "only verified members"
        );
        assert_eq!(log[0].skipped_reuse, vec![100], "reused pid skipped");

        // Remaining verified member was killed → empty.
        assert!(platform.is_empty(&group).expect("empty"));
        // After the force stop no verifiable member remains, and the reused
        // PID never surfaces as one.
        let members = platform.member_identities(&group).expect("members");
        assert!(members.is_empty());
    }

    #[test]
    fn grace_signals_without_killing() {
        let platform = MockPlatform::default();
        let group = platform.create_anonymous_group().expect("create");
        platform.set_members(&group.reference, vec![MockMember::alive(identity(7, "s"))]);
        platform
            .terminate_owned(&group, StopPhase::Grace)
            .expect("grace");
        let log = platform.terminate_log();
        assert_eq!(log[0].phase, StopPhase::Grace);
        assert_eq!(log[0].signalled.len(), 1);
        assert!(!platform.is_empty(&group).expect("still alive"));
    }

    #[test]
    fn is_empty_script_and_derivation() {
        let platform = MockPlatform::default();
        let group = platform.create_anonymous_group().expect("create");
        platform.script_is_empty(&group.reference, vec![false, true]);
        assert!(!platform.is_empty(&group).expect("scripted false"));
        assert!(platform.is_empty(&group).expect("scripted true"));
        assert!(platform.is_empty(&group).expect("last sticks"));

        let group2 = platform.create_anonymous_group().expect("create 2");
        platform.set_members(&group2.reference, vec![MockMember::alive(identity(9, "9"))]);
        assert!(!platform.is_empty(&group2).expect("derived not empty"));
    }

    /// 08 §2: 호출은 `(workload, tier)`로 기록되고, 재사용 pid는 절대 대상이
    /// 아니며, 스크립트된 실패/미지원이 그대로 올라온다.
    #[test]
    fn set_scheduling_records_calls_and_honours_the_script() {
        let platform = MockPlatform::default();
        let group = platform.create_anonymous_group().expect("create");
        platform.set_members(
            &group.reference,
            vec![
                MockMember::alive(identity(1, "a")),
                MockMember::alive(identity(2, "b")),
            ],
        );
        let out = platform
            .set_scheduling(&group, SchedulingTier::Background)
            .expect("ok by default");
        assert_eq!(out.applied, 2);
        assert!(!out.is_partial());

        platform.simulate_pid_reuse(&group.reference, 1, identity(1, "9999"));
        let out = platform
            .set_scheduling(&group, SchedulingTier::Normal)
            .expect("restore");
        assert_eq!(
            out,
            SchedulingOutcome {
                applied: 1,
                failed: 0,
                skipped_reused: 1
            },
            "a reused pid is skipped, never changed"
        );

        platform.script_scheduling(MockScheduling::Partial);
        let out = platform
            .set_scheduling(&group, SchedulingTier::Background)
            .expect("partial still reaches the OS");
        assert!(out.is_partial());

        platform.script_scheduling(MockScheduling::Unsupported);
        let err = platform
            .set_scheduling(&group, SchedulingTier::Background)
            .expect_err("scripted unsupported");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);

        let log = platform.scheduling_log();
        assert_eq!(log.len(), 4, "every attempt is observable");
        assert!(log.iter().all(|c| c.workload_id == group.workload_id));
        assert_eq!(
            log.iter().map(|c| c.tier).collect::<Vec<_>>(),
            vec![
                SchedulingTier::Background,
                SchedulingTier::Normal,
                SchedulingTier::Background,
                SchedulingTier::Background,
            ]
        );
    }

    /// 기본 mock capability는 양보를 지원한다(관측 전용 프로필 기준).
    #[test]
    fn mock_capabilities_support_scheduling_yield_by_default() {
        use term_contracts::snapshot::LimitSupport;
        let platform = MockPlatform::default();
        assert_eq!(
            platform.capabilities().scheduling_yield.support,
            LimitSupport::Supported
        );
    }

    #[test]
    fn sample_reports_member_count_with_group_coverage() {
        let platform = MockPlatform::default();
        let group = platform.create_anonymous_group().expect("create");
        platform.set_members(
            &group.reference,
            vec![
                MockMember::alive(identity(1, "a")),
                MockMember::alive(identity(2, "b")),
                MockMember {
                    recorded: identity(3, "c"),
                    reused_as: None,
                    alive: false,
                },
            ],
        );
        let usage = platform.sample_group(&group, 1_000).expect("sample");
        assert_eq!(usage.process_count.value, Some(2));
        assert_eq!(usage.coverage, UsageCoverage::Group);
        assert_eq!(usage.workload_id, group.workload_id);
        assert!(usage.cpu_cores.value.is_none(), "mock reports unavailable");
    }
}
