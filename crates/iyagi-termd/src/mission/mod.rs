//! O1 mission orchestration service (tickets O04/O05): typed RPC handlers
//! over the shared storage writer, snapshot pagination with per-connection
//! caching, the artifact transfer protocol, and the `mission.changed` hint.
//!
//! Layering (00 §3): dispatch delegates here; this module owns domain rules
//! and transaction assembly, `term_storage::mission` owns atomicity, and the
//! engine (O11+) will feed it transitions. Handlers are synchronous — the
//! dispatcher runs them on the blocking pool like `workload.launch`.

mod activity;
pub mod actor;
pub mod artifacts;
mod attestation;
mod binding_evidence;
mod controls;
mod costs;
pub mod engine;
mod exec_store;
pub mod execution;
mod failure_repair;
mod failures;
mod integration_exclusion;
pub mod integration_exec;
mod integration_recovery;
mod messaging;
pub mod outbox;
mod pipeline;
mod plan_repair;
pub mod planning;
mod provider_blocks;
mod rate_limits;
mod reconciliation;
mod run_evidence;
pub mod service;
pub mod snapshot;
mod timing;
mod transient_retry;
mod verification_exec;
mod verification_isolation;
pub mod workflow;
mod workspace_cleanup;

pub use service::{Handled, MissionService};
