import { describe, expect, it } from "vitest";
import {
  DIVIDER_PX,
  balanceAxisGroup,
  LEAF_MIN_HEIGHT_PX,
  LEAF_MIN_WIDTH_PX,
  MAX_PANES_PER_TAB,
  SPLIT_NO_SPACE_MESSAGE,
  adjustRatio,
  axisGroupMembers,
  canAddPane,
  canSplit,
  clampRatio,
  closeLeaf,
  detachLeaf,
  gridLayout,
  joinTrees,
  leafCount,
  listLeaves,
  makeLeaf,
  minSize,
  split,
  dockLeaf,
  findLeaf,
  insertBeside,
  layoutShape,
  replaceLeaf,
  swapLeaves,
  type SplitNode,
} from "../src/features/terminal/splitTree";

describe("split — U01/U02/U03 semantics", () => {
  it("preserves the existing leaf object/session/view as first, new leaf second, ratio 0.5", () => {
    const existing = makeLeaf("leaf-1", "view-1", "session-1");
    const next = split(existing, "leaf-1", { id: "leaf-2", view_id: "view-2" }, { splitId: "split-1", axis: "row" });
    expect(next).not.toBeNull();
    expect(next && next.kind === "split").toBe(true);
    if (!next || next.kind !== "split") return;
    // 기존 leaf 객체를 그대로 first에 둔다(동일 참조).
    expect(next.first).toBe(existing);
    expect(next.first.session_id).toBe("session-1");
    expect(next.first.view_id).toBe("view-1");
    expect(next.second).toMatchObject({ kind: "leaf", id: "leaf-2", view_id: "view-2", session_id: null });
    expect(next.ratio).toBe(0.5);
    expect(next.axis).toBe("row");
  });

  it("splits a nested leaf without touching siblings", () => {
    // leaf1 | (leaf2 / leaf3)
    const tree = splitTree3();
    const next = split(tree, "leaf-2", { id: "leaf-4", view_id: "view-4" }, { splitId: "split-4", axis: "column" });
    if (!next || next.kind !== "split") throw new Error("root must stay split");
    expect(leafCount(next)).toBe(4);
    expect(next.first).toMatchObject({ kind: "leaf", id: "leaf-1" });
    if (next.second.kind !== "split") throw new Error("unexpected");
    // leaf-1 untouched, second subtree now splits leaf-2.
    expect(next.second.first.kind).toBe("split");
    expect(listLeaves(next).map((l) => l.id)).toEqual(["leaf-1", "leaf-2", "leaf-4", "leaf-3"]);
  });

  it("returns null for an unknown leaf id", () => {
    const tree = makeLeaf("a", "v-a");
    expect(split(tree, "nope", { id: "b", view_id: "v-b" }, { splitId: "s", axis: "row" })).toBeNull();
  });
});

describe("recursive min sizes", () => {
  it("row sums widths + divider and takes max height; column inverts", () => {
    const row = split(makeLeaf("a", "va"), "a", { id: "b", view_id: "vb" }, { splitId: "s1", axis: "row" });
    const m = minSize(row as never);
    expect(m.width).toBe(LEAF_MIN_WIDTH_PX * 2 + DIVIDER_PX);
    expect(m.height).toBe(LEAF_MIN_HEIGHT_PX);

    const col = split(makeLeaf("a", "va"), "a", { id: "b", view_id: "vb" }, { splitId: "s2", axis: "column" });
    const mc = minSize(col as never);
    expect(mc.height).toBe(LEAF_MIN_HEIGHT_PX * 2 + DIVIDER_PX);
    expect(mc.width).toBe(LEAF_MIN_WIDTH_PX);
  });

  it("computes nested tree minimums recursively", () => {
    const tree = splitTree3(); // leaf1 | (leaf2 / leaf3)
    const m = minSize(tree);
    expect(m.width).toBe(LEAF_MIN_WIDTH_PX + (LEAF_MIN_WIDTH_PX + DIVIDER_PX));
    expect(m.height).toBe(Math.max(LEAF_MIN_HEIGHT_PX, LEAF_MIN_HEIGHT_PX * 2 + DIVIDER_PX));
  });
});

