/**
 * Claude Code 제공자 라우팅(설정 → 연동 → Z.ai Coding Plan)의 순수 판정.
 *
 * - 프론트는 `LaunchRequest.claude_provider`에 비밀 아닌 선택자만 싣는다.
 *   키는 데몬이 실행 시점에 자기 비밀 저장소에서 꺼내 환경에 넣는다.
 * - 구 데몬(capabilities.claude_provider_routing 없음 → false)은 이 필드를
 *   모른다. 그때는 조용히 Anthropic으로 떨어지지 않고 실행 자체를 거절한다 —
 *   사용자가 GLM으로 돌아간다고 믿는 채 Anthropic 사용량을 쓰면 안 된다.
 * - 데몬의 env 충돌 규칙을 그대로 미러해 관리 실행 폼에서 먼저 막는다
 *   (데몬도 같은 reason_code로 거부한다).
 */

import type { ClaudeProvider } from "../../generated/ClaudeProvider";
import { t } from "../../i18n";
import { RpcClientError } from "../daemon/client";
import type { PreferenceValues } from "../../store/preferences";
import type { CliKind, ProfileEnvEntry } from "../profiles/types";

export type ClaudeProviderResolution =
  | { ok: true; provider: ClaudeProvider | null }
  | { ok: false; reason: "daemon_outdated" };

/** 판정에 필요한 설정 값만(스토어 전체를 요구하지 않는다 — 시험·셀렉터용). */
export type ClaudeProviderPrefs = Pick<PreferenceValues, "claudeProvider" | "zaiMainModel">;

/**
 * 이 실행에 붙일 제공자 선택자.
 *
 * - Claude가 아니거나 설정이 Anthropic이면 `provider: null`(라우팅 없음).
 * - 설정이 Z.ai인데 데몬이 라우팅을 광고하지 않으면 `ok: false` — 호출자는
 *   반드시 토스트로 거절하고 실행하지 않는다.
 */
export function claudeProviderFor(
  kind: CliKind,
  prefs: ClaudeProviderPrefs,
  routingCapability: boolean,
): ClaudeProviderResolution {
  if (kind !== "claude" || prefs.claudeProvider !== "zai-coding-plan") {
    return { ok: true, provider: null };
  }
  if (!routingCapability) return { ok: false, reason: "daemon_outdated" };
  return { ok: true, provider: { kind: "zai_coding_plan", main_model: prefs.zaiMainModel } };
}

/**
 * 라우팅과 충돌하는 환경 변수(데몬 orchestrator의 규칙과 같은 목록).
 * 이 키가 `env_overrides`에 있으면 데몬은 `claude_provider_env_conflict`로
 * 거부한다. `CLAUDE_CONFIG_DIR`는 허용한다(사용자의 ~/.claude를 그대로 쓴다).
 */
export const CLAUDE_PROVIDER_ENV_CONFLICT_KEYS: readonly string[] = [
  "ANTHROPIC_BASE_URL",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_API_KEY",
  "CLAUDE_CODE_OAUTH_TOKEN",
  "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
  "CLAUDE_CODE_USE_BEDROCK",
  "CLAUDE_CODE_USE_VERTEX",
  "CLAUDE_CODE_USE_FOUNDRY",
];

/** env_overrides에 실릴 키 중 라우팅과 충돌하는 것(목록 순서대로). */
export function claudeProviderEnvConflicts(keys: Iterable<string>): string[] {
  const present = new Set(keys);
  return CLAUDE_PROVIDER_ENV_CONFLICT_KEYS.filter((key) => present.has(key));
}

/**
 * 프로필 env 항목에 대한 미러 검사. 데몬은 `env_overrides`만 보므로 일반 값
 * 항목만 센다 — secretRef 항목은 env_overrides에 실리지 않는다
 * (profiles/validation.ts envOverridesForLaunch).
 */
export function profileEnvClaudeProviderConflicts(entries: readonly ProfileEnvEntry[]): string[] {
  return claudeProviderEnvConflicts(entries.filter((entry) => entry.value !== null).map((entry) => entry.key));
}

/** 데몬이 라우팅 실패에 붙이는 reason_code → 문구 키. */
const REASON_MESSAGE_KEYS: Readonly<Record<string, string>> = {
  zai_key_missing: "settings.zai.launch.keyMissing",
  zai_key_unreadable: "settings.zai.launch.keyUnreadable",
  claude_provider_program_mismatch: "settings.zai.launch.programMismatch",
  claude_provider_env_conflict: "settings.zai.launch.envConflict",
};

/** 프론트 판정(구 데몬)에 쓰는 문구 키 — 토스트·폼 경고가 같은 문장을 쓴다. */
export const CLAUDE_PROVIDER_DAEMON_OUTDATED_KEY = "settings.zai.launch.daemonOutdated";

/**
 * 실행 오류가 제공자 라우팅 때문이면 그 문구, 아니면 null. 데몬 오류의
 * message는 쓰지 않는다 — reason_code만 해석한다(키·경로가 섞일 여지 차단).
 */
export function claudeProviderErrorMessage(error: unknown): string | null {
  if (!(error instanceof RpcClientError)) return null;
  const code = error.details?.reason_code;
  if (typeof code !== "string") return null;
  const key = REASON_MESSAGE_KEYS[code];
  return key ? t(key) : null;
}
