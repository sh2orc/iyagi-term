/**
 * 상태바의 에이전트 요약: 터미널마다 자동 감지한 에이전트(claude·codex·
 * opencode, GLM으로 도는 Claude는 Z.ai)를 종류별로 세고, 그중 작업 중·확인
 * 대기인 수를 함께 센다. 탭 마커(TabItem)와 같은 판정이다 — 살아 있는 pane만,
 * 활동은 자체 보고가 있으면 그것을, 없으면 최근 출력으로.
 */

import type { PaneMeta } from "../../store/workbenchStore";
import { agentDisplayId, agentDisplayName, resolveAgentActivity } from "../terminal/agentNames";

/** 표시 순서. 모르는 에이전트는 이 뒤에 이름순으로 붙는다. */
const ORDER = ["claude", "zai", "codex", "opencode"];

export interface AgentKindCount {
  /** 표시 id(agentDisplayId) — GLM 모델 Claude는 "zai". */
  id: string;
  name: string;
  count: number;
}

export interface AgentSummary {
  kinds: AgentKindCount[];
  total: number;
  working: number;
  waiting: number;
}

export function summarizeAgents(
  panes: Iterable<Pick<PaneMeta, "phase" | "agent" | "sessionId">>,
  activeSessions: ReadonlySet<string>,
): AgentSummary {
  const kinds = new Map<string, AgentKindCount>();
  let total = 0, working = 0, waiting = 0;
  for (const pane of panes) {
    if (!pane.agent || pane.phase !== "live") continue;
    const id = agentDisplayId(pane.agent.agent, pane.agent.model);
    const kind = kinds.get(id) ?? { id, name: agentDisplayName(pane.agent.agent, pane.agent.model), count: 0 };
    kind.count += 1;
    kinds.set(id, kind);
    total += 1;
    const activity = resolveAgentActivity(
      pane.agent,
      typeof pane.sessionId === "string" && activeSessions.has(pane.sessionId),
    );
    if (activity === "working") working += 1;
    else if (activity === "waiting") waiting += 1;
  }
  const rank = (id: string): number => {
    const index = ORDER.indexOf(id);
    return index === -1 ? ORDER.length : index;
  };
  return {
    kinds: [...kinds.values()].sort((a, b) => rank(a.id) - rank(b.id) || a.name.localeCompare(b.name)),
    total,
    working,
    waiting,
  };
}
