//! Daemon-wide shared state: workloads/sessions registries, connections
//! (event fan-out), queue, ledger, telemetry, and the revision counter.
//!
//! Locking discipline: every field has its own mutex; never hold two
//! registry locks while performing storage/platform calls. Cross-cutting
//! operations (launch, finalize) run on dedicated blocking threads.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use term_contracts::error::RpcError;
use term_contracts::ids::{
    ConnectionId, ProcessIdentity, RequestId, SessionId, ViewId, WorkloadId,
};
use term_contracts::intervention::{InterventionNotice, InterventionReport};
use term_contracts::launch::{ClaudeProvider, LaunchMode, LaunchPolicy, Priority};
use term_contracts::metrics::{HostSample, PressureLevel, WorkloadUsage};
use term_contracts::rpc::RpcEventKind;
use term_contracts::snapshot::{Capabilities, QueueEntry, QueueReason, ReliefPolicy, ReliefState};
use term_contracts::state::{TerminalConnection, WorkloadState};
use term_contracts::workload::WorkloadDescriptor;
use term_core::{
    Clock as _, CpuPressureTracker, MonotonicClock, PressureTracker, ReservationLedger,
    WorkloadQueue,
};
use term_platform::telemetry::TelemetrySampler;
use term_platform::{GroupHandle, ResourcePlatform};
use term_storage::Storage;
use tokio::sync::{mpsc, Notify};

use crate::auth::DataTokens;
use crate::config::DaemonConfig;
use crate::paths::Paths;
use crate::sessions::SessionEntry;

/// Connection role after hello.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnRole {
    Control,
    Data,
}

/// [`ConnHandle::reserve_frame`]이 큐 칸을 주지 못한 이유 — 데이터 프레임(펌프) 전용.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameBackoff {
    /// 큐가 가득 찼다(연결은 살아 있다) — 보내는 쪽이 물러나 잠시 뒤 다시 시도한다.
    QueueFull,
    /// 연결이 이미 닫혀 있다 — 이 연결의 뷰는 정리 대상이다.
    Closed,
}

/// One live IPC connection: an outbound frame queue (encoded bytes) plus
/// routing metadata.
pub struct ConnHandle {
    pub conn_id: ConnectionId,
    pub role: ConnRole,
    /// For data connections: the control connection whose views this data
    /// connection serves.
    pub linked_control: Option<ConnectionId>,
    /// Bounded outbound FIFO (`limits.control_queue_entries`, defaults.json).
    /// Control producers `try_send`; a full control queue means the peer
    /// stopped draining. Data producers (session pumps) reserve a slot and
    /// back off on a full queue instead — see [`ConnHandle::reserve_frame`].
    pub tx: mpsc::Sender<Vec<u8>>,
    /// Latest-only copies of full-snapshot events (`queue.changed`,
    /// `resource.snapshot`), per-workload `workload.changed` and per-mission
    /// `mission.changed` frames.
    /// These events carry complete state that the UI reconciles via
    /// `revision` + snapshot, so an intermediate copy may be dropped when a
    /// newer one is pending — under a launch storm this keeps RPC responses
    /// from starving behind dozens of ~50 KiB queue frames.
    pub coalesced: std::sync::Mutex<CoalescedOut>,
    pub closed: std::sync::atomic::AtomicBool,
    /// Wakes the connection's writer loop when `closed` is set from outside
    /// it (control queue overflow, control teardown closing its data link);
    /// the writer races its socket writes against this, so teardown does
    /// not wait for a stalled peer.
    pub close_wake: Notify,
}

/// Pending latest-only outbound frames. `wake_pending` records whether a
/// WAKE sentinel for the current contents is queued; when a sentinel could
/// not be enqueued (full queue) the next coalesced send retries it instead
/// of leaving the newest frame stuck behind the cap. The workload map is
/// bounded by the workload registry (live + retained-finished ids only);
/// the mission map by the stored missions (one small hint frame per id).
#[derive(Default)]
pub struct CoalescedOut {
    /// Full-snapshot slots (event-kind discriminant).
    slots: HashMap<u8, Vec<u8>>,
    /// Latest `workload.changed` frame per workload id — every
    /// `agent_session.report` hook and telemetry tick re-broadcasts the
    /// full summary, so only the newest copy per workload needs to land.
    workloads: HashMap<String, Vec<u8>>,
    /// Latest `mission.changed` hint per mission id. The hint carries only
    /// `latest_seq` (the UI re-syncs when it is newer), and the mission
    /// actor's first sweep announces every non-archived mission at once —
    /// FIFO frames would overflow the cap and close a fresh control link.
    missions: HashMap<String, Vec<u8>>,
    wake_pending: bool,
}

impl ConnHandle {
    /// Sentinel waking the writer loop without carrying data.
    pub const WAKE: &'static [u8] = b"";

