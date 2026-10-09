//! # term-contracts
//!
//! Single source of truth for every value that crosses a process boundary in
//! iyagi: the local IPC wire (control + data), the launch gate between
//! daemon and helper, persistence-visible enums, and the TS bindings the
//! frontend consumes.
//!
//! Invariants enforced here (spec `01-contracts.md`):
//! * persistent IDs are UUID v4 strings;
//! * byte counts and monotonic sequences travel as decimal `U64String`;
//! * persisted integers fit SQLite signed INTEGER (`0..2^63-1`);
//! * unknown measurements are `null` + reason, never zero;
//! * no Tauri/React/OS dependency — pure data and validation only.
//!
//! The crate deliberately contains no I/O runtime: frame encode/decode works
//! over `std::io` traits so both the tokio daemon and synchronous tests can
//! use it.

pub mod agent_session;
pub mod defaults;
pub mod error;
pub mod gate;
pub mod ids;
pub mod intervention;
pub mod launch;
pub mod metrics;
pub mod mission;
pub mod remote;
pub mod rpc;
pub mod session;
pub mod snapshot;
pub mod state;
pub mod workload;

pub use agent_session::{
    AgentSessionEvent, AgentSessionForgetParams, AgentSessionForgetResult, AgentSessionListParams,
    AgentSessionRecord, AgentSessionReport, AgentSessionReportResult, AgentSessionSource,
};
pub use error::{ErrorCode, RpcError};
pub use ids::{
    BootId, ConnectionId, ProcessIdentity, RequestId, SessionId, U64String, ViewId, WorkloadId,
};
pub use intervention::{InterventionKind, InterventionNotice, InterventionReport};
pub use launch::{
    ClaudeProvider, Enforcement, LaunchMode, LaunchPolicy, LaunchRequest, LaunchValidation,
};
pub use metrics::{HostSample, Metric, MetricQuality, PressureLevel, WorkloadUsage};
pub use remote::{
    ExecutorChoice, GatewayEnvelope, RemoteConnectionState, RemoteError, RemoteErrorKind,
    RemoteHostConfig, RemoteHostStatus, RemoteResultManifest, SnapshotFile, SourceSnapshotRequest,
};
pub use rpc::{
    Frame, FrameError, HelloParams, HelloResult, RpcEvent, RpcEventKind, RpcRequest, RpcResponse,
    MAX_FRAME_BYTES,
};
pub use session::{
    AttachAccess, AttachParams, AttachResult, ExitReason, InputParams, ResizeParams, SessionAck,
    SessionExit, SessionOutput, SessionSearchMatch, SessionSearchParams, SessionSearchResult,
    TerminalFrameKind,
};
pub use snapshot::{Capabilities, QueueEntry, QueueReason, Snapshot, WorkloadSummary};
pub use state::{TerminalConnection, WorkloadState};
pub use workload::{WorkloadDescriptor, WorkloadRecord};
