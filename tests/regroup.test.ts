/**
 * 탭 재그룹핑 플래너(04-ui.md §2-5) — 순수 함수 계약.
 *
 * - 프로젝트 키는 pane의 cwd다(git 최상위 경로는 보지 않는다), 첫 등장 순서로 묶는다.
 * - 이미 한 프로젝트만 담은 탭은 배치가 이미 2행 격자 모양이면 같은 객체
 *   그대로(id·이름·비율 보존), 모양이 다르면 id·이름만 두고 트리를 다시
 *   짠다(`relayout: true`) — layoutShape로 비교한다(비율·split id는 무시).
 * - 바뀌는 그룹만 새 탭, 이름은 경로의 마지막 조각, 에이전트 pane이 앞.
 * - 상한(8)을 넘는 그룹은 여러 탭으로 나눈다.
 * - 초점 leaf는 살아남고 활성 탭은 그 leaf가 들어간 탭이다.
 */

import { describe, expect, it } from "vitest";
import { applyRegroupPlan, paneProjectKey, planRegroup, projectTitle } from "../src/features/terminal/regroup";
import { gridLayout, layoutShape, leafCount, listLeaves, makeLeaf, split, type SplitNode } from "../src/features/terminal/splitTree";
import type { PaneMeta, TabState } from "../src/store/workbenchStore";

function pane(leafId: string, extra: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId,
    viewId: `v-${leafId}`,
    sessionId: `s-${leafId}`,
    workloadId: `w-${leafId}`,
    title: leafId,
    cwd: null,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    ...extra,
  };
}

function rowOf(ids: string[]): SplitNode {
  let root: SplitNode = makeLeaf(ids[0], `v-${ids[0]}`, `s-${ids[0]}`);
  for (let i = 1; i < ids.length; i += 1) {
    root = split(root, ids[i - 1], { id: ids[i], view_id: `v-${ids[i]}`, session_id: `s-${ids[i]}` }, { splitId: `sp-${i}`, axis: "row" })!;
  }
  return root;
}

/** 이미 2행 격자(gridLayout) 모양인 트리 — relayout이 걸리지 않는 "그대로" 고정값. */
function gridOf(ids: string[]): SplitNode {
  const leaves = ids.map((id) => makeLeaf(id, `v-${id}`, `s-${id}`));
  let n = 0;
  return gridLayout(leaves, () => `g-${++n}`)!;
}

let counter = 0;
const makeId = () => `id-${++counter}`;
const opts = { noProjectTitle: "기타" };

describe("projectTitle / paneProjectKey", () => {
  it("takes the last path segment on both separators and ignores trailing slashes", () => {
    expect(projectTitle("/Users/me/project/iyagi")).toBe("iyagi");
    expect(projectTitle("/Users/me/project/iyagi/")).toBe("iyagi");
    expect(projectTitle("C:\\work\\api\\")).toBe("api");
    expect(projectTitle("/")).toBe("/");
  });

  it("uses only cwd and ignores project entirely, yielding null when cwd is unknown", () => {
    expect(paneProjectKey(pane("a", { cwd: "/repo/sub", project: "/repo" }))).toBe("/repo/sub");
    expect(paneProjectKey(pane("a", { cwd: "/repo/sub" }))).toBe("/repo/sub");
    expect(paneProjectKey(pane("a", { project: "/repo" }))).toBeNull(); // cwd가 없으면 project를 보지 않는다
    expect(paneProjectKey(pane("a"))).toBeNull();
    expect(paneProjectKey(undefined)).toBeNull();
  });

  it("normalizes trailing separators so /a/b and /a/b/ are the same key, but keeps bare roots intact", () => {
    expect(paneProjectKey(pane("a", { cwd: "/a/b/" }))).toBe("/a/b");
    expect(paneProjectKey(pane("a", { cwd: "/a/b/" }))).toBe(paneProjectKey(pane("b", { cwd: "/a/b" })));
    expect(paneProjectKey(pane("a", { cwd: "/" }))).toBe("/");
    expect(paneProjectKey(pane("a", { cwd: "C:\\" }))).toBe("C:\\");
    expect(paneProjectKey(pane("a", { cwd: "" }))).toBeNull();
  });
});

