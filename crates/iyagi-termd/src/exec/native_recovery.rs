//! Native OS observations run on a dedicated worker, never on the mission actor.
use super::{gated::GateConfig, persistence::RecoveredAction, ExecSupervisor};
use std::{
    collections::{HashMap, HashSet},
    io,
    time::{Duration, Instant},
};
use term_contracts::{
    ids::WorkloadId,
    mission::types::{ExecRecord, ExecState, Id},
};
use term_platform::{GroupHandle, StopPhase};

/// Pinned-handle budget shared by recovered and stranded entries so the
/// reconciliation worker cannot exhaust FDs on pinned native objects.
const MAX_PINNED_GROUPS: usize = 64;

#[derive(Default)]
pub(super) struct NativeRecovery {
    cursor: usize,
    cleanup_cursor: usize,
    stranded_cursor: usize,
    groups: HashMap<Id, RecoveredGroup>,
}
struct RecoveredGroup {
    handle: GroupHandle,
    grace_at: Option<Instant>,
    ended_at: Option<String>,
    confirmed: bool,
    /// Pinned by a stop or launch-cleanup worker whose force drain hit the
    /// deadline: the workload is recorded as stopped-with-stranded-members
    /// and only this worker's reconciliation may drop the pin.
    stranded: bool,
    /// Launch-cleanup pins only. No `ExecHandle` (and so no group watcher)
    /// exists for a failed launch, so reconciliation owns its completion:
    /// after a verified empty observation it commits this record Exited,
    /// releases the reservation, and retires the retained native group.
    /// `None` for stop-path pins, whose exec finalize does all three.
    owner: Option<ExecRecord>,
}

/// One stranded pin copied out of the registry so platform calls and the
/// durable commit run without holding the native-recovery lock.
struct StrandedPin {
    id: Id,
    handle: GroupHandle,
    owner: Option<ExecRecord>,
    ended_at: Option<String>,
}

/// What a verified empty observation settles for a stranded pin.
enum StrandedOutcome {
    /// Nothing left for reconciliation: drop the pin.
    Drop,
    /// Exited is durable and the reservation released; the retained native
    /// group still needs retirement (the confirmed cleanup pass retries it).
    Retire,
    /// The Exited commit failed: keep the pin and the reservation, and
    /// retry later with the same observation time.
    CommitPending(String),
}

impl ExecSupervisor {
    /// Pin a group whose force drain hit the deadline: the workload is
    /// stopped-with-stranded-members. [`Self::reconcile_native_recovery`]
    /// keeps retrying Force and settles the pin only on a verified empty
    /// observation. A stop-path pin (`owner == None`) is then dropped and
    /// the owning exec's finalize commits Exited and releases the
    /// reservation; a launch-cleanup pin carries its `owner` record, which
    /// reconciliation commits Exited itself before releasing the reservation
    /// and retiring the group. Idempotent per exec id; `false` when the pin
    /// budget is exhausted.
    pub(super) fn register_stranded_group(
        &self,
        exec_id: &Id,
        handle: &GroupHandle,
        owner: Option<ExecRecord>,
    ) -> bool {
        let mut native = self
            .inner
            .native_recovery
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if native.groups.contains_key(exec_id) {
            return true;
        }
        if native.groups.len() >= MAX_PINNED_GROUPS {
            return false;
        }
        native.groups.insert(
            exec_id.clone(),
            RecoveredGroup {
                handle: handle.clone(),
                grace_at: None,
                ended_at: None,
                confirmed: false,
                stranded: true,
                owner,
            },
        );
        true
    }

    /// Drop a stop-path stranded pin once its exec finalized (the finalize
    /// itself verified the group empty, committed Exited and released the
    /// reservation), so an unobservable group cannot hold a pin slot
    /// forever. Recovered entries and launch-cleanup pins are untouched.
    pub(super) fn drop_stranded_pin(&self, exec_id: &Id) {
        let mut native = self
            .inner
            .native_recovery
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if native
            .groups
            .get(exec_id)
            .is_some_and(|group| group.stranded && group.owner.is_none())
        {
            native.groups.remove(exec_id);
        }
    }