describe("canSplit — U05 space rejection", () => {
  it("rejects narrow parents for row with the exact message", () => {
    const result = canSplit({ width: 480, height: 600 }, "row");
    expect(result.ok).toBe(false);
    expect(result.reason).toBe("width");
    expect(result.message).toBe(SPLIT_NO_SPACE_MESSAGE);
    expect(SPLIT_NO_SPACE_MESSAGE).toBe("분할할 공간이 부족합니다");
  });

  it("rejects short parents for column", () => {
    expect(canSplit({ width: 800, height: 200 }, "column").reason).toBe("height");
  });

  it("accepts exactly at the minimum boundary", () => {
    expect(canSplit({ width: LEAF_MIN_WIDTH_PX * 2 + DIVIDER_PX, height: LEAF_MIN_HEIGHT_PX }, "row").ok).toBe(true);
    expect(
      canSplit({ width: LEAF_MIN_WIDTH_PX, height: LEAF_MIN_HEIGHT_PX * 2 + DIVIDER_PX }, "column").ok,
    ).toBe(true);
  });
});

describe("closeLeaf — promotion and focus", () => {
  it("promotes the sibling into the parent slot and focuses the nearest survivor", () => {
    const tree = splitTree3();
    // close leaf-2: its parent(column) collapses, leaf-3 moves into that slot.
    const result = closeLeaf(tree, "leaf-2");
    expect(result.root && result.root.kind === "split").toBe(true);
    if (!result.root || result.root.kind !== "split") return;
    expect(result.root.first).toMatchObject({ kind: "leaf", id: "leaf-1" }); // untouched
    expect(result.root.second).toMatchObject({ kind: "leaf", id: "leaf-3" }); // promoted
    expect(result.focusLeafId).toBe("leaf-3");
  });

  it("closing the second pane focuses the end edge of the first subtree", () => {
    const tree = splitTree3();
    // close leaf-3 → column split collapses; focus nearest = leaf-2 (first subtree의 끝).
    const result = closeLeaf(tree, "leaf-3");
    expect(result.root && result.root.kind === "split" && result.root.first.id).toBe("leaf-1");
    expect(result.focusLeafId).toBe("leaf-2");
  });

  it("closing a deep leaf promotes within the subtree only", () => {
    const tree = splitTree3();
    const next = split(tree, "leaf-2", { id: "leaf-4", view_id: "v4" }, { splitId: "s4", axis: "row" });
    // close leaf-4 → its row split collapses; leaf-2 promoted, focus leaf-2.
    const result = closeLeaf(next as never, "leaf-4");
    expect(result.focusLeafId).toBe("leaf-2");
    expect(leafCount(result.root)).toBe(3);
  });

  it("closing the only leaf empties the tab", () => {
    const result = closeLeaf(makeLeaf("only", "v"), "only");
    expect(result.root).toBeNull();
    expect(result.focusLeafId).toBeNull();
  });

  it("closing an unknown leaf leaves the tree unchanged", () => {
    const tree = splitTree3();
    const result = closeLeaf(tree, "ghost");
    expect(result.root).toBe(tree);
  });
});

