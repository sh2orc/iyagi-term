//! # term-core
//!
//! Pure decision logic with no OS/UI dependency: the conservative
//! deterministic admission controller, the host pressure classifier with
//! hysteresis, the bounded managed queue with priority aging and head-of-line
//! bypass, and the atomic reservation ledger (spec `03-resources.md` §3).
//!
//! * [`admission`] — integer-math admission; the normative check order and
//!   the `docs/implementation/admission-cases.json` fixtures are the contract
//!   (parity gate: `tests/admission_fixtures.rs`).
//! * [`pressure`] — CRITICAL/WARNING/NORMAL classification with 2-sample
//!   worsening hysteresis and 10 s sustained recovery.
//! * [`queue`] — 64-entry queue, 30 s aging steps, effective priority →
//!   queued_at → id ordering, head-of-line bypass with recorded reasons.
//! * [`reservation`] — decide + reserve in one mutex critical section;
//!   STARTING counts as active.
//! * [`clock`] — injectable monotonic time (fake clock for tests).
//!
//! The workload state machine and launch idempotency ledger land with their
//! own tickets and will extend this crate. PTYs, OS enforcement, and storage
//! live in `term-pty` / `term-platform` / `term-storage` behind traits.

pub mod admission;
pub mod clock;
pub mod error;
pub mod mission;
pub mod pressure;
pub mod queue;
pub mod reservation;

pub use admission::{
    host_reserve_bytes, pending_reservation, percent_floor, ActiveWorkload, AdmissionConfig,
    AdmissionHost, AdmissionInput, AdmissionRequest,
};
pub use clock::{Clock, FakeClock, MonotonicClock};
pub use error::CoreError;
pub use pressure::{CpuPressureConfig, CpuPressureTracker, PressureConfig, PressureTracker};
pub use queue::{
    effective_priority, PickOutcome, QueueCandidate, QueueConfig, SkippedEntry, WorkloadQueue,
};
pub use reservation::{ReservationGuard, ReservationLedger};
