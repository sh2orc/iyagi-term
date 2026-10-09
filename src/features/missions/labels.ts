/**
 * mission 상태 → 사람 문구/아이콘 매핑(05-ui §5 상태표).
 *
 * 색 이외의 수단(아이콘 글리프 + 텍스트)으로 상태를 항상 표시한다.
 * 문구는 i18n 키를 반환하고 호출부가 t()로 바꾼다 — 이 파일 자체는
 * 순수 함수로 둬서 node 시험이 렌더 없이 계약을 검사할 수 있게 한다.
 */

import type { MessageDelivery } from "../../generated/MessageDelivery";
import type { MissionState } from "../../generated/MissionState";
import type { Phase } from "../../generated/Phase";
import type { RunState } from "../../generated/RunState";
import type { TaskState } from "../../generated/TaskState";
import type { VerificationStatus } from "../../generated/VerificationStatus";
import type { InputIntegrity } from "../../generated/InputIntegrity";
import type { DecisionOption } from "../../generated/DecisionOption";
import type { FindingSeverity } from "../../generated/FindingSeverity";
import type { Role } from "../../generated/Role";
import type { MessageParams } from "../../i18n";
import type { TeamFilter } from "./uiStore";

export interface TaskStatusDisplay {
  /** i18n 키 — missions.taskStatus.* */
  labelKey: string;
  /** 색 외 수단용 글리프(아이콘 역할). */
  glyph: string;
}

/** Task 상태 → 표시(05 §5 표). running은 Run 상태와 조합해 별도 키를 쓴다. */
export function taskStatusDisplay(state: TaskState, liveRunState: RunState | null): TaskStatusDisplay {
  if (state === "running" && liveRunState !== null) {
    if (liveRunState === "prepared" || liveRunState === "starting") {
      return { labelKey: "missions.taskStatus.starting", glyph: "◔" };
    }
    return { labelKey: "missions.taskStatus.running", glyph: "▶" };
  }
  switch (state) {
    case "planned":
      return { labelKey: "missions.taskStatus.planned", glyph: "◷" };
    case "ready":
      return { labelKey: "missions.taskStatus.ready", glyph: "…" };
    case "running":
      return { labelKey: "missions.taskStatus.running", glyph: "▶" };
    case "awaiting_input":
      return { labelKey: "missions.taskStatus.awaitingInput", glyph: "?" };
    case "awaiting_review":
      return { labelKey: "missions.taskStatus.awaitingReview", glyph: "◸" };
    case "blocked":
      return { labelKey: "missions.taskStatus.blocked", glyph: "⊘" };
    case "succeeded":
      return { labelKey: "missions.taskStatus.succeeded", glyph: "✓" };
    case "failed":
      return { labelKey: "missions.taskStatus.failed", glyph: "✗" };
    case "cancelled":
      return { labelKey: "missions.taskStatus.cancelled", glyph: "—" };
    case "superseded":
      return { labelKey: "missions.taskStatus.superseded", glyph: "↘" };
  }
}

/** 필터 매칭(05 §5: 전체/실행 중/응답 필요/대기/완료). */
export function taskMatchesFilter(state: TaskState, filter: TeamFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "running":
      return state === "running";
    case "awaiting":
      return state === "awaiting_input";
    case "waiting":
      return state === "planned" || state === "ready" || state === "blocked" || state === "awaiting_review";
    case "done":
      return state === "succeeded" || state === "failed" || state === "cancelled" || state === "superseded";
  }
}

/** Mission 상태 → 표시 문구 키. */
export function missionStateLabelKey(state: MissionState): string {
  switch (state) {
    case "draft":
      return "missions.missionState.draft";
    case "running":
      return "missions.missionState.running";
    case "pausing":
      return "missions.missionState.pausing";
    case "paused":
      return "missions.missionState.paused";
    case "stopping":
      return "missions.missionState.stopping";
    case "completed":
      return "missions.missionState.completed";
    case "failed":
      return "missions.missionState.failed";
    case "cancelled":
      return "missions.missionState.cancelled";
  }
}

/** Mission phase → 표시 문구 키(05 §1 헤더). */
export function phaseLabelKey(phase: Phase): string {
  switch (phase) {
    case "planning":
      return "missions.phase.planning";
    case "implementing":
      return "missions.phase.implementing";
    case "integrating":
      return "missions.phase.integrating";
    case "validating":
      return "missions.phase.validating";
    case "reviewing":
      return "missions.phase.reviewing";
    case "awaiting_acceptance":
      return "missions.phase.awaitingAcceptance";
    case "done":
      return "missions.phase.done";
  }
}