describe("clampRatio — U04 extremes respect recursive minimums", () => {
  it("clamps at leaf minimums for a flat split", () => {
    const tree = split(makeLeaf("a", "va"), "a", { id: "b", view_id: "vb" }, { splitId: "s", axis: "row" });
    const parent = { width: 1000 + DIVIDER_PX, height: 500 }; // available = 1000
    const lower = LEAF_MIN_WIDTH_PX / 1000;
    const upper = 1 - LEAF_MIN_WIDTH_PX / 1000;
    expect(clampRatio(tree as never, parent, 0.01)).toBeCloseTo(lower, 6);
    expect(clampRatio(tree as never, parent, 0.99)).toBeCloseTo(upper, 6);
    expect(clampRatio(tree as never, parent, 0.5)).toBe(0.5);
  });

  it("uses recursive subtree minimum for the nested side", () => {
    const tree = splitTree3(); // first=leaf(240), second=column split(min width 240)
    const parent = { width: 1000 + DIVIDER_PX, height: 900 }; // available = 1000
    // first subtree min width = 240 → lower = 0.24
    expect(clampRatio(tree, parent, 0.05)).toBeCloseTo(0.24, 6);
    // second subtree min width = 240 → upper = 0.76
    expect(clampRatio(tree, parent, 0.95)).toBeCloseTo(0.76, 6);

    // Now make `second` a row split (min width 486) — deeper pane must not collapse.
    const tree2 = split(
      makeLeaf("leaf-1", "view-1"),
      "leaf-1",
      { id: "leaf-2", view_id: "view-2" },
      { splitId: "split-outer", axis: "row" },
    );
    const inner = split(tree2 as never, "leaf-2", { id: "leaf-4", view_id: "view-4" }, { splitId: "s4", axis: "row" });
    // inner splits leaf-2 horizontally → second subtree of the OUTER split is
    // the row split(leaf-2|leaf-4) whose min width is 240*2 + DIVIDER_PX.
    const upper2 = 1 - (LEAF_MIN_WIDTH_PX * 2 + DIVIDER_PX) / 1000;
    expect(clampRatio(inner as never, parent, 0.95)).toBeCloseTo(upper2, 6);
  });

  it("keeps the current ratio when the parent is already below minimums", () => {
    const tree = split(makeLeaf("a", "va"), "a", { id: "b", view_id: "vb" }, { splitId: "s", axis: "row", ratio: 0.6 });
    expect(clampRatio(tree as never, { width: 200, height: 100 }, 0.1)).toBe(0.6);
  });

  it("keyboard adjust works in 2% steps (10% with Shift semantics)", () => {
    const tree = split(makeLeaf("a", "va"), "a", { id: "b", view_id: "vb" }, { splitId: "s", axis: "row" });
    const parent = { width: 1006, height: 500 };
    expect(adjustRatio(tree as never, parent, 0.02)).toBeCloseTo(0.52, 6);
    expect(adjustRatio(tree as never, parent, 0.1)).toBeCloseTo(0.6, 6);
  });
});

describe("8-leaf tab cap", () => {
  it("blocks the 9th pane (상한 안내 문구는 statusStrings.paneLimitText가 생성)", () => {
    let tree = makeLeaf("l1", "v1");
    for (let i = 2; i <= 8; i++) {
      const focused = listLeaves(tree)[listLeaves(tree).length - 1].id;
      tree = split(tree, focused, { id: `l${i}`, view_id: `v${i}` }, { splitId: `s${i}`, axis: "row" }) as never;
    }
    expect(leafCount(tree)).toBe(8);
    expect(canAddPane(tree)).toBe(false);
    expect(MAX_PANES_PER_TAB).toBe(8);
  });
});

