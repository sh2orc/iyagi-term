/**
 * Claude Code 제공자 라우팅 판정: Claude + Z.ai 설정에서만 선택자를 만들고,
 * 구 데몬이면 조용히 떨어지지 않고 거절하며, env 충돌 규칙은 데몬과 같다.
 */

import { afterEach, describe, expect, it } from "vitest";
import { RpcClientError, type RpcErrorDetails } from "../daemon/client";
import { useI18nStore } from "../../i18n";
import {
  CLAUDE_PROVIDER_ENV_CONFLICT_KEYS,
  claudeProviderEnvConflicts,
  claudeProviderErrorMessage,
  claudeProviderFor,
  profileEnvClaudeProviderConflicts,
} from "./claudeProvider";

const zai = { claudeProvider: "zai-coding-plan", zaiMainModel: "glm-5.3[1m]" } as const;
const anthropic = { claudeProvider: "anthropic", zaiMainModel: "glm-5.3[1m]" } as const;

describe("claudeProviderFor", () => {
  it("Claude + Z.ai 설정 + 데몬 지원이면 선택자를 만든다(주 모델 그대로)", () => {
    expect(claudeProviderFor("claude", zai, true)).toEqual({
      ok: true,
      provider: { kind: "zai_coding_plan", main_model: "glm-5.3[1m]" },
    });
    expect(claudeProviderFor("claude", { ...zai, zaiMainModel: "glm-5.3-flash[1m]" }, true)).toEqual({
      ok: true,
      provider: { kind: "zai_coding_plan", main_model: "glm-5.3-flash[1m]" },
    });
  });

  it("Anthropic 설정이면 데몬 지원과 무관하게 라우팅 없음", () => {
    expect(claudeProviderFor("claude", anthropic, true)).toEqual({ ok: true, provider: null });
    expect(claudeProviderFor("claude", anthropic, false)).toEqual({ ok: true, provider: null });
  });

  it.each(["codex", "opencode", "custom"] as const)("%s 실행에는 절대 붙지 않는다", (kind) => {
    expect(claudeProviderFor(kind, zai, true)).toEqual({ ok: true, provider: null });
    expect(claudeProviderFor(kind, zai, false)).toEqual({ ok: true, provider: null });
  });

  it("Z.ai 설정인데 데몬이 라우팅을 모르면 거절한다(조용한 폴백 금지)", () => {
    expect(claudeProviderFor("claude", zai, false)).toEqual({ ok: false, reason: "daemon_outdated" });
  });
});

describe("env 충돌 미러(데몬 orchestrator 규칙)", () => {
  it("충돌 키 목록은 데몬과 같다", () => {
    expect([...CLAUDE_PROVIDER_ENV_CONFLICT_KEYS]).toEqual([
      "ANTHROPIC_BASE_URL",
      "ANTHROPIC_AUTH_TOKEN",
      "ANTHROPIC_API_KEY",
      "CLAUDE_CODE_OAUTH_TOKEN",
      "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
      "CLAUDE_CODE_USE_BEDROCK",
      "CLAUDE_CODE_USE_VERTEX",
      "CLAUDE_CODE_USE_FOUNDRY",
    ]);
  });

  it("충돌 키만 목록 순서로 돌려주고, CLAUDE_CONFIG_DIR는 허용한다", () => {
    expect(claudeProviderEnvConflicts(["CLAUDE_CONFIG_DIR", "PATH"])).toEqual([]);
    expect(claudeProviderEnvConflicts(["CLAUDE_CODE_USE_VERTEX", "ANTHROPIC_API_KEY"])).toEqual([
      "ANTHROPIC_API_KEY",
      "CLAUDE_CODE_USE_VERTEX",
    ]);
    // 대소문자 변형은 다른 키다(데몬도 정확히 같은 이름만 본다).
    expect(claudeProviderEnvConflicts(["anthropic_api_key"])).toEqual([]);
  });

  it("프로필 env는 일반 값 항목만 센다(secretRef는 env_overrides에 실리지 않는다)", () => {
    expect(
      profileEnvClaudeProviderConflicts([
        { key: "ANTHROPIC_API_KEY", value: null, secretRef: "keychain:iyagi/claude" },
        { key: "ANTHROPIC_BASE_URL", value: "https://proxy.example", secretRef: null },
        { key: "IYAGI_PROFILE", value: "claude", secretRef: null },
      ]),
    ).toEqual(["ANTHROPIC_BASE_URL"]);
  });
});

describe("claudeProviderErrorMessage", () => {
  afterEach(() => {
    useI18nStore.getState().setLanguage(null);
  });

  const rpc = (reason: unknown) =>
    new RpcClientError("INVALID_ARGUMENT", "daemon detail must not leak", false, {
      reason_code: reason,
    } as RpcErrorDetails);

  it("라우팅 reason_code는 문구로 바꾸고 데몬 message는 쓰지 않는다", () => {
    for (const reason of [
      "zai_key_missing",
      "zai_key_unreadable",
      "claude_provider_program_mismatch",
      "claude_provider_env_conflict",
    ]) {
      const message = claudeProviderErrorMessage(rpc(reason));
      expect(message, reason).not.toBeNull();
      expect(message).not.toContain("daemon detail must not leak");
    }
    expect(claudeProviderErrorMessage(rpc("zai_key_missing"))).toContain("Z.ai Coding Plan");
  });

  it("다른 오류·다른 reason_code·비문자열 reason은 null", () => {
    expect(claudeProviderErrorMessage(rpc("claude_transcript_missing"))).toBeNull();
    expect(claudeProviderErrorMessage(rpc(42))).toBeNull();
    expect(claudeProviderErrorMessage(new RpcClientError("INTERNAL", "boom", false))).toBeNull();
    expect(claudeProviderErrorMessage(new Error("plain"))).toBeNull();
    expect(claudeProviderErrorMessage(null)).toBeNull();
  });

  it("영어에서도 같은 키를 번역한다", () => {
    useI18nStore.getState().setLanguage("en");
    expect(claudeProviderErrorMessage(rpc("zai_key_missing"))).toContain("Settings");
  });
});
