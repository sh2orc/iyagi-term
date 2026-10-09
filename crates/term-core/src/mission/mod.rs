//! O1 mission engine core (ticket O11, docs/orchestration/02-engine.md):
//! pure decision logic over projection snapshots — plan validation, task
//! readiness, scheduling order, and control-application rules. No I/O, no
//! clock, no DB: the daemon feeds observed snapshots in and applies the
//! returned intents through the storage transaction.

pub mod budget;
pub mod capability;
pub mod rate_limits;
pub mod retry;
pub mod plan;
pub mod reducer;
pub mod scheduler;

pub use plan::{validate_proposal, PlanApplication, PlanCandidate};
pub use reducer::{apply_control, apply_run_terminal, ControlIntent, MissionRules};
pub use scheduler::{select_dispatches, CapLimits, DispatchChoice, DispatchVerdict, SkipReason};