    /// Enqueue an order-sensitive frame. When a CONTROL queue sits at the
    /// `control_queue_entries` cap the peer is a slow reader: THIS
    /// connection only is closed (spec §3 violation semantics) and the
    /// caller sees `false`; daemon memory stays bounded to the cap.
    ///
    /// A data connection is never closed for a full queue (its budget is
    /// byte-based, 02-runner §2): the frame is refused (`false`) and the
    /// connection stays up. Data producers use [`ConnHandle::reserve_frame`].
    pub fn send(&self, frame: Vec<u8>) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        match self.tx.try_send(frame) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                if self.role == ConnRole::Control {
                    self.close();
                }
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// 순서 있는 데이터 프레임(세션 레코드) 한 칸을 예약한다 — 큐가 가득 차도
    /// 연결을 끊지 않는다. 저널 재생은 여러 세션의 펌프가 한 데이터 연결의
    /// 유한 큐(기본 128칸)를 나눠 쓰고 흐름 예산은 바이트 기준이므로(02-runner
    /// §2·§4), 순간적인 가득 참은 느린 독자가 아니라 역압이다: `QueueFull`이면
    /// 펌프가 커서를 그대로 두고 몇 ms 뒤 같은 레코드를 다시 시도한다. 가득 찬
    /// 큐에서 연결을 닫으면 한 pane의 재생이 같은 연결의 모든 pane 재생을
    /// 죽이고 → UI가 다시 붙어 모든 저널을 처음부터 재생하고 → 다시 가득 차는
    /// 순환이 된다(화면 깜빡임, "기록 재생 중…" 갇힘).
    ///
    /// 예약은 "여유 확인 + 넣기"를 한 단계로 만든다: 펌프는 칸을 확보한 뒤에만
    /// 흐름 원장에 기록하고 그 permit으로 보낸다. 그래서 같은 연결을 쓰는 여러
    /// 펌프가 동시에 경계에 닿아도 넘치지 않고, 원장이 커서보다 앞서지도 않는다.
    /// permit을 보내지 않고 버리면 칸이 돌아간다. 메모리는 큐 상한이 막는다.
    pub fn reserve_frame(&self) -> Result<mpsc::Permit<'_, Vec<u8>>, FrameBackoff> {
        if self.closed.load(Ordering::Acquire) {
            return Err(FrameBackoff::Closed);
        }
        self.tx.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(()) => FrameBackoff::QueueFull,
            mpsc::error::TrySendError::Closed(()) => FrameBackoff::Closed,
        })
    }

    /// Queue a full-snapshot event; at most one pending per `slot`
    /// (event-kind discriminant). Returns true when a frame is now pending.
    pub fn send_coalesced(&self, slot: u8, frame: Vec<u8>) -> bool {
        self.send_latest(|out| {
            out.slots.insert(slot, frame);
        })
    }

    /// Queue `workload.changed`; at most one pending per workload id —
    /// the summary fully describes one workload, so intermediates may drop
    /// while the newest copy survives.
    pub fn send_workload_changed(&self, workload_id: &str, frame: Vec<u8>) -> bool {
        self.send_latest(|out| {
            out.workloads.insert(workload_id.to_string(), frame);
        })
    }

    /// Queue `mission.changed`; at most one pending per mission id — the
    /// hint only says "re-sync if `latest_seq` is newer", so the newest
    /// copy per mission is all that needs to land.
    pub fn send_mission_changed(&self, mission_id: &str, frame: Vec<u8>) -> bool {
        self.send_latest(|out| {
            out.missions.insert(mission_id.to_string(), frame);
        })
    }

    /// Insert one latest-only frame and make sure a WAKE sentinel covers it.
    fn send_latest(&self, insert: impl FnOnce(&mut CoalescedOut)) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let mut pending = self.coalesced.lock().unwrap_or_else(|p| p.into_inner());
        insert(&mut pending);
        if pending.wake_pending {
            // A wake draining these contents is already queued.
            return true;
        }
        match self.tx.try_send(Self::WAKE.to_vec()) {
            Ok(()) => {
                pending.wake_pending = true;
                true
            }
            // Queue full: keep the latest copy coalesced (revision covers
            // the loss); the next coalesced send retries the wake.
            Err(_) => false,
        }
    }

    /// Mark the connection closed for producers and wake its writer loop,
    /// which tears the connection down even while blocked writing to a
    /// peer that stopped reading. Idempotent.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.close_wake.notify_one();
    }

    /// Take all pending coalesced frames, ordered for determinism (snapshot
    /// slots first, then workload ids, then mission ids), and re-arm the
    /// wake sentinel.
    pub fn take_coalesced(&self) -> Vec<Vec<u8>> {
        let mut pending = self.coalesced.lock().unwrap_or_else(|p| p.into_inner());
        pending.wake_pending = false;
        let mut slots: Vec<(u8, Vec<u8>)> = pending.slots.drain().collect();
        slots.sort_by_key(|(slot, _)| *slot);
        let mut workloads: Vec<(String, Vec<u8>)> = pending.workloads.drain().collect();
        workloads.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut missions: Vec<(String, Vec<u8>)> = pending.missions.drain().collect();
        missions.sort_by(|(a, _), (b, _)| a.cmp(b));
        slots
            .into_iter()
            .map(|(_, frame)| frame)
            .chain(workloads.into_iter().map(|(_, frame)| frame))
            .chain(missions.into_iter().map(|(_, frame)| frame))
            .collect()
    }
}

/// In-memory workload registry entry. Sensitive argv/env live ONLY here
/// (spec §6: 원문 argv/env는 DB에 쓰지 않는다) — never persisted, never logged.
pub struct WorkloadEntry {
    pub workload_id: WorkloadId,
    pub session_id: SessionId,
    pub request_id: Option<RequestId>,
    pub mode: LaunchMode,
    /// Mirrored `WorkloadState`; storage remains the source of truth (the
    /// mirror is updated after each successful storage transition).
    pub state: WorkloadState,
    pub title: String,
    pub cwd: String,
    pub program: String,
    /// Effective policy at launch; updated in place by
    /// `workload.update_policy` (storage has no policy-update API — see
    /// done/I04 deviations: the runtime copy is authoritative).
    pub policy: LaunchPolicy,
    /// Priority at launch; updated by `workload.reprioritize`.
    pub priority: Priority,
    /// Full launch descriptor for QUEUED managed workloads (queue input;
    /// taken when the scheduler starts the launch).
    pub descriptor: Option<WorkloadDescriptor>,
    pub queue_reason: Option<QueueReason>,
    pub cancel_requested: bool,
    pub root_exited: bool,
    pub exit_code: Option<i32>,
    pub last_error_code: Option<String>,
    pub missing_capabilities: Vec<String>,
    /// Live OS resource group (managed, while attached).
    pub group: Option<GroupHandle>,
    /// Session actor driver (set once the PTY exists).
    pub actor: Option<Arc<term_pty::actor::SessionActorHandle>>,
    /// True once the reservation was released (terminal or launch abort);
    /// release happens exactly once.
    pub reservation_released: bool,
    /// Managed workload holds a reservation from admission to terminal.
    pub reserved: bool,
    /// Target exit code reported through the launch gate (Windows helpers
    /// wait for the target; Unix helpers exec so the PTY child IS the target).
    pub gate_exit_code: Mutex<Option<i32>>,
    /// Attach state for the workload summary.
    pub connection: TerminalConnection,
    /// PTY 첫 자식(셸 모드에서는 셸) pid — 에이전트 트리 감시의 루트.
    /// 재사용 방어는 이 맨 pid만으로 안 된다: 관측·완화의 닻은
    /// [`DaemonState::shell_identity`]가 돌려주는 스폰 시점 신원이다(F1).
    pub shell_pid: Option<u32>,
    /// 세션 안에서 관찰 중인 AI 코딩 에이전트(에이전트 감시 루프 소유).
    pub agent: Option<term_contracts::snapshot::AgentStatus>,
    /// Set when the launch pipeline finished; guards a single terminal
    /// transition from racing paths.
    pub finalized: bool,
    /// 라우팅된 Claude pane의 provider 선택자(비밀 아님). 메모리에만 살고
    /// 엔트리와 함께 사라진다 — DB·스냅샷에는 쓰지 않는다. 관리 실행은
    /// 대기열에서 깨어날 때 이 값으로 `GateTarget.env_remove`를 다시 만들고
    /// 토큰을 저장소에서 다시 읽는다(`descriptor`에는 토큰이 없다).
    pub claude_provider: Option<ClaudeProvider>,
    /// 라우팅된 pane의 PTY 출력에서 provider 토큰을 지우는 redactor(저널
    /// 앞단, [`crate::sessions::RedactingJournal`]). 셸 모드는 런치 때, 관리
    /// 모드는 시작 시점에 다시 읽은 키로 넣는다(대기열에 있는 동안은
    /// `None`). 종료·실패·취소 전이에서 놓는다 — 토큰 사본을 종료된 엔트리가
    /// 붙들지 않게. `None`이면 출력은 손대지 않는다.
    pub redactor: Option<Arc<crate::connections::SecretRedactor>>,
}

