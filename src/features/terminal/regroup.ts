/**
 * 탭 재그룹핑 플래너(04-ui.md §2-5) — 순수 함수.
 *
 * "프로젝트별로 다시 묶기"는 두 단계다: `planRegroup`이 현재 탭·pane에서
 * 결과 배치(어떤 pane이 어느 탭으로 가는지)를 계산하고, 사용자가 미리보기를
 * 확인하면 `applyRegroupPlan`이 그 계획을 새 탭 배열로 만든다. 둘 다 세션·
 * 뷰·pane 메타에는 손대지 않는다 — leaf 노드(view_id·session_id)를 그대로
 * 옮길 뿐이라 xterm 인스턴스와 PTY는 탭 전환 때와 같은 경로로 따라온다.
 *
 * 프로젝트 키는 pane의 cwd(셸의 현재 작업 디렉터리 — OSC 7로 보고된다;
 * claude/codex 같은 에이전트 pane이면 그 에이전트를 시작한 디렉터리)다.
 * 두 pane은 cwd가 같을 때만 같은 프로젝트다. 이미 한 프로젝트만 담고 있는
 * 탭은 id·이름을 그대로 둔다 — 배치가 이미 새 격자(gridLayout, 위아래
 * 두 줄) 모양이면 **같은 객체 그대로**(비율까지 보존, 다시 그리지 않음)
 * 남기고, 모양이 다르면(예: 한 줄로 늘어선 옛 배치) id·이름은 남긴 채
 * leaf의 현재 시각 순서로 트리만 새로 짠다("배치만 정리"). 섞이거나
 * 흩어진 그룹만 완전히 새 탭이 되고, 이름은 프로젝트 경로의 마지막
 * 조각이다.
 *
 * mission/agent-view 탭(05-ui §2)에는 pane이 없다 — 빈 탭과 같이 자기
 * 자리에 그대로 남고, pane을 받거나 다른 탭과 묶이지 않는다.
 */

import type { PaneMeta, TabState } from "../../store/workbenchStore";
import { findLeaf, gridLayout, layoutShape, listLeaves, MAX_PANES_PER_TAB, nearestLeaf, type SplitNode } from "./splitTree";

export interface RegroupInput {
  tabs: TabState[];
  panes: Record<string, PaneMeta>;
  activeTabId: string | null;
  focusedLeafId: string | null;
}

export interface RegroupGroup {
  /** 프로젝트 키(경로). 프로젝트를 모르는 pane 묶음은 null. */
  key: string | null;
  /** 결과 탭 이름. */
  title: string;
  /** 결과 탭에 들어갈 leaf(시각 순서). */
  leafIds: string[];
  /** 이 leaf들이 원래 있던 탭들. */
  fromTabIds: string[];
  /**
   * 원래 탭 하나가 통째로 이 그룹이면 그 탭 id — id·이름은 그대로 두고,
   * 배치가 이미 격자 모양이면 탭 객체까지 손대지 않는다(`relayout` 참고).
   * 새로 만드는 탭이면 null. (빈 탭·mission 탭도 자기 자신만의 그룹으로 남는다.)
   */
  keepTabId: string | null;
  /**
   * `keepTabId`가 있는 그룹인데 배치가 새 2행 격자 모양(`gridLayout`)과
   * 달라 다시 짜야 하는가. id·이름·leaf는 그대로, 트리만 새로 만든다.
   * 이미 격자 모양인 유지 그룹과 새로 만드는 탭은 항상 false다.
   */
  relayout: boolean;
}

export interface RegroupPlan {
  groups: RegroupGroup[];
  /** 하나라도 새로 만들어지는 탭이 있는가(없으면 적용할 것이 없다). */
  changed: boolean;
  tabsBefore: number;
  tabsAfter: number;
}

export interface RegroupOptions {
  /** 탭당 pane 상한(기본 MAX_PANES_PER_TAB). 넘치는 그룹은 여러 탭으로 나눈다. */
  maxPerTab?: number;
  /** 프로젝트를 모르는 pane 묶음의 탭 이름. */
  noProjectTitle: string;
}

/**
 * 재배치가 다루는 트리: terminal 탭이면 그 root, pane이 없는 mission/
 * agent-view 탭이면 null. 재배치 코드는 `.root`에 바로 닿지 않고 이것을 거친다.
 */
export function terminalRoot(tab: TabState): SplitNode | null {
  return tab.kind === "terminal" ? tab.root : null;
}

