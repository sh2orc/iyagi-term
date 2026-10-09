//! Conservative deterministic admission (spec `03-resources.md` §3).
//!
//! Pure integer math on byte counts: no floats, no OS calls, no interior
//! mutability. The decision order is normative and mirrors the Python
//! reference `admission()` in `docs/implementation/verify_spec.py`; the 17
//! fixtures in `docs/implementation/admission-cases.json` are the parity gate
//! (`tests/admission_fixtures.rs`).
//!
//! Variables (all bytes unless noted):
//!
//! ```text
//! T = host physical total          A = host available (sample <= 3s old)
//! S = max(2 GiB, ceil(T*0.15))     B = floor(T*0.50)   (managed budget)
//! R_i / M_i = active reservation / last resident estimate (None = 0)
//! P = sum(max(0, R_i - M_i))       R_new = new reservation
//! C = max(1, logical_cpus / 2)     (CPU scheduling slots)
//! ```
//!
//! Order: `WAIT_TELEMETRY` → `RESOURCE_UNSCHEDULABLE` → `WAIT_HOST_PRESSURE`
//! → `WAIT_CONCURRENCY` → `WAIT_CPU_SLOTS` → `WAIT_RESERVATION_BUDGET` →
//! `WAIT_MEMORY_HEADROOM` → `ADMIT`.

use term_contracts::defaults::Defaults;
use term_contracts::metrics::PressureLevel;
use term_contracts::snapshot::QueueReason;

/// Static admission policy from `defaults.json` plus the host's CPU count.
///
/// `logical_cpus` lives in the config (not the per-request input) because it
/// is host state: `C = max(1, logical_cpus / 2)` (spec `03-resources.md` §3).
/// Sustained staleness threshold after which admission fails open (10× the
/// 3 s freshness window): a telemetry loop the supervisor cannot bring back
/// must not freeze every launch behind WAIT_TELEMETRY forever.
const TELEMETRY_FAIL_OPEN_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionConfig {
    /// Logical cores of the host; drives the CPU slot capacity `C`.
    pub logical_cpus: u32,
    /// `limits.managed_concurrency` (default 2): running+starting managed cap.
    pub managed_concurrency: u32,
    /// `timing_ms.telemetry_stale` (default 3000): host sample freshness.
    pub telemetry_stale_ms: u64,
    /// `admission.host_reserve_min_bytes` (default 2 GiB): floor of `S`.
    pub host_reserve_min_bytes: u64,
    /// `admission.host_reserve_percent` (default 15): `S = ceil(T*15%)`.
    pub host_reserve_percent: u64,
    /// `admission.managed_budget_percent` (default 50): `B = floor(T*50%)`.
    pub managed_budget_percent: u64,
}

impl AdmissionConfig {
    pub fn from_defaults(defaults: &Defaults, logical_cpus: u32) -> Self {
        Self {
            logical_cpus,
            managed_concurrency: defaults.limits.managed_concurrency,
            telemetry_stale_ms: defaults.timing_ms.telemetry_stale,
            host_reserve_min_bytes: defaults.admission.host_reserve_min_bytes,
            host_reserve_percent: defaults.admission.host_reserve_percent,
            managed_budget_percent: defaults.admission.managed_budget_percent,
        }
    }

    /// CPU scheduling slots `C = max(1, floor(logical_cpus / 2))`.
    pub fn cpu_slot_capacity(&self) -> u32 {
        (self.logical_cpus / 2).max(1)
    }

    /// Managed reservation budget `B = floor(T * managed_budget_percent / 100)`.
    pub fn managed_budget_bytes(&self, total_bytes: u64) -> u64 {
        percent_floor(total_bytes, self.managed_budget_percent)
    }

    /// Host safety reserve `S = max(host_reserve_min_bytes, ceil(T*pct/100))`.
    pub fn host_reserve_bytes(&self, total_bytes: u64) -> u64 {
        host_reserve_bytes(
            total_bytes,
            self.host_reserve_min_bytes,
            self.host_reserve_percent,
        )
    }

