/**
 * 관리 실행 요청 조립 (04 §1: 프로필 → cwd/argv/자원 정책 → LaunchRequest).
 *
 * 순수 함수다: 프로필 + 폼 상태 → 생성된 LaunchRequest(정확한 JSON) 또는
 * 차단 오직 목록. Windows shim 규칙은 실행 직전에도 동일하게 재검증한다.
 */

import type { ClaudeProvider } from "../../generated/ClaudeProvider";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { Enforcement } from "../../generated/Enforcement";
import { t } from "../../i18n";
import type { LaunchProfile } from "../profiles/types";
import { MIN_RESERVATION_BYTES } from "../profiles/types";
import {
  argvBudgetMessage,
  envOverridesForLaunch,
  validateArgvBudget,
  validateCwd,
  validateEnvEntries,
  validateProfileForLaunch,
  type PlatformFlag,
} from "../profiles/validation";

import { effectiveLaunchCommand } from "./autonomy";
import { claudeProviderEnvConflicts } from "./claudeProvider";

export interface ManagedRunPolicyInput {
  enforcement: Enforcement;
  /** 바이트 단위(03 §8 기본 2 GiB, 하한 256 MiB). */
  reservationBytes: number;
  cpuSlots: number;
  memoryMaxBytes: number | null;
  cpuMaxCores: number | null;
  pidsMax: number | null;
}

export interface ManagedRunFormInput {
  profileId: string;
  cwd: string;
  /** 프로필 고정 prefix 뒤에 붙는 사용자 추가 인수. */
  extraArgv: string[];
  fullAutonomy?: boolean;
  /**
   * Claude 제공자 선택자(claudeProviderFor 결과). null이면 라우팅하지 않는다 —
   * 프로필 kind가 claude가 아니거나 설정이 Anthropic이면 호출자가 null을 준다.
   * 비밀이 아닌 선택자만 실린다(키는 데몬이 실행 시점에 해석).
   */
  claudeProvider: ClaudeProvider | null;
  priority: number;
  policy: ManagedRunPolicyInput;
  cols: number;
  rows: number;
}

export interface ComposeLaunchDeps {
  uuid: () => string;
  platform: PlatformFlag;
}

export type ComposeLaunchResult =
  | { ok: true; request: LaunchRequest }
  | { ok: false; errors: string[] };

/** 폼의 GiB 값 → bytes(반올림, 소수 GiB 허용). */
export function gibToBytes(gib: number): number {
  return Math.round(gib * 1024 ** 3);
}

/**
 * 프로필 + 폼 → LaunchRequest. 오류는 모아서 돌려준다(폼 표시용).
 * 통과하면 request는 생성된 형식(src/generated/LaunchRequest) 그대로다.
 */
export function composeLaunchRequest(
  profile: LaunchProfile,
  form: ManagedRunFormInput,
  deps: ComposeLaunchDeps,
): ComposeLaunchResult {
  const errors: string[] = [];

  // 1) Windows shim 규칙 — 저장 시와 실행 전 동일 검증(02 §3).
  const programCheck = validateProfileForLaunch(profile, deps.platform);
  if (!programCheck.ok) errors.push(programCheck.message);

  // 2) cwd: 폼 값 우선, 없으면 프로필 기본값. 존재 확인은 daemon이 한다.
  const cwd = form.cwd.trim() || profile.cwd.trim();
  if (!cwd) {
    errors.push(t("launch.cwdRequired"));
  } else {
    const cwdCheck = validateCwd(cwd, deps.platform);
    if (!cwdCheck.ok) errors.push(cwdCheck.message);
  }

  // 3) argv: interpreter prefix + 프로필 prefix + 추가 인수, 예산 검증 포함.
  const effective = effectiveLaunchCommand(profile, form.extraArgv, form.fullAutonomy);
  const budget = validateArgvBudget(effective.argv);
  if (!budget.ok) {
    const msg = argvBudgetMessage(budget);
    if (msg) errors.push(msg);
  }

  // 4) env: allowlist 검증 + 일반 값만 env_overrides로(비밀 참조는 제외).
  const envCheck = validateEnvEntries(profile.env);
  if (!envCheck.ok) errors.push(...envCheck.issues.map((i) => i.message));
  const env_overrides = envOverridesForLaunch(profile.env);

  // 4-1) 제공자 라우팅과 충돌하는 env — 데몬 규칙의 미러(데몬도 같은 키를
  //      claude_provider_env_conflict로 거부한다). secretRef 항목은 env_overrides에
  //      실리지 않으므로 여기서도 세지 않는다.
  if (form.claudeProvider) {
    const conflicts = claudeProviderEnvConflicts(Object.keys(env_overrides));
    if (conflicts.length > 0) {
      errors.push(t("launch.claudeProviderEnvConflict", { keys: conflicts.join(", ") }));
    }
  }

  // 5) 정책 수치.
  const { policy } = form;
  if (!Number.isFinite(policy.reservationBytes) || policy.reservationBytes < MIN_RESERVATION_BYTES) {
    errors.push(t("launch.reservationMin", { minBytes: MIN_RESERVATION_BYTES }));
  }
  if (!Number.isInteger(policy.cpuSlots) || policy.cpuSlots < 1) {
    errors.push(t("launch.cpuSlotsMin"));
  }

  if (errors.length > 0) return { ok: false, errors };

  const request: LaunchRequest = {
    request_id: deps.uuid(),
    profile_id: profile.id,
    cwd,
    program: effective.program,
    argv: effective.argv,
    env_overrides,
    mode: "managed",
    executor: { kind: "local" },
    cols: form.cols,
    rows: form.rows,
    priority: form.priority,
    policy: {
      reservation_bytes: String(policy.reservationBytes),
      cpu_slots: policy.cpuSlots,
      enforcement: policy.enforcement,
      memory_max_bytes: policy.memoryMaxBytes === null ? null : String(policy.memoryMaxBytes),
      cpu_max_cores: policy.cpuMaxCores,
      pids_max: policy.pidsMax,
    },
  };
  // 라우팅이 없으면 키 자체를 싣지 않는다(구 데몬과의 호환, 지문 불변).
  if (form.claudeProvider) request.claude_provider = form.claudeProvider;
  return { ok: true, request };
}
