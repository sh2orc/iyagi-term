/**
 * 셸 프로필 deps seam — App이 SystemProbe를 주입하고 Workbench가 소비한다
 * (managedRunDeps와 같은 패턴: 모듈 스코프 1줄 wiring).
 */

import type { SystemProbe } from "../bridge/systemProbe";
import type { Platform } from "./shortcuts";
import { builtinProfiles, resolveProfile, type ShellProfile } from "./shellProfiles";

let probe: SystemProbe | null = null;
let cache: { platform: Platform; profiles: ShellProfile[] } | null = null;

export function setShellProbe(next: SystemProbe | null): void {
  if (probe !== next) cache = null;
  probe = next;
}

/**
 * 감지된 셸로 프로필 목록을 만든다(1회 캐시). 탐지 실패 시 플랫폼 기본값만.
 * 목록은 builtin(탐지) 프로필이다 — 사용자 추가는 스토어 custom에 있다.
 */
export async function detectShellProfiles(
  platform: Platform,
): Promise<ShellProfile[]> {
  if (cache && cache.platform === platform) return cache.profiles;
  let profiles: ShellProfile[];
  try {
    const detected = probe ? await probe.listShells() : [];
    profiles = builtinProfiles(platform, detected);
  } catch {
    profiles = builtinProfiles(platform, []);
  }
  cache = { platform, profiles };
  return profiles;
}

/** 기본 프로필(비동기 탐지 포함) — controller deps.resolveShell용. */
export async function resolveDefaultShell(
  platform: Platform,
  preferredId: string | null,
  custom: ShellProfile[],
): Promise<ShellProfile | null> {
  const builtin = await detectShellProfiles(platform);
  return resolveProfile([...builtin, ...custom], preferredId);
}

/**
 * 마지막으로 탐지해 둔 내장 프로필(동기). 탐지 전이면 플랫폼 관례 기본값.
 * controller의 동기 resolveShell이 즉시 답해야 할 때 쓴다.
 */
export function cachedBuiltinProfiles(platform: Platform): ShellProfile[] {
  if (cache && cache.platform === platform) return cache.profiles;
  return builtinProfiles(platform, []);
}
