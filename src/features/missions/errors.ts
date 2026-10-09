/**
 * mission 오류 → 사람 문장 + 기본 행동(05-ui §8 오류 표시).
 *
 * - RPC 오류(RpcClientError / 직렬화된 `{code,message,details}`)는
 *   (reason_code, code) 순서로 원인 문장과 기본 행동을 고른다. 모르는
 *   reason_code는 무시하고 code 매핑으로 내려간다.
 * - 원문 `code: message`는 사용자 문장에 섞지 않고 `detail`(자세히)로만 둔다.
 * - 매핑이 없는 code는 일반 거절 문장, code가 없으면 "요청을 처리하지 못했습니다".
 *
 * 렌더러(MissionErrorNotice)는 JSX가 필요해 별도 .tsx에 두고 여기서 재수출한다.
 */

import type { MessageParams } from "../../i18n";
import { formatClock } from "./viewUtils";

export type MissionErrorAction = "resync" | "open_settings" | "retry" | "login" | "change_model" | "open_list";

export interface MissionUiError {
  code: string | null;
  reasonCode: string | null;
  message: string;
  detail: string | null;
  action: MissionErrorAction | null;
}

/** useI18n().t 와 전역 t 둘 다 받는다. */
export type MissionTranslate = (key: string, params?: MessageParams) => string;

/** 화면이 들고 있던 대상이 최신 store에서 더 이상 유효하지 않다(mutateWithResync). */
export class StaleActionError extends Error {
  constructor(message = "mission action is stale") {
    super(message);
    this.name = "StaleActionError";
  }
}

export { MissionErrorNotice } from "./MissionErrorNotice";

interface ErrorFacts {
  code: string | null;
  reasonCode: string | null;
  message: string;
  retryAfterMs: number | null;
}

interface Mapping {
  key: string;
  action: MissionErrorAction | null;
}

/**
 * reason_code(details.reason_code) → 원인 문장. code보다 구체적이라 먼저 본다.
 * 이름은 데몬 실제 slug(01-contracts §7 reason_code 표, mission/workflow.rs ·
 * service.rs · provider_blocks.rs · engine.rs)를 따른다.
 */
