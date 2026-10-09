//! Tauri ↔ iyagi-termd IPC bridge (ticket I04).
//!
//! Topology (`01-contracts.md` §3): the bridge keeps TWO connections to the
//! daemon — **control** (hello, RPCs, lifecycle events) and **data**
//! (`session.output` push + `session.ack`). Frontend outputs travel over
//! `tauri::ipc::Channel`s; a channel delivery is NOT treated as an
//! xterm-consumption ACK — ACKs flow back only through `bridge_ack` once the
//! UI pipeline actually consumed the records (`02-runner.md` §4).
//!
//! The daemon binary is built concurrently (its wire contract lives in
//! term-contracts only). Default tests use in-memory duplex streams or state
//! machines; an opt-in native smoke test also exercises a built daemon.

pub mod claude_hooks;
pub mod claude_usage;
pub mod codec;
pub mod codex_hooks;
pub mod commands;
pub mod connection;
pub mod daemon_manager;
mod hooks_json;
pub mod ime;
pub mod paste_image;
/// zsh `ccd`/`ccg` 런치 프로필 설치(동의 기반 — hooks 연동과 같은 계약).
pub mod shell_profiles;
pub mod state;
pub mod subscriptions;
pub mod system;