describe("balanceAxisGroup — 분할할 때마다 축 그룹을 균등 비율로 재조정", () => {
  it("50:50에서 다시 분할하면 세 pane이 1/3씩 나눠 갖는다", () => {
    const three = balanced(row2(), "leaf-2", "leaf-3", "s2", "row");
    if (three.kind !== "split") throw new Error("expected split root");
    // 재조정 후: 바깥 1/3(A) : 2/3, 안쪽 1/2 → A=B=C=1/3.
    expect(three.ratio).toBeCloseTo(1 / 3, 10);
    expect((three.second as Extract<SplitNode, { kind: "split" }>).ratio).toBeCloseTo(0.5, 10);
    expectShares(three, { "leaf-1": 1 / 3, "leaf-2": 1 / 3, "leaf-3": 1 / 3 });
  });

  it("한 번 더 분할하면 네 pane이 1/4씩 나눠 갖는다", () => {
    const three = balanced(row2(), "leaf-2", "leaf-3", "s2", "row");
    const four = balanced(three, "leaf-3", "leaf-4", "s3", "row");
    if (four.kind !== "split") throw new Error("expected split root");
    expect(four.ratio).toBeCloseTo(0.25, 10);
    expectShares(four, {
      "leaf-1": 0.25,
      "leaf-2": 0.25,
      "leaf-3": 0.25,
      "leaf-4": 0.25,
    });
  });

  it("첫 pane을 분할해도(왼쪽 중첩) 같은 균등 결과가 나온다", () => {
    const three = balanced(row2(), "leaf-1", "leaf-3", "s2", "row");
    if (three.kind !== "split") throw new Error("expected split root");
    // 이번엔 first가 member 2개라 바깥 ratio가 2/3다.
    expect(three.ratio).toBeCloseTo(2 / 3, 10);
    expectShares(three, { "leaf-1": 1 / 3, "leaf-3": 1 / 3, "leaf-2": 1 / 3 });
  });

  it("드래그로 바꿔 둔 비율도 같은 축 그룹이면 기준대로 되돌린다", () => {
    const dragged = { ...row2(), ratio: 0.8 } as SplitNode;
    const three = balanced(dragged, "leaf-2", "leaf-3", "s2", "row");
    expectShares(three, { "leaf-1": 1 / 3, "leaf-2": 1 / 3, "leaf-3": 1 / 3 });
  });

  it("다른 axis로 분할하면 부모 그룹은 그대로 두고 새 그룹만 50:50이다", () => {
    const mixed = balanced(row2(), "leaf-2", "leaf-3", "s2", "column");
    if (mixed.kind !== "split") throw new Error("expected split root");
    // row 그룹 member는 여전히 2개(A, column split) → 0.5 유지.
    expect(mixed.ratio).toBeCloseTo(0.5, 10);
    expect((mixed.second as Extract<SplitNode, { kind: "split" }>).ratio).toBeCloseTo(0.5, 10);
    expect(axisGroupMembers(mixed, "row")).toHaveLength(2);
  });

  it("다른 축 그룹에서 조정한 비율은 재조정 대상이 아니다", () => {
    // A | (B / C) 에서 상하 비율을 0.8로 둔 뒤, row 축으로 다시 분할한다.
    const mixed = balanced(row2(), "leaf-2", "leaf-3", "s2", "column");
    if (mixed.kind !== "split" || mixed.second.kind !== "split") throw new Error("bad fixture");
    const tuned: SplitNode = { ...mixed, second: { ...mixed.second, ratio: 0.8 } };
    const next = balanced(tuned, "leaf-1", "leaf-4", "s3", "row");
    if (next.kind !== "split") throw new Error("expected split root");
    const column = findColumn(next);
    expect(column.ratio).toBeCloseTo(0.8, 10); // 사용자가 맞춘 상하 비율 보존
    // row 그룹은 member 3개(A, 새 pane, column split) → 각 1/3.
    expect(next.ratio).toBeCloseTo(2 / 3, 10);
  });

  it("모르는 split id면 트리를 그대로 돌려준다", () => {
    const tree = row2();
    expect(balanceAxisGroup(tree, "nope")).toBe(tree);
  });
});

/** A | B, 50:50 */
function row2(): SplitNode {
  const tree = split(
    makeLeaf("leaf-1", "view-1", "session-1"),
    "leaf-1",
    { id: "leaf-2", view_id: "view-2" },
    { splitId: "s1", axis: "row" },
  );
  if (!tree) throw new Error("fixture build failed");
  return tree;
}

