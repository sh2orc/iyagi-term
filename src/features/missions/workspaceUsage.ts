/**
 * 작업 공간 정리(계약 D `workspace.usage`/`workspace.cleanup`) 공용 순수 도구.
 *
 * - 크기 표기(B/KB/MB/GB/TB, 1024 단위) — 목록·결과 화면·보관 토스트가 같은 표기를 쓴다.
 * - 정리 가능 판정과 blocked_reason/kept.reason → 짧은 사람 문구 키.
 *   모르는 slug는 원문 대신 일반 문구로 떨어진다.
 *
 * 자동 삭제는 없다 — 사용자가 누를 때만 정리한다.
 */

import type { WorkspaceUsageEntry } from "../../generated/WorkspaceUsageEntry";
import type { WorkspaceUsageResult } from "../../generated/WorkspaceUsageResult";

const UNITS = ["KB", "MB", "GB", "TB"] as const;

/** U64 wire string/number → 바이트 수(표기용 — 정밀도 손실은 무해). 읽을 수 없으면 0. */
export function storageBytes(value: string | number | null | undefined): number {
  const parsed = typeof value === "number" ? value : Number(value ?? 0);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
}

/** 저장 공간 크기: `512 B`, `3.2 KB`, `1.5 MB`, `2 GB`(1024 단위, 소수 한 자리). */
export function formatStorageSize(value: string | number | null | undefined): string {
  const bytes = storageBytes(value);
  if (bytes < 1024) return `${Math.floor(bytes)} B`;
  let size = bytes / 1024;
  let unit = 0;
  const round = (n: number) => Math.round(n * 10) / 10;
  // 반올림 결과가 1024가 되면 다음 단위로 올린다(1023.96 KB → 1 MB).
  while (round(size) >= 1024 && unit < UNITS.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const rounded = round(size);
  return `${Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1)} ${UNITS[unit]}`;
}

/** 사용량 결과에서 한 작업의 항목. */
export function workspaceUsageEntry(usage: WorkspaceUsageResult | null, missionId: string): WorkspaceUsageEntry | null {
  return usage?.missions.find((entry) => entry.mission_id === missionId) ?? null;
}

/**
 * 지울 것이 있는 정리 가능 항목인가(데몬 cleanable + 작업 공간 또는 용량이 남아 있음).
 * 판별 함수(`entry is …`)로 두면 false 쪽에서 entry가 never로 좁혀져 뒤따르는 검사가
 * 깨진다 — boolean만 돌려주고 호출부가 `entry !== null &&`로 좁힌다.
 */
export function workspaceCleanable(entry: WorkspaceUsageEntry | null): boolean {
  return entry !== null && entry.cleanable && (entry.workspaces > 0 || storageBytes(entry.bytes) > 0);
}

/** 정리할 것이 남아 있으나 지금은 막힌 항목인가. */
export function workspaceCleanupBlocked(entry: WorkspaceUsageEntry | null): boolean {
  return entry !== null && !entry.cleanable && entry.blocked_reason !== null
    && (entry.workspaces > 0 || storageBytes(entry.bytes) > 0);
}

const BLOCKED_KEYS: Record<string, string> = {
  mission_active: "missions.workspaceCleanup.blocked.missionActive",
  run_unreconciled: "missions.workspaceCleanup.blocked.runUnreconciled",
};

/** blocked_reason → 짧은 이유 문구 키. */
export function workspaceBlockedReasonKey(reason: string | null): string {
  return (reason !== null && BLOCKED_KEYS[reason]) || "missions.workspaceCleanup.blocked.other";
}

/** 데몬 workspace_cleanup.rs `keep(...)` slug 전체. */
const KEPT_KEYS: Record<string, string> = {
  dirty: "missions.workspaceCleanup.kept.dirty",
  run_active: "missions.workspaceCleanup.kept.inUse",
  // 결과 확인이 필요한 격리 작업 공간(종료 미확인 실행·통합 복구 보존).
  quarantined: "missions.workspaceCleanup.kept.quarantined",
  not_daemon_owned: "missions.workspaceCleanup.kept.notOwned",
  unregistered_worktree: "missions.workspaceCleanup.kept.notOwned",
  repository_unavailable: "missions.workspaceCleanup.kept.repositoryUnavailable",
  status_unavailable: "missions.workspaceCleanup.kept.statusUnavailable",
  deferred: "missions.workspaceCleanup.kept.deferred",
  remove_failed: "missions.workspaceCleanup.kept.other",
};

/** 정리에서 남긴 항목의 reason → 사람 문구 키. */
export function workspaceKeptReasonKey(reason: string): string {
  return KEPT_KEYS[reason] ?? "missions.workspaceCleanup.kept.other";
}