    /// The decision function. Checks run in the normative order; the first
    /// failing check wins so the UI can show the most fundamental reason.
    pub fn decide(&self, input: &AdmissionInput) -> QueueReason {
        // (2) Reconciliation unresolved or host sample stale: no safe math.
        //     A missing available value is telemetry staleness, not zero.
        let stale = input.reconciliation_required
            || input.sample_age_ms > self.telemetry_stale_ms
            || input.available_bytes.is_none();
        // Sustained staleness fails OPEN instead of blocking forever: the
        // supervisor restarts a panicked telemetry loop within seconds, but
        // a wedge it cannot recover from must not freeze every future
        // launch behind WAIT_TELEMETRY. Host-derived checks (pressure,
        // memory headroom) are skipped — their inputs are exactly what is
        // unknown — while policy checks that need no host data still apply.
        let fail_open = stale
            && input.sample_age_ms >= TELEMETRY_FAIL_OPEN_MS
            && !input.reconciliation_required;
        if stale && !fail_open {
            return QueueReason::WaitTelemetry;
        }

        let budget = self.managed_budget_bytes(input.total_bytes);
        let slot_capacity = self.cpu_slot_capacity();
        let request = &input.request;

        // (3) The request alone exceeds policy: only a config change helps.
        if request.reservation_bytes > budget || request.cpu_slots > slot_capacity {
            return QueueReason::ResourceUnschedulable;
        }

        // (4) CRITICAL pressure blocks every new managed start — skipped
        //     under fail-open (the sample behind it is the stale part).
        if !fail_open && input.pressure == PressureLevel::Critical {
            return QueueReason::WaitHostPressure;
        }

        // (5) managed_concurrency counts STARTING as active (03 §3).
        if input.active.len() >= self.managed_concurrency as usize {
            return QueueReason::WaitConcurrency;
        }

        // (6) Existing cpu_slots sum + new must stay within C.
        let active_slots = input
            .active
            .iter()
            .try_fold(0u32, |acc, w| acc.checked_add(w.cpu_slots))
            .unwrap_or(u32::MAX);
        if active_slots.saturating_add(request.cpu_slots) > slot_capacity {
            return QueueReason::WaitCpuSlots;
        }

        // (7) Existing reservations + new must stay within B.
        let active_reserved = input
            .active
            .iter()
            .try_fold(0u64, |acc, w| acc.checked_add(w.reservation_bytes))
            .unwrap_or(u64::MAX);
        let within_budget = match active_reserved.checked_add(request.reservation_bytes) {
            Some(sum) => sum <= budget,
            None => false, // overflow: certainly over budget, deny
        };
        if !within_budget {
            return QueueReason::WaitReservationBudget;
        }

        // (8) A - S - P >= R_new. Checked subtraction: an underflowed
        //     difference counts as "less than R_new", exactly like the
        //     Python reference where the difference may go negative.
        // (8) Under fail-open the headroom math is exactly the unknown, so
        //     it cannot gate; everything else above already ran.
        if fail_open {
            return QueueReason::Admit;
        }
        let reserve = self.host_reserve_bytes(input.total_bytes);
        let pending = pending_reservation(&input.active);
        let headroom = input
            .available_bytes
            .and_then(|a| a.checked_sub(reserve))
            .and_then(|h| h.checked_sub(pending));
        match headroom {
            Some(h) if h >= request.reservation_bytes => QueueReason::Admit,
            _ => QueueReason::WaitMemoryHeadroom,
        }
    }
}

/// Host telemetry facts one admission pass runs against. `available_bytes` is
/// `None` when the sample is stale/unavailable — the caller must treat that
/// as stale telemetry, never as zero (spec `03-resources.md` §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionHost {
    pub total_bytes: u64,
    pub available_bytes: Option<u64>,
    /// Age of the host sample in monotonic milliseconds.
    pub sample_age_ms: u64,
    pub reconciliation_required: bool,
    pub pressure: PressureLevel,
}

impl AdmissionHost {
    /// Expand host facts with the active set and the candidate request.
    pub fn into_input(
        self,
        active: Vec<ActiveWorkload>,
        request: AdmissionRequest,
    ) -> AdmissionInput {
        AdmissionInput {
            total_bytes: self.total_bytes,
            available_bytes: self.available_bytes,
            sample_age_ms: self.sample_age_ms,
            reconciliation_required: self.reconciliation_required,
            pressure: self.pressure,
            active,
            request,
        }
    }
}