/// 종료된 세션을 레지스트리에 더 붙들어 두는 개수. UI의 대기열 서랍은
/// 방금 끝난 실행에 "붙기"를 제공하므로 최근 세션은 재생 가능해야 하지만,
/// 그 밖의 세션은 회수한다 — `SessionEntry` 하나가 저널 오프셋 맵·흐름
/// 제어 상태·에폭을 데몬이 사는 내내 붙들고 있었다.
pub const FINALIZED_SESSIONS_RETAINED: usize = 32;

/// 종료된 워크로드를 레지스트리에 더 붙들어 두는 개수. 스냅샷은 매번
/// 레지스트리 전체를 직렬화하므로(01 §2, 64 KiB 프레임 예산) 무한히 쌓이면
/// `system.snapshot`이 영구히 BUSY가 된다. 이력 조회는 스토리지가 답한다.
pub const FINISHED_WORKLOADS_RETAINED: usize = 64;

/// The whole-daemon shared state.
pub struct DaemonState {
    pub daemon_id: String,
    pub config: DaemonConfig,
    pub paths: Paths,
    pub storage: Arc<Storage>,
    /// O1 mission orchestration service (None while the feature gate is off).
    pub missions: Option<Arc<crate::mission::MissionService>>,
    pub platform: Arc<dyn ResourcePlatform>,
    pub caps: Mutex<Capabilities>,
    pub boot_id_reliable: bool,
    /// Host logical CPU count (admission `C = max(1, cpus/2)`).
    pub logical_cpus: u32,

    /// Globally increasing snapshot revision (starts at 1; every mutation
    /// bumps it; snapshots carry the current value).
    pub revision: AtomicU64,
    pub clock: MonotonicClock,

    pub queue: WorkloadQueue<MonotonicClock>,
    pub ledger: ReservationLedger,
    pub telemetry: Mutex<TelemetrySampler>,
    pub pressure: Mutex<PressureTracker<MonotonicClock>>,
    /// CPU 포화도 히스테리시스(08 §1) — 메모리 압력과 같은 규칙, 다른 축.
    pub cpu_pressure: Mutex<CpuPressureTracker<MonotonicClock>>,
    /// Last host sample + the monotonic ms it was taken at.
    pub host: Mutex<(Option<HostSample>, u64)>,
    pub pressure_level: Mutex<PressureLevel>,
    /// Effective CPU saturation level (the tracker's output, cached for
    /// readers that must not take the tracker lock).
    pub cpu_pressure_level: Mutex<PressureLevel>,
    pub reconciliation_required: AtomicBool,

    pub connections: Mutex<HashMap<ConnectionId, Arc<ConnHandle>>>,
    pub workloads: Mutex<HashMap<WorkloadId, Arc<Mutex<WorkloadEntry>>>>,
    pub sessions: Mutex<HashMap<SessionId, Arc<SessionEntry>>>,
    /// 최근 종료된 세션 id 링([`FINALIZED_SESSIONS_RETAINED`]개). 링에 있는
    /// 동안은 레지스트리에 남아 재attach·재생이 가능하고, 밀려나면
    /// [`DaemonState::retire_session_if_cold`]가 회수한다.
    pub finalized_sessions: Mutex<VecDeque<SessionId>>,
    /// 종료 상태에 도달한 워크로드 id 링([`FINISHED_WORKLOADS_RETAINED`]개).
    /// 이력의 진실 원본은 스토리지다 — 메모리 레지스트리는 살아 있는 것과
    /// "최근"만 담는다.
    pub finished_workloads: Mutex<VecDeque<WorkloadId>>,

    /// Daemon-wide journal byte budget (2 GiB default, shared by sessions).
    pub journal_budget: Arc<Mutex<term_pty::journal::GlobalJournalBudget>>,
    /// 최근 개입 신호 링(W1-5: `intervention.list`가 반환) + 멱등 판정.
    pub interventions: Mutex<InterventionRing>,
    /// Daemon-wide raw/transport output budgets (8/24 MiB, shared by views).
    pub flow_budget: Arc<Mutex<term_pty::flow::GlobalOutputBudget>>,

    pub data_tokens: DataTokens,

    /// Cached per-workload usage from the telemetry loop.
    pub usage_cache: Mutex<HashMap<WorkloadId, WorkloadUsage>>,
    /// 컨트롤 연결이 지금 보고 있는 세션(`session.focus`, 08 §1). 연결 하나에
    /// 최대 하나 — 연결이 닫히면 [`DaemonState::remove_connection`]이 지운다.
    pub focused_sessions: Mutex<HashMap<ConnectionId, SessionId>>,
    /// 압력 완화(08 §2)의 결정 코어. 텔레메트리 틱과 `session.relief`가
    /// 공유하며, 락을 쥔 채로 플랫폼을 부르지 않는다(계획 → 락 해제 →
    /// 적용 → 락 → 기록).
    pub relief: Mutex<crate::relief::ReliefController>,
    /// 자원 가드(08 §5)의 결정 코어. 같은 규율로 공유된다.
    pub guard: Mutex<crate::guard::GuardController>,
    /// `Capabilities.suspend_resume`이 Supported인가 — 데몬 기동에 한 번.
    pub suspend_resume_supported: bool,
    /// `Capabilities.scheduling_yield`가 Supported인가 — 데몬 기동에 한 번만
    /// 확인한다(틱마다 백엔드를 다시 탐지하지 않는다).
    pub scheduling_yield_supported: bool,

    /// The tokio runtime handle (blocking adapters + gate pipes need it on
    /// threads outside the async context).
    pub runtime: tokio::runtime::Handle,
    /// Shutdown signaling: `true` once shutdown was requested.
    pub shutdown: tokio::sync::watch::Sender<bool>,
    /// daemon.shutdown(stop_workloads=true) sets this before shutdown.
    pub stop_workloads_on_shutdown: AtomicBool,
    /// Whether the retention sweep already ran a successful VACUUM this
    /// daemon lifetime (the file rebuild is too expensive to repeat).
    pub db_vacuumed: AtomicBool,
    /// Last client/workload activity for the idle-exit policy.
    pub last_activity: Mutex<Instant>,
}