describe("planRegroup", () => {
  it("leaves a tab alone when every pane in it already belongs to one project and the layout is already the grid", () => {
    const tabs: TabState[] = [
      // 이미 2행 격자 모양(gridOf) — 그래야 손대지 않는 "그대로" 경로를 탄다.
      { kind: "terminal", id: "t1", title: "배포", root: gridOf(["a", "b"]) },
      { kind: "terminal", id: "t2", title: "탭 2", root: rowOf(["c"]) },
    ];
    const panes = {
      a: pane("a", { cwd: "/repo/x", project: "/repo/other" }), // project는 무시된다
      b: pane("b", { cwd: "/repo/x" }),
      c: pane("c", { cwd: "/repo/y" }),
    };
    const plan = planRegroup({ tabs, panes, activeTabId: "t1", focusedLeafId: "a" }, opts);
    expect(plan.changed).toBe(false);
    expect(plan.groups.map((g) => g.keepTabId)).toEqual(["t1", "t2"]);
    expect(plan.groups.every((g) => g.relayout === false)).toBe(true);
    expect(plan.tabsBefore).toBe(2);
    expect(plan.tabsAfter).toBe(2);
  });

  it("gathers panes of the same project from several tabs and splits mixed tabs, in first-seen order", () => {
    const tabs: TabState[] = [
      { kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["a", "b"]) }, // x, y
      { kind: "terminal", id: "t2", title: "탭 2", root: rowOf(["c"]) }, // x
      { kind: "terminal", id: "t3", title: "탭 3", root: rowOf(["d"]) }, // no cwd
    ];
    const panes = {
      a: pane("a", { cwd: "/repo/x" }),
      b: pane("b", { cwd: "/repo/y" }),
      c: pane("c", { cwd: "/repo/x" }),
      d: pane("d"),
    };
    const plan = planRegroup({ tabs, panes, activeTabId: "t1", focusedLeafId: "b" }, opts);
    expect(plan.changed).toBe(true);
    expect(plan.groups.map((g) => [g.title, g.leafIds, g.keepTabId])).toEqual([
      ["x", ["a", "c"], null],
      ["y", ["b"], null],
      ["탭 3", ["d"], "t3"], // 프로젝트를 몰라도 탭이 통째로 한 묶음이면 그대로
    ]);
    expect(plan.groups[0].fromTabIds).toEqual(["t1", "t2"]);
  });

  it("names a mixed no-project group with the fallback title", () => {
    const tabs: TabState[] = [{ kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["a", "b"]) }];
    const panes = { a: pane("a"), b: pane("b", { cwd: "/repo/x" }) };
    const plan = planRegroup({ tabs, panes, activeTabId: "t1", focusedLeafId: null }, opts);
    expect(plan.groups.map((g) => g.title)).toEqual(["기타", "x"]);
  });

  it("puts agent panes first inside a new group and keeps the rest in visual order", () => {
    const tabs: TabState[] = [
      { kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["shell1", "claude"]) },
      { kind: "terminal", id: "t2", title: "탭 2", root: rowOf(["shell2", "codex"]) },
    ];
    const agent = (id: string) => ({ agent: id, pid: 1, detected_at_ms: 0 });
    const panes = {
      shell1: pane("shell1", { cwd: "/p" }),
      claude: pane("claude", { cwd: "/p", agent: agent("claude") }),
      shell2: pane("shell2", { cwd: "/p" }),
      codex: pane("codex", { cwd: "/p", agent: agent("codex") }),
    };
    const plan = planRegroup({ tabs, panes, activeTabId: "t1", focusedLeafId: null }, opts);
    expect(plan.groups).toHaveLength(1);
    expect(plan.groups[0].leafIds).toEqual(["claude", "codex", "shell1", "shell2"]);
  });

  it("splits a project with more panes than the cap across numbered tabs", () => {
    const ids = Array.from({ length: 10 }, (_, i) => `p${i}`);
    const tabs: TabState[] = [
      { kind: "terminal", id: "t1", title: "탭 1", root: rowOf(ids.slice(0, 5)) },
      { kind: "terminal", id: "t2", title: "탭 2", root: rowOf(ids.slice(5)) },
    ];
    const panes = Object.fromEntries(ids.map((id) => [id, pane(id, { cwd: "/big" })]));
    const plan = planRegroup({ tabs, panes, activeTabId: "t1", focusedLeafId: null }, { ...opts, maxPerTab: 8 });
    expect(plan.groups.map((g) => [g.title, g.leafIds.length])).toEqual([
      ["big 1", 8],
      ["big 2", 2],
    ]);
  });

  it("keeps empty tabs where they are as their own untouched group", () => {
    const tabs: TabState[] = [
      { kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["a"]) },
      { kind: "terminal", id: "empty", title: "빈 탭", root: null },
      { kind: "terminal", id: "t3", title: "탭 3", root: rowOf(["b"]) },
    ];
    const panes = { a: pane("a", { cwd: "/p" }), b: pane("b", { cwd: "/p" }) };
    const plan = planRegroup({ tabs, panes, activeTabId: "empty", focusedLeafId: null }, opts);
    expect(plan.groups.map((g) => [g.title, g.keepTabId])).toEqual([
      ["p", null],
      ["빈 탭", "empty"],
    ]);
  });

  it("treats trailing separators as the same cwd, so /a/b/ and /a/b group together", () => {
    const tabs: TabState[] = [
      { kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["a"]) },
      { kind: "terminal", id: "t2", title: "탭 2", root: rowOf(["b"]) },
    ];
    const panes = { a: pane("a", { cwd: "/a/b/" }), b: pane("b", { cwd: "/a/b" }) };
    const plan = planRegroup({ tabs, panes, activeTabId: "t1", focusedLeafId: null }, opts);
    expect(plan.groups.map((g) => [g.title, g.leafIds])).toEqual([["b", ["a", "b"]]]);
  });
});