/** 메시지 전달 상태 → 정직한 문구 키(05 §4: queued ≠ delivered). */
export function deliveryLabelKey(delivery: MessageDelivery): string {
  switch (delivery) {
    case "queued":
      return "missions.delivery.queued";
    case "delivered":
      return "missions.delivery.delivered";
    case "rejected":
      return "missions.delivery.rejected";
    case "unknown":
      return "missions.delivery.unknown";
  }
}

/** 검증 상태 문구 키. */
export function verificationStatusLabelKey(status: VerificationStatus): string {
  switch (status) {
    case "passed":
      return "missions.verification.passed";
    case "failed":
      return "missions.verification.failed";
    case "cancelled":
      return "missions.verification.cancelled";
    case "unknown":
      return "missions.verification.unknown";
  }
}

/**
 * 입력 무결성 문구 키(05 §9: observed는 보장이 아니라 관찰).
 * 무결성 수준만 말한다 — 검증 결과(통과/실패)는 verificationStatusLabelKey로 따로 붙인다.
 * 실패한 검증 옆에도 놓일 수 있으므로 "통과 · " 같은 결과 접두어를 넣지 않는다.
 */
export function inputIntegrityLabelKey(integrity: InputIntegrity): string {
  switch (integrity) {
    case "enforced":
      return "missions.integrity.enforced";
    case "observed":
      return "missions.integrity.observed";
    case "unknown":
      return "missions.integrity.unknown";
  }
}

// ---------------------------------------------------------------- 사람 문구(t 주입)
//
// 아래 함수는 문구 자체를 돌려준다(t 주입) — 원문 enum/slug를 화면에 노출하지
// 않기 위한 공용 매핑이다. 모르는 값은 원문 대신 안전한 일반 문구로 떨어진다.

/** labels 함수가 받는 번역 함수(useI18n().t 또는 전역 t). */
export type LabelTranslate = (key: string, params?: MessageParams) => string;

const ROLE_KEYS: Record<Role, string> = {
  lead: "missions.role.lead",
  researcher: "missions.role.researcher",
  architect: "missions.role.architect",
  builder: "missions.role.builder",
  test_author: "missions.role.testAuthor",
  reviewer: "missions.role.reviewer",
  specialist: "missions.role.specialist",
  diagnostician: "missions.role.diagnostician",
  integrator: "missions.role.integrator",
  documenter: "missions.role.documenter",
};

/** 역할 → 사람 문구. null(자동 단계)은 "자동". */
export function roleLabel(t: LabelTranslate, role: Role | string | null | undefined): string {
  if (role === null || role === undefined) return t("missions.role.automatic");
  const key = (ROLE_KEYS as Record<string, string>)[role];
  return key ? t(key) : role;
}

const RUN_STATE_KEYS: Record<RunState, string> = {
  prepared: "missions.runState.prepared",
  starting: "missions.runState.starting",
  running: "missions.runState.running",
  awaiting_input: "missions.runState.awaitingInput",
  stopping: "missions.runState.stopping",
  succeeded: "missions.runState.succeeded",
  failed: "missions.runState.failed",
  cancelled: "missions.runState.cancelled",
  interrupted: "missions.runState.interrupted",
  unknown: "missions.runState.unknown",
};

/** 실행 상태 → 사람 문구. */
export function runStateLabel(t: LabelTranslate, state: RunState | string): string {
  const key = (RUN_STATE_KEYS as Record<string, string>)[state];
  return key ? t(key) : t("missions.runState.unknown");
}

const SEVERITY_KEYS: Record<FindingSeverity, string> = {
  blocking: "missions.severity.blocking",
  major: "missions.severity.major",
  minor: "missions.severity.minor",
  note: "missions.severity.note",
};

/** 리뷰 지적 심각도 → 차단/주요/보통/참고. */
export function severityLabel(t: LabelTranslate, severity: FindingSeverity | string): string {
  const key = (SEVERITY_KEYS as Record<string, string>)[severity];
  return key ? t(key) : t("missions.severity.note");
}