    /// Stranded pins: a stop or launch-cleanup worker already exhausted its
    /// bounded force drain. Retry Force here, off that worker's thread, and
    /// settle a pin only on a verified empty observation (see
    /// [`Self::register_stranded_group`]). At most eight pins per call,
    /// rotating fairly. The pins are copied out and the native lock is
    /// released across the platform calls (a Linux cgroup Force can poll
    /// `populated` for ~2s) and the Exited commit, so registration and the
    /// recovered-record pass are never held behind them; results are applied
    /// by exec id under a short re-lock.
    fn reconcile_stranded_pins(&self, config: &GateConfig) -> Vec<(Id, io::Error)> {
        let batch: Vec<StrandedPin> = {
            let mut native = self
                .inner
                .native_recovery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let mut ids: Vec<Id> = native
                .groups
                .iter()
                .filter(|(_, group)| group.stranded)
                .map(|(id, _)| id.clone())
                .collect();
            if ids.is_empty() {
                return Vec::new();
            }
            ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            let start = native.stranded_cursor % ids.len();
            let count = ids.len().min(8);
            native.stranded_cursor = (start + count) % ids.len();
            (0..count)
                .filter_map(|offset| {
                    let id = &ids[(start + offset) % ids.len()];
                    native.groups.get(id).map(|group| StrandedPin {
                        id: id.clone(),
                        handle: group.handle.clone(),
                        owner: group.owner.clone(),
                        ended_at: group.ended_at.clone(),
                    })
                })
                .collect()
        };
        let platform = &config.platform;
        let mut errors = Vec::new();
        let mut outcomes = Vec::new();
        for pin in batch {
            match platform.is_empty(&pin.handle) {
                Ok(true) => {
                    let outcome = self.settle_stranded_pin(&pin, &mut errors);
                    outcomes.push((pin.id, outcome));
                }
                Ok(false) => {
                    if let Err(error) = platform.terminate_owned(&pin.handle, StopPhase::Force) {
                        errors.push((pin.id, error));
                    }
                }
                Err(error) => errors.push((pin.id, error)),
            }
        }
        if outcomes.is_empty() {
            return errors;
        }
        let mut native = self
            .inner
            .native_recovery
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        for (id, outcome) in outcomes {
            match outcome {
                StrandedOutcome::Drop => {
                    native.groups.remove(&id);
                }
                StrandedOutcome::Retire => {
                    if let Some(group) = native.groups.get_mut(&id) {
                        // Same hand-off as a durable recovered exit: the
                        // confirmed cleanup pass keeps the handle pinned and
                        // retries retirement until it succeeds.
                        group.stranded = false;
                        group.owner = None;
                        group.confirmed = true;
                    }
                }
                StrandedOutcome::CommitPending(ended_at) => {
                    if let Some(group) = native.groups.get_mut(&id) {
                        group.ended_at = Some(ended_at);
                    }
                }
            }
        }
        errors
    }

    /// Settle one stranded pin whose group was just verified empty.
    fn settle_stranded_pin(
        &self,
        pin: &StrandedPin,
        errors: &mut Vec<(Id, io::Error)>,
    ) -> StrandedOutcome {
        let Some(owner) = &pin.owner else {
            return StrandedOutcome::Drop;
        };
        // A retry reuses the first observation time, so the Exited commit
        // stays identical (idempotent) across attempts.
        let ended_at = pin.ended_at.clone().unwrap_or_else(super::now_iso8601);
        let mut exited = owner.clone();
        exited.state = ExecState::Exited;
        exited.ended_at = Some(ended_at.clone());
        match self.inner.persistence.update(exited) {
            Ok(()) => {
                // No reservation release before the durable exit commit.
                self.inner.ledger.release(&pin.id);
                if owner.group_identity.is_some() {
                    StrandedOutcome::Retire
                } else {
                    StrandedOutcome::Drop
                }
            }
            Err(error) => {
                errors.push((pin.id.clone(), error));
                StrandedOutcome::CommitPending(ended_at)
            }
        }
    }

