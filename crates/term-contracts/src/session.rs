//! Session data path: output records, ACKs, input and resize contracts
//! (spec `01-contracts.md` §4, `02-runner.md` §4–6).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{SessionId, U64String, ViewId};

/// Attach role. Exactly one writer per session; others are read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum AttachAccess {
    Writer,
    Reader,
}

/// Record kind on the session stream. `seq` covers outputs AND resizes in one
/// journal order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum TerminalFrameKind {
    Output,
    Resize,
}

/// One output frame toward the UI (`session.output` event payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionOutput {
    pub session_id: SessionId,
    /// New UUID on every attach/owner change; replayed records carry the
    /// current connection's epoch.
    pub epoch: String,
    pub seq: U64String,
    pub kind: TerminalFrameKind,
    /// Base64 of raw PTY bytes (output) or empty (resize).
    #[serde(default)]
    pub data_b64: String,
    pub raw_len: u32,
    /// Present only on resize frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
}

/// Data-connection ACK: the last consecutively processed seq in this epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionAck {
    pub session_id: SessionId,
    pub epoch: String,
    pub through_seq: U64String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AttachParams {
    pub session_id: SessionId,
    pub view_id: ViewId,
    pub access: AttachAccess,
    /// The view already shows the screen through `resume_from_seq - 1`
    /// (a UI snapshot). When the retained journal still holds that seq, replay
    /// starts there instead of at the head; `replay_from_seq` in the result
    /// equals this value exactly when it was honored. Absent or out of range:
    /// replay from the first retained record as usual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from_seq: Option<U64String>,
    /// Replay byte budget. When the retained journal is larger, replay starts
    /// at the head of the oldest trailing segment that still fits (the active
    /// segment always counts) — the skipped head is reported like a trimmed
    /// one through `replay_dropped_bytes`, so the UI nudges a redraw when it
    /// goes live and drops an older snapshot on the `replay_from_seq`
    /// mismatch. A `resume_from_seq` already inside the budget is honored as
    /// is. Absent: replay everything retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_replay_bytes: Option<U64String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AttachResult {
    pub epoch: String,
    /// First journal seq the replay will deliver (1 for a fresh session;
    /// larger once a rolling journal trimmed its head — the replay then
    /// opens with that segment's size record).
    pub replay_from_seq: U64String,
    pub last_seq: U64String,
    pub cols: u16,
    pub rows: u16,
    /// Bytes deleted from the head of the journal before this attach
    /// (rolling journal, 02-runner §5). Absent when nothing was trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_dropped_bytes: Option<U64String>,
    /// The process has finished; replay remains available, input does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InputParams {
    pub session_id: SessionId,
    pub epoch: String,
    /// Idempotency key; last 256 outcomes are remembered per session.
    pub input_id: String,
    pub data_b64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InputResult {
    pub input_id: String,
    /// Bytes actually written when the reply was produced.
    pub accepted_bytes: u32,
    /// True when this is the honest 750 ms `Queued` fallback (W2): the bytes
    /// are accepted by the daemon but NOT yet written to the tty. Clients use
    /// it to tell "delivered" (which clears the input-stalled badge) from
    /// "merely accepted". Old daemons omit the field — treat missing as
    /// delivered, matching their always-`Written` behavior.
    #[serde(default)]
    #[ts(optional)]
    pub queued: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResizeParams {
    pub session_id: SessionId,
    pub epoch: String,
    pub resize_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResizeResult {
    pub resize_id: String,
    /// Journal seq of the applied resize record.
    pub applied_seq: U64String,
}

/// `session.focus` params: which session this window is looking at right now
/// (spec `08-pressure-relief.md` §1). `None` = this window focuses no
/// session (every pane hidden / the window lost focus).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionFocusParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// `session.focus` result: the daemon-wide aggregate (one entry per control
/// connection that focuses something), sorted and deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionFocusResult {
    pub focused_session_ids: Vec<SessionId>,
}

/// `session.relief` 액션(spec `08-pressure-relief.md` §2). 수동은 항상
/// 자동보다 우선한다(§0-2): 수동 양보는 압력이 풀려도 자동 복원되지 않고,
/// 수동 복원은 압력이 남아 있는 동안 다시 양보 대상이 되지 않는다.
/// P3–P5가 게이트 해제·동면·정지 액션을 이 열거형에 덧붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ReliefAction {
    /// 지금 이 세션을 양보시킨다(압력 level과 무관).
    Yield,
    /// 양보를 즉시 되돌린다.
    Restore,
    /// 자동 완화 대상에서 영구히 제외한다(§0-3).
    Protect,
    /// 보호 표시를 해제한다.
    Unprotect,
}

/// `session.relief` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReliefParams {
    pub session_id: SessionId,
    pub action: ReliefAction,
}

