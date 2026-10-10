//! `system.snapshot` result: queue, workload summaries, capabilities, and the
//! admission decision vocabulary (spec `03-resources.md` §3).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent_session::AgentSessionSource;
use crate::ids::{RequestId, SessionId, U64String, WorkloadId};
use crate::launch::{Enforcement, LaunchMode, Priority};
use crate::metrics::WorkloadUsage;
use crate::state::{TerminalConnection, WorkloadState};

/// `AgentStatus.model`·`effort` 값의 출처. `StatusLine`·`Transcript`·`Rollout`은
/// 세션이 스스로 남긴 기록이고, `Config`는 Codex `/model`이 즉시 저장한 전역
/// 설정을 마지막 입력 pane에 귀속한 값, `Defaults`는 세션 기록이 생기기 전의
/// 잠정값(실행 인수·설정 기본값)이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelSource {
    /// Claude Code 상태줄(세션 id별 — `/model`·`/effort` 뒤 즉시).
    StatusLine,
    /// Claude Code transcript(로컬 명령 결과·응답 — 즉시).
    Transcript,
    /// Codex rollout(`thread_settings_applied`·`turn_context` — 다음 턴).
    Rollout,
    /// Codex `config.toml`(`/model` 선택이 즉시 저장한 값 — pane 귀속).
    Config,
    /// 실행 인수·설정 기본값(세션 기록 전 잠정).
    Defaults,
}

/// 셸 세션 안에서 감지된 AI 코딩 에이전트(claude/codex/opencode) —
/// 런타임 관찰 결과. 이 구조체 자체는 영속화하지 않고, 세션 id가
/// 확인되면 `agent_sessions` 표에 별도 행으로 기록한다(02 §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStatus {
    /// 서명 테이블의 에이전트 id("claude" | "codex" | "opencode").
    pub agent: String,
    /// 감지된 에이전트 루트 프로세스 PID.
    pub pid: u32,
    /// 감지 시각(단조 ms — 표시용 경과 계산에 쓴다).
    pub detected_at_ms: u64,
    /// 에이전트 자체 세션(스레드) id — `claude --resume <id>` /
    /// `codex resume <id>`의 인자. 아직 알아내지 못했으면 없음.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 에이전트가 붙인 표시 이름(Claude 레지스트리 `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// 세션 id의 출처.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_source: Option<AgentSessionSource>,
    /// 에이전트가 스스로 보고한 활동 상태 원문(Claude 레지스트리
    /// `status`: "idle" | "busy" 등). 라벨용이며 해석하지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_status: Option<String>,
    /// 에이전트의 현재 모델 표시 이름(관찰값 — "Opus 5 (1M context)",
    /// "gpt-5.6-sol"). `/model` 등으로 바뀌면 즉시 따라간다. 모르면 없음.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 현재 effort/추론 수준 원문("low" | "medium" | "high" | "xhigh" |
    /// "max" | "auto" …). 모델이 effort를 쓰지 않거나 모르면 없음.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// `model`·`effort`의 출처.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_source: Option<AgentModelSource>,
}

/// Ordered admission outcomes (03 §3). The order is normative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum QueueReason {
    /// Reconciliation pending or host sample older than 3s.
    WaitTelemetry,
    /// Request exceeds the configured budget/slots; needs a config change.
    ResourceUnschedulable,
    /// Host memory pressure CRITICAL.
    WaitHostPressure,
    /// managed_concurrency active workloads reached.
    WaitConcurrency,
    WaitCpuSlots,
    WaitReservationBudget,
    WaitMemoryHeadroom,
    /// Not a wait state: the scheduler admitted the launch.
    Admit,
}

/// 압력 완화(spec `08-pressure-relief.md`)가 이 세션에 적용한 상태.
/// P2는 양보(`YIELDED`)까지만 정의한다 — `GATED`/`HIBERNATED`/`SUSPENDED`는
/// P3–P5에서 같은 태그 유니온에 덧붙는다. 구 데몬은 이 필드를 보내지 않으며
/// 그때는 `NONE`이다(`serde(default)`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind")]
pub enum ReliefState {
    /// 완화 수단이 적용되지 않은 정상 상태.
    #[default]
    #[serde(rename = "NONE")]
    None,
    /// 스케줄링 양보 중(08 §2). `manual`은 사용자가 직접 요청했다는 뜻이고
    /// (자동 복원 대상이 아니다), `partial`은 검증된 멤버 일부에만 적용됐다는
    /// 뜻이다(나머지는 다음 틱에 다시 시도한다).
    #[serde(rename = "YIELDED")]
    Yielded {
        since_ms: U64String,
        manual: bool,
        partial: bool,
    },
}

