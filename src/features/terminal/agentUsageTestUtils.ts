/**
 * agentUsage 테스트용 최소 도구 — splitTree 잎을 얹은 탭 루트와 얇은 pane.
 * (splitTree 자체의 테스트가 아니므로 여기서 만든다.)
 */

import { makeLeaf, type SplitNode } from "./splitTree";
import type { AgentStatus } from "../../generated/AgentStatus";

export interface PaneLike {
  agent?: AgentStatus | null;
}

/** 잎들을 가로로 잇는 탭 루트. 잎이 하나면 그 잎이 루트다. */
export function makeRoot(leafIds: string[]): SplitNode {
  if (leafIds.length === 0) throw new Error("at least one leaf");
  return leafIds.slice(1).reduce<SplitNode>(
    (first, id) => ({
      kind: "split",
      id: `split-${id}`,
      axis: "row",
      ratio: 0.5,
      first,
      second: makeLeaf(id, `view-${id}`),
    }),
    makeLeaf(leafIds[0], `view-${leafIds[0]}`),
  );
}
