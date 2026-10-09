/**
 * 프로필/실행 입력 검증 (02 §3 마지막 문단, 04 §5, 01 §4 계약 미러).
 *
 * Windows 셸 shim 규칙: `.cmd`/`.bat`/`.ps1`은 관리 프로필에서
 * `.exe`처럼 직접 실행하지 않는다. native 실행 파일 또는 명시된
 * interpreter 실행 파일 + argv prefix 형태만 등록할 수 있다.
 * 검증은 저장 시와 실행 직전 모두 수행한다(같은 함수 사용).
 */

import type { LaunchProfile, ProfileEnvEntry, ProfileInterpreter } from "./types";
import { t } from "../../i18n";

/** 테스트 주입용 플랫폼 판별(브라우저/노드 공통). */
export type PlatformFlag = "windows" | "other";

export const WINDOWS_SHELL_SHIM_EXTENSIONS = [".cmd", ".bat", ".ps1"] as const;

export interface ValidationOk {
  ok: true;
}
export interface ValidationFailure {
  ok: false;
  /** 사용자에게 그대로 보여 주는 문구(행동 지시 포함). */
  message: string;
}
export type ValidationResult = ValidationOk | ValidationFailure;

/** 확장자(소문자) — 쿼리 파라미터 등은 취급하지 않는다. */
export function fileExtension(path: string): string {
  const base = path.replace(/[\\/]+$/, "");
  const dot = base.lastIndexOf(".");
  const sep = Math.max(base.lastIndexOf("/"), base.lastIndexOf("\\"));
  if (dot <= sep) return "";
  return base.slice(dot).toLowerCase();
}

export function isWindowsShellShim(program: string): boolean {
  return (WINDOWS_SHELL_SHIM_EXTENSIONS as readonly string[]).includes(fileExtension(program));
}

export function isAbsolutePath(path: string, platform: PlatformFlag): boolean {
  if (!path) return false;
  if (platform === "windows") {
    return /^[a-zA-Z]:[\\/]/.test(path) || /^\\\\[^\\]/.test(path);
  }
  return path.startsWith("/");
}

/**
 * 경로를 "실행될 그대로" 정규화한다: 앞뒤 공백 제거, Windows는 구분자를
 * 백슬래시로, 중복 구분자/끝 구분자 제거. daemon 쪽 정규화와 별개로
 * UI가 같은 문자열을 보여 주기 위한 것이다.
 */