impl ReliefState {
    /// 지금 양보 중인가(어떤 종류든).
    pub fn is_yielded(&self) -> bool {
        matches!(self, ReliefState::Yielded { .. })
    }
}

/// 데몬 전체의 완화 정책(08 §7 `settings: relief.*`). P2는 자동 양보
/// 스위치 하나다; P3–P5가 `auto_gate`/`auto_hibernate`/`auto_suspend`를
/// 덧붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ReliefPolicy {
    /// CPU 압력 ≥ WARNING에서 비포커스·비보호 세션을 자동으로 양보시킨다.
    pub auto_yield: bool,
}

/// 자원 가드(08 §5 — 강한 자동 제어)가 이 세션에 적용한 상태. 일시정지는
/// 되돌릴 수 있다: 검증된 트리에 SIGSTOP/SIGCONT(cgroup 위임 Linux는
/// freeze)를 걸고, 사용자가 언제든 재개할 수 있다. 구 데몬은 이 필드를
/// 보내지 않으며 그때는 `NONE`이다(`serde(default)`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind")]
pub enum GuardState {
    /// 가드가 개입하지 않은 상태.
    #[default]
    #[serde(rename = "NONE")]
    None,
    /// 일시정지 중. `manual`은 사용자가 직접 정지시켰다는 뜻이고(자동
    /// 재개 대상이 아니다), `partial`은 검증된 멤버 일부에만 걸렸다는
    /// 뜻이다(나머지는 다음 틱에 다시 시도한다).
    #[serde(rename = "SUSPENDED")]
    Suspended {
        since_ms: U64String,
        reason: GuardReason,
        manual: bool,
        partial: bool,
    },
}

impl GuardState {
    /// 지금 일시정지 중인가.
    pub fn is_suspended(&self) -> bool {
        matches!(self, GuardState::Suspended { .. })
    }
}

/// 가드가 개입한 이유.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GuardReason {
    /// 워크로드 귀속 CPU가 한도를 지속적으로 초과했다.
    CpuLimit,
    /// 워크로드 귀속 메모리가 한도를 지속적으로 초과했다.
    MemoryLimit,
    /// 호스트 메모리 압력 CRITICAL에서 가장 큰 비포커스 독점 업.
    HostMemoryPressure,
    /// 사용자가 직접 정지시켰다.
    Manual,
}

/// 데몬 전체의 가드 정책(08 §7 `settings: resource_guard.*`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GuardPolicy {
    /// 한도 초과가 지속되면 비포커스 워크로드를 자동으로 일시정지한다.
    pub auto_suspend: bool,
    /// 워크로드 귀속 CPU 한도(논리 코어 단위).
    pub cpu_cores_limit: u32,
    /// 워크로드 귀속 메모리 한도(바이트).
    pub rss_limit_bytes: U64String,
    /// 한도 초과를 "지속"으로 인정하는 시간(밀리초) — CPU 양보 판정이 쓴다.
    pub sustain_ms: U64String,
    /// 메모리 한도 초과를 "지속"으로 인정하는 시간(밀리초). CPU보다 짧다 —
    /// 메모리 급등은 20초를 기다리면 스왑이 먼저 온다.
    #[serde(default = "default_rss_sustain_ms")]
    pub rss_sustain_ms: U64String,
    /// 정지된 워크로드를 조건 회복 시 자동 재개한다(기본 off: 사용자 재개).
    pub auto_resume: bool,
}

fn default_rss_sustain_ms() -> U64String {
    U64String::new(5_000).expect("5 s in range")
}

