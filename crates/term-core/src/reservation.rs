//! Reservation ledger: admission decision + reservation in one critical
//! section (spec `03-resources.md` §3 step 9: "reservation을 같은 scheduler
//! critical section 안에서 할당하고 STARTING으로 변경").
//!
//! STARTING counts as active, so a reserved workload is visible to every
//! later admission pass immediately. The [`ReservationGuard`] is plain info:
//! dropping it does NOT release — the daemon owns the lifecycle and calls
//! [`ReservationLedger::release`] explicitly (exit, cancel, interrupt).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use term_contracts::ids::WorkloadId;
use term_contracts::snapshot::QueueReason;

use crate::admission::{ActiveWorkload, AdmissionConfig, AdmissionHost, AdmissionRequest};
use crate::error::CoreError;

/// Live reservations for the active (STARTING/RUNNING/STOPPING/DRAINING)
/// managed set. All methods take `&self`; the state hides behind one mutex.
pub struct ReservationLedger {
    config: AdmissionConfig,
    state: Mutex<LedgerState>,
}

#[derive(Default)]
struct LedgerState {
    slots: HashMap<WorkloadId, ReservedSlot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReservedSlot {
    reservation_bytes: u64,
    cpu_slots: u32,
    /// Last observed resident estimate `M_i`; `None` until first sample.
    resident_bytes: Option<u64>,
}

/// Info about a live reservation. Returned **by value**; dropping it does not
/// release the reservation (explicit release only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationGuard {
    pub workload_id: WorkloadId,
    pub reservation_bytes: u64,
    pub cpu_slots: u32,
}

impl ReservationLedger {
    pub fn new(config: AdmissionConfig) -> Self {
        Self {
            config,
            state: Mutex::new(LedgerState::default()),
        }
    }

    pub fn config(&self) -> &AdmissionConfig {
        &self.config
    }

    fn lock(&self) -> MutexGuard<'_, LedgerState> {
        // Every ledger mutation is a single HashMap op performed after the
        // admission read, so a panicked owner cannot leave half-written
        // state; recover the lock instead of poisoning the daemon.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Atomic decide + reserve: builds the active set from ledger state and
    /// runs [`AdmissionConfig::decide`] inside the same critical section that
    /// inserts the reservation. Returns the guard on `ADMIT`, the first
    /// failing [`QueueReason`] otherwise (nothing is reserved on denial).
    pub fn try_admit_and_reserve(
        &self,
        host: &AdmissionHost,
        workload_id: WorkloadId,
        request: AdmissionRequest,
    ) -> Result<ReservationGuard, CoreError> {
        let mut state = self.lock();
        if state.slots.contains_key(&workload_id) {
            return Err(CoreError::DuplicateReservation { workload_id });
        }
        let active: Vec<ActiveWorkload> = state
            .slots
            .values()
            .map(|slot| ActiveWorkload {
                reservation_bytes: slot.reservation_bytes,
                resident_bytes: slot.resident_bytes,
                cpu_slots: slot.cpu_slots,
            })
            .collect();
        let input = host.into_input(active, request);
        match self.config.decide(&input) {
            QueueReason::Admit => {
                let guard = ReservationGuard {
                    reservation_bytes: input.request.reservation_bytes,
                    cpu_slots: input.request.cpu_slots,
                    workload_id: workload_id.clone(),
                };
                state.slots.insert(
                    workload_id,
                    ReservedSlot {
                        reservation_bytes: guard.reservation_bytes,
                        cpu_slots: guard.cpu_slots,
                        resident_bytes: None,
                    },
                );
                Ok(guard)
            }
            reason => Err(CoreError::AdmissionDenied { reason }),
        }
    }

