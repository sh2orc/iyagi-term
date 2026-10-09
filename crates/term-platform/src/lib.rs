//! # term-platform
//!
//! OS adapters behind the `ResourcePlatform` trait plus the sysinfo telemetry
//! sampler. Capabilities reflect what the *current process* can actually use
//! (permissions/delegation), not what the OS supports in theory.
//!
//! Module ownership (parallel implementation, do not cross-edit):
//! * `identity`, `group` — ticket I06
//! * `telemetry` — ticket I09

pub mod group;
pub mod identity;
pub mod proc_fds;
pub mod proc_scan;
pub mod telemetry;

pub use group::{GroupHandle, ResourcePlatform, StopPhase};
pub use identity::{boot_id, current_process_identity, process_identity};