impl Default for GuardPolicy {
    fn default() -> Self {
        Self {
            auto_suspend: true,
            cpu_cores_limit: 6,
            rss_limit_bytes: U64String::new(4 * 1024 * 1024 * 1024).expect("4 GiB in range"),
            sustain_ms: U64String::new(20_000).expect("20 s in range"),
            rss_sustain_ms: default_rss_sustain_ms(),
            auto_resume: false,
        }
    }
}

impl Default for ReliefPolicy {
    fn default() -> Self {
        Self { auto_yield: true }
    }
}

/// Per-limit capability tri-state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LimitSupport {
    Supported,
    Unsupported,
    PermissionRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LimitCapability {
    pub support: LimitSupport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl LimitCapability {
    /// 이 필드를 아직 보내지 않는 구 데몬을 읽을 때의 값. 되돌릴 수 있는지
    /// 알 수 없으면 적용하지 않는 쪽(`unsupported`, 08 §0-4)으로 읽는다 —
    /// 새 앱이 구 데몬의 hello를 거절해 살아 있는 세션을 못 붙이면 안 된다.
    fn omitted_by_older_daemon() -> Self {
        Self {
            support: LimitSupport::Unsupported,
            reason: Some("older daemon does not report this capability".into()),
        }
    }
}

/// Executor capabilities named after the design table `ExecutorCapabilities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Capabilities {
    /// Kind of memory limit the platform can enforce
    /// (`cgroup.v2 memory.max`, `job commit`, none, ...).
    pub memory_limit_kind: LimitCapability,
    pub cpu_quota: LimitCapability,
    pub process_count_limit: LimitCapability,
    /// Whether usage covers the full owned group including re-parented
    /// descendants.
    pub tree_accounting: LimitCapability,
    /// Whether a detached UI can re-attach to a live session.
    pub reattach: LimitCapability,
    /// Whether CLI sessions survive daemon restart (R1: no).
    pub resume: LimitCapability,
    /// 스케줄링 양보(08 §2)를 이 프로세스가 **적용하고 되돌릴 수** 있는가.
    /// 되돌릴 수 없는 플랫폼은 `unsupported`이며 아무것도 적용하지 않는다
    /// (08 §0-4). 구 데몬은 이 필드를 보내지 않으며 그때는 `unsupported`다.
    #[serde(default = "LimitCapability::omitted_by_older_daemon")]
    pub scheduling_yield: LimitCapability,
    /// 자원 가드(08 §5)의 일시정지/재개를 이 프로세스가 **적용할 수** 있는가.
    /// 검증된 트리에 걸리는 되돌릴 수 있는 조작이라 관측 전용 프로필에서도
    /// 지원한다(매킨토시 SIGSTOP/SIGCONT, 위임 cgroup Linux freeze). 구
    /// 데몬은 이 필드를 보내지 않으며 그때는 `unsupported`다.
    #[serde(default = "LimitCapability::omitted_by_older_daemon")]
    pub suspend_resume: LimitCapability,
    pub platform: String,
    /// Extra machine-readable notes (which subtree is delegated, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// O1 mission orchestration protocol revision advertised by this daemon
    /// (`1` when the feature gate is on). Absent means the daemon cannot
    /// serve mission RPCs — the UI must lock mission creation and keep plain
    /// terminals working (O1 spec `01-contracts.md` §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_protocol: Option<u32>,
    /// Whether this daemon can resolve LaunchRequest.claude_provider (Z.ai routing for Claude Code panes). Older daemons omit it -> false; the UI must then refuse routed launches instead of silently falling back.
    #[serde(default)]
    pub claude_provider_routing: bool,
}