    /// Reserve without an admission check. Only for callers that already
    /// hold a decision made against this exact ledger state (the daemon's
    /// normal path is [`Self::try_admit_and_reserve`]).
    pub fn reserve(
        &self,
        workload_id: WorkloadId,
        reservation_bytes: u64,
        cpu_slots: u32,
    ) -> Result<ReservationGuard, CoreError> {
        let mut state = self.lock();
        if state.slots.contains_key(&workload_id) {
            return Err(CoreError::DuplicateReservation { workload_id });
        }
        state.slots.insert(
            workload_id.clone(),
            ReservedSlot {
                reservation_bytes,
                cpu_slots,
                resident_bytes: None,
            },
        );
        Ok(ReservationGuard {
            workload_id,
            reservation_bytes,
            cpu_slots,
        })
    }

    /// Explicit release. Returns `true` exactly once per reservation:
    /// a double release returns `false` and reserves nothing.
    pub fn release(&self, workload_id: &WorkloadId) -> bool {
        self.lock().slots.remove(workload_id).is_some()
    }

    /// Update the last observed resident estimate `M_i` for a live
    /// reservation (spec `03-resources.md` §3: unknown/stale → `None` → the
    /// whole reservation stays pending in `P`). Returns `false` for an
    /// unknown workload id.
    pub fn update_resident(&self, workload_id: &WorkloadId, resident_bytes: Option<u64>) -> bool {
        let mut state = self.lock();
        match state.slots.get_mut(workload_id) {
            Some(slot) => {
                slot.resident_bytes = resident_bytes;
                true
            }
            None => false,
        }
    }

    pub fn is_active(&self, workload_id: &WorkloadId) -> bool {
        self.lock().slots.contains_key(workload_id)
    }

    /// Number of live reservations (STARTING included).
    pub fn active_count(&self) -> usize {
        self.lock().slots.len()
    }

    /// Active set exactly as admission math sees it (order unspecified).
    pub fn active_workloads(&self) -> Vec<ActiveWorkload> {
        self.lock()
            .slots
            .values()
            .map(|slot| ActiveWorkload {
                reservation_bytes: slot.reservation_bytes,
                resident_bytes: slot.resident_bytes,
                cpu_slots: slot.cpu_slots,
            })
            .collect()
    }