impl DaemonState {
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub fn bump_revision(&self) -> u64 {
        self.revision.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn now_ms(&self) -> u64 {
        self.clock.now_ms()
    }

    pub fn touch_activity(&self) {
        *self.last_activity.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
    }

    // -- connections ---------------------------------------------------------

    pub fn register_connection(&self, handle: Arc<ConnHandle>) {
        self.connections
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(handle.conn_id.clone(), handle);
        self.touch_activity();
    }

    pub fn remove_connection(&self, conn_id: &ConnectionId) {
        self.connections
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(conn_id);
        if let Some(missions) = &self.missions {
            missions.drop_connection(conn_id);
        }
        // 닫힌 창은 아무것도 보고 있지 않다(08 §1): 그 연결의 포커스만
        // 지운다 — 다른 창의 포커스는 그대로다.
        let had_focus = self
            .focused_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(conn_id)
            .is_some();
        if had_focus {
            self.bump_revision();
        }
        self.touch_activity();
    }

    // -- focus (08 §1) ---------------------------------------------------

    /// 모든 컨트롤 연결의 포커스 합집합(정렬·중복 제거).
    pub fn focused_session_ids(&self) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .focused_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        ids.dedup_by(|a, b| a.as_str() == b.as_str());
        ids
    }

    /// 이 연결의 포커스를 세우거나(Some) 지운다(None). 실제로 바뀌었으면 true.
    pub fn set_focused_session(&self, conn: &ConnectionId, session_id: Option<SessionId>) -> bool {
        let mut focused = self
            .focused_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match session_id {
            Some(id) => {
                let previous = focused.insert(conn.clone(), id.clone());
                previous.is_none_or(|old| old.as_str() != id.as_str())
            }
            None => focused.remove(conn).is_some(),
        }
    }

    /// 세션이 사라졌다(워크로드 종료/회수): 그 세션을 보고 있던 모든 연결의
    /// 포커스를 지운다 — 스냅샷이 죽은 세션을 포커스로 광고하면 안 된다.
    pub fn clear_focus_for_session(&self, session_id: &SessionId) {
        let mut focused = self
            .focused_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let before = focused.len();
        focused.retain(|_, id| id.as_str() != session_id.as_str());
        let changed = focused.len() != before;
        drop(focused);
        if changed {
            self.bump_revision();
        }
    }

    // -- relief (08 §2) ---------------------------------------------------

    /// 스냅샷이 보고할 `(relief, protected)`.
    pub fn relief_view(&self, workload_id: &WorkloadId) -> (ReliefState, bool) {
        self.relief
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .view(workload_id)
    }

    /// 현재 완화 정책(`relief.set_policy`가 소유한다).
    pub fn relief_policy(&self) -> ReliefPolicy {
        self.relief
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .policy()
    }

    /// 현재 자원 가드 정책(`guard.set_policy`가 소유한다).
    pub fn guard_policy(&self) -> term_contracts::snapshot::GuardPolicy {
        self.guard
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .policy()
    }

    /// 08 §5: 가드 상태 보고용 `(guard, warning)`.
    pub fn guard_view(
        &self,
        workload_id: &WorkloadId,
    ) -> (
        term_contracts::snapshot::GuardState,
        Option<term_contracts::snapshot::GuardReason>,
    ) {
        self.guard
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .view(workload_id)
    }

    /// 지금 레지스트리에 살아 있는(종료 상태가 아닌) 워크로드 — 완화 정책의
    /// 입력. 여기서 빠진 워크로드의 기록은 컨트롤러가 스스로 버린다.
    pub fn live_workloads_for_relief(&self) -> Vec<crate::relief::LiveWorkload> {
        self.workloads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(|entry| {
                let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
                (!guard.state.is_terminal()).then(|| crate::relief::LiveWorkload {
                    workload_id: guard.workload_id.clone(),
                    session_id: guard.session_id.clone(),
                    state: guard.state,
                })
            })
            .collect()
    }

    // -- events ---------------------------------------------------------------

    /// Broadcast a metadata event to ALL control connections
    /// (`workload.changed`, `queue.changed`, `resource.snapshot`,
    /// `session.exited`, ...). Encoded once. Full-snapshot events coalesce to
    /// the latest pending copy per connection (revision covers loss);
    /// `workload.changed` coalesces per workload id — `agent_session.report`
    /// hooks and telemetry ticks can re-broadcast one workload rapidly —
    /// and `mission.changed` per mission id (the mission actor's first
    /// sweep hints every non-archived mission in one burst).
    pub fn broadcast_control(&self, kind: RpcEventKind, payload: serde_json::Value) {
        // Peek the entity id before encoding consumes the payload; a
        // payload without one falls back to the FIFO path.
        let entity_key = coalesce_entity_field(kind)
            .and_then(|field| payload.get(field))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let frame = match encode_event(kind, payload) {
            Ok(f) => f,
            Err(_) => return,
        };
        let slot = coalesce_slot(kind);
        let conns = self.connections.lock().unwrap_or_else(|p| p.into_inner());
        for handle in conns.values().filter(|h| h.role == ConnRole::Control) {
            match (&entity_key, slot) {
                (Some(id), _) if matches!(kind, RpcEventKind::MissionChanged) => {
                    handle.send_mission_changed(id, frame.clone());
                }
                (Some(id), _) => {
                    handle.send_workload_changed(id, frame.clone());
                }
                (None, Some(slot)) => {
                    handle.send_coalesced(slot, frame.clone());
                }
                (None, None) => {
                    handle.send(frame.clone());
                }
            }
        }
    }

    /// Close every data connection serving `control`'s views. Without its
    /// control connection a data link carries nothing (views are keyed by
    /// the control id and its data token was single-use), and its writer
    /// may be blocked on a peer that stopped reading — a data queue never
    /// overflow-closes, so this is what tears such a link down.
    pub fn close_linked_data(&self, control: &ConnectionId) {
        let conns = self.connections.lock().unwrap_or_else(|p| p.into_inner());
        for handle in conns.values() {
            if handle.role == ConnRole::Data && handle.linked_control.as_ref() == Some(control) {
                handle.close();
            }
        }
    }

    /// Data connection (if any) currently serving `control`'s views.
    pub fn data_conn_for(&self, control: &ConnectionId) -> Option<Arc<ConnHandle>> {
        let conns = self.connections.lock().unwrap_or_else(|p| p.into_inner());
        conns
            .values()
            .find(|h| h.role == ConnRole::Data && h.linked_control.as_ref() == Some(control))
            .cloned()
    }

    // -- workload / session registry ------------------------------------------

    pub fn workload_entry(&self, id: &WorkloadId) -> Option<Arc<Mutex<WorkloadEntry>>> {
        self.workloads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned()
    }

