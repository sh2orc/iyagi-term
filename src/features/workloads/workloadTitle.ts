/**
 * 관리 작업 목록에 보이는 이름(04-ui §1).
 *
 * 데몬의 WorkloadSummary.title은 실행 시점에 정해진 값이라 셸·에이전트가
 * 터미널 제목(OSC 0/2)을 바꿔도 따라가지 않는다. 이 작업에 붙은 pane이
 * 터미널에서 받은 최신 제목이 있으면 그것을, 창을 닫아 pane이 없으면 그
 * 작업이 마지막으로 보고한 제목(store.workloadMemory)을, 둘 다 없으면(대기
 * 중·아직 제목 보고 전) 실행 제목을 쓴다. `exit`처럼 셸을 끝낸 명령이 남긴
 * 제목은 이름으로 쓰지 않는다(isShellExitTitle) — pane이 아직 남아 있어도 닫은
 * 뒤와 같은 이름이 보이게.
 */

import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { PaneMeta } from "../../store/workbenchStore";
import { isShellExitTitle, rememberedWorkload, type WorkloadMemory } from "../../store/workloadMemory";
import { terminalDisplayTitle } from "../terminal/shellEnvironment";

export function workloadListTitle(
  workload: Pick<WorkloadSummary, "workload_id" | "session_id" | "title">,
  panes: Record<string, PaneMeta>,
  memory: WorkloadMemory,
): string {
  // session_id는 생략될 수 있다(대기 중) — 없는 값끼리 짝짓지 않는다.
  const sessionId = workload.session_id ?? null;
  for (const pane of Object.values(panes)) {
    const sameWorkload =
      pane.workloadId === workload.workload_id || (sessionId !== null && pane.sessionId === sessionId);
    if (sameWorkload && pane.terminalTitle && !isShellExitTitle(pane.terminalTitle)) {
      return terminalDisplayTitle(pane.terminalTitle);
    }
  }
  const remembered = rememberedWorkload(memory, workload.workload_id)?.title;
  return terminalDisplayTitle(remembered ?? workload.title);
}

/**
 * 끝난 작업 이름 앞에 붙일 에이전트(claude/codex/opencode): 그 터미널에서
 * 마지막으로 감지한 에이전트. 에이전트가 먼저 끝나 요약의 감지값이 비어도
 * 기억한 값을 쓴다. 에이전트를 실행한 적이 없으면 null.
 */
export function workloadListAgent(
  workload: Pick<WorkloadSummary, "workload_id" | "agent">,
  memory: WorkloadMemory,
): string | null {
  return rememberedWorkload(memory, workload.workload_id)?.agent ?? workload.agent?.agent ?? null;
}
