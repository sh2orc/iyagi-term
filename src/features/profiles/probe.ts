/**
 * CLI 발견(discovery) + 버전 조회 UI 로직 (04 §5).
 *
 * - 프로필 폼을 열면 probe.listClis()로 codex/claude/opencode 후보를
 *   제안한다: 실행 파일 경로, symlink target, 설치 형태를 그대로 보여 준다.
 * - 버전 조회는 queryVersion만 2초 timeout으로 실행하고, 실패/거절/시간
 *   초과면 null → "감지 안 됨". 값을 지어내지 않는다.
 * - Windows에서 발견된 .cmd/.bat/.ps1 shim은 관리 직접 실행 금지이므로
 *   interpreter 형태 제안 또는 일반 셸 실행 안내로 연결한다(02 §3).
 */

import type { CliCandidate, SystemProbe } from "./probeTypes";
import type { PlatformFlag } from "./validation";
import { fileExtension, isWindowsShellShim } from "./validation";
import { t } from "../../i18n";

/** 04 §5: 버전 조회 timeout 2초(출력 상한 8 KiB는 bridge가 담당). */
export const VERSION_QUERY_TIMEOUT_MS = 2000;

/** timeout 포함 버전 조회 — 결과는 값 또는 null(감지 안 됨). 예외를 던지지 않는다. */
export async function queryVersionWithTimeout(
  probe: SystemProbe,
  program: string,
  timeoutMs: number = VERSION_QUERY_TIMEOUT_MS,
): Promise<string | null> {
  if (!program.trim()) return null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      probe.queryVersion(program).catch(() => null),
      new Promise<null>((resolve) => {
        timer = setTimeout(() => resolve(null), timeoutMs);
      }),
    ]);
  } catch {
    return null;
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

export type VersionProbeStatus = "idle" | "pending" | "value" | "unavailable";

export interface VersionProbeState {
  status: VersionProbeStatus;
  value: string | null;
}

export const VERSION_PROBE_IDLE: VersionProbeState = { status: "idle", value: null };

export function versionProbeText(state: VersionProbeState): string {
  switch (state.status) {
    case "idle":
      return t("profile.version.idle");
    case "pending":
      return t("profile.version.pending");
    case "value":
      return t("profile.version.detected", { value: state.value ?? "" }).trim();
    case "unavailable":
    default:
      return t("profile.version.unavailable");
  }
}

// ------------------------------------------------------------ 발견 제안

export type DiscoveryStatus = "idle" | "pending" | "ready" | "failed";

export interface DiscoveryState {
  status: DiscoveryStatus;
  candidates: CliCandidate[];
  error: string | null;
}

export const DISCOVERY_IDLE: DiscoveryState = { status: "idle", candidates: [], error: null };

export async function discoverClis(probe: SystemProbe | null): Promise<DiscoveryState> {
  if (!probe) return { status: "ready", candidates: [], error: null };
  try {
    const candidates = await probe.listClis();
    return { status: "ready", candidates, error: null };
  } catch (error) {
    return {
      status: "failed",
      candidates: [],
      error: error instanceof Error ? error.message : t("profile.discovery.failed"),
    };
  }
}

export function candidatesForKind(candidates: readonly CliCandidate[], kind: CliCandidate["kind"]): CliCandidate[] {
  return candidates.filter((c) => c.kind === kind);
}

/** native 스크립트 확장자 — interpreter prefix로 실행되는 대상 후보. */
const SCRIPT_EXTENSIONS = [".js", ".mjs", ".cjs"] as const;

export interface InterpreterSuggestion {
  /** interpreter 뒤에 붙일 스크립트 argv prefix(절대 경로). */
  scriptArgvPrefix: string[];
  /** 사용할 native 실행 파일 종류 안내(경로는 사용자가 지정). */
  executableHint: string;
  reason: string;
}

/**
 * Windows shim 후보에 대한 interpreter 형태 제안(02 §3).
 * - shim → .js/.mjs/.cjs target: node interpreter + 스크립트 경로 prefix.
 * - shim → native exe target: interpreter가 아니라 그 exe를 program으로
 *   직접 등록하면 된다(제안 없음 — UI가 native 경로를 제시).
 */
export function interpreterSuggestionFor(
  candidate: CliCandidate,
  platform: PlatformFlag,
): InterpreterSuggestion | null {
  if (platform !== "windows" || !isWindowsShellShim(candidate.program)) return null;
  const target = candidate.resolvedTarget;
  if (target && (SCRIPT_EXTENSIONS as readonly string[]).includes(fileExtension(target))) {
    return {
      scriptArgvPrefix: [target],
      executableHint: "node.exe",
      reason: t("profile.discovery.shimReason", { ext: fileExtension(candidate.program).slice(1) }),
    };
  }
  return null;
}

/** 후보 한 줄 표시: 경로 · 설치 형태 · symlink target(있으면). */
export function candidateSummary(candidate: CliCandidate): string {
  const parts = [candidate.program];
  if (candidate.installForm) parts.push(t("profile.candidate.install", { form: candidate.installForm }));
  if (candidate.resolvedTarget && candidate.resolvedTarget !== candidate.program) {
    parts.push(t("profile.candidate.target", { target: candidate.resolvedTarget }));
  }
  return parts.join(" · ");
}