impl Capabilities {
    /// macOS-style observation-only profile.
    pub fn observe_only(platform: &str) -> Self {
        Self {
            memory_limit_kind: LimitCapability {
                support: LimitSupport::Unsupported,
                reason: Some("no group memory hard cap on this platform".into()),
            },
            cpu_quota: LimitCapability {
                support: LimitSupport::Unsupported,
                reason: Some("no group cpu quota on this platform".into()),
            },
            process_count_limit: LimitCapability {
                support: LimitSupport::Unsupported,
                reason: Some("no group process limit on this platform".into()),
            },
            tree_accounting: LimitCapability {
                support: LimitSupport::Supported,
                reason: None,
            },
            reattach: LimitCapability {
                support: LimitSupport::Supported,
                reason: None,
            },
            resume: LimitCapability {
                support: LimitSupport::Unsupported,
                reason: Some("daemon restart marks workloads INTERRUPTED".into()),
            },
            // 양보는 그룹이 아니라 검증된 멤버 단위의 per-process 정책이라
            // 관측 전용 프로필에서도 적용하고 되돌릴 수 있다(08 §2).
            scheduling_yield: LimitCapability {
                support: LimitSupport::Supported,
                reason: Some("per-process scheduling policy on verified members".into()),
            },
            // 일시정지도 같은 이유로 관측 전용 프로필에서 지원한다(08 §5):
            // SIGSTOP/SIGCONT는 검증된 신원에만 걸고 되돌릴 수 있다.
            suspend_resume: LimitCapability {
                support: LimitSupport::Supported,
                reason: Some("SIGSTOP/SIGCONT on verified members".into()),
            },
            platform: platform.to_string(),
            notes: Vec::new(),
            mission_protocol: None,
            claude_provider_routing: false,
        }
    }

    /// Daemon-side feature gate stamp: platform backends never decide this.
    pub fn with_mission_protocol(mut self, revision: Option<u32>) -> Self {
        self.mission_protocol = revision;
        self
    }

    /// Daemon-side stamp for `LaunchRequest.claude_provider` support: the
    /// platform backend never decides this either (it is a launch-path
    /// feature, not an OS capability).
    pub fn with_claude_provider_routing(mut self, enabled: bool) -> Self {
        self.claude_provider_routing = enabled;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QueueEntry {
    pub workload_id: WorkloadId,
    pub request_id: RequestId,
    pub priority: Priority,
    /// Effective priority after aging (never below 0 = highest).
    pub effective_priority: Priority,
    /// Monotonic queue insertion time for ordering/aging.
    pub queued_at_ms: u64,
    /// Last computed wait reason, when the scheduler evaluated it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_reason: Option<QueueReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkloadSummary {
    pub workload_id: WorkloadId,
    /// Existing PTY to reattach after the UI restarts; never relaunch it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub mode: LaunchMode,
    pub state: WorkloadState,
    pub priority: Priority,
    pub title: String,
    pub cwd: String,
    pub program: String,
    pub reservation_bytes: U64String,
    pub cpu_slots: u32,
    pub enforcement: Enforcement,
    pub root_exited: bool,
    pub cancel_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_reason: Option<QueueReason>,
    pub connection: TerminalConnection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<WorkloadUsage>,
    /// 셸 세션에서 관찰 중인 AI 코딩 에이전트(없으면 미실행).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentStatus>,
    /// 압력 완화 상태(08 §2). 구 데몬은 보내지 않으며 그때는 `NONE`이다.
    #[serde(default)]
    pub relief: ReliefState,
    /// 사용자가 이 세션을 자동 완화에서 제외했다(08 §0-3).
    #[serde(default)]
    pub protected: bool,
    /// 자원 가드 상태(08 §5). 구 데몬은 보내지 않으며 그때는 `NONE`이다.
    #[serde(default)]
    pub guard: GuardState,
    /// 포커스된 세션이 한도를 초과해 자동 정지 대신 경고 중이다(08 §5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_warning: Option<GuardReason>,
}

/// Whole-daemon snapshot; `revision` is globally increasing so the UI can
/// drop stale snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Snapshot {
    pub revision: u64,
    pub host: crate::metrics::HostSample,
    pub workloads: Vec<WorkloadSummary>,
    pub queue: Vec<QueueEntry>,
    pub capabilities: Capabilities,
    pub reconciliation_required: bool,
    /// Sessions currently focused by connected windows, one per control
    /// connection; spec `08-pressure-relief.md` §1. Older daemons omit it.
    #[serde(default)]
    pub focused_session_ids: Vec<SessionId>,
    /// 현재 완화 정책(08 §2 `relief.auto_yield`). 구 데몬은 생략한다.
    #[serde(default)]
    pub relief_policy: ReliefPolicy,
    /// 현재 자원 가드 정책(08 §5 `resource_guard.*`). 구 데몬은 생략한다.
    #[serde(default)]
    pub guard_policy: GuardPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_reason_serializes_as_spec_constants() {
        assert_eq!(
            serde_json::to_string(&QueueReason::WaitMemoryHeadroom).unwrap(),
            "\"WAIT_MEMORY_HEADROOM\""
        );
        assert_eq!(
            serde_json::to_string(&QueueReason::Admit).unwrap(),
            "\"ADMIT\""
        );
    }

    #[test]
    fn capabilities_round_trip() {
        let caps = Capabilities::observe_only("macos");
        let json = serde_json::to_string(&caps).unwrap();
        let back: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back.memory_limit_kind.support, LimitSupport::Unsupported);
        assert_eq!(back.reattach.support, LimitSupport::Supported);
        assert_eq!(back.scheduling_yield.support, LimitSupport::Supported);
    }