    pub fn session(&self, id: &SessionId) -> Option<Arc<SessionEntry>> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned()
    }

    // -- bounded retention of finished work ------------------------------

    /// 세션 actor가 종료됐음을 기록한다. 보관 링이 상한을 넘으면 가장
    /// 오래된 세션을 레지스트리에서 회수한다 — 아직 뷰가 붙어 있으면
    /// (종료된 출력을 보고 있는 중) 마지막 detach가 회수를 맡는다.
    pub fn note_session_finalized(&self, session_id: &SessionId) {
        let evicted = {
            let mut ring = self
                .finalized_sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            ring.retain(|id| id != session_id);
            ring.push_back(session_id.clone());
            let mut evicted = Vec::new();
            while ring.len() > FINALIZED_SESSIONS_RETAINED {
                if let Some(old) = ring.pop_front() {
                    evicted.push(old);
                }
            }
            evicted
        };
        for id in evicted {
            self.retire_session_if_cold(&id);
        }
    }

    /// 식은 세션(actor 종료 + 뷰 0 + 보관 링 밖)을 레지스트리에서 제거한다.
    /// actor가 아직 살아 있는 세션은 절대 제거하지 않는다. 제거했으면 true.
    ///
    /// 마지막 `Arc<SessionEntry>`가 사라지기 전에 펌프 정지 신호를 세우고
    /// 깨운다 — 펌프 스레드가 자기 Arc를 놓아야 항목이 실제로 해제된다.
    pub fn retire_session_if_cold(&self, session_id: &SessionId) -> bool {
        let retained = self
            .finalized_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|id| id == session_id);
        if retained {
            return false;
        }
        // 워크로드가 아직 종료 상태가 아니면(루트는 나갔지만 소유 자손이
        // 살아 RUNNING 유지, 02-runner §5) 세션도 회수하지 않는다 —
        // 스냅샷이 광고하는 session_id에 attach가 "session not found"로
        // 답해선 안 된다. 워크로드 종료 시 finish_workload가 다시 링에
        // 넣는다. 잠금 순서: workloads → (해제) → sessions.
        let workload_live = self.session(session_id).is_some_and(|entry| {
            self.workload_entry(&entry.workload_id).is_some_and(|w| {
                !w.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .state
                    .is_terminal()
            })
        });
        if workload_live {
            return false;
        }
        let removed = {
            let mut registry = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let cold = registry.get(session_id).is_some_and(|entry| {
                entry.actor_finalized.load(Ordering::Acquire)
                    && entry
                        .views
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .is_empty()
            });
            if cold {
                registry.remove(session_id)
            } else {
                None
            }
        };
        match removed {
            Some(entry) => {
                entry.pump_stop.store(true, Ordering::Release);
                entry.wake();
                tracing::debug!(session = %session_id, "finished session retired from the registry");
                true
            }
            None => false,
        }
    }

    /// 세션을 보관 링에서도 지우고 회수한다 — retention이 저널 파일을
    /// 삭제한 뒤 죽은 항목에 attach되지 않게 한다.
    pub fn forget_session(&self, session_id: &SessionId) {
        self.finalized_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|id| id != session_id);
        self.retire_session_if_cold(session_id);
    }

    /// 워크로드가 종료 상태에 도달했음을 기록한다. 링이 상한을 넘으면 가장
    /// 오래된 종료 워크로드를 레지스트리(+usage 캐시)에서 축출한다.
    /// 종료 상태가 아닌 항목은 절대 축출하지 않는다.
    pub fn note_workload_terminal(&self, workload_id: &WorkloadId) {
        let evicted = {
            let mut ring = self
                .finished_workloads
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            ring.retain(|id| id != workload_id);
            ring.push_back(workload_id.clone());
            let mut evicted = Vec::new();
            while ring.len() > FINISHED_WORKLOADS_RETAINED {
                if let Some(old) = ring.pop_front() {
                    evicted.push(old);
                }
            }
            evicted
        };
        // F1: 종료 워크로드의 셸 루트 신원도 이제 필요 없다 — 보관소가 살아
        // 있는 워크로드 수에 묶이게 종료 시점에 지운다(다른 락을 쥔 뒤가
        // 아니라 링 락을 놓은 지점에서 호출한다).
        self.forget_shell_identity(workload_id);
        for id in evicted {
            let removed = {
                let mut registry = self.workloads.lock().unwrap_or_else(|p| p.into_inner());
                let terminal = registry.get(&id).is_some_and(|entry| {
                    entry
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .state
                        .is_terminal()
                });
                if terminal {
                    registry.remove(&id)
                } else {
                    None
                }
            };
            if removed.is_some() {
                self.usage_cache
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
                tracing::debug!(workload = %id, "terminal workload evicted from the registry");
            }
        }
    }

    /// Update a workload's mirrored state (after the storage transition
    /// succeeded) and broadcast `workload.changed`.
    pub fn workload_state_changed(&self, id: &WorkloadId) {
        let summary = self.workload_summary(id);
        self.bump_revision();
        if let Some(summary) = summary {
            self.broadcast_control(
                RpcEventKind::WorkloadChanged,
                serde_json::to_value(&summary).unwrap_or(serde_json::Value::Null),
            );
        }
    }

    /// Build a summary for one workload (None when unknown).
    pub fn workload_summary(
        &self,
        id: &WorkloadId,
    ) -> Option<term_contracts::snapshot::WorkloadSummary> {
        let entry = self.workload_entry(id)?;
        let guard = entry.lock().unwrap_or_else(|p| p.into_inner());
        Some(self.summarize_locked(&guard))
    }

    /// Build a summary for a locked workload entry (no registry locks held).
    /// Terminal workloads omit `usage`: a 100-workload storm's snapshot with
    /// per-workload metrics blows the 64 KiB RPC frame (spec 01 §4: snapshot
    /// carries summaries only; 대형 목록은 프레임 안에 들어가게 잘라낸다).
    fn summarize_locked(&self, entry: &WorkloadEntry) -> term_contracts::snapshot::WorkloadSummary {
        let usage = if entry.state.is_terminal() {
            None
        } else {
            self.usage_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&entry.workload_id)
                .cloned()
        };
        // B18: while the workload is still live, surface the actor's journal
        // stop reason (JOURNAL_LIMIT/DISK_FULL) even before the terminal
        // record persists it (02-runner §5: read 중지와 cap 이유 표시).
        // Lock order is one-way (workload entry -> actor status); the actor
        // never takes registry locks while holding its status lock.
        let live_journal_stop = entry
            .actor
            .as_ref()
            .and_then(|actor| actor.status().journal_error)
            .and_then(|error| crate::orchestrator::journal_error_code(&error).map(str::to_string));
        // 08 §2: 완화 상태는 컨트롤러가 진실 원본이다. 종료된 워크로드는
        // 프로세스가 이미 없으므로 계약 기본값(NONE/비보호)으로 보고한다.
        let (relief, protected) = if entry.state.is_terminal() {
            (ReliefState::None, false)
        } else {
            self.relief_view(&entry.workload_id)
        };
        // 08 §5: 가드 상태도 같은 규율. 종료 워크로드는 NONE/경고 없음.
        let (guard, guard_warning) = if entry.state.is_terminal() {
            (term_contracts::snapshot::GuardState::None, None)
        } else {
            self.guard_view(&entry.workload_id)
        };
        term_contracts::snapshot::WorkloadSummary {
            workload_id: entry.workload_id.clone(),
            session_id: Some(entry.session_id.clone()),
            mode: entry.mode,
            state: entry.state,
            priority: entry.priority,
            title: entry.title.clone(),
            cwd: entry.cwd.clone(),
            program: entry.program.clone(),
            reservation_bytes: entry.policy.reservation_bytes.clone(),
            cpu_slots: entry.policy.cpu_slots,
            enforcement: entry.policy.enforcement,
            root_exited: entry.root_exited,
            cancel_requested: entry.cancel_requested,
            exit_code: entry.exit_code,
            last_error_code: entry.last_error_code.clone().or(live_journal_stop),
            queue_reason: entry.queue_reason,
            connection: entry.connection,
            usage,
            agent: entry.agent.clone(),
            relief,
            protected,
            guard,
            guard_warning,
        }
    }

    /// Full snapshot assembly. `revision` is monotonic per mutation (the
    /// caller-facing monotonicity property: successive snapshots never go
    /// back while changes happen).
    pub fn build_snapshot(&self) -> term_contracts::snapshot::Snapshot {
        let host = self
            .host
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .clone()
            .unwrap_or_else(empty_host_sample);
        let caps = self.caps.lock().unwrap_or_else(|p| p.into_inner()).clone();

        let workloads: Vec<_> = {
            let registry = self.workloads.lock().unwrap_or_else(|p| p.into_inner());
            let mut list: Vec<_> = registry
                .values()
                .map(|e| {
                    let guard = e.lock().unwrap_or_else(|p| p.into_inner());
                    self.summarize_locked(&guard)
                })
                .collect();
            list.sort_by(|a, b| a.workload_id.as_str().cmp(b.workload_id.as_str()));
            list
        };
        let queue: Vec<QueueEntry> = self.queue.snapshot();

        term_contracts::snapshot::Snapshot {
            revision: self.revision(),
            host,
            workloads,
            queue,
            capabilities: caps,
            reconciliation_required: self.reconciliation_required.load(Ordering::Acquire),
            focused_session_ids: self.focused_session_ids(),
            relief_policy: self.relief_policy(),
            guard_policy: self.guard_policy(),
        }
    }

    /// Count of non-terminal workloads (session-limit admission).
    pub fn active_workload_count(&self) -> usize {
        self.workloads
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter(|e| {
                let guard = e.lock().unwrap_or_else(|p| p.into_inner());
                !guard.state.is_terminal()
            })
            .count()
    }

    /// Broadcast `queue.changed` with the current queue snapshot.
    pub fn broadcast_queue_changed(&self) {
        let queue = self.queue.snapshot();
        self.bump_revision();
        let payload = serde_json::json!({
            "revision": self.revision(),
            "queue": queue,
        });
        self.broadcast_control(RpcEventKind::QueueChanged, payload);
    }

    /// Current admission host facts from the live telemetry sample.
    pub fn admission_host(&self) -> term_core::AdmissionHost {
        let (sample, at) = self.host.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let now = self.now_ms();
        let pressure = *self
            .pressure_level
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let reconciliation = self.reconciliation_required.load(Ordering::Acquire);
        match sample {
            Some(host) => term_core::AdmissionHost {
                total_bytes: host.total_bytes().unwrap_or(0),
                available_bytes: host.available_bytes(),
                sample_age_ms: now.saturating_sub(at),
                reconciliation_required: reconciliation,
                pressure,
            },
            None => term_core::AdmissionHost {
                total_bytes: 0,
                available_bytes: None,
                sample_age_ms: u64::MAX,
                reconciliation_required: reconciliation,
                pressure,
            },
        }
    }

    /// View ids of a session attached by one control connection.
    pub fn views_for_conn(session: &Arc<SessionEntry>, conn: &ConnectionId) -> Vec<ViewId> {
        session
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(_, v)| &v.conn == conn)
            .map(|(id, _)| id.clone())
            .collect()
    }
}

