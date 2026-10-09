//! Restore resource reservations; this never adopts a process or sends a signal.
use super::{persistence::same_launch, ExecLedger, ExecSupervisor, ReservedExec};
use std::{collections::HashMap, io};
use term_contracts::mission::types::{ExecRecord, ExecState};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn compatible(previous: &ExecRecord, current: &ExecRecord) -> bool {
    same_launch(previous, current)
        && previous.launch_manifest_ref == current.launch_manifest_ref
        && (previous.identity.is_none()
            || (previous.identity == current.identity
                && previous.group_kind == current.group_kind
                && previous.group_reference == current.group_reference
                && previous.group_identity == current.group_identity
                && previous.started_at == current.started_at))
        && (previous.state == current.state
            || matches!(
                (previous.state, current.state),
                (
                    ExecState::Prepared,
                    ExecState::Spawned
                        | ExecState::Stopping
                        | ExecState::Unknown
                        | ExecState::Exited
                ) | (
                    ExecState::Spawned,
                    ExecState::Stopping | ExecState::Unknown | ExecState::Exited
                ) | (ExecState::Stopping | ExecState::Unknown, ExecState::Exited)
            ))
}

impl ExecLedger {
    pub(super) fn require_recovery(&self) {
        self.lock().recovery_ready = false;
    }

    fn apply_recovery(&self, records: Vec<ExecRecord>) -> io::Result<usize> {
        let mut state = self.lock();
        state.recovery_ready = false;
        let mut next = HashMap::new();
        for record in records {
            if record.state == ExecState::Exited && record.ended_at.is_none() {
                return Err(invalid("recovered execution lacks termination evidence"));
            }
            if let Some(previous) = state.recovered.get(&record.id) {
                if !compatible(previous, &record) {
                    return Err(invalid(
                        "recovered execution changed its owned launch or regressed",
                    ));
                }
            } else if state.reservations.contains_key(&record.id) {
                return Err(invalid(
                    "recovery collides with a current execution reservation",
                ));
            }
            if next.insert(record.id.clone(), record).is_some() {
                return Err(invalid("duplicate recovered execution identity"));
            }
        }
        if state.recovered.keys().any(|id| !next.contains_key(id)) {
            return Err(invalid(
                "a recovered execution disappeared without termination evidence",
            ));
        }
        // Validate the entire snapshot before changing any reservation. In
        // particular, missing rows and partial reads cannot free resources.
        for record in next.values() {
            if record.state == ExecState::Exited {
                state.reservations.remove(&record.id);
            } else {
                state.reservations.insert(
                    record.id.clone(),
                    ReservedExec {
                        reservation_bytes: record.resource_policy.reservation_bytes.get(),
                        cpu_slots: record.resource_policy.cpu_slots,
                    },
                );
            }
        }
        next.retain(|_, record| record.state != ExecState::Exited);
        state.recovered = next;
        state.recovery_ready = true;
        Ok(state.recovered.len())
    }

    pub fn recovery_ready(&self) -> bool {
        self.lock().recovery_ready
    }
    pub fn recovered_count(&self) -> usize {
        self.lock().recovered.len()
    }
}

