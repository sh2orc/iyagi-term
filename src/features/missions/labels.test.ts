/**
 * 사람 문구 매핑(labels.ts): 원문 enum/slug를 화면에 노출하지 않고, 모르는
 * 값은 안전한 폴백으로 떨어진다.
 */

import { beforeEach, describe, expect, it } from "vitest";
import type { Role } from "../../generated/Role";
import type { RunState } from "../../generated/RunState";
import { t, useI18nStore } from "../../i18n";
import {
  blockedReasonLabel,
  decisionOptionEffect,
  decisionOptionLabel,
  inputIntegrityLabelKey,
  inputIntegrityNoteKey,
  roleLabel,
  runStateLabel,
  severityLabel,
} from "./labels";

beforeEach(() => {
  useI18nStore.setState({ language: "ko" });
});

const ROLES: Role[] = ["lead", "researcher", "architect", "builder", "test_author", "reviewer", "specialist", "diagnostician", "integrator", "documenter"];
const RUN_STATES: RunState[] = ["prepared", "starting", "running", "awaiting_input", "stopping", "succeeded", "failed", "cancelled", "interrupted", "unknown"];

describe("roleLabel / runStateLabel / severityLabel", () => {
  it("모든 역할과 실행 상태를 번역 문구로 바꾼다", () => {
    for (const role of ROLES) {
      const label = roleLabel(t, role);
      expect(label).not.toContain("missions.");
      expect(label).not.toBe(role);
    }
    expect(roleLabel(t, "builder")).toBe("Builder · 구현");
    expect(roleLabel(t, null)).toBe(t("missions.role.automatic"));
    expect(roleLabel(t, "future_role")).toBe("future_role");
    const states = new Set<string>();
    for (const state of RUN_STATES) {
      const label = runStateLabel(t, state);
      expect(label).not.toContain("missions.");
      expect(label).not.toBe(state);
      states.add(label);
    }
    expect(states.size).toBe(RUN_STATES.length);
    expect(runStateLabel(t, "awaiting_input")).toBe("입력 대기");
  });

  it("심각도는 차단/주요/보통/참고", () => {
    expect(["blocking", "major", "minor", "note"].map((s) => severityLabel(t, s))).toEqual(["차단", "주요", "보통", "참고"]);
  });
});

describe("blockedReasonLabel", () => {
  it.each([
    ["dependency_failed", "선행 할 일 실패"],
    ["outcome_unknown", "이전 실행 종료 확인 중"],
    ["recovery_held", "복구 방법 선택 대기"],
    ["integration_conflict", "통합 충돌"],
    ["active_time_limit", "활성 시간 한도 도달"],
    ["automatic_start_limit", "자동 시작 한도 도달"],
    ["cost_limit", "비용 한도 초과"],
    ["cost_something_new", "비용 정책 대기"],
    ["capability_scoped_write", "필요 기능 미검증"],
    ["provider_blocked:needs_credentials", "AI가 진행 불가를 보고함"],
    ["AuthRequired", "로그인 필요"],
    ["Some(ProviderUnavailable)", "제공자 연결 불가"],
    ["AUTH_REQUIRED", "로그인 필요"],
  ])("%s → %s", (code, label) => {
    expect(blockedReasonLabel(t, code)).toBe(label);
  });

  it("모르거나 없는 code는 진행 불가", () => {
    expect(blockedReasonLabel(t, "never_seen")).toBe("진행 불가");
    expect(blockedReasonLabel(t, null)).toBe("진행 불가");
    expect(blockedReasonLabel(t, "None")).toBe("진행 불가");
  });
});

describe("decisionOptionLabel / decisionOptionEffect", () => {
  it("알려진 선택지 id는 사람 문구와 효과 1줄을 갖는다", () => {
    const ids = [
      "apply", "revise", "accept", "decline", "resume_unsent", "stop_mission", "retry_with_instruction", "change_model",
      "replan", "adjust_limits", "retry_failed_task", "stop_failed_mission", "retry_reconciled_task", "stop_reconciled_mission",
      "resolve_and_reintegrate", "exclude_candidate", "stop_review_repair", "stop_cost_mission",
    ];
    for (const id of ids) {
      const option = { id, label: "HOST LABEL" };
      expect(decisionOptionLabel(t, option)).not.toBe("HOST LABEL");
      expect(decisionOptionLabel(t, option)).not.toContain("missions.");
      expect(decisionOptionEffect(t, option)).not.toContain("missions.");
      expect(decisionOptionEffect(t, option)).not.toBe(t("missions.decision.effect.default"));
    }
    expect(decisionOptionLabel(t, { id: "apply", label: "x" })).toBe("계획 적용");
    expect(decisionOptionLabel(t, { id: "resume_unsent", label: "x" })).toBe("보내지 않은 실행 시작");
    expect(decisionOptionLabel(t, { id: "retry_failed_task", label: "x" })).toBe(t("missions.decision.retryFailedTask"));
  });

  it("모르는 선택지는 받은 label과 일반 효과 문구", () => {
    expect(decisionOptionLabel(t, { id: "keep", label: "기존 API 유지" })).toBe("기존 API 유지");
    expect(decisionOptionEffect(t, { id: "keep", label: "기존 API 유지" })).toBe(t("missions.decision.effect.default"));
  });
});

it("입력 무결성 문구는 검증 결과(통과)를 섞지 않는다", () => {
  for (const integrity of ["enforced", "observed", "unknown"] as const) {
    const text = t(inputIntegrityNoteKey(integrity));
    expect(text).not.toContain("missions.");
    expect(text).not.toContain("통과");
  }
});

it("무결성 라벨(missions.integrity.*)도 결과 접두어 없이 보장 수준만 말한다 — 실패한 검증 옆에서도 모순이 없다", () => {
  for (const language of ["ko", "en"] as const) {
    useI18nStore.setState({ language });
    for (const integrity of ["enforced", "observed", "unknown"] as const) {
      const text = t(inputIntegrityLabelKey(integrity));
      expect(text).not.toContain("missions.");
      expect(text).not.toContain("통과");
      expect(text).not.toMatch(/passed/i);
    }
  }
  useI18nStore.setState({ language: "ko" });
  expect(t(inputIntegrityLabelKey("enforced"))).toBe("입력 쓰기 차단 보장");
  expect(t(inputIntegrityLabelKey("observed"))).toBe("입력 쓰기 차단 미보장");
});
