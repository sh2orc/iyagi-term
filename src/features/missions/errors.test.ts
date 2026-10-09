/**
 * missionError 계약: (reason_code → code) 매핑, 기본 행동, 폴백 문장,
 * 원문 `code: message`는 detail로만.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { MissionErrorCode } from "../../generated/MissionErrorCode";
import { RpcClientError } from "../daemon/client";
import { t, useI18nStore } from "../../i18n";
import { isRevisionConflict, missionError, StaleActionError, withSupportedActions } from "./errors";
import { formatClock } from "./viewUtils";

beforeEach(() => {
  useI18nStore.setState({ language: "ko" });
});

afterEach(() => {
  vi.useRealTimers();
});

const ALL_MISSION_CODES: MissionErrorCode[] = [
  "INVALID_ARGUMENT", "INVALID_STATE", "NOT_FOUND", "REQUEST_CONFLICT", "REVISION_CONFLICT",
  "CAPABILITY_UNSUPPORTED", "MODEL_UNAVAILABLE", "AUTH_REQUIRED", "PROVIDER_RATE_LIMITED",
  "PROVIDER_UNAVAILABLE", "BUDGET_EXCEEDED", "UNKNOWN_COST", "POLICY_DENIED", "DIRTY_WORKTREE",
  "WORKSPACE_BUSY", "PLAN_CYCLE", "PLAN_LIMIT", "CONTEXT_TOO_LARGE", "STALE_DECISION",
  "STALE_CANDIDATE", "RESULT_INVALID", "OUTCOME_UNKNOWN", "CONTENT_EXPIRED", "SNAPSHOT_EXPIRED",
  "CURSOR_EXPIRED", "ARTIFACT_LIMIT", "INTEGRITY_FAILED", "STORAGE_UNAVAILABLE", "INTERNAL",
];

/** 데몬이 실제로 보내는 slug(01-contracts §7 표 + workflow/service/provider_blocks/engine). */
const REASONS = [
  "git_unavailable", "no_commits", "not_a_repository", "dirty_worktree", "unmerged_paths", "path_not_accessible", "repository_changed",
  "base_changed", "snapshot_too_large", "bindings_missing", "lead_missing", "lead_not_allowed", "builder_missing", "reviewer_missing",
  "model_unavailable", "capability_unsupported",
  "illegal_transition", "verification_unsupported_os", "integration_conflict", "integration_empty",
  "revision_mismatch", "candidate_mismatch", "not_awaiting_acceptance", "required_task_not_succeeded",
  "verification_missing", "verification_not_passed_on_candidate", "integrity_policy_unmet", "observed_not_acknowledged",
  "review_incomplete", "open_finding", "human_check_missing", "live_run", "unknown_run", "open_blocking_decision",
  "policy_update_required", "option_required", "option_invalid", "answer_not_text", "answer_too_large",
  "model_not_changed", "lead_plan_in_progress", "plan_limit", "mission_not_active", "plan_format_rejected",
  "excluded_source", "verification_task_mismatch", "verification_already_finished", "verification_command_mismatch",
  "verify_run_missing", "verify_fence_changed", "verify_task_missing", "findings_require_review_run",
  "deterministic_cancelled_before_launch", "experimental_version_mismatch", "experimental_consent_withdrawn", "attestation_not_applicable",
  "follow_up_not_accepted", "follow_up_base_mismatch", "follow_up_snapshot", "mission_active", "run_unreconciled",
];

/** 초기 추정 이름 → 실제 slug(같은 문장이어야 한다). */
const ALIASES: Array<[string, string]> = [
  ["required_task_incomplete", "required_task_not_succeeded"],
  ["verification_failed", "verification_not_passed_on_candidate"],
  ["open_findings", "open_finding"],
  ["open_decisions", "open_blocking_decision"],
  ["human_check_unacknowledged", "human_check_missing"],
  ["reconciled_run_unacknowledged", "unknown_run"],
  ["runs_active", "live_run"],
];