/** split() 후 balanceAxisGroup()까지 — store.applySplit과 같은 순서. */
function balanced(
  tree: SplitNode,
  targetLeafId: string,
  newLeafId: string,
  splitId: string,
  axis: "row" | "column",
): SplitNode {
  const next = split(tree, targetLeafId, { id: newLeafId, view_id: `view-${newLeafId}` }, { splitId, axis });
  if (!next) throw new Error(`split failed for ${targetLeafId}`);
  return balanceAxisGroup(next, splitId);
}

/** 각 leaf가 차지하는 면적 비율(divider 두께는 무시). */
function shares(node: SplitNode, share = 1): Record<string, number> {
  if (node.kind === "leaf") return { [node.id]: share };
  return {
    ...shares(node.first, share * node.ratio),
    ...shares(node.second, share * (1 - node.ratio)),
  };
}

function expectShares(node: SplitNode, expected: Record<string, number>): void {
  const actual = shares(node);
  expect(Object.keys(actual).sort()).toEqual(Object.keys(expected).sort());
  for (const [id, value] of Object.entries(expected)) {
    expect(actual[id]).toBeCloseTo(value, 10);
  }
}

function findColumn(node: SplitNode): Extract<SplitNode, { kind: "split" }> {
  const found = searchColumn(node);
  if (!found) throw new Error("column split not found");
  return found;
}

function searchColumn(node: SplitNode): Extract<SplitNode, { kind: "split" }> | null {
  if (node.kind !== "split") return null;
  if (node.axis === "column") return node;
  return searchColumn(node.first) ?? searchColumn(node.second);
}

// leaf1 | (leaf2 / leaf3)
function splitTree3() {
  const columnInner = split(
    makeLeaf("leaf-2", "view-2"),
    "leaf-2",
    { id: "leaf-3", view_id: "view-3" },
    { splitId: "split-col", axis: "column" },
  );
  const outer = split(
    makeLeaf("leaf-1", "view-1"),
    "leaf-1",
    { id: "leaf-2", view_id: "view-2" },
    { splitId: "split-outer", axis: "row" },
  );
  if (!outer || outer.kind !== "split" || !columnInner) throw new Error("fixture build failed");
  // Replace the outer's second (fresh leaf-2) with the column split — same ids.
  return { ...outer, second: columnInner };
}