describe("applyRegroupPlan", () => {
  it("reuses untouched TabState objects and rebuilds only changed groups as grids with the same leaf nodes", () => {
    // 이미 2행 격자 모양이어야 손대지 않는다(relayout이면 새 객체가 된다).
    const keptRoot = gridOf(["k1", "k2"]);
    const tabs: TabState[] = [
      { kind: "terminal", id: "kept", title: "내 이름", root: keptRoot },
      { kind: "terminal", id: "t2", title: "탭 2", root: rowOf(["a", "b", "c"]) }, // x, y, x
      { kind: "terminal", id: "t3", title: "탭 3", root: rowOf(["d"]) }, // y
    ];
    const panes = {
      k1: pane("k1", { cwd: "/repo/k" }),
      k2: pane("k2", { cwd: "/repo/k" }),
      a: pane("a", { cwd: "/repo/x" }),
      b: pane("b", { cwd: "/repo/y" }),
      c: pane("c", { cwd: "/repo/x" }),
      d: pane("d", { cwd: "/repo/y" }),
    };
    const input = { tabs, panes, activeTabId: "t2", focusedLeafId: "b" };
    const plan = planRegroup(input, opts);
    const result = applyRegroupPlan(input, plan, makeId);
    expect(result).not.toBeNull();
    if (!result) return;
    expect(result.tabs).toHaveLength(3);
    // 유지 탭은 같은 객체(비율·이름·id 모두 보존, 다시 그리지 않는다).
    expect(result.tabs[0]).toBe(tabs[0]);
    expect(result.tabs[1].title).toBe("x");
    expect(result.tabs[2].title).toBe("y");
    // leaf 노드(view_id·session_id)는 이동만 한다.
    const originalLeaves = new Map(tabs.flatMap((t) => listLeaves(t.root)).map((l) => [l.id, l]));
    for (const tab of result.tabs.slice(1)) {
      for (const leaf of listLeaves(tab.root)) expect(leaf).toBe(originalLeaves.get(leaf.id));
    }
    expect(listLeaves(result.tabs[1].root).map((l) => l.id)).toEqual(["a", "c"]);
    expect(listLeaves(result.tabs[2].root).map((l) => l.id)).toEqual(["b", "d"]);
    // 초점 leaf는 그대로, 활성 탭은 그 leaf가 들어간 탭.
    expect(result.focusedLeafId).toBe("b");
    expect(result.activeTabId).toBe(result.tabs[2].id);
  });

  it("falls back to the tab that absorbed the previous active tab when nothing was focused", () => {
    const tabs: TabState[] = [
      { kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["a"]) },
      { kind: "terminal", id: "empty", title: "빈", root: null },
      { kind: "terminal", id: "t3", title: "탭 3", root: rowOf(["b"]) },
    ];
    const panes = { a: pane("a", { cwd: "/p" }), b: pane("b", { cwd: "/p" }) };
    const input = { tabs, panes, activeTabId: "t3", focusedLeafId: null };
    const plan = planRegroup(input, opts);
    const result = applyRegroupPlan(input, plan, makeId)!;
    expect(result.tabs.map((t) => t.title)).toEqual(["p", "빈"]);
    expect(result.activeTabId).toBe(result.tabs[0].id);
    expect(result.focusedLeafId).toBe("a");
    expect(leafCount(result.tabs[0].root)).toBe(2);
  });

  it("refuses a plan that names a leaf which no longer exists (no partial apply)", () => {
    const tabs: TabState[] = [{ kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["a", "b"]) }];
    const panes = { a: pane("a", { cwd: "/x" }), b: pane("b", { cwd: "/y" }) };
    const input = { tabs, panes, activeTabId: "t1", focusedLeafId: "a" };
    const plan = planRegroup(input, opts);
    const stale = { ...input, tabs: [{ kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["a"]) }] };
    expect(applyRegroupPlan(stale, plan, makeId)).toBeNull();
  });

  it("keeps mission tabs as untouched groups in first-appearance order — they have no panes to regroup (05 §2)", () => {
    const mission: TabState = { kind: "mission", id: "m", title: "미션", missionId: "m-1" };
    const tabs: TabState[] = [
      { kind: "terminal", id: "t1", title: "탭 1", root: rowOf(["a", "b"]) }, // x, y
      mission,
      { kind: "terminal", id: "t3", title: "탭 3", root: rowOf(["c"]) }, // x
    ];
    const panes = {
      a: pane("a", { cwd: "/repo/x" }),
      b: pane("b", { cwd: "/repo/y" }),
      c: pane("c", { cwd: "/repo/x" }),
    };
    const input = { tabs, panes, activeTabId: "m", focusedLeafId: null };
    const plan = planRegroup(input, opts);
    expect(plan.groups.map((g) => [g.title, g.keepTabId])).toEqual([
      ["x", null],
      ["y", null],
      ["미션", "m"],
    ]);
    const result = applyRegroupPlan(input, plan, makeId)!;
    expect(result.tabs[2]).toBe(mission);
    expect(result.tabs.slice(0, 2).map((t) => t.kind)).toEqual(["terminal", "terminal"]);
    // mission 탭을 보고 있었으면 그 탭이 그대로 활성이고, leaf 초점은 없다.
    expect(result.activeTabId).toBe("m");
    expect(result.focusedLeafId).toBeNull();
  });
});

