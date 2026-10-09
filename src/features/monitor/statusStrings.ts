/**
 * 사용자가 보는 상태 문구 (04-ui.md §7).
 * 모든 문구는 enum과 실제 측정값에서 생성한다 — 리터럴 하드코딩 금지.
 * 구현 모듈명·cgroup 파일명·IPC stack은 기본 화면이 아닌 상세 진단에 둔다.
 * 문구는 i18n 사전(sections/monitor.ts)에서 언어별로 꺼낸다 — 반드시
 * 함수 본문(호출 시점)에서 t()를 평가해 언어 전환이 반영되게 한다.
 */

import { t } from "../../i18n";
import type { QueueReason } from "../../generated/QueueReason";
import type { TerminalConnection } from "../../generated/TerminalConnection";
import type { WorkloadState } from "../../generated/WorkloadState";
import { formatGiB } from "./format";

export interface QueueWaitContext {
  /** 현재 RUNNING/STARTING 관리 작업 수. */
  runningManaged: number;
  /** 대기 작업의 요청 reservation bytes. */
  needBytes: number | null;
  /** 안전 여유를 제외한 사용 가능 bytes (host available - reserve). */
  safeAvailableBytes: number | null;
}

/**
 * QueueReason → 대기 문구. ADMIT은 대기가 아니므로 null.
 * WAIT_CONCURRENCY/WAIT_MEMORY_HEADROOM의 형식은 04-ui.md §7의 예시와
 * 정확히 일치해야 한다.
 */
export function queueWaitText(reason: QueueReason | null | undefined, ctx: QueueWaitContext): string | null {
  if (!reason || reason === "ADMIT") return null;
  switch (reason) {
    case "WAIT_CONCURRENCY":
      return t("monitor.queue.concurrency", { count: ctx.runningManaged });
    case "WAIT_MEMORY_HEADROOM": {
      const need = ctx.needBytes !== null ? formatGiB(ctx.needBytes) : "—";
      const safe = ctx.safeAvailableBytes !== null ? formatGiB(ctx.safeAvailableBytes) : "—";
      return t("monitor.queue.memoryHeadroom", { need, safe });
    }
    case "WAIT_TELEMETRY":
      return t("monitor.queue.telemetry");
    case "WAIT_HOST_PRESSURE":
      return t("monitor.queue.hostPressure");
    case "WAIT_CPU_SLOTS":
      return t("monitor.queue.cpuSlots");
    case "WAIT_RESERVATION_BUDGET":
      return t("monitor.queue.reservationBudget");
    case "RESOURCE_UNSCHEDULABLE":
      return t("monitor.queue.unschedulable");
    default:
      return t("monitor.queue.unknown");
  }
}

const PLATFORM_LABELS: Record<string, string> = {
  darwin: "macOS",
  macos: "macOS",
  windows: "Windows",
  win32: "Windows",
  linux: "Linux",
};

/**
 * 관측만 가능 안내(04-ui.md §7). platform enum에서 생성; 강제 상한을
 * 적용할 수 없는 플랫폼에서만 문구가 나온다.
 */
export function observeOnlyText(platform: string, enforcement: "observe" | "prefer" | "require"): string | null {
  if (enforcement === "require") return null;
  const label = PLATFORM_LABELS[platform.toLowerCase()] ?? null;
  if (label !== "macOS") return null;
  return t("monitor.observeOnly", { platform: label });
}

/** 화면 연결 해제 + 프로세스 실행 중 (04-ui.md §7). */
export function detachedRunningText(connection: TerminalConnection, state: WorkloadState): string | null {
  const live = state === "RUNNING" || state === "STARTING" || state === "STOPPING";
  if (connection === "detached" && live) return t("monitor.detachedRunning");
  return null;
}

/** 로그 상한으로 출력 읽기 중지 (04-ui.md §7). */
export function journalLimitPausedText(limitBytes: number): string {
  return t("monitor.journalPaused", { limit: formatGiB(limitBytes) });
}

/** daemon 재시작 후 수동 확인 요구 (04-ui.md §7). */
export function interruptedText(state: WorkloadState): string | null {
  if (state !== "INTERRUPTED") return null;
  return t("monitor.interrupted");
}