/**
 * 뒤따르는 `/`·`\` 구분자를 지운다. `/`처럼 구분자만으로 된 경로나
 * `C:\`처럼 드라이브 루트(구분자를 지우면 `C:`가 되어 의미가 달라진다)는
 * 그대로 둔다 — 빈 문자열이 되거나 루트가 아닌 경로로 바뀌지 않는다.
 */
function stripTrailingSeparators(path: string): string {
  let end = path.length;
  while (end > 0 && (path[end - 1] === "/" || path[end - 1] === "\\")) {
    end -= 1;
  }
  if (end === 0) return path;
  if (end < path.length && /^[A-Za-z]:$/.test(path.slice(0, end))) {
    return path.slice(0, end + 1);
  }
  return path.slice(0, end);
}

/** pane의 프로젝트 키: pane의 cwd(정규화된 값). cwd가 없으면 null. */
export function paneProjectKey(pane: Pick<PaneMeta, "cwd"> | undefined): string | null {
  const cwd = pane?.cwd;
  if (!cwd) return null;
  return stripTrailingSeparators(cwd);
}

/**
 * 경로의 마지막 조각(탭 이름용). 뒤따르는 구분자는 무시하고 `/`·`\` 둘 다
 * 자른다. 루트(`/`, `C:\`)처럼 조각이 없으면 경로 그대로.
 */
export function projectTitle(key: string): string {
  const trimmed = key.replace(/[\\/]+$/, "");
  const idx = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  const last = idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
  return last.length > 0 ? last : key;
}

/** 에이전트 pane을 앞에 두는 안정 정렬(같은 종류끼리의 순서는 유지). */
function agentsFirst(leafIds: string[], panes: Record<string, PaneMeta>): string[] {
  const rank = (id: string): number => (panes[id]?.agent ? 0 : 1);
  return leafIds
    .map((id, index) => ({ id, index, rank: rank(id) }))
    .sort((a, b) => a.rank - b.rank || a.index - b.index)
    .map((entry) => entry.id);
}

export function planRegroup(input: RegroupInput, options: RegroupOptions): RegroupPlan {
  const maxPerTab = options.maxPerTab ?? MAX_PANES_PER_TAB;
  // 키 → 첫 등장 순서대로의 묶음. pane이 없는 탭(빈 terminal 탭, mission 계열
  // 탭)은 자기만의 자리(키 `tab:<id>`)를 갖는다.
  const buckets = new Map<string, { key: string | null; leafIds: string[]; fromTabIds: Set<string> }>();
  const leafTab = new Map<string, string>();
  for (const tab of input.tabs) {
    const leaves = listLeaves(terminalRoot(tab));
    if (leaves.length === 0) {
      buckets.set(`tab:${tab.id}`, { key: null, leafIds: [], fromTabIds: new Set([tab.id]) });
      continue;
    }
    for (const leaf of leaves) {
      leafTab.set(leaf.id, tab.id);
      const key = paneProjectKey(input.panes[leaf.id]);
      const bucketKey = key === null ? "none" : `project:${key}`;
      let bucket = buckets.get(bucketKey);
      if (!bucket) {
        bucket = { key, leafIds: [], fromTabIds: new Set() };
        buckets.set(bucketKey, bucket);
      }
      bucket.leafIds.push(leaf.id);
      bucket.fromTabIds.add(tab.id);
    }
  }

  const groups: RegroupGroup[] = [];
  // gridLayout 비교용 더미 id — layoutShape은 split id·비율을 보지 않으므로
  // 아무 값이나 되고, 실제 트리에는 쓰이지 않는다(순수성 유지).
  let dummyCounter = 0;
  const dummyId = () => `dummy-${dummyCounter++}`;
  for (const bucket of buckets.values()) {
    const fromTabIds = [...bucket.fromTabIds];
    // 탭 하나가 통째로 이 묶음이면(그 탭의 leaf 집합 == 묶음) id·이름은 둔다.
    const soleTab = fromTabIds.length === 1 ? input.tabs.find((t) => t.id === fromTabIds[0]) ?? null : null;
    const wholeTab =
      soleTab !== null &&
      listLeaves(terminalRoot(soleTab)).length === bucket.leafIds.length &&
      bucket.leafIds.length <= maxPerTab;
    if (wholeTab && soleTab !== null) {
      // 배치가 이미 새 격자 모양이면(비율·split id는 무시) 손대지 않는다.
      // 다르면 id·이름은 그대로, leaf의 지금 시각 순서로 트리만 새로 짠다
      // — agentsFirst로 순서를 바꾸지 않는다(사용자가 맞춰 둔 배치 존중).
      const currentRoot = terminalRoot(soleTab);
      const currentLeaves = listLeaves(currentRoot);
      const expectedShape = layoutShape(gridLayout(currentLeaves, dummyId));
      const relayout = layoutShape(currentRoot) !== expectedShape;
      groups.push({
        key: bucket.key,
        title: soleTab.title,
        leafIds: bucket.leafIds,
        fromTabIds,
        keepTabId: soleTab.id,
        relayout,
      });
      continue;
    }
    const ordered = agentsFirst(bucket.leafIds, input.panes);
    const baseTitle = bucket.key === null ? options.noProjectTitle : projectTitle(bucket.key);
    const chunks = Math.max(1, Math.ceil(ordered.length / maxPerTab));
    for (let i = 0; i < chunks; i += 1) {
      const leafIds = ordered.slice(i * maxPerTab, (i + 1) * maxPerTab);
      groups.push({
        key: bucket.key,
        title: chunks === 1 ? baseTitle : `${baseTitle} ${i + 1}`,
        leafIds,
        fromTabIds: [...new Set(leafIds.map((id) => leafTab.get(id)!))],
        keepTabId: null,
        relayout: false,
      });
    }
  }

  return {
    groups,
    changed: groups.some((g) => g.keepTabId === null || g.relayout),
    tabsBefore: input.tabs.length,
    tabsAfter: groups.length,
  };
}

