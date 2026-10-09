//! Public input/output DTOs for the storage API.
//!
//! Nothing here carries argv/env: the launch intent mirrors the *policy*
//! portion of a validated `LaunchRequest` plus the ids the daemon generated,
//! per spec `01-contracts.md` §6 (원문 argv/env는 DB에 쓰지 않는다).

use term_contracts::agent_session::AgentSessionSource;
use term_contracts::ids::{ProcessIdentity, RequestId, SessionId, WorkloadId};
use term_contracts::launch::{LaunchMode, LaunchPolicy, Priority};
use term_contracts::metrics::UsageCoverage;
use term_contracts::snapshot::QueueReason;
use term_contracts::state::WorkloadState;
use term_contracts::workload::GroupKind;

use crate::error::{StorageError, StorageResult};
use crate::ids::{AttemptId, TaskId};

/// `requests.method` — the CHECK-constrained method strings from schema.sql.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestMethod {
    Launch,
    Cancel,
    Shutdown,
}

impl RequestMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            RequestMethod::Launch => "workload.launch",
            RequestMethod::Cancel => "workload.cancel",
            RequestMethod::Shutdown => "daemon.shutdown",
        }
    }
}

/// `requests.outcome` lifecycle of a request id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOutcome {
    Accepted,
    Completed,
    Failed,
    Unknown,
}

/// One durable launch intent: everything needed to insert
/// tasks/attempts/workloads/sessions/requests + the first lifecycle event in a
/// single transaction. The daemon computes the fingerprint itself (contract
/// `launch_fingerprint`); storage only records and compares it.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchIntent {
    pub request_id: RequestId,
    /// Ledger method for the requests row; launch intents use
    /// [`RequestMethod::Launch`] (kept explicit to match the requests table).
    pub method: RequestMethod,
    /// SHA-256 hex (64 chars) of the canonicalized LaunchRequest.
    pub fingerprint: String,
    /// tasks.title for managed mode.
    pub title: String,
    /// Managed execution requires task+attempt; shell requires neither.
    pub task_id: Option<TaskId>,
    pub attempt_id: Option<AttemptId>,
    /// attempts.ordinal, >= 1.
    pub attempt_ordinal: i64,
    pub workload_id: WorkloadId,
    pub session_id: SessionId,
    pub mode: LaunchMode,
    pub priority: Priority,
    /// Effective (post-capability) policy; also serialized into
    /// `workloads.effective_policy_json`.
    pub policy: LaunchPolicy,
    pub journal_relative_path: String,
    /// sessions.journal_limit_bytes (defaults.json `journal_session_bytes`).
    pub journal_limit_bytes: u64,
    /// Initial PTY dimensions; persisted on the session row only.
    pub cols: u16,
    pub rows: u16,
}

impl LaunchIntent {
    /// Friendly pre-validation; SQL CHECKs remain the backstop.
    pub fn validate(&self) -> StorageResult<()> {
        let managed = self.task_id.is_some() || self.attempt_id.is_some();
        match self.mode {
            LaunchMode::Managed => {
                if self.task_id.is_none() || self.attempt_id.is_none() {
                    return Err(StorageError::InvalidArgument(
                        "managed launch intent requires task_id and attempt_id",
                    ));
                }
            }
            LaunchMode::Shell => {
                if managed {
                    return Err(StorageError::InvalidArgument(
                        "shell launch intent must not carry task/attempt ids",
                    ));
                }
            }
        }
        if self.fingerprint.len() != 64 || !self.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(StorageError::InvalidArgument(
                "fingerprint must be 64 lowercase/uppercase hex characters",
            ));
        }
        if self.attempt_ordinal < 1 {
            return Err(StorageError::InvalidArgument(
                "attempt_ordinal must be >= 1",
            ));
        }
        if self.priority.0 > 2 {
            return Err(StorageError::InvalidArgument("priority must be 0..=2"));
        }
        if self.policy.cpu_slots == 0 {
            return Err(StorageError::InvalidArgument("cpu_slots must be >= 1"));
        }
        if self.policy.reservation_bytes.get() == 0 {
            return Err(StorageError::InvalidArgument(
                "reservation_bytes must be > 0",
            ));
        }
        if self.journal_limit_bytes == 0 || self.journal_limit_bytes > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(
                "journal_limit_bytes must be 1..=2^63-1",
            ));
        }
        if !(2..=1000).contains(&self.cols) || !(2..=1000).contains(&self.rows) {
            return Err(StorageError::InvalidArgument("cols/rows must be 2..=1000"));
        }
        Ok(())
    }
}