describe("재배치 — detachLeaf / joinTrees / gridLayout (04-ui §2-5)", () => {
  const row3 = (): SplitNode =>
    split(
      split(makeLeaf("a", "va", "sa"), "a", { id: "b", view_id: "vb", session_id: "sb" }, { splitId: "s1", axis: "row" })!,
      "b",
      { id: "c", view_id: "vc", session_id: "sc" },
      { splitId: "s2", axis: "row" },
    )!;

  it("detachLeaf returns the very same leaf object and folds the tree like closeLeaf", () => {
    const root = row3();
    const before = listLeaves(root).find((l) => l.id === "b")!;
    const result = detachLeaf(root, "b");
    expect(result.leaf).toBe(before);
    expect(listLeaves(result.root).map((l) => l.id)).toEqual(["a", "c"]);
    expect(result.focusLeafId).toBe("c");
    // 모르는 id: 트리 그대로, leaf 없음.
    const miss = detachLeaf(root, "zzz");
    expect(miss.root).toBe(root);
    expect(miss.leaf).toBeNull();
    // 마지막 leaf를 떼면 빈 트리.
    expect(detachLeaf(makeLeaf("only", "v", null), "only")).toEqual({ root: null, leaf: makeLeaf("only", "v", null), focusLeafId: null });
  });

  it("joinTrees keeps both subtrees intact and shares the axis group evenly", () => {
    const left = row3(); // a | b | c
    const right = makeLeaf("d", "vd", "sd");
    const joined = joinTrees(left, right, "j", "row");
    expect(joined.kind).toBe("split");
    if (joined.kind !== "split") return;
    expect(joined.id).toBe("j");
    expect(joined.second).toBe(right);
    // a|b|c 와 d 가 한 row 그룹(4 member)이 되어 각각 1/4씩.
    expect(joined.ratio).toBeCloseTo(3 / 4);
    expect(listLeaves(joined).map((l) => l.id)).toEqual(["a", "b", "c", "d"]);
  });

  it.each([
    [1, [1]],
    [2, [1, 1]],
    [3, [2, 1]],
    [4, [2, 2]],
    [5, [3, 2]],
    [6, [3, 3]],
    [7, [4, 3]],
    [8, [4, 4]],
  ])("gridLayout(%i leaves) → rows %j, all leaves preserved in order", (n, rows) => {
    let id = 0;
    const leaves = Array.from({ length: n }, (_, i) => makeLeaf(`l${i}`, `v${i}`, null));
    const root = gridLayout(leaves, () => `g${++id}`);
    expect(root).not.toBeNull();
    if (!root) return;
    expect(listLeaves(root).map((l) => l.id)).toEqual(leaves.map((l) => l.id));
    for (const leaf of listLeaves(root)) expect(leaf).toBe(leaves[Number(leaf.id.slice(1))]);
    // 행 구조: column chain의 member 하나가 한 행이고, 각 행은 row chain이다.
    const rowNodes = root.kind === "split" && root.axis === "column" ? columnMembers(root) : [root];
    expect(rowNodes.map((r) => leafCount(r))).toEqual(rows);
    expect(leafCount(root)).toBe(n);
  });

  it("gridLayout balances every axis group (1/n shares) and returns null for no leaves", () => {
    let id = 0;
    const root = gridLayout(
      Array.from({ length: 6 }, (_, i) => makeLeaf(`l${i}`, `v${i}`, null)),
      () => `g${++id}`,
    );
    if (!root || root.kind !== "split") throw new Error("expected split");
    expect(root.axis).toBe("column");
    expect(root.ratio).toBeCloseTo(0.5);
    const firstRow = root.first;
    if (firstRow.kind !== "split") throw new Error("expected row");
    expect(firstRow.axis).toBe("row");
    expect(firstRow.ratio).toBeCloseTo(2 / 3);
    expect(gridLayout([], () => "x")).toBeNull();
  });

  function columnMembers(node: SplitNode): SplitNode[] {
    if (node.kind !== "split" || node.axis !== "column") return [node];
    return [...columnMembers(node.first), ...columnMembers(node.second)];
  }
});

