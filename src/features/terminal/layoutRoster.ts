/**
 * 배치 편집의 "실행 중인 터미널" 목록(오른쪽 패널, 04-ui §2-6) — 순수 함수.
 *
 * 배치됨: 탭(그룹)에 붙어 있는 터미널 pane, 탭 순서·화면 순서대로. 끌면 그 pane이 원래
 * 자리에서 빠져 새 자리로 옮겨진다.
 * 배치 안 됨: 데몬에서는 실행 중인데 어느 pane에도 붙어 있지 않은 세션(창만 닫기·그룹 삭제로
 * 떨어져 나간 터미널). 끌면 그 자리에 새로 붙인다. 세션당 화면은 하나라 두 목록에 같은
 * 터미널이 겹치지 않는다.
 */

import type { AgentStatus } from "../../generated/AgentStatus";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { WorkbenchState } from "../../store/workbenchStore";
import type { PanePhase } from "../monitor/statusStrings";
import { workloadListTitle } from "../workloads/workloadTitle";
import { layoutTabName } from "./layoutDrag";
import { terminalDisplayTitle } from "./shellEnvironment";
import { listLeaves } from "./splitTree";

export interface RosterPlaced {
  leafId: string;
  tabId: string;
  /** 그 pane이 들어 있는 그룹(탭) 이름. */
  tabTitle: string;
  title: string;
  cwd: string | null;
  phase: PanePhase;
  agent: AgentStatus | null;
}

export interface RosterUnplaced {
  sessionId: string;
  workloadId: string;
  title: string;
  cwd: string;
  state: WorkloadSummary["state"];
  agent: AgentStatus | null;
}

export interface LayoutRoster {
  placed: RosterPlaced[];
  unplaced: RosterUnplaced[];
}

/** 붙일 화면이 있는 작업 상태 — 대기 중(세션 없음)·끝난 작업은 배치 안 됨에 올리지 않는다. */
const ATTACHABLE_STATES: ReadonlySet<WorkloadSummary["state"]> = new Set(["STARTING", "RUNNING"]);

export function layoutRoster(
  state: Pick<WorkbenchState, "tabs" | "panes" | "workloads" | "workloadMemory">,
): LayoutRoster {
  const attachedSessions = new Set<string>();
  const attachedWorkloads = new Set<string>();
  for (const pane of Object.values(state.panes)) {
    if (pane.sessionId) attachedSessions.add(pane.sessionId);
    if (pane.workloadId) attachedWorkloads.add(pane.workloadId);
  }

  const placed: RosterPlaced[] = [];
  for (const tab of state.tabs) {
    if (tab.kind !== "terminal") continue;
    const tabTitle = layoutTabName(state.tabs, tab.id);
    for (const leaf of listLeaves(tab.root)) {
      const pane = state.panes[leaf.id];
      if (!pane) continue;
      placed.push({
        leafId: leaf.id,
        tabId: tab.id,
        tabTitle,
        title: terminalDisplayTitle(pane.title),
        cwd: pane.cwd,
        phase: pane.phase,
        agent: pane.agent ?? null,
      });
    }
  }

  const unplaced: RosterUnplaced[] = [];
  for (const workload of state.workloads) {
    const sessionId = workload.session_id ?? null;
    if (!sessionId || !ATTACHABLE_STATES.has(workload.state)) continue;
    if (attachedSessions.has(sessionId) || attachedWorkloads.has(workload.workload_id)) continue;
    unplaced.push({
      sessionId,
      workloadId: workload.workload_id,
      // 창만 닫은 터미널은 pane이 없다 — 마지막으로 보고한 제목을 쓴다.
      title: workloadListTitle(workload, state.panes, state.workloadMemory),
      cwd: workload.cwd,
      state: workload.state,
      agent: workload.agent ?? null,
    });
  }
  return { placed, unplaced };
}
