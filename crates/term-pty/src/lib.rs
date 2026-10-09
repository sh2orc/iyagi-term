//! # term-pty
//!
//! Session actor owning one PTY (reader/writer threads, journal appends,
//! epoch/ACK ledger, resize coalescing) and the MTJ1 journal codec used for
//! full-stream replay on re-attach.
//!
//! Module ownership (parallel implementation, do not cross-edit):
//! * `journal`, `flow` — ticket I07
//! * `pty`, `actor`, `gate`, `input` — ticket I05

pub mod actor;
pub mod flow;
pub mod gate;
pub mod input;
pub mod journal;
pub mod pty;
pub mod segments;