export interface RegroupResult {
  tabs: TabState[];
  activeTabId: string | null;
  focusedLeafId: string | null;
}

/**
 * 계획을 새 탭 배열로 만든다. 유지 그룹은 기존 TabState 객체 그대로(단,
 * `relayout`이면 id·이름은 두고 leaf를 `group.leafIds` 순서로 다시 격자에
 * 짜 넣는다), 새 그룹은 leaf 노드를 옮겨 격자(gridLayout)로 배치한다.
 * 초점 leaf는 그대로 두고 활성 탭은 그 leaf가 들어간 탭이다 — 사용자가
 * 보던 터미널이 계속 보인다.
 *
 * 계획이 지금 상태와 맞지 않으면(leaf가 사라짐 등) null — 호출자는 다시
 * 계산하거나 조용히 포기한다. 부분 적용은 없다.
 */
export function applyRegroupPlan(input: RegroupInput, plan: RegroupPlan, makeId: () => string): RegroupResult | null {
  const tabById = new Map(input.tabs.map((t) => [t.id, t]));
  const tabs: TabState[] = [];
  for (const group of plan.groups) {
    if (group.keepTabId !== null) {
      const kept = tabById.get(group.keepTabId);
      if (!kept) return null;
      if (!group.relayout) {
        tabs.push(kept);
        continue;
      }
      // 배치만 다시 짠다 — 실제로는 항상 terminal 탭이다(relayout은
      // leaf가 있는 탭에서만 계산된다). kind가 아니면 계획이 낡은 것.
      if (kept.kind !== "terminal") return null;
      const leaves: SplitNode[] = [];
      for (const leafId of group.leafIds) {
        const found = findLeaf(kept.root, leafId);
        if (!found) return null;
        leaves.push(found);
      }
      tabs.push({ ...kept, root: gridLayout(leaves, makeId) });
      continue;
    }
    const leaves: SplitNode[] = [];
    for (const leafId of group.leafIds) {
      let found: SplitNode | null = null;
      for (const tab of input.tabs) {
        found = findLeaf(terminalRoot(tab), leafId);
        if (found) break;
      }
      if (!found) return null;
      leaves.push(found);
    }
    tabs.push({ kind: "terminal", id: makeId(), title: group.title, root: gridLayout(leaves, makeId) });
  }
  const focused = input.focusedLeafId;
  const focusedTab = focused ? tabs.find((t) => findLeaf(terminalRoot(t), focused)) ?? null : null;
  // 초점 leaf가 없으면(빈 탭·mission 탭을 보고 있었음) 이전 활성 탭이 흘러간
  // 첫 그룹. tabs는 plan.groups와 같은 순서로 만들어졌으므로 인덱스가 곧 대응이다.
  const activeIndex = plan.groups.findIndex((g) => g.fromTabIds.includes(input.activeTabId ?? ""));
  const activeTab = focusedTab ?? (activeIndex >= 0 ? tabs[activeIndex] : null) ?? tabs[0] ?? null;
  const activeRoot = activeTab ? terminalRoot(activeTab) : null;
  const focusedLeafId = focusedTab ? focused : activeRoot ? nearestLeaf(activeRoot, "start") : null;
  return { tabs, activeTabId: activeTab?.id ?? null, focusedLeafId };
}
