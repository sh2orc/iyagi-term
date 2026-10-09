import { create } from "zustand";
import { useWorkbenchStore } from "../../store/workbenchStore";

/**
 * pane 입력이 데몬에서 거절된 원인(좀비 pane 진단).
 *
 * - `stalled`: 터미널 안 프로그램이 입력을 읽지 않는다(tty 입력 큐가 가득 참 —
 *   멈추거나 바쁜 TUI). 데몬이 `BUSY` + `reason_code: INPUT_STALLED`로 거절한다.
 * - `suspended`: 자원 가드가 일시정지했다(SIGSTOP). `INVALID_STATE` +
 *   `reason_code: GUARD_SUSPENDED`. 포커스가 오면 데몬이 자동 정지를 푼다.
 *
 * 둘 다 예전에는 pane이 live로 보이면서 키가 조용히 버려졌다.
 */
export type InputIssue = "stalled" | "suspended";

/** 입력 거절의 분류: 다시 붙어야 하는 경우(detached)와 pane에 알릴 원인. */
export type InputRejection = "detached" | InputIssue | "other";

/**
 * 데몬이 `session.input` 거절에 싣는 `details.reason_code` 목록(dispatch.rs의
 * `REASON_*` 상수와 한 쌍). Rust 쪽 이름이 바뀌면 이 표가 컴파일 타임에 잡지
 * 못하므로 — 양쪽을 함께 고친다(01-contracts 오류 표가 계약의 정본이다).
 */
export const INPUT_REJECTION_REASONS = {
  /** 프로그램이 tty 입력을 읽지 않는다(막힘 — 배지). */
  stalled: "INPUT_STALLED",
  /** 자원 가드가 일시정지했다(배지 + 재개 버튼). */
  guardSuspended: "GUARD_SUSPENDED",
  /** 연결 단위 완료 예산 초과(dispatch 전 거절 — 클라이언트 재시도). */
  pendingBudget: "PENDING_BUDGET",
  /** 액터 큐 포화(프로그램은 건강 — 배지 없음, 경고만). */
  queueFull: "INPUT_QUEUE_FULL",
} as const;

const DETACHED_CODES = new Set(["STALE_EPOCH", "NOT_INPUT_OWNER", "DAEMON_UNAVAILABLE"]);

export function classifyInputRejection(error: Error): InputRejection {
  const { code, details } = error as Error & { code?: unknown; details?: { reason_code?: unknown } };
  if ((typeof code === "string" && DETACHED_CODES.has(code)) || error.message === "not attached") {
    return "detached";
  }
  const reason = details?.reason_code;
  if (reason === INPUT_REJECTION_REASONS.stalled) return "stalled";
  if (reason === INPUT_REJECTION_REASONS.guardSuspended) return "suspended";
  if (reason === INPUT_REJECTION_REASONS.queueFull) return "other";
  // reason_code를 싣지 않는 구 데몬: 메시지로 같은 두 경우를 알아본다.
  if (code === "BUSY" && error.message === "input queue is full") return "stalled";
  if (code === "INVALID_STATE" && error.message.includes("suspended by the resource guard")) return "suspended";
  return "other";
}

export const useInputHealth = create<{ issues: ReadonlyMap<string, InputIssue> }>(() => ({ issues: new Map() }));

/**
 * `stalled` 표시는 더 이상 거절이 오지 않으면 이만큼 뒤 스스로 걷힌다 — 사용자가
 * 타이핑을 멈추면 회복을 알릴 성공 응답도 오지 않으므로.
 */
export const INPUT_STALL_BADGE_MS = 15_000;
const stallTimers = new Map<string, ReturnType<typeof setTimeout>>();

/** 세션의 입력 문제를 기록하거나(issue) 걷는다(null). 바뀔 때만 상태를 갈아 끼운다. */
export function setInputIssue(sessionId: string, issue: InputIssue | null): void {
  clearTimeout(stallTimers.get(sessionId));
  stallTimers.delete(sessionId);
  if (issue === "stalled") {
    stallTimers.set(
      sessionId,
      setTimeout(() => {
        stallTimers.delete(sessionId);
        if (inputIssueOf(sessionId) === "stalled") setInputIssue(sessionId, null);
      }, INPUT_STALL_BADGE_MS),
    );
  }
  const current = useInputHealth.getState().issues;
  if ((current.get(sessionId) ?? null) === issue) return;
  const next = new Map(current);
  if (issue === null) next.delete(sessionId);
  else next.set(sessionId, issue);
  useInputHealth.setState({ issues: next });
}

export function inputIssueOf(sessionId: string | null | undefined): InputIssue | null {
  if (typeof sessionId !== "string") return null;
  return useInputHealth.getState().issues.get(sessionId) ?? null;
}

/**
 * 가드가 포커스로 자동 재개하면 'suspended' 입력 문제를 걷는다 — 미러
 * (workbenchStore.workloads)가 일시정지→실행 **전이**를 확인하는 순간.
 * 스토어 구독으로 처리하는 이유: pane 컴포넌트의 생애와 무관해야 탭 전환
 * (unmount/remount) 사이에 벌어진 재개의 전이를 놓치지 않는다. 스냅샷 지연
 * (≈1 s) 동안의 잘못된 걷기를 막으려 전이로만 판정한다 — 거절 직후의 오래된
 * Running 스냅샷이 방금 설정된 표시를 지우지 않는다. 미러에서 사라진 세션
 * (종료·회수)은 전이가 아니므로 건드리지 않는다(세션 종료 경로가 정리한다).
 */
function attachGuardMirrorWatch(): void {
  let previous: ReadonlyMap<string, boolean> | null = null;
  useWorkbenchStore.subscribe((state, prev) => {
    if (state.workloads === prev.workloads) return;
    const next = new Map<string, boolean>();
    for (const workload of state.workloads) {
      if (workload.session_id == null) continue;
      next.set(workload.session_id, workload.guard?.kind === "SUSPENDED");
    }
    if (previous !== null) {
      for (const [sessionId, suspended] of next) {
        if (previous.get(sessionId) === true && !suspended && inputIssueOf(sessionId) === "suspended") {
          setInputIssue(sessionId, null);
        }
      }
    }
    previous = next;
  });
}
attachGuardMirrorWatch();