fn empty_host_sample() -> HostSample {
    use term_contracts::metrics::Metric;
    let unavailable =
        || Metric::<term_contracts::U64String>::unavailable("sysinfo", "no sample yet");
    HostSample {
        monotonic_ms: 0,
        physical_total_bytes: unavailable(),
        physical_available_bytes: unavailable(),
        physical_used_bytes: None,
        swap_used_bytes: unavailable(),
        pressure: PressureLevel::Normal,
        cpu_pressure: PressureLevel::Normal,
        cpu_cores_used: Metric::unavailable("sysinfo", "no sample yet"),
        logical_cpu_count: 0,
        disks: Vec::new(),
        interfaces: Vec::new(),
    }
}

/// Encode one event frame.
pub fn encode_event(
    kind: RpcEventKind,
    payload: serde_json::Value,
) -> Result<Vec<u8>, term_contracts::rpc::FrameError> {
    let event = term_contracts::rpc::RpcEvent {
        v: term_contracts::rpc::PROTOCOL_VERSION,
        event: kind,
        payload,
    };
    let value = serde_json::to_value(&event)
        .map_err(|e| term_contracts::rpc::FrameError::InvalidJson(e.to_string()))?;
    term_contracts::rpc::encode_frame(&value)
}

/// Full-snapshot events coalesce to one pending copy per connection; every
/// other event kind is order-sensitive and flows through the FIFO.
/// `workload.changed` and `mission.changed` coalesce per entity id instead
/// of by slot (see [`coalesce_entity_field`]).
fn coalesce_slot(kind: RpcEventKind) -> Option<u8> {
    match kind {
        RpcEventKind::QueueChanged => Some(1),
        RpcEventKind::ResourceSnapshot => Some(2),
        _ => None,
    }
}