export function normalizeProgramPath(path: string, platform: PlatformFlag): string {
  let p = path.trim();
  if (!p) return p;
  if (platform === "windows") {
    p = p.replace(/\//g, "\\");
    // UNC 선행 백슬래시 2개는 유지한 채 나머지 중복만 정리한다.
    const unc = p.startsWith("\\\\");
    p = p.replace(/\\{2,}/g, "\\");
    if (unc) p = "\\" + p;
    return p.replace(/\\+$/, "");
  }
  p = p.replace(/\/{2,}/g, "/");
  return p.replace(/\/+$/, "");
}

export function shimRejectionMessage(program: string): string {
  const ext = fileExtension(program).replace(".", "");
  return t("profile.error.shimRejected", { ext });
}

export interface ProgramValidationInput {
  program: string;
  platform: PlatformFlag;
  interpreter: ProfileInterpreter | null;
}

/**
 * 관리 프로필 program 검증 — 저장 시/실행 전 동일 규칙.
 * - Windows + shim(.cmd/.bat/.ps1) 직접 실행 → 거부(행동 지시 문구).
 * - interpreter 형태: interpreter.executable은 native여야 하고
 *   scriptArgvPrefix는 비어 있으면 안 된다. 이때 descriptor.program은
 *   신원/버전 조회용이므로 shim이어도 실행되지 않는다(통과).
 * - Windows가 아니면 shim 확장자를 문제 삼지 않는다(shebang 실행).
 */
export function validateProgram(input: ProgramValidationInput): ValidationResult {
  const { program, platform, interpreter } = input;
  if (!program.trim()) {
    return { ok: false, message: t("profile.error.programRequired") };
  }
  if (interpreter) {
    const exe = interpreter.executable.trim();
    if (!exe) {
      return { ok: false, message: t("profile.error.interpreterRequired") };
    }
    if (platform === "windows" && isWindowsShellShim(exe)) {
      return {
        ok: false,
        message: shimRejectionMessage(exe) + "\n" + t("profile.error.interpreterNative"),
      };
    }
    if (!isAbsolutePath(exe, platform)) {
      return { ok: false, message: t("profile.error.interpreterAbsolute") };
    }
    if (interpreter.scriptArgvPrefix.length === 0 || interpreter.scriptArgvPrefix.every((a) => !a.trim())) {
      return {
        ok: false,
        message: t("profile.error.scriptPrefixRequired"),
      };
    }
    return { ok: true };
  }
  if (!isAbsolutePath(program, platform)) {
    return { ok: false, message: t("profile.error.programAbsolute") };
  }
  if (platform === "windows" && isWindowsShellShim(program)) {
    return { ok: false, message: shimRejectionMessage(program) };
  }
  return { ok: true };
}

/** 실행 직전 검증: program 규칙 + 프로필 최소 형태. */
export function validateProfileForLaunch(
  profile: Pick<LaunchProfile, "descriptor" | "interpreter">,
  platform: PlatformFlag,
): ValidationResult {
  return validateProgram({
    program: profile.descriptor.program,
    platform,
    interpreter: profile.interpreter,
  });
}

// ---------------------------------------------------------------- argv 예산

/** 01 §4 LaunchRequest.argv 클라이언트 미러 — 256개 / 64 KiB. */
export const MAX_ARGV_COUNT = 256;
export const MAX_ARGV_TOTAL_BYTES = 64 * 1024;

export function argvTotalBytes(args: readonly string[]): number {
  let total = 0;
  for (const a of args) total += new TextEncoder().encode(a).length;
  return total;
}

export interface ArgvBudget {
  count: number;
  maxCount: number;
  bytes: number;
  maxBytes: number;
  countOverflow: boolean;
  bytesOverflow: boolean;
  ok: boolean;
}

export function validateArgvBudget(args: readonly string[]): ArgvBudget {
  const count = args.length;
  const bytes = argvTotalBytes(args);
  const countOverflow = count > MAX_ARGV_COUNT;
  const bytesOverflow = bytes > MAX_ARGV_TOTAL_BYTES;
  return {
    count,
    maxCount: MAX_ARGV_COUNT,
    bytes,
    maxBytes: MAX_ARGV_TOTAL_BYTES,
    countOverflow,
    bytesOverflow,
    ok: !countOverflow && !bytesOverflow,
  };
}

export function argvBudgetMessage(budget: ArgvBudget): string | null {
  if (budget.ok) return null;
  if (budget.countOverflow && budget.bytesOverflow) {
    return t("profile.error.argvBoth", {
      count: budget.count,
      maxCount: budget.maxCount,
      bytes: budget.bytes,
      maxBytes: budget.maxBytes,
    });
  }
  if (budget.countOverflow) {
    return t("profile.error.argvCount", { count: budget.count, maxCount: budget.maxCount });
  }
  return t("profile.error.argvBytes", { bytes: budget.bytes, maxBytes: budget.maxBytes });
}

/** argv 편집기 파싱: 한 줄이 인수 하나(공백 포함 인수 허용). 빈 줄 무시. */
export function parseArgvText(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

// ---------------------------------------------------------------- env 검증

export interface EnvValidationIssue {
  index: number;
  key: string;
  message: string;
}

export interface EnvValidation {
  ok: boolean;
  issues: EnvValidationIssue[];
}

const ENV_KEY_RE = /^[A-Za-z_][A-Za-z0-9_.]*$/;

/**
 * env allowlist 항목 검증 — NUL 거부, 키 형식, 중복 키, 값/참조 상호 배타.
 * 비밀 값 입력 필드는 없으므로 value 자체를 비밀로 취급하는 경로도 없다.
 */
export function validateEnvEntries(entries: readonly ProfileEnvEntry[]): EnvValidation {
  const issues: EnvValidationIssue[] = [];
  const seen = new Map<string, number>();
  entries.forEach((entry, index) => {
    const key = entry.key;
    if (!key) {
      issues.push({ index, key, message: t("profile.error.envKeyEmpty") });
    } else if (key.includes("\0")) {
      issues.push({ index, key, message: t("profile.error.envKeyNul") });
    } else if (!ENV_KEY_RE.test(key)) {
      issues.push({ index, key, message: t("profile.error.envKeyForm") });
    }
    const prev = seen.get(key);
    if (prev !== undefined) {
      issues.push({ index, key, message: t("profile.error.envDuplicate", { key, index: prev + 1 }) });
    } else {
      seen.set(key, index);
    }
    const hasValue = entry.value !== null && entry.value !== "";
    const hasSecret = entry.secretRef !== null && entry.secretRef.trim() !== "";
    if (!hasValue && !hasSecret) {
      issues.push({ index, key, message: t("profile.error.envValueRequired") });
    }
    if (entry.value !== null && entry.value.includes("\0")) {
      issues.push({ index, key, message: t("profile.error.envValueNul") });
    }
    if (entry.secretRef !== null && entry.secretRef.includes("\0")) {
      issues.push({ index, key, message: t("profile.error.envSecretNul") });
    }
  });
  return { ok: issues.length === 0, issues };
}

/** 일반 값 항목만 env_overrides로 합성한다(secretRef는 참조이므로 제외). */
export function envOverridesForLaunch(entries: readonly ProfileEnvEntry[]): { [key: string]: string } {
  const out: { [key: string]: string } = {};
  for (const entry of entries) {
    if (entry.value !== null && entry.key && !entry.key.includes("\0") && !entry.value.includes("\0")) {
      out[entry.key] = entry.value;
    }
  }
  return out;
}

// ---------------------------------------------------------------- cwd 검증

/**
 * cwd 텍스트 검증(존재 여부는 daemon이 검사 — 01 §4 CWD_UNAVAILABLE).
 * 절대 경로 형식만 허용한다.
 */
export function validateCwd(cwd: string, platform: PlatformFlag): ValidationResult {
  if (!cwd.trim()) return { ok: false, message: t("profile.error.cwdRequired") };
  if (!isAbsolutePath(cwd.trim(), platform)) {
    return { ok: false, message: t("profile.error.cwdAbsolute") };
  }
  return { ok: true };
}
