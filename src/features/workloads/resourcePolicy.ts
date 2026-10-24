/**
 * 자원 가드 정책(08 §5 `guard.set_policy`)의 프런트 쪽 규칙 — 설정 화면의 입력
 * 검증, 사용자 변경분(preferences) 정리, 데몬에 보낼 전체 정책 합성이 같은
 * 범위를 쓰도록 한곳에 둔다.
 *
 * - 데몬은 정책 전체(`GuardPolicyParams = {policy}`)를 받고 아래 범위를 벗어나면
 *   INVALID_ARGUMENT로 거절한다. 같은 범위로 먼저 걸러 사용자가 왜 안 되는지를
 *   그 자리에서 보게 한다.
 * - 데몬 정책은 메모리에만 있다(데몬 재시작 → 호스트 기본값). 그래서 사용자가
 *   바꾼 항목만 저장하고(나머지는 호스트 기본값을 따른다) 붙을 때마다 다시 적용한다.
 */

import type { GuardPolicy } from "../../generated/GuardPolicy";

const MiB = 1024 * 1024;
const GiB = 1024 * MiB;

export const GUARD_CPU_CORES_MIN = 1;
export const GUARD_CPU_CORES_MAX = 1024;
export const GUARD_RSS_MIN_BYTES = 256 * MiB;
export const GUARD_RSS_MAX_BYTES = 1024 * GiB;
export const GUARD_SUSTAIN_MIN_MS = 1000;
export const GUARD_SUSTAIN_MAX_MS = 600_000;

/** 바이트 ↔ GiB(설정 화면은 GiB로 받는다). */
export const BYTES_PER_GIB = GiB;

/**
 * 기본 정책을 알리지 않는 구 데몬의 기본값 — term-contracts `GuardPolicy::default`
 * 미러다. 새 데몬은 호스트 코어·메모리로 유도한 값을 `guard_policy_default`로 준다.
 */
export const FALLBACK_GUARD_POLICY: GuardPolicy = {
  auto_suspend: true,
  cpu_cores_limit: 6,
  rss_limit_bytes: String(4 * GiB),
  sustain_ms: "20000",
  rss_sustain_ms: "5000",
  auto_resume: false,
};

export type GuardField = keyof GuardPolicy;

/** 화면·저장 순서(설정 패널의 행 순서와 같다). */
export const GUARD_FIELDS: readonly GuardField[] = [
  "auto_suspend",
  "rss_limit_bytes",
  "rss_sustain_ms",
  "cpu_cores_limit",
  "sustain_ms",
  "auto_resume",
];

const U64_PATTERN = /^\d{1,16}$/;

function u64Value(value: unknown): number | null {
  if (typeof value !== "string" || !U64_PATTERN.test(value)) return null;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : null;
}

function inRange(value: number | null, min: number, max: number): boolean {
  return value !== null && value >= min && value <= max;
}

/**
 * 한 항목의 값이 데몬이 받아들이는 범위인가. 범위 밖이면 그 이유의 i18n 키를,
 * 맞으면 null을 돌려준다(설정 화면이 그대로 문구로 쓴다).
 */
export function guardFieldError(field: GuardField, value: unknown): string | null {
  switch (field) {
    case "auto_suspend":
    case "auto_resume":
      return typeof value === "boolean" ? null : "settings.resources.error.type";
    case "cpu_cores_limit":
      return typeof value === "number" &&
        Number.isInteger(value) &&
        inRange(value, GUARD_CPU_CORES_MIN, GUARD_CPU_CORES_MAX)
        ? null
        : "settings.resources.error.cpuCores";
    case "rss_limit_bytes":
      return inRange(u64Value(value), GUARD_RSS_MIN_BYTES, GUARD_RSS_MAX_BYTES)
        ? null
        : "settings.resources.error.rssLimit";
    case "sustain_ms":
    case "rss_sustain_ms":
      return inRange(u64Value(value), GUARD_SUSTAIN_MIN_MS, GUARD_SUSTAIN_MAX_MS)
        ? null
        : "settings.resources.error.sustain";
  }
}

/** 정책 전체에서 범위를 벗어난 첫 항목(없으면 null). */
export function guardPolicyProblem(policy: GuardPolicy): { field: GuardField; key: string } | null {
  for (const field of GUARD_FIELDS) {
    const key = guardFieldError(field, policy[field]);
    if (key) return { field, key };
  }
  return null;
}