const BLOCKED_KEYS: Record<string, string> = {
  dependency_failed: "missions.blocked.dependencyFailed",
  outcome_unknown: "missions.blocked.outcomeUnknown",
  outcome_unknown_ended: "missions.blocked.outcomeUnknownEnded",
  recovery_held: "missions.blocked.recoveryHeld",
  integration_conflict: "missions.blocked.integrationConflict",
  active_time_limit: "missions.blocked.activeTimeLimit",
  automatic_start_limit: "missions.blocked.automaticStartLimit",
  attempt_limit: "missions.blocked.attemptLimit",
  cost_limit: "missions.blocked.costLimit",
  cost_unknown: "missions.blocked.costUnknown",
  provider_rate_limited: "missions.blocked.providerRateLimited",
  transient_retry: "missions.blocked.transientRetry",
  plan_format_repair: "missions.blocked.planFormatRepair",
  binding_missing: "missions.blocked.bindingMissing",
  binding_unavailable: "missions.blocked.bindingUnavailable",
  // 실행 실패 code가 그대로 blocked_code로 남는 경우(SCREAMING_SNAKE로 정규화).
  AUTH_REQUIRED: "missions.blocked.authRequired",
  MODEL_UNAVAILABLE: "missions.blocked.modelUnavailable",
  CAPABILITY_UNSUPPORTED: "missions.blocked.capability",
  PROVIDER_RATE_LIMITED: "missions.blocked.providerRateLimited",
  PROVIDER_UNAVAILABLE: "missions.blocked.providerUnavailable",
  OUTCOME_UNKNOWN: "missions.blocked.outcomeUnknown",
};

/** 데몬이 Rust Debug 형식(`AuthRequired`, `Some(AuthRequired)`)으로 남긴 code를 wire 형식으로. */
function normalizeDebugCode(code: string): string {
  const inner = /^Some\((.+)\)$/.exec(code)?.[1] ?? code;
  if (!/^[A-Z][A-Za-z]+$/.test(inner)) return inner;
  return inner.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toUpperCase();
}

/** Task.blocked_code → 짧은 이유. 모르면 "진행 불가". */
export function blockedReasonLabel(t: LabelTranslate, code: string | null | undefined): string {
  if (!code) return t("missions.blocked.unknown");
  const direct = BLOCKED_KEYS[code];
  if (direct) return t(direct);
  if (code.startsWith("capability_")) return t("missions.blocked.capability");
  if (code.startsWith("cost_")) return t("missions.blocked.cost");
  if (code.startsWith("provider_blocked:")) return t("missions.blocked.providerBlocked");
  const normalized = BLOCKED_KEYS[normalizeDebugCode(code)];
  return normalized ? t(normalized) : t("missions.blocked.unknown");
}

/** 결정 선택지 id → i18n 키(데몬 slug 전체). */
const DECISION_OPTION_KEYS: Record<string, string> = {
  apply: "missions.decision.option.apply",
  revise: "missions.decision.option.revise",
  accept: "missions.decision.option.accept",
  decline: "missions.decision.option.decline",
  resume_unsent: "missions.decision.option.resumeUnsent",
  stop_mission: "missions.decision.option.stopMission",
  retry_with_instruction: "missions.decision.option.retryWithInstruction",
  change_model: "missions.decision.option.changeModel",
  replan: "missions.decision.option.replan",
  adjust_limits: "missions.decision.option.adjustLimits",
  retry_failed_task: "missions.decision.retryFailedTask",
  stop_failed_mission: "missions.decision.stopFailedMission",
  retry_reconciled_task: "missions.recovery.retry",
  stop_reconciled_mission: "missions.recovery.stop",
  resolve_and_reintegrate: "missions.integration.resolve",
  exclude_candidate: "missions.integration.exclude",
  stop_review_repair: "missions.requiredRepair.stopReview",
  stop_cost_mission: "missions.cost.stop",
};

/** 선택지 id를 알면 사람 문구, 모르면 데몬/모델이 준 label. */
export function decisionOptionLabel(t: LabelTranslate, option: DecisionOption): string {
  const key = DECISION_OPTION_KEYS[option.id];
  return key ? t(key) : option.label;
}

/** 선택지를 누르면 생기는 일(1줄). 모르는 선택지는 일반 문구. */
export function decisionOptionEffect(t: LabelTranslate, option: DecisionOption): string {
  return DECISION_OPTION_KEYS[option.id]
    ? t(`missions.decision.effect.${option.id}`)
    : t("missions.decision.effect.default");
}

/** 검증 결과와 분리한 입력 무결성 문구 키(결과 "통과"와 섞지 않는다). */
export function inputIntegrityNoteKey(integrity: InputIntegrity): string {
  switch (integrity) {
    case "enforced":
      return "missions.integrity.note.enforced";
    case "observed":
      return "missions.integrity.note.observed";
    case "unknown":
      return "missions.integrity.note.unknown";
  }
}