describe("끌어 놓기 트리 연산(04-ui §2-5) — insertBeside / dockLeaf / swapLeaves / replaceLeaf / layoutShape", () => {
  const leafNode = (id: string) => makeLeaf(id, `v-${id}`, `s-${id}`);
  const rowOf = (first: SplitNode, second: SplitNode, id: string, ratio = 0.5): SplitNode => ({
    kind: "split", id, axis: "row", ratio, first, second,
  });
  const columnOf = (first: SplitNode, second: SplitNode, id: string, ratio = 0.5): SplitNode => ({
    kind: "split", id, axis: "column", ratio, first, second,
  });

  it("insertBeside wraps the target on the edge's axis, ordered by side, reusing the leaf object", () => {
    const a = leafNode("a");
    const b = leafNode("b");
    expect(layoutShape(insertBeside(a, "a", b, "left", "s1"))).toBe('row("b","a")');
    expect(layoutShape(insertBeside(a, "a", b, "right", "s1"))).toBe('row("a","b")');
    expect(layoutShape(insertBeside(a, "a", b, "top", "s1"))).toBe('column("b","a")');
    const bottom = insertBeside(a, "a", b, "bottom", "s1")!;
    expect(layoutShape(bottom)).toBe('column("a","b")');
    expect(listLeaves(bottom)[1]).toBe(b);
  });

  it("insertBeside balances the axis group the new split joins", () => {
    const root = rowOf(leafNode("a"), leafNode("b"), "r1", 0.8);
    const next = insertBeside(root, "b", leafNode("c"), "right", "s1")!;
    expect(layoutShape(next)).toBe('row("a","b","c")');
    expect(next.kind === "split" ? next.ratio : null).toBeCloseTo(1 / 3);
  });

  it("insertBeside refuses an unknown target or a leaf that is already in the tree", () => {
    const root = rowOf(leafNode("a"), leafNode("b"), "r1");
    expect(insertBeside(root, "zzz", leafNode("c"), "left", "s1")).toBeNull();
    expect(insertBeside(root, "a", leafNode("b"), "left", "s1")).toBeNull();
  });

  it("dockLeaf moves a leaf beside another inside the same tree and keeps the leaf object", () => {
    const root = rowOf(leafNode("a"), rowOf(leafNode("b"), leafNode("c"), "r2"), "r1");
    const next = dockLeaf(root, "a", "c", "bottom", "s1")!;
    expect(layoutShape(next)).toBe('row("b",column("c","a"))');
    expect(findLeaf(next, "a")).toBe(findLeaf(root, "a"));
  });

  it("dockLeaf returns the same root when the pane already sits there, so tuned ratios survive", () => {
    const pair = rowOf(leafNode("a"), leafNode("b"), "r1", 0.7);
    expect(dockLeaf(pair, "b", "a", "right", "s1")).toBe(pair);
    expect(dockLeaf(pair, "a", "b", "left", "s1")).toBe(pair);
    // A | B | C 사슬에서 C를 B 오른쪽에 = 이미 그 자리(split 모양만 다르다).
    const chain = rowOf(rowOf(leafNode("a"), leafNode("b"), "r2"), leafNode("c"), "r1");
    expect(dockLeaf(chain, "c", "b", "right", "s1")).toBe(chain);
    expect(layoutShape(dockLeaf(pair, "b", "a", "left", "s1"))).toBe('row("b","a")');
  });

  it("dockLeaf refuses docking onto itself, a lone leaf, and unknown ids", () => {
    const root = rowOf(leafNode("a"), leafNode("b"), "r1");
    expect(dockLeaf(root, "a", "a", "left", "s1")).toBeNull();
    expect(dockLeaf(leafNode("a"), "a", "b", "left", "s1")).toBeNull();
    expect(dockLeaf(root, "zzz", "a", "left", "s1")).toBeNull();
    expect(dockLeaf(root, "a", "zzz", "left", "s1")).toBeNull();
  });

  it("swapLeaves exchanges two positions and keeps every split id and ratio", () => {
    const root = rowOf(leafNode("a"), columnOf(leafNode("b"), leafNode("c"), "c1", 0.3), "r1", 0.6);
    const next = swapLeaves(root, "a", "c")!;
    expect(layoutShape(next)).toBe('row("c",column("b","a"))');
    expect(next.kind === "split" ? [next.id, next.ratio] : null).toEqual(["r1", 0.6]);
    expect(next.kind === "split" && next.second.kind === "split" ? next.second.ratio : null).toBe(0.3);
    expect(swapLeaves(root, "a", "a")).toBeNull();
    expect(swapLeaves(root, "a", "zzz")).toBeNull();
  });

  it("replaceLeaf swaps in a leaf from another tree and refuses duplicates", () => {
    const root = rowOf(leafNode("a"), leafNode("b"), "r1");
    expect(layoutShape(replaceLeaf(root, "a", leafNode("x")))).toBe('row("x","b")');
    expect(replaceLeaf(root, "zzz", leafNode("x"))).toBeNull();
    expect(replaceLeaf(root, "a", leafNode("b"))).toBeNull();
  });

  it("layoutShape flattens same-axis chains so equivalent layouts compare equal", () => {
    const left = rowOf(rowOf(leafNode("a"), leafNode("b"), "x"), leafNode("c"), "y");
    const right = rowOf(leafNode("a"), rowOf(leafNode("b"), leafNode("c"), "z"), "w", 0.9);
    expect(layoutShape(left)).toBe(layoutShape(right));
    expect(layoutShape(null)).toBe("");
  });
});