/// Events that coalesce to the latest pending copy per entity id, and the
/// payload field carrying that id ([`ConnHandle::send_workload_changed`],
/// [`ConnHandle::send_mission_changed`]). A payload lacking the field
/// falls back to the FIFO path.
fn coalesce_entity_field(kind: RpcEventKind) -> Option<&'static str> {
    match kind {
        RpcEventKind::WorkloadChanged => Some("workload_id"),
        RpcEventKind::MissionChanged => Some("mission_id"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_counter_is_monotonic() {
        let revision = AtomicU64::new(1);
        let a = revision.fetch_add(1, Ordering::AcqRel) + 1;
        let b = revision.fetch_add(1, Ordering::AcqRel) + 1;
        assert!(b > a);
    }

    fn test_handle(capacity: usize) -> (ConnHandle, mpsc::Receiver<Vec<u8>>) {
        let (tx, rx) = mpsc::channel::<Vec<u8>>(capacity);
        (
            ConnHandle {
                conn_id: ConnectionId::generate(),
                role: ConnRole::Control,
                linked_control: None,
                tx,
                coalesced: std::sync::Mutex::new(CoalescedOut::default()),
                closed: std::sync::atomic::AtomicBool::new(false),
                close_wake: Notify::new(),
            },
            rx,
        )
    }

    /// At the FIFO cap the connection is a slow reader: the overflowing
    /// order-sensitive frame is refused and the connection closes for
    /// producers (this connection only).
    #[test]
    fn conn_queue_overflow_closes_only_that_connection() {
        let (handle, mut rx) = test_handle(2);
        assert!(handle.send(b"one".to_vec()));
        assert!(handle.send(b"two".to_vec()));
        assert!(!handle.send(b"three".to_vec()));
        assert!(handle.closed.load(Ordering::Acquire));
        assert!(!handle.send(b"four".to_vec()));
        assert!(!handle.send_coalesced(1, b"q".to_vec()));
        assert!(handle.take_coalesced().is_empty());
        // The queued FIFO frames are still drainable by the writer.
        assert_eq!(rx.try_recv().unwrap(), b"one".to_vec());
        assert_eq!(rx.try_recv().unwrap(), b"two".to_vec());
        assert!(rx.try_recv().is_err());
    }

    /// 데이터 프레임 예약(펌프)은 가득 찬 큐를 역압으로 다룬다 — 연결을
    /// 끊지 않는다. writer가 비우면 같은 프레임이 다시 들어간다(재생 재시작
    /// 순환의 원인이던 연결 종료를 없앤 것). 데이터 연결은 `send`로도
    /// 넘침 종료되지 않는다.
    #[test]
    fn data_frame_full_queue_is_backpressure_not_death() {
        let (mut handle, mut rx) = test_handle(1);
        handle.role = ConnRole::Data;
        let permit = handle.reserve_frame().expect("free slot");
        permit.send(b"one".to_vec());
        let refused = handle.reserve_frame().err();
        assert_eq!(refused, Some(FrameBackoff::QueueFull));
        assert!(!handle.send(b"two".to_vec()));
        // 연결은 살아 있다 — 닫힌 흔적도, 이후 전송 거부도 없다.
        assert!(!handle.closed.load(Ordering::Acquire));
        assert_eq!(rx.try_recv().unwrap(), b"one".to_vec());
        let permit = handle.reserve_frame().expect("slot drained");
        permit.send(b"two".to_vec());
        assert_eq!(rx.try_recv().unwrap(), b"two".to_vec());
    }

    /// 보내지 않고 버린 예약(흐름 회계가 거절한 레코드)은 칸을 돌려준다.
    #[test]
    fn unsent_data_reservation_returns_its_slot() {
        let (mut handle, mut rx) = test_handle(1);
        handle.role = ConnRole::Data;
        drop(handle.reserve_frame().expect("free slot"));
        let permit = handle.reserve_frame().expect("slot returned");
        permit.send(b"x".to_vec());
        assert_eq!(rx.try_recv().unwrap(), b"x".to_vec());
    }

    /// 이미 닫힌 연결에 대한 데이터 프레임 예약은 `Closed` — 펌프가 뷰를 정리한다.
    #[test]
    fn data_frame_on_closed_connection_reports_closed() {
        let (mut handle, _rx) = test_handle(2);
        handle.role = ConnRole::Data;
        handle.close();
        let refused = handle.reserve_frame().err();
        assert_eq!(refused, Some(FrameBackoff::Closed));
    }

    /// `mission.changed` keeps one pending hint per mission id (after the
    /// workload frames), so a sweep hinting hundreds of missions cannot
    /// overflow — and close — a control connection's queue.
    #[test]
    fn mission_hints_coalesce_per_id_without_overflow() {
        let (handle, mut rx) = test_handle(2);
        for round in 0..3u8 {
            for mission in 0..200 {
                let id = format!("m{mission:03}");
                let hint = round.to_string().into_bytes();
                assert!(handle.send_mission_changed(&id, hint));
            }
        }
        assert!(handle.send_workload_changed("w1", b"w".to_vec()));
        assert!(!handle.closed.load(Ordering::Acquire));
        // One wake covers every pending hint.
        assert_eq!(rx.try_recv().unwrap(), ConnHandle::WAKE.to_vec());
        assert!(rx.try_recv().is_err());
        let frames = handle.take_coalesced();
        assert_eq!(frames.len(), 201);
        assert_eq!(frames[0], b"w".to_vec());
        assert!(frames[1..].iter().all(|frame| frame.as_slice() == b"2"));
    }

    /// A WAKE sentinel that could not be enqueued (full queue) is retried
    /// by the next coalesced send once the queue has room.
    #[test]
    fn coalesced_wake_retries_after_full_queue() {
        let (handle, mut rx) = test_handle(1);
        assert!(handle.send(b"fifo".to_vec()));
        assert!(!handle.send_coalesced(1, b"first".to_vec()));
        assert!(!handle.send_coalesced(1, b"second".to_vec()));
        assert_eq!(rx.try_recv().unwrap(), b"fifo".to_vec());
        // Room now: the newest copy wakes the writer without carrying data.
        assert!(handle.send_coalesced(1, b"third".to_vec()));
        assert_eq!(rx.try_recv().unwrap(), ConnHandle::WAKE.to_vec());
        assert!(rx.try_recv().is_err());
        assert_eq!(handle.take_coalesced(), vec![b"third".to_vec()]);
        assert!(handle.take_coalesced().is_empty());
    }

    /// `workload.changed` keeps one pending frame per workload id; take
    /// order is slots first, then workload ids sorted.
    #[test]
    fn workload_frames_coalesce_per_id() {
        let (handle, _rx) = test_handle(4);
        assert!(handle.send_workload_changed("w1", b"a".to_vec()));
        assert!(handle.send_workload_changed("w2", b"b".to_vec()));
        assert!(handle.send_workload_changed("w1", b"c".to_vec()));
        assert_eq!(handle.take_coalesced(), vec![b"c".to_vec(), b"b".to_vec()]);
    }
}

/// 개입 신호 보관 링(W1-5). 최근 [`RETAINED`]개만 유지하고,
/// [`DEDUP_ENTRIES`]개의 report_id로 멱등 중복을 판정한다.
pub struct InterventionRing {
    recent: VecDeque<InterventionNotice>,
    seen: VecDeque<String>,
}

impl InterventionRing {
    pub fn new() -> Self {
        Self {
            recent: VecDeque::new(),
            seen: VecDeque::new(),
        }
    }

    /// 이미 본 report_id면 false(중복). 아니면 기록하고 true.
    pub fn insert(&mut self, report: InterventionNotice) -> bool {
        if self.seen.iter().any(|id| id == &report.report.report_id) {
            return false;
        }
        self.seen.push_back(report.report.report_id.clone());
        while self.seen.len() > term_contracts::intervention::limits::DEDUP_ENTRIES {
            self.seen.pop_front();
        }
        self.recent.push_back(report);
        while self.recent.len() > term_contracts::intervention::limits::RETAINED {
            self.recent.pop_front();
        }
        true
    }

    /// 최근 항목을 오래된 순으로 반환(스냅샷 복사).
    pub fn recent(&self) -> Vec<InterventionNotice> {
        self.recent.iter().cloned().collect()
    }
}

impl Default for InterventionRing {
    fn default() -> Self {
        Self::new()
    }
}

/// F1: 직접 셸 루트의 신원 보관소(pid + start token + boot id,
/// spec `01-contracts.md` §1). `WorkloadEntry`에 필드를 더하면
/// `lifecycle.rs`의 구조체 리터럴이 깨지므로(이번 라운드 소유권 밖)
/// 프로세스 전역 옆 맵에 둔다 — 데몬 프로세스 하나에 `DaemonState`도
/// 하나(lib.rs)이고 키는 UUID workload id라 우연한 충돌이 없다.
/// 관측(telemetry_loop)과 완화(relief)는 이 신원을 닻으로 셸 루트 pid의
/// 재사용을 검증한다. 해제는 종료 전이
/// [`DaemonState::note_workload_terminal`]이 맡는다(무한히 자라지 않는다).
#[derive(Default)]
struct ShellRootIdentities {
    roots: HashMap<WorkloadId, ProcessIdentity>,
}

impl ShellRootIdentities {
    fn record(&mut self, workload_id: &WorkloadId, identity: ProcessIdentity) {
        self.roots.insert(workload_id.clone(), identity);
    }

    fn get(&self, workload_id: &WorkloadId) -> Option<ProcessIdentity> {
        self.roots.get(workload_id).cloned()
    }

    fn forget(&mut self, workload_id: &WorkloadId) {
        self.roots.remove(workload_id);
    }
}

static SHELL_ROOT_IDENTITIES: Mutex<Option<ShellRootIdentities>> = Mutex::new(None);

impl DaemonState {
    /// 스폰 직후 셸 루트의 신원을 기록한다(orchestrator의 셸 런치 경로).
    pub fn record_shell_identity(&self, workload_id: &WorkloadId, identity: ProcessIdentity) {
        let mut roots = SHELL_ROOT_IDENTITIES
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        roots
            .get_or_insert_with(ShellRootIdentities::default)
            .record(workload_id, identity);
    }

    /// 기록된 셸 루트 신원. 없으면 그 셸은 관측·완화의 닻이 없는 것이다 —
    /// 맨 pid를 닻으로 대신 쓰면 안 된다(재사용, 01 §1).
    pub fn shell_identity(&self, workload_id: &WorkloadId) -> Option<ProcessIdentity> {
        let roots = SHELL_ROOT_IDENTITIES
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        roots.as_ref()?.get(workload_id)
    }

    /// 종료된 워크로드의 셸 루트 신원을 지운다.
    pub fn forget_shell_identity(&self, workload_id: &WorkloadId) {
        let mut roots = SHELL_ROOT_IDENTITIES
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(roots) = roots.as_mut() {
            roots.forget(workload_id);
        }
    }
}

#[cfg(test)]
mod shell_root_identity_tests {
    use super::*;

    fn identity(pid: u32, token: &str) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_token: token.to_string(),
            boot_id: "test-boot".to_string(),
        }
    }

    #[test]
    fn shell_roots_record_lookup_and_forget() {
        let mut roots = ShellRootIdentities::default();
        let id = WorkloadId::generate();
        assert!(roots.get(&id).is_none(), "기록 전에는 없다");
        roots.record(&id, identity(4242, "t1"));
        assert_eq!(roots.get(&id), Some(identity(4242, "t1")));
        roots.forget(&id);
        assert!(roots.get(&id).is_none(), "해제 뒤에는 없다");
    }

    /// 같은 워크로드에 다시 기록하면 덮어쓴다 — 스폰은 한 번뿐이지만
    /// 멱등 재시도 경로가 생겨도 오래된 신원이 남지 않게 한다.
    #[test]
    fn recording_again_replaces_the_earlier_identity() {
        let mut roots = ShellRootIdentities::default();
        let id = WorkloadId::generate();
        roots.record(&id, identity(7, "old"));
        roots.record(&id, identity(9, "new"));
        assert_eq!(roots.get(&id), Some(identity(9, "new")));
    }
}