    /// 08 §7의 태그 유니온 그대로: `kind`가 판별자이며 NONE에는 다른 필드가
    /// 없다. 구 데몬(필드 자체가 없음)은 NONE으로 읽힌다.
    #[test]
    fn relief_state_uses_the_spec_tag_union() {
        assert_eq!(
            serde_json::to_string(&ReliefState::None).unwrap(),
            r#"{"kind":"NONE"}"#
        );
        let yielded = ReliefState::Yielded {
            since_ms: U64String::new(1_234).unwrap(),
            manual: true,
            partial: false,
        };
        let json = serde_json::to_string(&yielded).unwrap();
        assert!(json.contains(r#""kind":"YIELDED""#), "{json}");
        assert!(json.contains(r#""since_ms":"1234""#), "{json}");
        assert_eq!(
            serde_json::from_str::<ReliefState>(&json).unwrap(),
            yielded,
            "round trip"
        );
        assert_eq!(ReliefState::default(), ReliefState::None);
        assert!(yielded.is_yielded() && !ReliefState::None.is_yielded());
    }

    /// 08 §0-4: `scheduling_yield`를 보내지 않는 구 데몬의 capabilities는
    /// 거절되지 않고 `unsupported`로 읽힌다 — hello 단계에서 typed로 읽는
    /// 앱 브리지가 살아 있는 세션을 못 붙이게 되면 안 된다.
    #[test]
    fn capabilities_without_scheduling_yield_read_as_unsupported() {
        let mut json = serde_json::to_value(Capabilities::observe_only("macos")).unwrap();
        json.as_object_mut().unwrap().remove("scheduling_yield");
        let caps: Capabilities = serde_json::from_value(json).unwrap();
        assert_eq!(caps.scheduling_yield.support, LimitSupport::Unsupported);
        assert!(caps.scheduling_yield.reason.is_some());
    }

    /// `claude_provider_routing`을 보내지 않는 구 데몬은 `false`로 읽힌다 —
    /// 앱은 그때 라우팅된 실행을 거절해야지 조용히 Anthropic 직결로 넘기면
    /// 안 된다. 새 데몬의 `true`는 그대로 왕복하며, 백엔드 프로필은 항상
    /// `false`에서 출발하고 데몬이 스탬프한다.
    #[test]
    fn capabilities_claude_provider_routing_defaults_to_false_for_older_daemons() {
        let mut json = serde_json::to_value(Capabilities::observe_only("macos")).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("claude_provider_routing");
        let caps: Capabilities = serde_json::from_value(json).unwrap();
        assert!(!caps.claude_provider_routing);

        assert!(!Capabilities::observe_only("macos").claude_provider_routing);
        let stamped = Capabilities::observe_only("macos").with_claude_provider_routing(true);
        assert!(stamped.claude_provider_routing);
        let json = serde_json::to_value(&stamped).unwrap();
        assert_eq!(json["claude_provider_routing"], serde_json::json!(true));
        let back: Capabilities = serde_json::from_value(json).unwrap();
        assert_eq!(back, stamped);
    }

    /// 자동 양보는 기본으로 켜져 있고(08 §7 settings), 구 스냅샷은
    /// `relief_policy` 없이도 읽힌다.
    #[test]
    fn relief_policy_defaults_to_auto_yield_on() {
        assert!(ReliefPolicy::default().auto_yield);
        let policy: ReliefPolicy = serde_json::from_str(r#"{"auto_yield":false}"#).unwrap();
        assert!(!policy.auto_yield);
    }
}