/// One active (STARTING/RUNNING/STOPPING/DRAINING) managed workload as the
/// admission math sees it. `resident_bytes: None` = no fresh measurement,
/// which counts as 0 and therefore keeps the whole reservation in `P`
/// (conservative; spec `03-resources.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveWorkload {
    pub reservation_bytes: u64,
    pub resident_bytes: Option<u64>,
    pub cpu_slots: u32,
}

/// The launch's resource claim under evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionRequest {
    pub reservation_bytes: u64,
    pub cpu_slots: u32,
}

/// Everything one `decide()` call needs: host facts + active set + request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionInput {
    pub total_bytes: u64,
    pub available_bytes: Option<u64>,
    pub sample_age_ms: u64,
    pub reconciliation_required: bool,
    pub pressure: PressureLevel,
    pub active: Vec<ActiveWorkload>,
    pub request: AdmissionRequest,
}

/// Unused reservation headroom of the active set:
/// `P = sum(max(0, R_i - M_i))`. Per-workload clamp at 0 (a workload
/// resident above its reservation never *adds* budget), sums saturate at
/// `u64::MAX` (overflow stays conservative).
pub fn pending_reservation(active: &[ActiveWorkload]) -> u64 {
    active
        .iter()
        .try_fold(0u64, |acc, w| {
            let unused = w
                .reservation_bytes
                .saturating_sub(w.resident_bytes.unwrap_or(0));
            acc.checked_add(unused)
        })
        .unwrap_or(u64::MAX)
}

/// `S = max(min_bytes, ceil(total * percent / 100))` — host safety reserve.
/// Percent scaling runs in `u128` so byte counts near the `U64String` SQLite
/// bound cannot wrap; the result is clamped back into `u64`.
pub fn host_reserve_bytes(total_bytes: u64, min_bytes: u64, percent: u64) -> u64 {
    let scaled = (total_bytes as u128 * percent as u128).div_ceil(100);
    min_bytes.max(scaled.min(u64::MAX as u128) as u64)
}

