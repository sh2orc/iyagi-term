/**
 * 터미널 메모리 진단(W1-11 매트릭스의 프론트 측 데이터 소스).
 *
 * pane별 살아 있는 터미널 수·저널 사용량·scrollback 설정값을 모아
 * "32세션 × scrollback × 재생" 매트릭스의 실측 근거로 쓴다.
 * 측정은 JS 힙(useMemoryInfo) — webview에서만 유효하고 node 시험은
 * 구조만 검증한다.
 */

import type { WorkbenchState } from "../../store/workbenchStore";
import type { PreferenceValues } from "../../store/preferences";

export interface PaneMemoryEntry {
  leafId: string;
  sessionId: string | null;
  phase: string;
  /** 표시용 제목. */
  title: string;
}

export interface MemoryDiagnostics {
  liveTerminals: number;
  totalPanes: number;
  scrollbackLines: number;
  /** 이론 상한: liveTerminals × scrollbackLines × 평균 줄 폭(추정 120바이트). */
  scrollbackBudgetBytes: number;
  panes: PaneMemoryEntry[];
  /** performance.memory — Chromium webview에서만 존재. */
  jsHeap: { usedBytes: number; totalBytes: number; limitBytes: number } | null;
}

interface MemoryInfoLike {
  usedJSHeapSize: number;
  totalJSHeapSize: number;
  jsHeapSizeLimit: number;
}

export function collectMemoryDiagnostics(
  state: Pick<WorkbenchState, "panes">,
  prefs: Pick<PreferenceValues, "scrollbackLines">,
): MemoryDiagnostics {
  const panes = Object.values(state.panes);
  const live = panes.filter((p) => p.phase === "live" || p.phase === "replaying");
  const scrollbackLines = prefs.scrollbackLines;
  // xterm 버퍼의 실메모리는 줄 폭·attribute에 따라 변한다 — 평균 120바이트는
  // 합리적 추정치이며 실측은 jsHeap으로 보강한다(§8 기록).
  const avgLineBytes = 120;
  const perf = (
    globalThis as { performance?: { memory?: MemoryInfoLike } }
  ).performance?.memory;
  return {
    liveTerminals: live.length,
    totalPanes: panes.length,
    scrollbackLines,
    scrollbackBudgetBytes: live.length * scrollbackLines * avgLineBytes,
    panes: panes.map((p) => ({
      leafId: p.leafId,
      sessionId: p.sessionId,
      phase: p.phase,
      title: p.title,
    })),
    jsHeap: perf
      ? {
          usedBytes: perf.usedJSHeapSize,
          totalBytes: perf.totalJSHeapSize,
          limitBytes: perf.jsHeapSizeLimit,
        }
      : null,
  };
}

/** 진단을 클립보드용 텍스트로 직렬화 — 사용자가 이슈에 붙일 수 있게. */
export function formatMemoryDiagnostics(d: MemoryDiagnostics): string {
  const lines = [
    `iyagi memory diagnostics`,
    `  live terminals: ${d.liveTerminals} / ${d.totalPanes} panes`,
    `  scrollback: ${d.scrollbackLines} lines/pane`,
    `  scrollback budget (est): ${(d.scrollbackBudgetBytes / 1024 / 1024).toFixed(1)} MiB`,
    d.jsHeap
      ? `  JS heap: ${(d.jsHeap.usedBytes / 1024 / 1024).toFixed(1)} / ${(d.jsHeap.totalBytes / 1024 / 1024).toFixed(1)} MiB (limit ${(d.jsHeap.limitBytes / 1024 / 1024).toFixed(0)} MiB)`
      : `  JS heap: not available (non-Chromium webview)`,
    `  panes:`,
  ];
  for (const p of d.panes) {
    lines.push(`    ${p.leafId.slice(0, 8)} [${p.phase}] ${p.title}`);
  }
  return lines.join("\n");
}