    /// Live reservations with ids, for snapshots/UI.
    pub fn reservations(&self) -> Vec<(WorkloadId, ActiveWorkload)> {
        self.lock()
            .slots
            .iter()
            .map(|(id, slot)| {
                (
                    id.clone(),
                    ActiveWorkload {
                        reservation_bytes: slot.reservation_bytes,
                        resident_bytes: slot.resident_bytes,
                        cpu_slots: slot.cpu_slots,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::AdmissionConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use term_contracts::defaults::load_spec_defaults;
    use term_contracts::metrics::PressureLevel;

    const GIB: u64 = 1 << 30;

    fn config(logical_cpus: u32) -> AdmissionConfig {
        AdmissionConfig::from_defaults(&load_spec_defaults().expect("spec defaults"), logical_cpus)
    }

    fn healthy_host(total: u64, available: u64) -> AdmissionHost {
        AdmissionHost {
            total_bytes: total,
            available_bytes: Some(available),
            sample_age_ms: 0,
            reconciliation_required: false,
            pressure: PressureLevel::Normal,
        }
    }

    fn request() -> AdmissionRequest {
        AdmissionRequest {
            reservation_bytes: 2 * GIB,
            cpu_slots: 1,
        }
    }

    fn id(n: u64) -> WorkloadId {
        WorkloadId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).expect("valid v4 id")
    }

    #[test]
    fn admit_reserves_counts_and_reports_guard_info() {
        let ledger = ReservationLedger::new(config(8));
        let guard = ledger
            .try_admit_and_reserve(&healthy_host(16 * GIB, 10 * GIB), id(1), request())
            .expect("empty host admits");
        assert_eq!(guard.workload_id, id(1));
        assert_eq!(guard.reservation_bytes, 2 * GIB);
        assert_eq!(guard.cpu_slots, 1);
        assert_eq!(ledger.active_count(), 1);
        assert!(ledger.is_active(&id(1)));
        // Cap is 2: a second concurrent launch still admits...
        ledger
            .try_admit_and_reserve(&healthy_host(16 * GIB, 10 * GIB), id(2), request())
            .expect("second launch admits within the cap");
        // ...and once STARTING counts as active, the third waits on
        // concurrency even though memory math would still pass.
        assert_eq!(ledger.active_count(), 2);
        let denied = ledger
            .try_admit_and_reserve(&healthy_host(16 * GIB, 10 * GIB), id(3), request())
            .unwrap_err();
        assert_eq!(
            denied,
            CoreError::AdmissionDenied {
                reason: QueueReason::WaitConcurrency
            }
        );
        assert_eq!(ledger.active_count(), 2);
    }

    #[test]
    fn denied_admission_reserves_nothing() {
        let ledger = ReservationLedger::new(config(8));
        let mut stale = healthy_host(16 * GIB, 10 * GIB);
        stale.sample_age_ms = 3_001;
        let err = ledger
            .try_admit_and_reserve(&stale, id(7), request())
            .unwrap_err();
        assert_eq!(
            err,
            CoreError::AdmissionDenied {
                reason: QueueReason::WaitTelemetry
            }
        );
        assert_eq!(ledger.active_count(), 0);
        assert!(ledger.reservations().is_empty());
    }

    #[test]
    fn double_release_returns_true_exactly_once() {
        let ledger = ReservationLedger::new(config(8));
        ledger
            .try_admit_and_reserve(&healthy_host(16 * GIB, 10 * GIB), id(3), request())
            .expect("admits");
        assert!(ledger.release(&id(3)));
        assert!(!ledger.release(&id(3)));
        assert!(!ledger.release(&id(4)));
        assert_eq!(ledger.active_count(), 0);
        // Slot is reusable after release.
        ledger
            .try_admit_and_reserve(&healthy_host(16 * GIB, 10 * GIB), id(3), request())
            .expect("re-admits after release");
    }

    #[test]
    fn duplicate_reservation_for_a_live_workload_is_rejected() {
        let ledger = ReservationLedger::new(config(8));
        ledger
            .try_admit_and_reserve(&healthy_host(16 * GIB, 10 * GIB), id(5), request())
            .expect("admits");
        let err = ledger
            .try_admit_and_reserve(&healthy_host(16 * GIB, 10 * GIB), id(5), request())
            .unwrap_err();
        assert_eq!(err, CoreError::DuplicateReservation { workload_id: id(5) });
        assert_eq!(ledger.active_count(), 1);
        // The raw reserve primitive enforces the same invariant.
        assert_eq!(
            ledger.reserve(id(5), GIB, 1).unwrap_err(),
            CoreError::DuplicateReservation { workload_id: id(5) }
        );
    }

    #[test]
    fn dropping_the_guard_does_not_release() {
        let ledger = ReservationLedger::new(config(8));
        {
            let _guard = ledger
                .try_admit_and_reserve(&healthy_host(16 * GIB, 10 * GIB), id(6), request())
                .expect("admits");
        }
        assert_eq!(ledger.active_count(), 1, "guard drop must not release");
        assert!(ledger.release(&id(6)));
    }

    #[test]
    fn resident_over_reservation_unlocks_headroom_through_the_ledger() {
        // Fixture `resident-over-reservation` via the ledger: exact-headroom
        // host where the first workload's unknown resident blocks the second
        // until an above-reservation resident zeroes its pending share.
        let ledger = ReservationLedger::new(config(8));
        let host = healthy_host(16 * GIB, 4_724_464_026);
        ledger
            .try_admit_and_reserve(&host, id(1), request())
            .expect("first admits");
        // P = 2 GiB (no resident sample yet) -> A - S - P = 0 < R_new.
        let blocked = ledger
            .try_admit_and_reserve(&host, id(2), request())
            .unwrap_err();
        assert_eq!(
            blocked,
            CoreError::AdmissionDenied {
                reason: QueueReason::WaitMemoryHeadroom
            }
        );
        assert!(ledger.update_resident(&id(1), Some(3 * GIB)));
        assert!(!ledger.update_resident(&id(42), Some(GIB)));
        // P clamps at 0 -> headroom is A - S = 2 GiB exactly -> admit.
        ledger
            .try_admit_and_reserve(&host, id(2), request())
            .expect("resident over reservation admits the next launch");
        // A stale (None) resident makes the ledger conservative again.
        assert!(ledger.update_resident(&id(2), None));
        let active = ledger.active_workloads();
        assert!(active.contains(&ActiveWorkload {
            reservation_bytes: 2 * GIB,
            resident_bytes: None,
            cpu_slots: 1,
        }));
    }

    #[test]
    fn hundred_racing_launches_hit_the_concurrency_cap_exactly() {
        let ledger = ReservationLedger::new(config(8));
        let host = healthy_host(16 * GIB, 10 * GIB);
        let admits = AtomicUsize::new(0);
        let denials: Mutex<Vec<QueueReason>> = Mutex::new(Vec::new());

        thread::scope(|scope| {
            for n in 0..100u64 {
                let ledger = &ledger;
                let host = &host;
                let admits = &admits;
                let denials = &denials;
                scope.spawn(
                    move || match ledger.try_admit_and_reserve(host, id(n), request()) {
                        Ok(guard) => {
                            admits.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(guard.reservation_bytes, 2 * GIB);
                        }
                        Err(CoreError::AdmissionDenied { reason }) => {
                            denials.lock().expect("denial sink").push(reason);
                        }
                        Err(other) => panic!("unexpected ledger error: {other}"),
                    },
                );
            }
        });

        assert_eq!(admits.load(Ordering::SeqCst), 2, "exactly the cap admits");
        let denials = denials.into_inner().expect("denial sink");
        assert_eq!(denials.len(), 98);
        assert!(
            denials.iter().all(|r| *r == QueueReason::WaitConcurrency),
            "every denial must be WAIT_CONCURRENCY, got {denials:?}"
        );
        assert_eq!(ledger.active_count(), 2);
        let reserved: u64 = ledger
            .active_workloads()
            .iter()
            .map(|w| w.reservation_bytes)
            .sum();
        assert_eq!(reserved, 4 * GIB, "no cap breach: budget is 8 GiB");
    }

    #[test]
    fn hundred_racing_launches_never_exceed_the_budget_boundary() {
        // Concurrency raised so the byte budget binds instead: 8 GiB host ->
        // B = 4 GiB, R_new = 2 GiB: the third launch must fail on budget.
        // Headroom is exactly tight for two (A - S - P == R_new), proving the
        // boundary is held atomically, never exceeded by a byte.
        let cfg = AdmissionConfig {
            managed_concurrency: 64,
            ..config(8)
        };
        let ledger = ReservationLedger::new(cfg);
        let host = healthy_host(8 * GIB, 6 * GIB);
        let admits = AtomicUsize::new(0);
        let denials: Mutex<Vec<QueueReason>> = Mutex::new(Vec::new());

        thread::scope(|scope| {
            for n in 0..100u64 {
                let ledger = &ledger;
                let host = &host;
                let admits = &admits;
                let denials = &denials;
                scope.spawn(
                    move || match ledger.try_admit_and_reserve(host, id(n), request()) {
                        Ok(_) => {
                            admits.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(CoreError::AdmissionDenied { reason }) => {
                            denials.lock().expect("denial sink").push(reason)
                        }
                        Err(other) => panic!("unexpected ledger error: {other}"),
                    },
                );
            }
        });

        assert_eq!(admits.load(Ordering::SeqCst), 2, "budget fits exactly two");
        let denials = denials.into_inner().expect("denial sink");
        assert_eq!(denials.len(), 98);
        assert!(
            denials
                .iter()
                .all(|r| *r == QueueReason::WaitReservationBudget),
            "every denial must be WAIT_RESERVATION_BUDGET, got {denials:?}"
        );
        let reserved: u64 = ledger
            .active_workloads()
            .iter()
            .map(|w| w.reservation_bytes)
            .sum();
        assert_eq!(reserved, 4 * GIB, "sum equals B exactly, never over");
    }
}