/// `floor(total * percent / 100)` with the same overflow discipline.
pub fn percent_floor(total_bytes: u64, percent: u64) -> u64 {
    ((total_bytes as u128 * percent as u128) / 100).min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;
    /// Fixture base host: 16 GiB total, 10 GiB available, 8 logical CPUs.
    const T: u64 = 16 * GIB;

    fn config() -> AdmissionConfig {
        AdmissionConfig {
            logical_cpus: 8,
            managed_concurrency: 2,
            telemetry_stale_ms: 3_000,
            host_reserve_min_bytes: 2 * GIB,
            host_reserve_percent: 15,
            managed_budget_percent: 50,
        }
    }

    fn active(reservation: u64, resident: Option<u64>, slots: u32) -> ActiveWorkload {
        ActiveWorkload {
            reservation_bytes: reservation,
            resident_bytes: resident,
            cpu_slots: slots,
        }
    }

    fn input(
        total: u64,
        available: Option<u64>,
        age_ms: u64,
        active_set: Vec<ActiveWorkload>,
    ) -> AdmissionInput {
        AdmissionInput {
            total_bytes: total,
            available_bytes: available,
            sample_age_ms: age_ms,
            reconciliation_required: false,
            pressure: PressureLevel::Normal,
            active: active_set,
            request: AdmissionRequest {
                reservation_bytes: 2 * GIB,
                cpu_slots: 1,
            },
        }
    }

    #[test]
    fn from_defaults_matches_the_spec_asset() {
        let defaults = term_contracts::defaults::load_spec_defaults()
            .expect("docs/implementation/defaults.json must parse");
        assert_eq!(AdmissionConfig::from_defaults(&defaults, 8), config());
    }

    #[test]
    fn derived_quantities_follow_the_spec_formulas() {
        let cfg = config();
        // S = max(2 GiB, ceil(16 GiB * 0.15)) = 2576980378 (ceil, not floor).
        assert_eq!(cfg.host_reserve_bytes(T), 2_576_980_378);
        // Small host: the 2 GiB floor binds.
        assert_eq!(cfg.host_reserve_bytes(4 * GIB), 2 * GIB);
        // B = floor(T * 0.50).
        assert_eq!(cfg.managed_budget_bytes(T), 8 * GIB);
        // C = max(1, logical_cpus / 2); single-core host keeps one slot.
        assert_eq!(cfg.cpu_slot_capacity(), 4);
        assert_eq!(
            AdmissionConfig {
                logical_cpus: 1,
                ..config()
            }
            .cpu_slot_capacity(),
            1
        );
        assert_eq!(
            AdmissionConfig {
                logical_cpus: 0,
                ..config()
            }
            .cpu_slot_capacity(),
            1
        );
    }

    #[test]
    fn telemetry_gate_uses_strict_inequality_and_treats_none_as_stale() {
        // Exactly 3000 ms old is fresh; 3001 ms is stale (fixture boundary).
        assert_eq!(
            config().decide(&input(T, Some(10 * GIB), 3_000, vec![])),
            QueueReason::Admit
        );
        assert_eq!(
            config().decide(&input(T, Some(10 * GIB), 3_001, vec![])),
            QueueReason::WaitTelemetry
        );
        // Unknown availability is stale telemetry, never zero.
        assert_eq!(
            config().decide(&input(T, None, 0, vec![])),
            QueueReason::WaitTelemetry
        );
        let mut reconciling = input(T, Some(10 * GIB), 0, vec![]);
        reconciling.reconciliation_required = true;
        assert_eq!(config().decide(&reconciling), QueueReason::WaitTelemetry);
    }

    #[test]
    fn schedulability_boundaries_are_exact() {
        let cfg = config();
        // Reservation exactly B passes the schedulability gate... (available
        // is raised so the later headroom check also passes: A - S >= 8 GiB).
        let exact = AdmissionInput {
            request: AdmissionRequest {
                reservation_bytes: 8 * GIB,
                cpu_slots: 1,
            },
            ..input(T, Some(11 * GIB), 0, vec![])
        };
        assert_eq!(cfg.decide(&exact), QueueReason::Admit);
        // ...one byte over is unschedulable (fixture `oversized`).
        let over = AdmissionInput {
            request: AdmissionRequest {
                reservation_bytes: 8 * GIB + 1,
                cpu_slots: 1,
            },
            ..input(T, Some(10 * GIB), 0, vec![])
        };
        assert_eq!(cfg.decide(&over), QueueReason::ResourceUnschedulable);
        // Slots exactly C pass, C+1 do not (fixture `too-many-slots`).
        let slots_exact = AdmissionInput {
            request: AdmissionRequest {
                reservation_bytes: 2 * GIB,
                cpu_slots: 4,
            },
            ..input(T, Some(10 * GIB), 0, vec![])
        };
        assert_eq!(cfg.decide(&slots_exact), QueueReason::Admit);
        let slots_over = AdmissionInput {
            request: AdmissionRequest {
                reservation_bytes: 2 * GIB,
                cpu_slots: 5,
            },
            ..input(T, Some(10 * GIB), 0, vec![])
        };
        assert_eq!(cfg.decide(&slots_over), QueueReason::ResourceUnschedulable);
    }

    #[test]
    fn critical_pressure_blocks_and_warning_allows_when_formula_passes() {
        let mut critical = input(T, Some(10 * GIB), 0, vec![]);
        critical.pressure = PressureLevel::Critical;
        assert_eq!(config().decide(&critical), QueueReason::WaitHostPressure);
        let mut warning = input(T, Some(10 * GIB), 0, vec![]);
        warning.pressure = PressureLevel::Warning;
        assert_eq!(config().decide(&warning), QueueReason::Admit);
    }

    #[test]
    fn concurrency_slot_and_budget_checks_hit_in_order() {
        let cfg = config();
        // Two active (any state) -> WAIT_CONCURRENCY, even though slots and
        // budget would also overflow: concurrency is checked first.
        let full = input(
            T,
            Some(10 * GIB),
            0,
            vec![active(2 * GIB, Some(GIB), 1), active(2 * GIB, Some(GIB), 1)],
        );
        assert_eq!(cfg.decide(&full), QueueReason::WaitConcurrency);
        // One active with 4 slots: 4+1 > C -> WAIT_CPU_SLOTS.
        let slots_busy = input(T, Some(10 * GIB), 0, vec![active(2 * GIB, Some(GIB), 4)]);
        assert_eq!(cfg.decide(&slots_busy), QueueReason::WaitCpuSlots);
        // One active with 7 GiB reserved: 7+2 > B -> WAIT_RESERVATION_BUDGET.
        let budget_full = input(T, Some(10 * GIB), 0, vec![active(7 * GIB, Some(GIB), 1)]);
        assert_eq!(cfg.decide(&budget_full), QueueReason::WaitReservationBudget);
        // Boundary: existing 6 GiB + 2 GiB == B exactly -> passes budget.
        let budget_edge = input(
            T,
            Some(10 * GIB),
            0,
            vec![active(6 * GIB, Some(6 * GIB), 1)],
        );
        assert_eq!(cfg.decide(&budget_edge), QueueReason::Admit);
    }

    #[test]
    fn headroom_boundary_admits_exact_and_waits_one_byte_short() {
        let cfg = config();
        // A - S - P == R_new exactly (4724464026 - 2576980378 - 0 == 2 GiB).
        assert_eq!(
            cfg.decide(&input(T, Some(4_724_464_026), 0, vec![])),
            QueueReason::Admit
        );
        // One byte less available -> WAIT_MEMORY_HEADROOM.
        assert_eq!(
            cfg.decide(&input(T, Some(4_724_464_025), 0, vec![])),
            QueueReason::WaitMemoryHeadroom
        );
        // Underflowed difference (A < S) is "less than R_new".
        assert_eq!(
            cfg.decide(&input(T, Some(GIB), 0, vec![])),
            QueueReason::WaitMemoryHeadroom
        );
    }

    #[test]
    fn pending_clamps_per_workload_and_unknown_resident_is_conservative() {
        let cfg = config();
        // Resident above reservation contributes 0 to P (per-workload clamp).
        let over = input(
            T,
            Some(4_724_464_026),
            0,
            vec![active(2 * GIB, Some(3 * GIB), 1)],
        );
        assert_eq!(cfg.decide(&over), QueueReason::Admit);
        // Unknown resident keeps the whole reservation in P.
        let unknown = input(T, Some(6 * GIB), 0, vec![active(2 * GIB, None, 1)]);
        assert_eq!(cfg.decide(&unknown), QueueReason::WaitMemoryHeadroom);
        assert_eq!(
            pending_reservation(&[active(3 * GIB, Some(2 * GIB), 1)]),
            GIB
        );
        assert_eq!(pending_reservation(&[active(2 * GIB, None, 1)]), 2 * GIB);
        assert_eq!(
            pending_reservation(&[active(2 * GIB, Some(3 * GIB), 1), active(GIB, None, 1)]),
            GIB
        );
    }

    #[test]
    fn sustained_telemetry_staleness_fails_open_but_policy_checks_still_apply() {
        // 31 s stale, no availability: past the fail-open threshold.
        let wedged = input(T, None, 31_000, Vec::new());
        assert_eq!(config().decide(&wedged), QueueReason::Admit);

        // Non-host policy checks still gate: concurrency full.
        let full = input(
            T,
            None,
            31_000,
            vec![active(2 * GIB, None, 1), active(2 * GIB, None, 1)],
        );
        assert_eq!(config().decide(&full), QueueReason::WaitConcurrency);

        // Below the threshold the old fail-closed behavior holds.
        let fresh_stale = input(T, None, 5_000, Vec::new());
        assert_eq!(config().decide(&fresh_stale), QueueReason::WaitTelemetry);

        // Reconciliation is never bypassed by age.
        let mut reconciling = input(T, Some(10 * GIB), 120_000, Vec::new());
        reconciling.reconciliation_required = true;
        assert_eq!(config().decide(&reconciling), QueueReason::WaitTelemetry);
    }
}