/// Result of recording a launch intent.
#[derive(Debug, Clone, PartialEq)]
pub enum LaunchIntentOutcome {
    /// Fresh insert: workload is QUEUED and the request is recorded accepted.
    Created {
        workload_id: WorkloadId,
        session_id: SessionId,
        state: WorkloadState,
    },
    /// Idempotent replay of the same request id + fingerprint: nothing was
    /// inserted; this is the *current* state of the existing workload
    /// (spec §4: launch 중복 응답은 동일 workload의 현재 상태를 반환).
    Existing {
        outcome: RequestOutcome,
        workload_id: WorkloadId,
        state: WorkloadState,
    },
}

/// Read-only request id lookup (spec §6/§7 `DAEMON_UNAVAILABLE` retry path).
#[derive(Debug, Clone, PartialEq)]
pub enum RequestResolution {
    /// No row: caller may proceed to `record_launch_intent`.
    New,
    /// Same id seen before with the same fingerprint.
    Existing {
        outcome: RequestOutcome,
        workload_id: WorkloadId,
        state: WorkloadState,
    },
    /// Same id, different fingerprint -> `REQUEST_CONFLICT`.
    Conflict,
}

/// One workload flipped to INTERRUPTED by crash reconciliation on open.
/// `ownership` is the surviving process_ownership row (kept, never deleted:
/// the daemon uses it to identify — not kill — leftover processes, spec §6).
#[derive(Debug, Clone, PartialEq)]
pub struct ReconciledWorkload {
    pub workload_id: WorkloadId,
    pub previous_state: WorkloadState,
    pub ownership: Option<ProcessOwnershipRow>,
}

/// `process_ownership` row projection (contract `ProcessOwnership` plus the
/// recorded_at timestamp the contract type does not model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOwnershipRow {
    pub workload_id: WorkloadId,
    pub identity: ProcessIdentity,
    pub group_kind: GroupKind,
    pub group_reference: Option<String>,
    pub coverage: UsageCoverage,
    pub recorded_at: String,
}

/// A QUEUED entry from `queue_snapshot`, ordered priority -> created_at -> id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedWorkload {
    pub workload_id: WorkloadId,
    pub request_id: Option<RequestId>,
    pub priority: Priority,
    pub queue_reason: Option<QueueReason>,
    pub created_at: String,
}

/// 관찰한 에이전트 세션 한 건의 upsert 입력(spec `02-runner.md` §8).
///
/// 식별 정보만 담는다 — 대화 내용·프롬프트 원문·argv·env는 들어가지
/// 않는다(01 §7). `title`은 에이전트가 스스로 붙인 이름(Claude 레지스트리
/// `name`, Codex `session_index.jsonl`의 `thread_name`)뿐이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionUpsert {
    /// 이 세션을 관찰한 워크로드.
    pub workload_id: WorkloadId,
    /// 그 워크로드의 PTY 세션(hook 경로에서는 없을 수 있다).
    pub pty_session_id: Option<SessionId>,
    /// 서명 테이블의 에이전트 id("claude" | "codex" | ...).
    pub agent: String,
    /// 에이전트 자체 세션(스레드) id.
    pub agent_session_id: String,
    pub cwd: String,
    pub title: Option<String>,
    /// 관찰된 실행 파일 절대 경로(네이티브 바이너리일 때만).
    pub program: Option<String>,
    pub source: AgentSessionSource,
}

/// `sessions` row projection for the sessions listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub workload_id: WorkloadId,
    pub initial_cols: u16,
    pub initial_rows: u16,
    pub journal_relative_path: String,
    pub journal_limit_bytes: u64,
    pub journal_bytes: u64,
    pub last_seq: u64,
    pub replay_status: String,
    pub pinned: bool,
    pub created_at: String,
}