describe("missionError 매핑", () => {
  it("모든 MissionErrorCode는 일반 거절 문장이 아닌 전용 원인 문장을 갖는다", () => {
    const fallback = t("missions.error.code.unknown");
    for (const code of ALL_MISSION_CODES) {
      const error = missionError(t, new RpcClientError(code, `raw ${code}`));
      expect(error.code).toBe(code);
      expect(error.message).not.toBe(fallback);
      expect(error.message).not.toContain("missions.error");
      expect(error.message).not.toContain(`raw ${code}`);
      expect(error.detail).toBe(`${code}: raw ${code}`);
    }
  });

  it("모든 reason code는 전용 문장으로 바뀌고 code 문장보다 우선한다", () => {
    const codeMessage = missionError(t, new RpcClientError("INVALID_STATE", "x")).message;
    const seen = new Set<string>();
    for (const reason of REASONS) {
      const error = missionError(t, new RpcClientError("INVALID_STATE", "x", false, { reason_code: reason }));
      expect(error.reasonCode).toBe(reason);
      expect(error.message).not.toContain("missions.error");
      expect(error.message).not.toBe(codeMessage);
      expect(error.detail).toContain(`reason_code: ${reason}`);
      seen.add(error.message);
    }
    expect(seen.size).toBeGreaterThan(30);
  });

  it("추정 이름 별칭은 실제 slug와 같은 문장·행동으로 읽는다", () => {
    for (const [alias, actual] of ALIASES) {
      const fromAlias = missionError(t, new RpcClientError("INVALID_STATE", "x", false, { reason_code: alias }));
      const fromActual = missionError(t, new RpcClientError("INVALID_STATE", "x", false, { reason_code: actual }));
      expect(fromAlias.message).toBe(fromActual.message);
      expect(fromAlias.action).toBe(fromActual.action);
    }
  });

  it("결정 답변 거절 사유는 해결 동선으로 이어진다", () => {
    const reason = (code: string) => missionError(t, new RpcClientError("INVALID_ARGUMENT", "x", false, { reason_code: code }));
    expect(reason("model_not_changed").action).toBe("change_model");
    expect(reason("policy_update_required").message).toBe(t("missions.error.reason.policyUpdateRequired"));
    expect(reason("answer_too_large").message).toContain("64 KiB");
    expect(reason("bindings_missing").action).toBe("open_settings");
    expect(reason("illegal_transition").action).toBe("resync");
  });

  it("실험적 버전 불일치는 설정 열기, 사용자 확인 정리 거절은 다시 동기화로 이어진다", () => {
    const mismatch = missionError(t, new RpcClientError("CAPABILITY_UNSUPPORTED", "x", false, { reason_code: "experimental_version_mismatch" }));
    expect(mismatch).toMatchObject({ message: t("missions.error.reason.experimentalVersionMismatch"), action: "open_settings" });
    const attest = missionError(t, new RpcClientError("INVALID_STATE", "x", false, { reason_code: "attestation_not_applicable" }));
    expect(attest).toMatchObject({ message: t("missions.error.reason.attestationNotApplicable"), action: "resync" });
  });

  it("후속 작업 거절 사유는 각자 전용 문장이다(현재 HEAD에서 새로 만들기 / 다시 열기)", () => {
    const reason = (code: string) => missionError(t, new RpcClientError("INVALID_ARGUMENT", "x", false, { reason_code: code }));
    expect(reason("follow_up_not_accepted").message).toBe(t("missions.error.reason.followUpNotAccepted"));
    expect(reason("follow_up_base_mismatch").message).toBe(t("missions.error.reason.followUpBaseMismatch"));
    expect(reason("follow_up_not_accepted").message).not.toBe(reason("follow_up_base_mismatch").message);
    expect(reason("repository_changed").message).toBe(t("missions.error.reason.repositoryChanged"));
    expect(reason("path_not_accessible").message).toBe(t("missions.error.reason.pathNotAccessible"));
  });

  it("dirty_worktree는 다시 시도, base_changed는 새 작업 안내(행동 없음), 정리 후 동기화가 필요한 사유는 다시 동기화", () => {
    expect(missionError(t, new RpcClientError("DIRTY_WORKTREE", "x", false, { reason_code: "dirty_worktree" }))).toMatchObject({
      message: t("missions.error.reason.dirtyWorktree"), action: "retry",
    });
    expect(missionError(t, new RpcClientError("INVALID_STATE", "x", false, { reason_code: "base_changed" })).action).toBeNull();
    expect(missionError(t, new RpcClientError("STALE_CANDIDATE", "x", false, { reason_code: "candidate_mismatch" })).action).toBe("resync");
    expect(missionError(t, new RpcClientError("INVALID_STATE", "x", false, { reason_code: "lead_missing" })).action).toBe("open_settings");
  });

  it("커밋하지 않은 변경 base 스냅샷(include_uncommitted) 관련 사유는 각자 전용 문장과 행동을 갖는다", () => {
    expect(missionError(t, new RpcClientError("DIRTY_WORKTREE", "x", false, { reason_code: "unmerged_paths" }))).toMatchObject({
      message: t("missions.error.reason.unmergedPaths"), action: "retry",
    });
    expect(missionError(t, new RpcClientError("INVALID_ARGUMENT", "x", false, { reason_code: "snapshot_too_large" }))).toMatchObject({
      message: t("missions.error.reason.snapshotTooLarge"), action: null,
    });
    expect(missionError(t, new RpcClientError("INVALID_ARGUMENT", "x", false, { reason_code: "follow_up_snapshot" }))).toMatchObject({
      message: t("missions.error.reason.followUpSnapshot"), action: null,
    });
  });

  it("시작 전 팀 검사의 Builder·Reviewer 누락은 전용 문장과 설정 열기로 이어진다", () => {
    const reason = (code: string) => missionError(t, new RpcClientError("INVALID_ARGUMENT", "x", false, { reason_code: code }));
    expect(reason("builder_missing")).toMatchObject({ message: t("missions.error.reason.builderMissing"), action: "open_settings" });
    expect(reason("reviewer_missing")).toMatchObject({ message: t("missions.error.reason.reviewerMissing"), action: "open_settings" });
    expect(reason("builder_missing").message).not.toBe(reason("reviewer_missing").message);
  });

  it("모르는 reason code는 무시하고 code 매핑으로 내려간다", () => {
    const error = missionError(t, new RpcClientError("INVALID_STATE", "x", false, { reason_code: "brand_new_reason" }));
    expect(error.message).toBe(t("missions.error.code.invalidState"));
    expect(error.action).toBe("resync");
    expect(error.reasonCode).toBe("brand_new_reason");
  });

  it("capability_* 사유는 기능 미지원 문장과 모델 변경으로 묶는다", () => {
    const error = missionError(t, new RpcClientError("CAPABILITY_UNSUPPORTED", "x", false, { reason_code: "capability_scoped_write" }));
    expect(error.message).toBe(t("missions.error.reason.capabilityUnsupported"));
    expect(error.action).toBe("change_model");
  });

  it.each([
    ["REVISION_CONFLICT", "resync"],
    ["NOT_FOUND", "open_list"],
    ["AUTH_REQUIRED", "login"],
    ["MODEL_UNAVAILABLE", "change_model"],
    ["CAPABILITY_UNSUPPORTED", "change_model"],
    ["DAEMON_UNAVAILABLE", "retry"],
    ["BUSY", "retry"],
    ["STALE_DECISION", "resync"],
  ] as const)("%s → 기본 행동 %s", (code, action) => {
    expect(missionError(t, new RpcClientError(code, "m")).action).toBe(action);
  });

  it("PROVIDER_RATE_LIMITED는 retry_after_ms로 해제 시각을 알려 주고 모델 변경을 권한다", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-17T03:00:00.000Z"));
    const error = missionError(t, new RpcClientError("PROVIDER_RATE_LIMITED", "slow down", true, { retry_after_ms: 90 * 60_000 }));
    const time = formatClock(new Date(Date.now() + 90 * 60_000).toISOString());
    expect(error.message).toBe(t("missions.error.code.providerRateLimitedUntil", { time }));
    expect(error.action).toBe("change_model");
    const unknown = missionError(t, new RpcClientError("PROVIDER_RATE_LIMITED", "slow down"));
    expect(unknown.message).toBe(t("missions.error.code.providerRateLimited"));
  });

  it("직렬화된 RPC 오류 객체도 같은 방식으로 읽는다", () => {
    const error = missionError(t, { code: "AUTH_REQUIRED", message: "login", retryable: false, details: { reason_code: null } });
    expect(error).toMatchObject({ code: "AUTH_REQUIRED", reasonCode: null, action: "login", detail: "AUTH_REQUIRED: login" });
  });

  it("매핑 없는 code는 일반 거절 문장, code가 없으면 요청 실패 문장으로 떨어진다", () => {
    expect(missionError(t, new RpcClientError("SPAWN_FAILED", "secret raw text"))).toMatchObject({
      message: t("missions.error.code.unknown"), action: null, detail: "SPAWN_FAILED: secret raw text",
    });
    const plain = missionError(t, new Error("secret raw text"));
    expect(plain).toMatchObject({ code: null, message: t("missions.error.generic"), detail: "secret raw text", action: null });
    expect(missionError(t, "boom").message).toBe(t("missions.error.generic"));
    expect(missionError(t, undefined)).toMatchObject({ message: t("missions.error.generic"), detail: null });
  });

  it("코드 없는 timeout은 연결 문장과 다시 시도로 본다", () => {
    expect(missionError(t, new Error("request timed out after 5s"))).toMatchObject({
      message: t("missions.error.code.connection"), action: "retry",
    });
  });

  it("StaleActionError는 최신 상태 확인 문장과 다시 동기화", () => {
    expect(missionError(t, new StaleActionError())).toMatchObject({ message: t("missions.error.stale"), action: "resync", detail: null });
  });

  it("영어 설정이면 영어 문장을 돌려준다", () => {
    useI18nStore.setState({ language: "en" });
    expect(missionError(t, new RpcClientError("REVISION_CONFLICT", "x")).message).toBe("Another change was saved first. Resync and check again.");
  });
});

describe("오류 보조", () => {
  it("withSupportedActions는 수행할 수 없는 행동만 지우고 login은 유지한다", () => {
    const base = missionError(t, new RpcClientError("NOT_FOUND", "x"));
    expect(withSupportedActions(base, ["resync"]).action).toBeNull();
    expect(withSupportedActions(base, ["open_list"]).action).toBe("open_list");
    expect(withSupportedActions(missionError(t, new RpcClientError("AUTH_REQUIRED", "x")), []).action).toBe("login");
  });

  it("isRevisionConflict는 code만 본다", () => {
    expect(isRevisionConflict(new RpcClientError("REVISION_CONFLICT", "x"))).toBe(true);
    expect(isRevisionConflict({ code: "REVISION_CONFLICT", message: "x" })).toBe(true);
    expect(isRevisionConflict(new RpcClientError("REQUEST_CONFLICT", "x"))).toBe(false);
    expect(isRevisionConflict(new Error("REVISION_CONFLICT"))).toBe(false);
  });
});