/// `session.relief` result: 액션이 적용된 뒤의 세션 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionReliefResult {
    pub relief: crate::snapshot::ReliefState,
    pub protected: bool,
}

/// `relief.set_policy` params (결과 타입은 [`crate::snapshot::ReliefPolicy`]).
/// 자동 양보를 끄는 것만으로는 이미 양보 중인 세션이 복원되지 않는다 —
/// NORMAL 회복 경로나 수동 복원이 되돌린다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReliefPolicyParams {
    pub auto_yield: bool,
}

/// `workload.suspend` params (결과 타입은 [`WorkloadGuardResult`]). 사용자가
/// 직접 일시정지한다 — 수동 정지는 자동 재개 대상이 아니다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkloadSuspendParams {
    pub request_id: crate::ids::RequestId,
    pub workload_id: crate::ids::WorkloadId,
}

/// `workload.suspend`/`workload.resume` 결과: 조작 뒤의 가드 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkloadGuardResult {
    pub guard: crate::snapshot::GuardState,
}

/// `guard.set_policy` params (결과 타입은
/// [`crate::snapshot::GuardPolicy`]). 자동 정지를 꺼도 이미 정지된
/// 워크로드는 재개되지 않는다 — 수동 재개가 돌려놓는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GuardPolicyParams {
    pub policy: crate::snapshot::GuardPolicy,
}

/// Why a session ended — the frozen one-line classification
/// (`session.exited.reason`, SOTA_GAP_REVIEW W1-1). This enum is a storage
/// schema: adding variants is fine, changing the meaning of an existing one
/// is not. Human-readable wording is produced by the UI from the variant
/// plus `SessionExit::detail`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    /// Root process exited on its own (`exit_code` carries the code).
    ProcessExit,
    /// Cancelled by request (grace → force).
    Cancelled,
    /// Output recording stopped at the journal cap / disk boundary before
    /// the process ended (see `detail` for which limit).
    JournalLimit,
    /// The OS killed the group for exceeding its memory limit (Linux cgroup
    /// `memory.events` `oom_kill`; Windows job commit refusal).
    OomKill,
    /// Cause not observable on this platform (e.g. macOS observation-only).
    #[default]
    Unknown,
}

/// `session.exited` payload: process outcome, independent of UI state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionExit {
    pub session_id: SessionId,
    pub exit_code: Option<i32>,
    /// True when owned descendants outlived the root.
    pub descendants_remaining: bool,
    /// Frozen classification (W1-1). Older daemons omit it; deserializing
    /// those payloads yields `Unknown`.
    #[serde(default)]
    pub reason: ExitReason,
    /// Non-localized technical evidence for the reason, e.g.
    /// `memory.events oom_kill=1` or `journal cap reached (session)`.
    /// Never secrets; shown as-is in diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `session.search` params — 유계 substring 검색(W2 저널 검색).
/// 스크롤백(2000줄) 밖의 과거 출력까지 찾는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSearchParams {
    /// 검색어(1..=256자). 정규식 아님 — substring만.
    pub query: String,
    #[serde(default)]
    pub case_sensitive: bool,
    /// 특정 세션만 찾는다(None = 전체).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// 저널 검색 일치 항목.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSearchMatch {
    pub session_id: SessionId,
    /// 일치가 담긴 출력 레코드의 journal seq.
    pub seq: U64String,
    /// 일치 줄의 유계 발췌(240자).
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionSearchResult {
    /// 최근 세션 우선, 세션 내에서는 seq 내림차순. 상한 50건.
    pub matches: Vec<SessionSearchMatch>,
    pub truncated: bool,
    pub sessions_scanned: u32,
}