const REASONS: Record<string, Mapping> = {
  // 저장소 · 시작
  git_unavailable: { key: "missions.error.reason.gitUnavailable", action: "retry" },
  no_commits: { key: "missions.error.reason.noCommits", action: "retry" },
  not_a_repository: { key: "missions.error.reason.notRepository", action: null },
  dirty_worktree: { key: "missions.error.reason.dirtyWorktree", action: "retry" },
  // 우발적 base 스냅샷(include_uncommitted): 병합·리베이스 진행 중이거나 변경이 너무 많다.
  unmerged_paths: { key: "missions.error.reason.unmergedPaths", action: "retry" },
  path_not_accessible: { key: "missions.error.reason.pathNotAccessible", action: null },
  repository_changed: { key: "missions.error.reason.repositoryChanged", action: null },
  base_changed: { key: "missions.error.reason.baseChanged", action: null },
  snapshot_too_large: { key: "missions.error.reason.snapshotTooLarge", action: null },
  bindings_missing: { key: "missions.error.reason.bindingsMissing", action: "open_settings" },
  lead_missing: { key: "missions.error.reason.leadMissing", action: "open_settings" },
  lead_not_allowed: { key: "missions.error.reason.leadNotAllowed", action: "open_settings" },
  // 시작 전 팀 검사(service.rs): Builder는 항상, Reviewer는 리뷰 포함일 때만 필요하다.
  builder_missing: { key: "missions.error.reason.builderMissing", action: "open_settings" },
  reviewer_missing: { key: "missions.error.reason.reviewerMissing", action: "open_settings" },
  model_unavailable: { key: "missions.error.reason.modelUnavailable", action: "change_model" },
  capability_unsupported: { key: "missions.error.reason.capabilityUnsupported", action: "change_model" },
  illegal_transition: { key: "missions.error.reason.illegalTransition", action: "resync" },
  verification_unsupported_os: { key: "missions.error.reason.verificationUnsupportedOs", action: null },
  // 후속 작업(계약 E): 이전 작업이 확정 완료가 아니거나, 확정 결과 커밋이 기대와 다르다.
  follow_up_not_accepted: { key: "missions.error.reason.followUpNotAccepted", action: null },
  follow_up_base_mismatch: { key: "missions.error.reason.followUpBaseMismatch", action: null },
  // include_uncommitted은 follow_up_of와 함께 보낼 수 없다(UI에서는 닿지 않지만 코드는 있어야 한다).
  follow_up_snapshot: { key: "missions.error.reason.followUpSnapshot", action: null },
  // 실험적 연결(계약 A): 동의한 CLI 버전과 관측 버전이 달라 실험 허용이 적용되지 않았다.
  experimental_version_mismatch: { key: "missions.error.reason.experimentalVersionMismatch", action: "open_settings" },
  experimental_consent_withdrawn: { key: "missions.error.reason.experimentalConsentWithdrawn", action: "open_settings" },
  // 멈춘 실행 사용자 확인 정리(계약 C): 데몬이 대상 조건이 아니라고 판단했다.
  attestation_not_applicable: { key: "missions.error.reason.attestationNotApplicable", action: "resync" },
  // 작업 공간 정리(계약 D workspace.cleanup) 거절.
  mission_active: { key: "missions.error.reason.workspaceMissionActive", action: null },
  run_unreconciled: { key: "missions.error.reason.workspaceRunUnreconciled", action: "resync" },
  // 통합
  integration_conflict: { key: "missions.error.reason.integrationConflict", action: "resync" },
  integration_empty: { key: "missions.error.reason.integrationEmpty", action: null },
  // 인수(accept) 거절 사유
  revision_mismatch: { key: "missions.error.reason.revisionMismatch", action: "resync" },
  candidate_mismatch: { key: "missions.error.reason.candidateMismatch", action: "resync" },
  not_awaiting_acceptance: { key: "missions.error.reason.notAwaitingAcceptance", action: "resync" },
  required_task_not_succeeded: { key: "missions.error.reason.requiredTaskIncomplete", action: null },
  verification_missing: { key: "missions.error.reason.verificationMissing", action: null },
  verification_not_passed_on_candidate: { key: "missions.error.reason.verificationFailed", action: null },
  integrity_policy_unmet: { key: "missions.error.reason.integrityPolicyUnmet", action: null },
  observed_not_acknowledged: { key: "missions.error.reason.observedNotAcknowledged", action: null },
  review_incomplete: { key: "missions.error.reason.reviewIncomplete", action: null },
  open_finding: { key: "missions.error.reason.openFindings", action: null },
  human_check_missing: { key: "missions.error.reason.humanCheckUnacknowledged", action: null },
  live_run: { key: "missions.error.reason.runsActive", action: "retry" },
  unknown_run: { key: "missions.error.reason.reconciledRunUnacknowledged", action: null },
  open_blocking_decision: { key: "missions.error.reason.openDecisions", action: null },
  // 결정 답변(decision.answer)
  policy_update_required: { key: "missions.error.reason.policyUpdateRequired", action: null },
  option_required: { key: "missions.error.reason.optionRequired", action: null },
  option_invalid: { key: "missions.error.reason.optionRequired", action: "resync" },
  answer_not_text: { key: "missions.error.reason.answerNotText", action: null },
  answer_too_large: { key: "missions.error.reason.answerTooLarge", action: null },
  model_not_changed: { key: "missions.error.reason.modelNotChanged", action: "change_model" },
  lead_plan_in_progress: { key: "missions.error.reason.leadPlanInProgress", action: null },
  plan_limit: { key: "missions.error.reason.planLimit", action: null },
  mission_not_active: { key: "missions.error.reason.missionNotActive", action: "resync" },
  plan_format_rejected: { key: "missions.error.reason.planFormatRejected", action: null },
  // 엔진 내부 기록(실행 실패 기록으로 남는 사유)
  excluded_source: { key: "missions.error.reason.excludedSource", action: "resync" },
  verification_task_mismatch: { key: "missions.error.reason.internalMismatch", action: "resync" },
  verification_already_finished: { key: "missions.error.reason.internalMismatch", action: "resync" },
  verification_command_mismatch: { key: "missions.error.reason.internalMismatch", action: "resync" },
  verify_run_missing: { key: "missions.error.reason.internalMismatch", action: "resync" },
  verify_task_missing: { key: "missions.error.reason.internalMismatch", action: "resync" },
  findings_require_review_run: { key: "missions.error.reason.internalMismatch", action: "resync" },
  verify_fence_changed: { key: "missions.error.reason.verifyFenceChanged", action: null },
  deterministic_cancelled_before_launch: { key: "missions.error.reason.cancelledBeforeLaunch", action: null },
  // 초기 브리프의 추정 이름 — 데몬이 쓰지 않지만 호환 별칭으로 남긴다.
  required_task_incomplete: { key: "missions.error.reason.requiredTaskIncomplete", action: null },
  verification_failed: { key: "missions.error.reason.verificationFailed", action: null },
  open_findings: { key: "missions.error.reason.openFindings", action: null },
  open_decisions: { key: "missions.error.reason.openDecisions", action: null },
  human_check_unacknowledged: { key: "missions.error.reason.humanCheckUnacknowledged", action: null },
  reconciled_run_unacknowledged: { key: "missions.error.reason.reconciledRunUnacknowledged", action: null },
  runs_active: { key: "missions.error.reason.runsActive", action: "retry" },
};