const WORKLOAD_STATE_KEY: Record<WorkloadState, string> = {
  QUEUED: "monitor.state.queued",
  STARTING: "monitor.state.starting",
  RUNNING: "monitor.state.running",
  STOPPING: "monitor.state.stopping",
  DRAINING: "monitor.state.draining",
  SUCCEEDED: "monitor.state.succeeded",
  FAILED: "monitor.state.failed",
  CANCELLED: "monitor.state.cancelled",
  INTERRUPTED: "monitor.state.interrupted",
};

export function workloadStateText(state: WorkloadState): string {
  return t(WORKLOAD_STATE_KEY[state]);
}

/** Priority: 0이 가장 높음. */
export function priorityText(priority: number): string {
  if (priority <= 0) return t("monitor.priority.high");
  if (priority === 1) return t("monitor.priority.normal");
  return t("monitor.priority.low");
}

export type PanePhase = "starting" | "replaying" | "live" | "failed" | "exited" | "detached";

const PANE_PHASE_KEY: Record<PanePhase, string> = {
  starting: "monitor.phase.starting",
  replaying: "monitor.phase.replaying",
  live: "monitor.phase.live",
  failed: "monitor.phase.failed",
  exited: "monitor.phase.exited",
  detached: "monitor.phase.detached",
};

export function panePhaseText(phase: PanePhase): string {
  return t(PANE_PHASE_KEY[phase]);
}

export interface ExitInfoLike {
  code: number | null;
  reason: string;
  detail: string | null;
}

/**
 * 사용자가 알아야 할 종료인가 — 정상 종료(코드 0)와 사용자 취소는 아니고,
 * 그 밖의 종료 코드·코드 없음(시그널)·OOM·저널 한도·미확인은 그렇다.
 * 이어서 열기 오버레이는 이 경우에만 사유 줄을 덧붙인다.
 */
export function isAbnormalExit(exit: ExitInfoLike): boolean {
  switch (exit.reason) {
    case "process_exit":
      return exit.code !== 0;
    case "cancelled":
      return false;
    default:
      return true;
  }
}

/**
 * 아무 일도 없이 끝난 종료인가 — 프로그램이 스스로 코드 0으로 끝난 경우만이다
 * (04-ui §5-1: 이 종료는 오버레이 없이 창을 닫는다). 취소·시그널·OOM·저널
 * 한도·미확인(구 데몬)은 보여 줄 근거가 있으므로 여기 들지 않는다.
 */
export function isCleanExit(exit: ExitInfoLike): boolean {
  return exit.reason === "process_exit" && exit.code === 0;
}

/**
 * 종료 사유 한 줄(SOTA_GAP_REVIEW W1-1). 문구는 ExitReason enum에서만
 * 만들고, 기술 근거(detail)는 원문 그대로 붙인다 — 현지화하지 않는다.
 */
export function exitReasonText(exit: ExitInfoLike): string {
  let base: string;
  switch (exit.reason) {
    case "process_exit":
      base = exit.code !== null
        ? t("monitor.exit.processExit", { code: exit.code })
        : t("monitor.exit.processExitNoCode");
      break;
    case "cancelled":
      base = t("monitor.exit.cancelled");
      break;
    case "journal_limit":
      base = t("monitor.exit.journalLimit");
      break;
    case "oom_kill":
      base = t("monitor.exit.oomKill");
      break;
    default:
      base = t("monitor.exit.unknown");
  }
  return exit.detail ? `${base} — ${t("monitor.exit.detail", { detail: exit.detail })}` : base;
}

/** 1 MiB 초과 붙여넣기 거절 + 파일 전달 안내 (02 §6, 04 §6). */
export function pasteTooLargeText(maxBytes: number): string {
  return t("monitor.paste.tooLarge", { max: formatGiB(maxBytes) });
}

export function splitNoSpaceText(): string {
  return t("monitor.split.noSpace");
}

export function paneLimitText(maxPanes: number): string {
  return t("monitor.pane.limit", { count: maxPanes });
}