describe("relayout — 배치가 격자와 다르면 id·이름은 두고 트리만 다시 짠다", () => {
  it("relays out a whole-tab group whose current layout isn't the 2-row grid yet (1×4 → 2×2)", () => {
    const tabs: TabState[] = [{ kind: "terminal", id: "solo", title: "내 탭", root: rowOf(["a", "b", "c", "d"]) }];
    const panes = {
      a: pane("a", { cwd: "/repo/x" }),
      b: pane("b", { cwd: "/repo/x" }),
      c: pane("c", { cwd: "/repo/x" }),
      d: pane("d", { cwd: "/repo/x" }),
    };
    const input = { tabs, panes, activeTabId: "solo", focusedLeafId: "a" };
    const plan = planRegroup(input, opts);
    expect(plan.changed).toBe(true);
    expect(plan.groups).toHaveLength(1);
    expect(plan.groups[0]).toMatchObject({ keepTabId: "solo", relayout: true, leafIds: ["a", "b", "c", "d"] });

    const result = applyRegroupPlan(input, plan, makeId);
    expect(result).not.toBeNull();
    if (!result) return;
    expect(result.tabs).toHaveLength(1);
    expect(result.tabs[0].id).toBe("solo"); // id 그대로
    expect(result.tabs[0].title).toBe("내 탭"); // 이름(사용자가 바꿨을 수도 있는 값) 그대로
    const root = result.tabs[0].kind === "terminal" ? result.tabs[0].root : null;
    // 시각 순서(a,b,c,d) 그대로 2×2 격자로 — agentsFirst로 순서를 바꾸지 않는다.
    expect(layoutShape(root)).toBe('column(row("a","b"),row("c","d"))');
  });

  it("leaves a whole-tab group untouched when its layout already matches the 2-row grid", () => {
    const tabs: TabState[] = [{ kind: "terminal", id: "solo", title: "내 탭", root: gridOf(["a", "b", "c", "d"]) }];
    const panes = {
      a: pane("a", { cwd: "/repo/x" }),
      b: pane("b", { cwd: "/repo/x" }),
      c: pane("c", { cwd: "/repo/x" }),
      d: pane("d", { cwd: "/repo/x" }),
    };
    const input = { tabs, panes, activeTabId: "solo", focusedLeafId: "a" };
    const plan = planRegroup(input, opts);
    expect(plan.changed).toBe(false);
    expect(plan.groups[0]).toMatchObject({ keepTabId: "solo", relayout: false });

    const result = applyRegroupPlan(input, plan, makeId);
    expect(result).not.toBeNull();
    expect(result?.tabs[0]).toBe(tabs[0]); // 같은 객체 그대로(다시 그리지 않는다).
  });

  it("never relays out a single-pane tab (always shape-equal to itself)", () => {
    const tabs: TabState[] = [{ kind: "terminal", id: "solo", title: "혼자", root: rowOf(["a"]) }];
    const panes = { a: pane("a", { cwd: "/repo/x" }) };
    const plan = planRegroup({ tabs, panes, activeTabId: "solo", focusedLeafId: "a" }, opts);
    expect(plan.changed).toBe(false);
    expect(plan.groups).toHaveLength(1);
    expect(plan.groups[0]).toMatchObject({ keepTabId: "solo", relayout: false });
  });

  it("refuses a relayout apply when a planned leaf is missing from the kept tab (no partial apply)", () => {
    const tabs: TabState[] = [{ kind: "terminal", id: "solo", title: "내 탭", root: rowOf(["a", "b", "c", "d"]) }];
    const panes = {
      a: pane("a", { cwd: "/repo/x" }),
      b: pane("b", { cwd: "/repo/x" }),
      c: pane("c", { cwd: "/repo/x" }),
      d: pane("d", { cwd: "/repo/x" }),
    };
    const input = { tabs, panes, activeTabId: "solo", focusedLeafId: "a" };
    const plan = planRegroup(input, opts);
    expect(plan.groups[0].relayout).toBe(true);
    // 계획을 세운 뒤 "solo" 탭에서 d가 사라졌다(그사이 닫힘 등) — 부분 적용 없음.
    const stale = { ...input, tabs: [{ kind: "terminal" as const, id: "solo", title: "내 탭", root: rowOf(["a", "b", "c"]) }] };
    expect(applyRegroupPlan(stale, plan, makeId)).toBeNull();
  });
});