impl ExecSupervisor {
    /// Blocking DB work, called before first dispatch and periodically on
    /// the mission thread. Host refresh cannot clear this independent gate.
    /// A failed read retains all reservations and blocks new admissions;
    /// cleanup of children already owned by this supervisor can continue.
    pub fn refresh_recovery(&self) -> io::Result<usize> {
        let _guard = self
            .inner
            .recovery_guard
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let previous: Vec<_> = self.inner.ledger.lock().recovered.keys().cloned().collect();
        let records = match self.inner.persistence.recovery_records(&previous) {
            Ok(records) => records,
            Err(error) => {
                self.inner.ledger.require_recovery();
                return Err(error);
            }
        };
        self.inner.ledger.apply_recovery(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecError;
    use term_contracts::mission::types::Id;
    use term_contracts::{
        ids::U64String, metrics::PressureLevel, mission::types::ArtifactRef, snapshot::QueueReason,
    };
    use term_core::{AdmissionConfig, AdmissionHost, AdmissionRequest};

    fn config() -> AdmissionConfig {
        AdmissionConfig {
            logical_cpus: 8,
            managed_concurrency: 8,
            telemetry_stale_ms: 3000,
            host_reserve_min_bytes: 2 << 30,
            host_reserve_percent: 15,
            managed_budget_percent: 50,
        }
    }
    fn host() -> AdmissionHost {
        AdmissionHost {
            total_bytes: 16 << 30,
            available_bytes: Some(14 << 30),
            sample_age_ms: 0,
            reconciliation_required: false,
            pressure: PressureLevel::Normal,
        }
    }
    fn record() -> ExecRecord {
        let mut policy = crate::agent_runtime::fake::fake_binding().resource_policy;
        policy.reservation_bytes = U64String::new(2 << 30).unwrap();
        policy.cpu_slots = 1;
        ExecRecord {
            id: Id::generate(),
            mission_id: Id::generate(),
            run_id: Id::generate(),
            state: ExecState::Prepared,
            identity: None,
            group_kind: None,
            group_reference: None,
            group_identity: None,
            resource_policy: policy,
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
    fn admit(
        ledger: &ExecLedger,
        host: &AdmissionHost,
    ) -> Result<super::super::ExecReservation, ExecError> {
        ledger.try_admit_and_reserve(
            host,
            &Id::generate(),
            AdmissionRequest {
                reservation_bytes: 1 << 30,
                cpu_slots: 1,
            },
        )
    }
    fn denied(result: Result<super::super::ExecReservation, ExecError>, reason: QueueReason) {
        assert!(
            matches!(result, Err(ExecError::AdmissionDenied { reason: actual }) if actual == reason)
        );
    }

    #[test]
    fn restored_reservations_apply_all_resource_caps_even_when_old_policy_exceeds_new_caps() {
        for (name, reason) in [
            ("concurrency", QueueReason::WaitConcurrency),
            ("cpu", QueueReason::WaitCpuSlots),
            ("memory", QueueReason::WaitReservationBudget),
            ("headroom", QueueReason::WaitMemoryHeadroom),
        ] {
            let mut config = config();
            let mut host = host();
            let mut old = record();
            match name {
                "concurrency" => config.managed_concurrency = 1,
                "cpu" => config.logical_cpus = 2,
                "memory" => {
                    old.resource_policy.reservation_bytes = U64String::new(16 << 30).unwrap()
                }
                "headroom" => host.available_bytes = Some(4 << 30),
                _ => unreachable!(),
            }
            let ledger = ExecLedger::new(config);
            ledger.require_recovery();
            denied(admit(&ledger, &host), QueueReason::WaitTelemetry);
            assert_eq!(ledger.apply_recovery(vec![old.clone()]).unwrap(), 1);
            assert!(
                !ledger.release(&old.id),
                "generic release forgot recovered ownership"
            );
            denied(admit(&ledger, &host), reason);
        }
    }

    #[test]
    fn repeated_recovery_counts_once_and_releases_only_an_explicit_immutable_exit() {
        let ledger = ExecLedger::new(config());
        let current = admit(&ledger, &host()).unwrap();
        let mut old = record();
        for _ in 0..3 {
            ledger.apply_recovery(vec![old.clone()]).unwrap();
        }
        assert_eq!(ledger.active_count(), 2);
        assert_eq!(ledger.recovered_count(), 1);
        assert!(ledger.release(&current.exec_id));
        old.state = ExecState::Exited;
        assert!(ledger.apply_recovery(vec![old.clone()]).is_err());
        assert!(ledger.is_active(&old.id));
        old.ended_at = Some("2026-09-16T00:00:00Z".into());
        assert_eq!(ledger.apply_recovery(vec![old]).unwrap(), 0);
        assert_eq!(ledger.active_count(), 0);
        admit(&ledger, &host()).unwrap();
    }

    #[test]
    fn incomplete_or_rewritten_snapshots_preserve_reservations_and_block_new_admission() {
        for variant in [
            "missing",
            "duplicate",
            "owner",
            "policy",
            "manifest",
            "regression",
        ] {
            let ledger = ExecLedger::new(config());
            let mut old = record();
            old.state = ExecState::Stopping;
            ledger.apply_recovery(vec![old.clone()]).unwrap();
            let current = admit(&ledger, &host()).unwrap();
            let mut changed = old.clone();
            match variant {
                "owner" => changed.owner_daemon_id = Id::generate(),
                "policy" => changed.resource_policy.cpu_slots = 2,
                "manifest" => changed.launch_manifest_ref.id = Id::generate(),
                "regression" => changed.state = ExecState::Prepared,
                _ => {}
            }
            let next = match variant {
                "missing" => vec![],
                "duplicate" => vec![changed.clone(), changed],
                _ => vec![changed],
            };
            assert!(ledger.apply_recovery(next).is_err(), "{variant}");
            assert_eq!(ledger.active_count(), 2);
            denied(admit(&ledger, &host()), QueueReason::WaitTelemetry);
            assert!(
                ledger.release(&current.exec_id),
                "cleanup must work during recovery failure"
            );
            ledger.apply_recovery(vec![old]).unwrap();
            admit(&ledger, &host()).unwrap();
        }
    }

    #[test]
    fn recovery_cannot_steal_or_release_a_current_execution() {
        let ledger = ExecLedger::new(config());
        let current = admit(&ledger, &host()).unwrap();
        let mut conflict = record();
        conflict.id = current.exec_id.clone();
        conflict.state = ExecState::Exited;
        conflict.ended_at = Some("2026-09-16T00:00:00Z".into());
        assert!(ledger.apply_recovery(vec![conflict]).is_err());
        assert!(ledger.is_active(&current.exec_id));
        assert_eq!(ledger.recovered_count(), 0);
    }
}