    /// Inspect at most eight previous executions per call, rotating fairly.
    /// A stored cancellation is the only authority to signal. Legacy records
    /// without native proof remain reserved. Failures retain handles and do
    /// not release resources; successful exit commits are picked up by the
    /// next consistent `refresh_recovery` snapshot. Stranded pins from
    /// bounded stop and launch-cleanup drains reconcile first, outside the
    /// native lock: Force is retried and a pin settles only on a verified
    /// empty observation.
    pub fn reconcile_native_recovery(&self) -> Vec<(Id, io::Error)> {
        let Some(config) = &self.inner.gate else {
            return vec![];
        };
        // A launch pin settled here hands its retirement to the confirmed
        // cleanup pass below in the same call.
        let mut errors = self.reconcile_stranded_pins(config);
        let mut native = self
            .inner
            .native_recovery
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut records: Vec<_> = {
            let ledger = self.inner.ledger.lock();
            // A failed global admission read cannot discard already restored
            // ownership. Each native action still validates its own durable
            // record and cancellation, so independent cleanup can continue.
            ledger.recovered.values().cloned().collect()
        };
        // A successful DB commit may precede a failed directory cleanup.
        // Keep that pinned object and retry cleanup after its reservation goes.
        let mut completed: Vec<_> = native
            .groups
            .iter()
            .filter(|(_, g)| g.confirmed)
            .map(|(id, _)| id.clone())
            .collect();
        completed.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let cleanup_start = if completed.is_empty() {
            0
        } else {
            native.cleanup_cursor % completed.len()
        };
        native.cleanup_cursor = cleanup_start + completed.len().min(8);
        for offset in 0..completed.len().min(8) {
            let id = &completed[(cleanup_start + offset) % completed.len()];
            match config
                .platform
                .retire_recovered_group(&native.groups[id].handle)
            {
                Ok(()) => {
                    native.groups.remove(id);
                }
                Err(error) => errors.push((id.clone(), error)),
            }
        }
        let ids: HashSet<_> = records.iter().map(|r| &r.id).collect();
        native
            .groups
            .retain(|id, group| group.confirmed || group.stranded || ids.contains(id));
        records.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        if records.is_empty() {
            return errors;
        }
        let start = native.cursor % records.len();
        let count = records.len().min(8);
        native.cursor = (start + count) % records.len();
        for offset in 0..count {
            let record = &records[(start + offset) % records.len()];
            let (Some(expected), Some(reference)) =
                (&record.group_identity, &record.group_reference)
            else {
                continue;
            };
            let result = (|| -> io::Result<bool> {
                let action = self.inner.persistence.recovered_action(record)?;
                if !native.groups.contains_key(&record.id) {
                    if native.groups.len() >= MAX_PINNED_GROUPS {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "native cleanup handle limit reached; reservation retained",
                        ));
                    }
                    let workload = WorkloadId::parse(record.id.as_str()).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "invalid recovered exec id")
                    })?;
                    let handle = config
                        .platform
                        .recover_group(&workload, reference, expected)?;
                    native.groups.insert(
                        record.id.clone(),
                        RecoveredGroup {
                            handle,
                            grace_at: None,
                            ended_at: None,
                            confirmed: false,
                            stranded: false,
                            owner: None,
                        },
                    );
                }
                let group = native
                    .groups
                    .get_mut(&record.id)
                    .expect("inserted recovered group");
                // Durable proof is immutable even while a handle remains pinned.
                if config.platform.recovery_identity(&group.handle)?.as_ref() != Some(expected) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "recovered native identity changed",
                    ));
                }
                config.platform.verify_recovered_root(
                    &group.handle,
                    record.identity.as_ref().ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "recovered root identity missing",
                        )
                    })?,
                )?;
                if !config.platform.is_empty(&group.handle)? && action == RecoveredAction::Stop {
                    match group.grace_at {
                        None => {
                            // Force remains available when this kernel cannot send
                            // cooperative signals via pidfds.
                            let _ = config
                                .platform
                                .terminate_owned(&group.handle, StopPhase::Grace);
                            group.grace_at = Some(Instant::now());
                        }
                        Some(at) if at.elapsed() >= Duration::from_secs(3) => {
                            config
                                .platform
                                .terminate_owned(&group.handle, StopPhase::Force)?;
                        }
                        _ => {}
                    }
                }
                if !config.platform.is_empty(&group.handle)? {
                    return Ok(false);
                }
                let ended = group.ended_at.get_or_insert_with(super::now_iso8601);
                self.inner
                    .persistence
                    .confirm_recovered_exit(record, ended)?;
                group.confirmed = true;
                // The group is deliberately retained until the durable proof
                // above succeeds, so a DB failure/restart can inspect it again.
                config.platform.retire_recovered_group(&group.handle)?;
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    native.groups.remove(&record.id);
                }
                Ok(false) => {
                    // Passive observation needs no retained FD between polls.
                    // Keep handles only through cancellation or a failed exit
                    // commit, so many surviving records cannot exhaust FDs.
                    if native
                        .groups
                        .get(&record.id)
                        .is_some_and(|g| g.grace_at.is_none() && g.ended_at.is_none())
                    {
                        native.groups.remove(&record.id);
                    }
                }
                Err(error) => errors.push((record.id.clone(), error)),
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::persistence::{ExecPersistence, Observer};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use term_contracts::ids::{ProcessIdentity, U64String};
    use term_contracts::metrics::PressureLevel;
    use term_contracts::mission::types::ArtifactRef;
    use term_contracts::snapshot::Capabilities;
    use term_contracts::workload::GroupRecoveryIdentity;
    use term_core::{AdmissionConfig, AdmissionHost};
    use term_platform::group::mock::{MockMember, MockPlatform};

    fn stranded_supervisor(platform: Arc<MockPlatform>) -> ExecSupervisor {
        supervisor_with(
            platform,
            Arc::new(Observer(Arc::new(|_record: ExecRecord| {}))),
        )
    }

    fn supervisor_with(
        platform: Arc<MockPlatform>,
        persistence: Arc<dyn ExecPersistence>,
    ) -> ExecSupervisor {
        ExecSupervisor::persistent(
            AdmissionConfig {
                logical_cpus: 8,
                managed_concurrency: 8,
                telemetry_stale_ms: 3_000,
                host_reserve_min_bytes: 2 << 30,
                host_reserve_percent: 15,
                managed_budget_percent: 50,
            },
            persistence,
            AdmissionHost {
                total_bytes: 16 << 30,
                available_bytes: Some(10 << 30),
                sample_age_ms: 0,
                reconciliation_required: false,
                pressure: PressureLevel::Normal,
            },
            GateConfig {
                helper_program: PathBuf::from("/nonexistent/iyagi-launch-helper"),
                directory: std::env::temp_dir(),
                platform,
                timeout: Duration::from_secs(1),
            },
        )
    }

    #[test]
    fn stranded_pin_retries_force_and_drops_on_a_verified_empty_group() {
        let platform = Arc::new(MockPlatform::new(Capabilities::observe_only("mock")));
        let group = platform.create_anonymous_group().expect("mock group");
        platform.set_members(
            &group.reference,
            vec![MockMember::alive(ProcessIdentity {
                pid: 4127,
                start_token: "start".into(),
                boot_id: "boot".into(),
            })],
        );
        let supervisor = stranded_supervisor(Arc::clone(&platform));
        let exec_id = Id::generate();
        assert!(supervisor.register_stranded_group(&exec_id, &group, None));
        assert!(
            supervisor.register_stranded_group(&exec_id, &group, None),
            "registration is idempotent per exec id"
        );
        // Still populated: reconciliation retries Force on its own worker.
        assert!(supervisor.reconcile_native_recovery().is_empty());
        assert_eq!(platform.terminate_log().len(), 1);
        assert_eq!(platform.terminate_log()[0].phase, StopPhase::Force);
        // The scripted member died from that force; the next pass observes
        // the group empty and drops the pin without signalling again.
        assert!(supervisor.reconcile_native_recovery().is_empty());
        assert_eq!(platform.terminate_log().len(), 1);
        // The pin is gone: later passes have nothing left to reconcile.
        assert!(supervisor.reconcile_native_recovery().is_empty());
        assert_eq!(platform.terminate_log().len(), 1);
    }

    #[test]
    fn stranded_pins_share_the_recovered_handle_budget() {
        let platform = Arc::new(MockPlatform::new(Capabilities::observe_only("mock")));
        let supervisor = stranded_supervisor(Arc::clone(&platform));
        for _ in 0..MAX_PINNED_GROUPS {
            let group = platform.create_anonymous_group().expect("mock group");
            assert!(supervisor.register_stranded_group(&Id::generate(), &group, None));
        }
        let overflow = platform.create_anonymous_group().expect("mock group");
        assert!(!supervisor.register_stranded_group(&Id::generate(), &overflow, None));
    }

    /// Records every transition; `fail` scripts a persistence outage.
    #[derive(Default)]
    struct FlakyStore {
        fail: AtomicBool,
        updates: Mutex<Vec<ExecRecord>>,
    }
    impl ExecPersistence for FlakyStore {
        fn prepare(&self, record: ExecRecord, _manifest: &[u8]) -> io::Result<ArtifactRef> {
            Ok(record.launch_manifest_ref)
        }
        fn update(&self, record: ExecRecord) -> io::Result<()> {
            if self.fail.load(Ordering::Acquire) {
                return Err(io::Error::other("scripted persistence outage"));
            }
            self.updates
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(record);
            Ok(())
        }
    }

    fn launch_cleanup_record(exec_id: &Id) -> ExecRecord {
        ExecRecord {
            id: exec_id.clone(),
            mission_id: Id::generate(),
            run_id: Id::generate(),
            state: ExecState::Stopping,
            identity: None,
            group_kind: None,
            group_reference: None,
            group_identity: Some(GroupRecoveryIdentity::CgroupV2 {
                boot_id: "boot".into(),
                kernel_id: "7".into(),
            }),
            resource_policy: crate::agent_runtime::fake::fake_binding().resource_policy,
            launch_manifest_ref: ArtifactRef {
                id: Id::generate(),
                sha256: "a".repeat(64),
                bytes: U64String::new(2).unwrap(),
                media_type: "application/json".into(),
            },
            owner_daemon_id: Id::generate(),
            started_at: None,
            ended_at: None,
            exit_code: None,
        }
    }

    /// A failed launch whose members outlived the cleanup drain has no
    /// ExecHandle: its pin keeps the reservation until reconciliation
    /// verifies the group empty, commits Exited (retrying a failed commit
    /// with the same observation time), and only then releases the
    /// reservation and hands the retained group to retirement.
    #[test]
    fn launch_cleanup_pin_commits_exited_before_releasing_the_reservation() {
        let platform = Arc::new(MockPlatform::new(Capabilities::observe_only("mock")));
        let group = platform.create_anonymous_group().expect("mock group");
        platform.set_members(
            &group.reference,
            vec![MockMember::alive(ProcessIdentity {
                pid: 4128,
                start_token: "start".into(),
                boot_id: "boot".into(),
            })],
        );
        let store = Arc::new(FlakyStore::default());
        let supervisor = supervisor_with(Arc::clone(&platform), store.clone());
        let exec_id = Id::generate();
        {
            let mut ledger = supervisor.inner.ledger.lock();
            ledger.reservations.insert(
                exec_id.clone(),
                crate::exec::ReservedExec {
                    reservation_bytes: 1 << 30,
                    cpu_slots: 1,
                },
            );
        }
        let owner = launch_cleanup_record(&exec_id);
        assert!(supervisor.register_stranded_group(&exec_id, &group, Some(owner)));

        // Populated: Force only, nothing committed or released.
        assert!(supervisor.reconcile_native_recovery().is_empty());
        assert_eq!(platform.terminate_log().len(), 1);
        assert!(store.updates.lock().unwrap().is_empty());
        assert!(supervisor.ledger().is_active(&exec_id));

        // Verified empty, but the Exited commit fails: the reservation and
        // the pin stay.
        store.fail.store(true, Ordering::Release);
        let errors = supervisor.reconcile_native_recovery();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, exec_id);
        assert!(supervisor.ledger().is_active(&exec_id));
        let pending = {
            let native = supervisor.inner.native_recovery.lock().unwrap();
            native.groups[&exec_id].ended_at.clone()
        };
        let pending = pending.expect("observation time is kept for the retry");

        // Storage recovers: Exited is durable with the first observation
        // time, then the reservation is released and the group retired.
        store.fail.store(false, Ordering::Release);
        assert!(supervisor.reconcile_native_recovery().is_empty());
        let updates = store.updates.lock().unwrap().clone();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].state, ExecState::Exited);
        assert_eq!(updates[0].ended_at.as_deref(), Some(pending.as_str()));
        assert!(!supervisor.ledger().is_active(&exec_id));
        let native = supervisor.inner.native_recovery.lock().unwrap();
        assert!(native.groups.is_empty(), "retired and unpinned");
        assert_eq!(platform.terminate_log().len(), 1, "no signal after empty");
    }

    /// A stop-path pin is dropped by its exec's finalize; launch-cleanup
    /// pins, which no finalize owns, are never dropped that way.
    #[test]
    fn finalize_drops_only_stop_path_pins() {
        let platform = Arc::new(MockPlatform::new(Capabilities::observe_only("mock")));
        let supervisor = stranded_supervisor(Arc::clone(&platform));
        let stop_id = Id::generate();
        let launch_id = Id::generate();
        let stop_group = platform.create_anonymous_group().expect("mock group");
        let launch_group = platform.create_anonymous_group().expect("mock group");
        let owner = launch_cleanup_record(&launch_id);
        assert!(supervisor.register_stranded_group(&stop_id, &stop_group, None));
        assert!(supervisor.register_stranded_group(&launch_id, &launch_group, Some(owner)));
        supervisor.drop_stranded_pin(&stop_id);
        supervisor.drop_stranded_pin(&launch_id);
        let native = supervisor.inner.native_recovery.lock().unwrap();
        assert!(!native.groups.contains_key(&stop_id));
        assert!(native.groups.contains_key(&launch_id));
    }
}