/// 저널 검색 상한.
pub mod search_limits {
    pub const QUERY_MAX: usize = 256;
    pub const MAX_MATCHES: usize = 50;
    pub const LINE_CONTEXT: usize = 240;
    /// 세션당 스캔 상한(바이트) — 검색이 데몬을 무겁게 하지 않게.
    pub const PER_SESSION_SCAN_BYTES: u64 = 8 * 1024 * 1024;
    /// 전체 스캔 상한(바이트).
    pub const TOTAL_SCAN_BYTES: u64 = 32 * 1024 * 1024;
    /// 한 번에 읽는 레코드 수.
    pub const BATCH_RECORDS: usize = 256;
}

/// Input/resize limits from `defaults.json`.
pub mod limits {
    pub const INPUT_CHUNK_BYTES: usize = 4_096;
    pub const INPUT_QUEUE_BYTES: usize = 65_536;
    pub const PASTE_BYTES: usize = 1_048_576;
    pub const OUTPUT_CHUNK_BYTES: usize = 16_384;
    pub const OUTPUT_HIGH_BYTES: usize = 262_144;
    pub const OUTPUT_LOW_BYTES: usize = 65_536;
    pub const OUTPUT_RAW_GLOBAL_BYTES: usize = 8 * 1024 * 1024;
    pub const TRANSPORT_GLOBAL_BYTES: usize = 24 * 1024 * 1024;
    pub const ACK_COALESCE_MS: u64 = 16;
    pub const ACK_COALESCE_BYTES: usize = 65_536;
    pub const RESIZE_COALESCE_MS: u64 = 16;
    pub const VIEWS_PER_SESSION: usize = 2;
    pub const INPUT_DEDUP_ENTRIES: usize = 256;
    pub const SCROLLBACK_LINES: usize = 2_000;
    pub const TERMINAL_MIN_DIM: u16 = 2;
    pub const TERMINAL_MAX_DIM: u16 = 1000;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_reason_defaults_for_older_daemons_and_omits_empty_detail() {
        let legacy = format!(
            r#"{{"session_id":"{}","exit_code":0,"descendants_remaining":false}}"#,
            SessionId::generate()
        );
        let back: SessionExit = serde_json::from_str(&legacy).unwrap();
        assert_eq!(back.reason, ExitReason::Unknown);
        assert_eq!(back.detail, None);

        let current = SessionExit {
            session_id: SessionId::generate(),
            exit_code: Some(137),
            descendants_remaining: false,
            reason: ExitReason::OomKill,
            detail: Some("memory.events oom_kill=1".into()),
        };
        let json = serde_json::to_string(&current).unwrap();
        assert!(json.contains(r#""reason":"oom_kill""#));
        assert!(json.contains("oom_kill=1"));
        let back: SessionExit = serde_json::from_str(&json).unwrap();
        assert_eq!(back, current);
    }

    #[test]
    fn output_record_round_trips_with_optional_resize_fields() {
        let out = SessionOutput {
            session_id: SessionId::generate(),
            epoch: "e".into(),
            seq: U64String::new(3).unwrap(),
            kind: TerminalFrameKind::Output,
            data_b64: "aGk=".into(),
            raw_len: 2,
            cols: None,
            rows: None,
        };
        let json = serde_json::to_string(&out).unwrap();
        assert!(!json.contains("cols"));
        let back: SessionOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(back, out);

        let resize = SessionOutput {
            kind: TerminalFrameKind::Resize,
            data_b64: String::new(),
            cols: Some(120),
            rows: Some(40),
            ..out
        };
        let back: SessionOutput =
            serde_json::from_str(&serde_json::to_string(&resize).unwrap()).unwrap();
        assert_eq!(back.cols, Some(120));
    }
}