impl DaemonState {
    /// `intervention.report` 처리: 검증 → 멱등 삽입 → 이벤트 브로드캐스트.
    /// 반환값은 새로 등록된 알림(중복이면 None).
    pub fn report_intervention(
        &self,
        report: InterventionReport,
    ) -> Result<Option<InterventionNotice>, term_contracts::error::RpcError> {
        use term_contracts::error::ErrorCode;
        if let Err(reason) = report.validate() {
            return Err(RpcError::new(
                ErrorCode::InvalidArgument,
                reason.to_string(),
            ));
        }
        let notice = InterventionNotice {
            report,
            reported_at: now_iso8601_utc(),
        };
        let fresh = self
            .interventions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(notice.clone());
        if !fresh {
            return Ok(None);
        }
        self.broadcast_control(
            RpcEventKind::InterventionReported,
            serde_json::to_value(&notice).unwrap_or(serde_json::Value::Null),
        );
        self.touch_activity();
        Ok(Some(notice))
    }
}

/// 현재 시각의 ISO-8601 UTC 문자열(밀리초). 알림 타임스탬프 전용 —
/// 측정·계량에는 단조 시계를 따로 쓴다(01 §1).
fn now_iso8601_utc() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format_iso8601_utc(millis)
}

fn format_iso8601_utc(millis: u64) -> String {
    let secs = millis / 1_000;
    let ms = millis % 1_000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3_600, (rem % 3_600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// 1970-01-01부터의 일수 → (연, 월, 일). 축소 civil-from-days(Hinnant).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod time_tests {
    use super::{format_iso8601_utc, now_iso8601_utc};

    /// 고정 epoch에 대한 회귀 값 — 윤년·자정 경계를 지나는 날도 포함.
    #[test]
    fn iso8601_matches_known_timestamps() {
        let now = now_iso8601_utc();
        assert_eq!(now.len(), 24, "{now}");
        assert!(now.ends_with('Z'));
        assert!(now.as_bytes().get(4) == Some(&b'-') && now.as_bytes().get(7) == Some(&b'-'));
        assert!(now.as_bytes().get(10) == Some(&b'T') && now.as_bytes().get(13) == Some(&b':'));
    }

    /// 알려진 epoch 값들 — 에포크·연말/연초·윤일(2024-02-29) 경계.
    #[test]
    fn formatter_matches_known_epoch_values() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        // 2023-12-31T23:59:59.999Z 직전 연도 경계.
        assert_eq!(
            format_iso8601_utc(1_704_067_199_999),
            "2023-12-31T23:59:59.999Z"
        );
        assert_eq!(
            format_iso8601_utc(1_704_067_200_000),
            "2024-01-01T00:00:00.000Z"
        );
        // 윤일.
        assert_eq!(
            format_iso8601_utc(1_709_164_800_000),
            "2024-02-29T00:00:00.000Z"
        );
        // 2026-09-08T00:00:00Z.
        assert_eq!(
            format_iso8601_utc(1_788_825_600_000),
            "2026-09-08T00:00:00.000Z"
        );
    }
}