/**
 * 저장된 사용자 변경분 정리 — 아는 항목이고 데몬이 받아들이는 값만 남긴다
 * (모양이 어긋난 저장값이 붙을 때마다 거절당하지 않게).
 */
export function sanitizeGuardOverrides(raw: unknown): Partial<GuardPolicy> {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return {};
  const source = raw as Record<string, unknown>;
  const result: Partial<GuardPolicy> = {};
  for (const field of GUARD_FIELDS) {
    const value = source[field];
    if (value === undefined || guardFieldError(field, value) !== null) continue;
    (result as Record<GuardField, unknown>)[field] = value;
  }
  return result;
}

/** 기본(또는 현재) 정책 위에 사용자 변경분을 얹은 전체 정책. */
export function overlayGuardPolicy(base: GuardPolicy, overrides: Partial<GuardPolicy>): GuardPolicy {
  const merged: GuardPolicy = { ...base };
  for (const field of GUARD_FIELDS) {
    const value = overrides[field];
    if (value !== undefined) (merged as Record<GuardField, unknown>)[field] = value;
  }
  return merged;
}

export function sameGuardPolicy(a: GuardPolicy, b: GuardPolicy): boolean {
  return GUARD_FIELDS.every((field) => a[field] === b[field]);
}

/**
 * 변경분 갱신: `patch`의 항목을 얹되, 기본값과 같아진 항목은 지운다 — "사용자가
 * 바꾼 것"만 남겨 호스트 기본값이 바뀌면(다른 기기·메모리 증설) 나머지가 따라간다.
 * `reset`에 든 항목은 값과 무관하게 지운다.
 */
export function nextGuardOverrides(
  overrides: Partial<GuardPolicy>,
  patch: Partial<GuardPolicy>,
  defaults: GuardPolicy,
  reset: readonly GuardField[] = [],
): Partial<GuardPolicy> {
  const next: Partial<GuardPolicy> = { ...overrides };
  for (const field of GUARD_FIELDS) {
    const value = patch[field];
    if (value === undefined) continue;
    if (value === defaults[field]) delete next[field];
    else (next as Record<GuardField, unknown>)[field] = value;
  }
  for (const field of reset) delete next[field];
  return next;
}

export function sameGuardOverrides(a: Partial<GuardPolicy>, b: Partial<GuardPolicy>): boolean {
  return GUARD_FIELDS.every((field) => a[field] === b[field]);
}

/** GiB 입력 → 바이트 문자열(반올림). 숫자가 아니면 null. */
export function gibToBytes(gib: number): string | null {
  if (!Number.isFinite(gib) || gib < 0) return null;
  return String(Math.round(gib * GiB));
}

/** 바이트 문자열 → GiB(표시용, 소수 둘째 자리까지). */
export function bytesToGib(bytes: string): number | null {
  const value = u64Value(bytes);
  return value === null ? null : Math.round((value / GiB) * 100) / 100;
}

/** 초 입력 → 밀리초 문자열(반올림). */
export function secondsToMs(seconds: number): string | null {
  if (!Number.isFinite(seconds) || seconds < 0) return null;
  return String(Math.round(seconds * 1000));
}

/** 밀리초 문자열 → 초(표시용, 소수 첫째 자리까지). */
export function msToSeconds(ms: string): number | null {
  const value = u64Value(ms);
  return value === null ? null : Math.round(value / 100) / 10;
}

/**
 * 데몬이 기동 때 유도하는 기본 정책(cpu = max(2, 논리 코어/2), rss = RAM/4를
 * 2–8 GiB로 자름) — mock과 시험이 실제 데몬과 같은 값을 쓰도록 둔다.
 */
export function hostDerivedGuardPolicy(logicalCpus: number, totalBytes: number): GuardPolicy {
  const cores = Math.max(2, Math.floor(Math.max(0, logicalCpus) / 2));
  const rss = Math.min(8 * GiB, Math.max(2 * GiB, Math.floor(Math.max(0, totalBytes) / 4)));
  return { ...FALLBACK_GUARD_POLICY, cpu_cores_limit: cores, rss_limit_bytes: String(rss) };
}