/** MissionErrorCode 전체 + 연결 계열 R1 code. */
const CODES: Record<string, Mapping> = {
  INVALID_ARGUMENT: { key: "missions.error.code.invalidArgument", action: null },
  INVALID_STATE: { key: "missions.error.code.invalidState", action: "resync" },
  NOT_FOUND: { key: "missions.error.code.notFound", action: "open_list" },
  REQUEST_CONFLICT: { key: "missions.error.code.requestConflict", action: "resync" },
  REVISION_CONFLICT: { key: "missions.error.code.revisionConflict", action: "resync" },
  CAPABILITY_UNSUPPORTED: { key: "missions.error.code.capabilityUnsupported", action: "change_model" },
  MODEL_UNAVAILABLE: { key: "missions.error.code.modelUnavailable", action: "change_model" },
  AUTH_REQUIRED: { key: "missions.error.code.authRequired", action: "login" },
  PROVIDER_RATE_LIMITED: { key: "missions.error.code.providerRateLimited", action: "change_model" },
  PROVIDER_UNAVAILABLE: { key: "missions.error.code.providerUnavailable", action: "retry" },
  BUDGET_EXCEEDED: { key: "missions.error.code.budgetExceeded", action: null },
  UNKNOWN_COST: { key: "missions.error.code.unknownCost", action: null },
  POLICY_DENIED: { key: "missions.error.code.policyDenied", action: null },
  DIRTY_WORKTREE: { key: "missions.error.reason.dirtyWorktree", action: "retry" },
  WORKSPACE_BUSY: { key: "missions.error.code.workspaceBusy", action: "retry" },
  PLAN_CYCLE: { key: "missions.error.code.planCycle", action: null },
  PLAN_LIMIT: { key: "missions.error.code.planLimit", action: null },
  CONTEXT_TOO_LARGE: { key: "missions.error.code.contextTooLarge", action: null },
  STALE_DECISION: { key: "missions.error.code.staleDecision", action: "resync" },
  STALE_CANDIDATE: { key: "missions.error.code.staleCandidate", action: "resync" },
  RESULT_INVALID: { key: "missions.error.code.resultInvalid", action: null },
  OUTCOME_UNKNOWN: { key: "missions.error.code.outcomeUnknown", action: "resync" },
  CONTENT_EXPIRED: { key: "missions.error.code.contentExpired", action: null },
  SNAPSHOT_EXPIRED: { key: "missions.error.code.snapshotExpired", action: "resync" },
  CURSOR_EXPIRED: { key: "missions.error.code.snapshotExpired", action: "resync" },
  ARTIFACT_LIMIT: { key: "missions.error.code.artifactLimit", action: null },
  INTEGRITY_FAILED: { key: "missions.error.code.integrityFailed", action: null },
  STORAGE_UNAVAILABLE: { key: "missions.error.code.storageUnavailable", action: "retry" },
  INTERNAL: { key: "missions.error.code.internal", action: "retry" },
  // 연결 계열(R1 ErrorCode) — bridge 5초 timeout도 DAEMON_UNAVAILABLE로 온다.
  DAEMON_UNAVAILABLE: { key: "missions.error.code.connection", action: "retry" },
  BUSY: { key: "missions.error.code.busy", action: "retry" },
  PROTOCOL_MISMATCH: { key: "missions.error.code.protocolMismatch", action: null },
};

function readFacts(cause: unknown): ErrorFacts {
  if (typeof cause === "string") return { code: null, reasonCode: null, message: cause, retryAfterMs: null };
  if (typeof cause !== "object" || cause === null) {
    return { code: null, reasonCode: null, message: cause === undefined ? "" : String(cause), retryAfterMs: null };
  }
  const record = cause as { code?: unknown; message?: unknown; details?: unknown };
  const code = typeof record.code === "string" && record.code ? record.code : null;
  const message = typeof record.message === "string" ? record.message : "";
  const details = typeof record.details === "object" && record.details !== null
    ? (record.details as { reason_code?: unknown; retry_after_ms?: unknown })
    : null;
  const reasonCode = typeof details?.reason_code === "string" && details.reason_code ? details.reason_code : null;
  const retryAfterMs = typeof details?.retry_after_ms === "number" && Number.isFinite(details.retry_after_ms)
    ? details.retry_after_ms
    : null;
  return { code, reasonCode, message, retryAfterMs };
}

function reasonMapping(reasonCode: string | null): Mapping | null {
  if (reasonCode === null) return null;
  const direct = REASONS[reasonCode];
  if (direct) return direct;
  // capability_scoped_write 같은 기능별 사유는 한 문장으로 묶는다.
  if (reasonCode.startsWith("capability_")) return REASONS.capability_unsupported;
  return null;
}

function looksLikeTimeout(message: string): boolean {
  return /\b(time(d)?\s?out|timeout)\b/i.test(message);
}

/**
 * 오류 원인을 사람 문장과 기본 행동으로 바꾼다. 반환값의 message는 항상
 * 번역된 문장이고, 원문은 detail에만 담긴다.
 */
export function missionError(t: MissionTranslate, cause: unknown): MissionUiError {
  if (cause instanceof StaleActionError) {
    return { code: null, reasonCode: null, message: t("missions.error.stale"), detail: null, action: "resync" };
  }
  const facts = readFacts(cause);
  const detailParts: string[] = [];
  const raw = facts.code ? `${facts.code}: ${facts.message}` : facts.message;
  if (raw.trim()) detailParts.push(raw);
  if (facts.reasonCode) detailParts.push(`reason_code: ${facts.reasonCode}`);
  const detail = detailParts.length > 0 ? detailParts.join("\n") : null;

  const byReason = reasonMapping(facts.reasonCode);
  if (byReason) {
    return { code: facts.code, reasonCode: facts.reasonCode, message: t(byReason.key), detail, action: byReason.action };
  }

  if (facts.code === "PROVIDER_RATE_LIMITED") {
    const message = facts.retryAfterMs !== null
      ? t("missions.error.code.providerRateLimitedUntil", {
          time: formatClock(new Date(Date.now() + Math.max(0, facts.retryAfterMs)).toISOString()),
        })
      : t("missions.error.code.providerRateLimited");
    return { code: facts.code, reasonCode: facts.reasonCode, message, detail, action: "change_model" };
  }

  const byCode = facts.code ? CODES[facts.code] : undefined;
  if (byCode) {
    return { code: facts.code, reasonCode: facts.reasonCode, message: t(byCode.key), detail, action: byCode.action };
  }
  if (facts.code) {
    return { code: facts.code, reasonCode: facts.reasonCode, message: t("missions.error.code.unknown"), detail, action: null };
  }
  if (looksLikeTimeout(facts.message)) {
    return { code: null, reasonCode: null, message: t("missions.error.code.connection"), detail, action: "retry" };
  }
  return { code: null, reasonCode: null, message: t("missions.error.generic"), detail, action: null };
}

/**
 * 호스트 화면이 수행할 수 없는 행동은 버튼을 만들지 않도록 action을 비운다.
 * login은 안내 문구만 펼치므로 항상 유지한다.
 */
export function withSupportedActions(error: MissionUiError, supported: readonly MissionErrorAction[]): MissionUiError {
  if (error.action === null || error.action === "login" || supported.includes(error.action)) return error;
  return { ...error, action: null };
}

/** 오류가 REVISION_CONFLICT인가(mutateWithResync 재시도 판정). */
export function isRevisionConflict(cause: unknown): boolean {
  return readFacts(cause).code === "REVISION_CONFLICT";
}
