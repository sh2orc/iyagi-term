/**
 * Pure split-tree logic for terminal panes (04-ui.md §2).
 *
 * `row` = 좌우 배치(side-by-side, flex-direction:row) → 세로 분할선.
 * `column` = 상하 배치(stacked) → 가로 분할선.
 *
 * Leaf minimum 240x144 (header 24px 포함), divider 4px, tab cap 8 leaves.
 * All functions are pure; ids are supplied by callers so tests stay
 * deterministic.
 */

export type SplitAxis = "row" | "column";

export type SplitNode =
  | { kind: "leaf"; id: string; session_id: string | null; view_id: string }
  | {
      kind: "split";
      id: string;
      axis: SplitAxis;
      ratio: number;
      first: SplitNode;
      second: SplitNode;
    };

export interface Size {
  width: number;
  height: number;
}

export interface CanSplitResult {
  ok: boolean;
  /** Which dimension is too small, for diagnostics. */
  reason?: "width" | "height";
  message?: string;
}

/** defaults.json ui{} */
export const LEAF_MIN_WIDTH_PX = 240;
export const LEAF_MIN_HEIGHT_PX = 144; // pane header 24px 포함
export const PANE_HEADER_PX = 24;
export const DIVIDER_PX = 4;
export const RESOURCE_STRIP_PX = 28;
export const MAX_PANES_PER_TAB = 8;

export const SPLIT_NO_SPACE_MESSAGE = "분할할 공간이 부족합니다";
// pane 상한 안내 문구는 statusStrings.paneLimitText가 생성한다(i18n).

/** Keyboard divider step: 2%, Shift 동반 10% (04-ui.md §2). */
export const DIVIDER_STEP = 0.02;
export const DIVIDER_STEP_LARGE = 0.1;

export function makeLeaf(id: string, view_id: string, session_id: string | null = null): SplitNode {
  return { kind: "leaf", id, session_id, view_id };
}

export function leafCount(node: SplitNode | null): number {
  if (!node) return 0;
  if (node.kind === "leaf") return 1;
  return leafCount(node.first) + leafCount(node.second);
}

export function canAddPane(node: SplitNode | null): boolean {
  return leafCount(node) < MAX_PANES_PER_TAB;
}

export function findLeaf(node: SplitNode | null, id: string): SplitNode | null {
  if (!node) return null;
  if (node.kind === "leaf") return node.id === id ? node : null;
  return findLeaf(node.first, id) ?? findLeaf(node.second, id);
}

/** Leaves in visual order: first(좌/상) → second(우/하), depth-first. */
export function listLeaves(node: SplitNode | null): SplitNode[] {
  if (!node) return [];
  if (node.kind === "leaf") return [node];
  return [...listLeaves(node.first), ...listLeaves(node.second)];
}

/**
 * Recursive minimum size (04-ui.md §2).
 * row: minWidth = 양쪽 합 + divider, minHeight = 큰 값. column은 반대.
 */
export function minSize(node: SplitNode): Size {
  if (node.kind === "leaf") {
    return { width: LEAF_MIN_WIDTH_PX, height: LEAF_MIN_HEIGHT_PX };
  }
  const first = minSize(node.first);
  const second = minSize(node.second);
  if (node.axis === "row") {
    return {
      width: first.width + second.width + DIVIDER_PX,
      height: Math.max(first.height, second.height),
    };
  }
  return {
    width: Math.max(first.width, second.width),
    height: first.height + second.height + DIVIDER_PX,
  };
}

/**
 * Whether two fresh leaves + divider fit in `parent` when split on `axis`.
 * Message per 04-ui.md §2-2: `분할할 공간이 부족합니다`, 아무 것도 생성하지 않는다.
 */
export function canSplit(parent: Size, axis: SplitAxis): CanSplitResult {
  if (axis === "row") {
    if (parent.width < LEAF_MIN_WIDTH_PX * 2 + DIVIDER_PX) {
      return { ok: false, reason: "width", message: SPLIT_NO_SPACE_MESSAGE };
    }
    if (parent.height < LEAF_MIN_HEIGHT_PX) {
      return { ok: false, reason: "height", message: SPLIT_NO_SPACE_MESSAGE };
    }
    return { ok: true };
  }
  if (parent.height < LEAF_MIN_HEIGHT_PX * 2 + DIVIDER_PX) {
    return { ok: false, reason: "height", message: SPLIT_NO_SPACE_MESSAGE };
  }
  if (parent.width < LEAF_MIN_WIDTH_PX) {
    return { ok: false, reason: "width", message: SPLIT_NO_SPACE_MESSAGE };
  }
  return { ok: true };
}

export interface SplitOptions {
  /** id for the new wrapping split node. */
  splitId: string;
  axis: SplitAxis;
  /** default 0.5 — 첫 분할 비율은 대상 영역의 50:50 */
  ratio?: number;
}

/**
 * Replace the target leaf with a split node. The existing leaf object
 * (session/view preserved untouched) becomes `first`; the new leaf becomes
 * `second` and receives focus (04-ui.md §2-3, design §10).
 *
 * Returns a new tree, or null when the leaf id is unknown.
 */
export function split(
  node: SplitNode,
  leafId: string,
  newLeaf: { id: string; view_id: string; session_id?: string | null },
  opts: SplitOptions,
): SplitNode | null {
  const newLeafNode: SplitNode = {
    kind: "leaf",
    id: newLeaf.id,
    view_id: newLeaf.view_id,
    session_id: newLeaf.session_id ?? null,
  };
  if (node.kind === "leaf") {
    if (node.id !== leafId) return null;
    return {
      kind: "split",
      id: opts.splitId,
      axis: opts.axis,
      ratio: opts.ratio ?? 0.5,
      first: node,
      second: newLeafNode,
    };
  }
  const first = split(node.first, leafId, newLeaf, opts);
  if (first) return { ...node, first };
  const second = split(node.second, leafId, newLeaf, opts);
  if (second) return { ...node, second };
  return null;
}

/**
 * 축 그룹(axis group): 같은 axis로 직접 이어진 split들의 최대 chain.
 * chain을 따라 내려가다 만나는 leaf / 다른 axis의 split이 그룹의 member다.
 * `A | (B | C)` 는 row 그룹 member 3개(A, B, C)이고,
 * `A | (B / C)` 는 row 그룹 member 2개(A, column split)다.
 */
export function axisGroupMembers(node: SplitNode, axis: SplitAxis): SplitNode[] {
  if (node.kind !== "split" || node.axis !== axis) return [node];
  return [...axisGroupMembers(node.first, axis), ...axisGroupMembers(node.second, axis)];
}

/** root에서 id까지의 경로(부모 → 자식 순). 없으면 null. */
function pathTo(node: SplitNode, id: string): SplitNode[] | null {
  if (node.id === id) return [node];
  if (node.kind === "leaf") return null;
  const first = pathTo(node.first, id);
  if (first) return [node, ...first];
  const second = pathTo(node.second, id);
  if (second) return [node, ...second];
  return null;
}

/**
 * id 노드를 next로 교체한 새 트리(경로만 다시 만든다).
 * 바뀐 것이 없으면 같은 참조를 돌려준다 — 이미 균등한 트리를 다시
 * 재조정해도 store가 새 상태를 만들지 않는다.
 */
function replaceNode(node: SplitNode, id: string, next: SplitNode): SplitNode {
  if (node.id === id) return next;
  if (node.kind === "leaf") return node;
  const first = replaceNode(node.first, id, next);
  const second = replaceNode(node.second, id, next);
  if (first === node.first && second === node.second) return node;
  return { ...node, first, second };
}

/**
 * 축 그룹의 모든 ratio를 member 수 비율로 다시 계산한다.
 * ratio = (first쪽 member 수) / (그룹 전체 member 수)이므로 그룹 안의
 * member는 모두 같은 몫을 가진다. 다른 axis 하위 트리는 건드리지 않아
 * 사용자가 조정해 둔 비율이 그대로 남는다.
 */
function equalizeAxisGroup(node: SplitNode, axis: SplitAxis): SplitNode {
  if (node.kind !== "split" || node.axis !== axis) return node;
  const ratio =
    axisGroupMembers(node.first, axis).length / axisGroupMembers(node, axis).length;
  const first = equalizeAxisGroup(node.first, axis);
  const second = equalizeAxisGroup(node.second, axis);
  if (ratio === node.ratio && first === node.first && second === node.second) return node;
  return { ...node, ratio, first, second };
}

/**
 * 축 그룹 균등 재조정(04-ui.md §2-3). `splitId` split이 속한 축 그룹
 * 전체를 균등 비율로 되돌린다. 2분할(50:50)에서 다시 분할하면 1/3씩,
 * 한 번 더 분할하면 1/4씩이 되어 분할 기준이 항상 같다.
 *
 * 분할 직후와 divider 더블클릭(드래그로 옮긴 경계선 되돌리기)이 같은
 * 함수를 쓴다. 그룹 밖(부모의 다른 축, 다른 axis의 하위 트리)은 그대로
 * 두고, 이미 균등하면 같은 참조를 돌려준다.
 */
export function balanceAxisGroup(root: SplitNode, splitId: string): SplitNode {
  const path = pathTo(root, splitId);
  if (!path) return root;
  const target = path[path.length - 1];
  if (target.kind !== "split") return root;
  // 같은 axis로 이어지는 동안 위로 올라가 그룹의 최상단을 찾는다.
  let top = path.length - 1;
  while (top > 0) {
    const parent = path[top - 1];
    if (parent.kind !== "split" || parent.axis !== target.axis) break;
    top -= 1;
  }
  const groupRoot = path[top];
  return replaceNode(root, groupRoot.id, equalizeAxisGroup(groupRoot, target.axis));
}

/**
 * Rebalance every horizontal/vertical group in a surviving tree. Closing a
 * leaf promotes its sibling, but the ancestors keep ratios calculated for the
 * old member count; this restores 1/2, 1/3, 1/4… shares after that collapse.
 */
export function balanceAllAxisGroups(root: SplitNode): SplitNode {
  if (root.kind === "leaf") return root;
  const first = balanceAllAxisGroups(root.first);
  const second = balanceAllAxisGroups(root.second);
  const withBalancedChildren =
    first === root.first && second === root.second ? root : { ...root, first, second };
  return equalizeAxisGroup(withBalancedChildren, root.axis);
}

export interface CloseResult {
  root: SplitNode | null;
  /** leaf to move focus to; null when the tab became empty. */
  focusLeafId: string | null;
}

/**
 * Close a leaf: sibling promoted to the parent's slot, focus moves to the
 * nearest surviving sibling (04-ui.md §2). Closing the only leaf empties the
 * tab (빈 프로젝트 화면).
 */
export function closeLeaf(node: SplitNode | null, leafId: string): CloseResult {
  if (!node) return { root: null, focusLeafId: null };
  if (node.kind === "leaf") {
    return node.id === leafId ? { root: null, focusLeafId: null } : { root: node, focusLeafId: null };
  }
  if (node.first.kind === "leaf" && node.first.id === leafId) {
    // Closing the 좌/상 pane → nearest survivor is the start edge of `second`.
    return { root: node.second, focusLeafId: nearestLeaf(node.second, "start") };
  }
  if (node.second.kind === "leaf" && node.second.id === leafId) {
    // Closing the 우/하 pane → nearest survivor is the end edge of `first`.
    return { root: node.first, focusLeafId: nearestLeaf(node.first, "end") };
  }
  if (findLeaf(node.first, leafId)) {
    const r = closeLeaf(node.first, leafId);
    return { root: { ...node, first: r.root as SplitNode }, focusLeafId: r.focusLeafId };
  }
  if (findLeaf(node.second, leafId)) {
    const r = closeLeaf(node.second, leafId);
    return { root: { ...node, second: r.root as SplitNode }, focusLeafId: r.focusLeafId };
  }
  return { root: node, focusLeafId: null };
}

/**
 * Deepest leaf at the given edge of a subtree.
 * `start` = first(좌/상) edge; `end` = second(우/하) edge.
 */
export function nearestLeaf(node: SplitNode, edge: "start" | "end"): string {
  if (node.kind === "leaf") return node.id;
  return edge === "start" ? nearestLeaf(node.first, "start") : nearestLeaf(node.second, "end");
}

/**
 * Clamp a proposed ratio against recursive leaf minimums (04-ui.md §2,
 * divider drag). available = parentSize - divider;
 * lower = minFirst/available; upper = 1 - minSecond/available.
 * When the parent is already smaller than the minimums (window shrunk),
 * keep the current ratio instead of collapsing deep panes.
 */
export function clampRatio(splitNode: SplitNode, parent: Size, proposed: number): number {
  if (splitNode.kind !== "split") return 0.5;
  const along = splitNode.axis === "row" ? parent.width : parent.height;
  const firstMin =
    splitNode.axis === "row" ? minSize(splitNode.first).width : minSize(splitNode.first).height;
  const secondMin =
    splitNode.axis === "row" ? minSize(splitNode.second).width : minSize(splitNode.second).height;
  const available = along - DIVIDER_PX;
  if (!(available > 0)) return splitNode.ratio;
  const lower = firstMin / available;
  const upper = 1 - secondMin / available;
  if (lower > upper) return splitNode.ratio;
  const lo = Math.max(0, Math.min(lower, 1));
  const hi = Math.min(1, Math.max(upper, 0));
  return Math.min(Math.max(proposed, lo), hi);
}

/** Keyboard adjustment: step is 0.02 (2%) or 0.10 with Shift. */
export function adjustRatio(splitNode: SplitNode, parent: Size, delta: number): number {
  const base = splitNode.kind === "split" ? splitNode.ratio : 0.5;
  return clampRatio(splitNode, parent, base + delta);
}

/** Update one split node's ratio by id (immutable path rewrite). */
export function withRatio(node: SplitNode, splitId: string, ratio: number): SplitNode {
  if (node.kind === "leaf") return node;
  return {
    ...node,
    ratio: node.id === splitId ? ratio : node.ratio,
    first: withRatio(node.first, splitId, ratio),
    second: withRatio(node.second, splitId, ratio),
  };
}

/** Find the split node with the given id (divider lookup). */
export function findSplit(node: SplitNode | null, id: string): SplitNode | null {
  if (!node || node.kind === "leaf") return null;
  if (node.id === id) return node;
  return findSplit(node.first, id) ?? findSplit(node.second, id);
}

// ---- 재배치(탭 재그룹핑): leaf를 떼어 다른 트리에 붙이거나 두 트리를 잇는다.

export interface DetachResult {
  /** leaf를 뺀 나머지 트리(마지막 leaf였으면 null). */
  root: SplitNode | null;
  /** 떼어 낸 leaf 노드(같은 객체 — view/session id 보존). 없으면 null. */
  leaf: SplitNode | null;
  /** 남은 트리에서 초점을 줄 leaf(closeLeaf와 같은 규칙). */
  focusLeafId: string | null;
}

/**
 * leaf를 트리에서 떼어 낸다. 닫기(closeLeaf)와 같은 접기 규칙을 쓰되 leaf
 * 객체를 돌려주므로 다른 탭에 그대로 붙일 수 있다 — 세션·뷰는 건드리지
 * 않는다(04-ui §4: 화면 이동은 CLI 종료 사유가 아니다).
 */
export function detachLeaf(root: SplitNode | null, leafId: string): DetachResult {
  const leaf = findLeaf(root, leafId);
  if (!leaf) return { root, leaf: null, focusLeafId: null };
  const closed = closeLeaf(root, leafId);
  return { root: closed.root, leaf, focusLeafId: closed.focusLeafId };
}

/** 두 트리를 새 split 아래 잇는다(탭 합치기). 비율은 member 수 기준 균등. */
export function joinTrees(first: SplitNode, second: SplitNode, splitId: string, axis: SplitAxis): SplitNode {
  return balanceAxisGroup({ kind: "split", id: splitId, axis, ratio: 0.5, first, second }, splitId);
}

/**
 * leaf들을 격자로 배치한다(재그룹핑으로 새로 만든 탭). 위아래 딱 두 줄로
 * 나누고, 한 줄에는 최대 4개까지만 옆으로 늘어놓는다(탭 상한
 * MAX_PANES_PER_TAB=8이라 호출자가 그룹을 8개씩 나눠 넘기므로 실제로도
 * 한 줄이 4개를 넘지 않는다). 위 줄이 ceil(n/2)개, 아래 줄이 floor(n/2)개를
 * 순서대로 갖는다: 2 → [1,1], 3 → [2,1], 5 → [3,2], 7 → [4,3], 8 → [4,4].
 * n=1이면 leaf 하나 그대로, n=0이면 null.
 *
 * 줄 안은 row split chain, 줄 사이는 column split chain이며 모두 균등
 * 비율이다. leaf 객체는 그대로 쓴다(view/session 보존).
 */
export function gridLayout(leaves: SplitNode[], makeId: () => string): SplitNode | null {
  if (leaves.length === 0) return null;
  if (leaves.length === 1) return leaves[0];
  const n = leaves.length;
  const topCount = Math.ceil(n / 2);
  const top = chain(leaves.slice(0, topCount), makeId, "row");
  const bottom = chain(leaves.slice(topCount), makeId, "row");
  return balanceAllAxisGroups(chain([top, bottom], makeId, "column"));
}

/** 같은 axis로 왼쪽부터 접은 split chain(비율은 호출자가 균등화한다). */
function chain(nodes: SplitNode[], makeId: () => string, axis: SplitAxis): SplitNode {
  let acc = nodes[0];
  for (let i = 1; i < nodes.length; i += 1) {
    acc = { kind: "split", id: makeId(), axis, ratio: 0.5, first: acc, second: nodes[i] };
  }
  return acc;
}

// ---- 끌어 놓기(04-ui §2-5): 창을 다른 창 옆에 붙이거나 두 창의 자리를 맞바꾼다.

/** 창을 놓을 가장자리. 좌우(left/right)는 row, 상하(top/bottom)는 column 분할이다. */
export type PaneEdge = "left" | "right" | "top" | "bottom";

export function edgeAxis(edge: PaneEdge): SplitAxis {
  return edge === "left" || edge === "right" ? "row" : "column";
}

/**
 * 화면에 보이는 배치의 모양(비율·split id 무시): 같은 axis로 이어진 split은 한
 * 묶음으로 편다. 모양이 같은 두 트리는 비율만 다를 뿐 같은 자리에 같은 창을
 * 그린다 — 끌어 놓기의 "이미 그 자리" 판정에 쓴다.
 */
export function layoutShape(node: SplitNode | null): string {
  if (!node) return "";
  if (node.kind === "leaf") return JSON.stringify(node.id);
  return `${node.axis}(${axisGroupMembers(node, node.axis).map(layoutShape).join(",")})`;
}

/**
 * leaf 노드를 target leaf 옆(edge)에 붙인다: target 자리를 새 split으로 감싸고
 * (left/top이면 붙인 leaf가 first, right/bottom이면 second) 그 축 그룹을 분할과
 * 같은 기준으로 균등화한다. leaf 객체는 그대로 쓴다(view/session 보존). target이
 * 없거나 붙일 leaf가 이미 트리에 있으면 null — 같은 leaf가 두 번 나오는 트리는
 * 만들지 않는다.
 */
export function insertBeside(
  root: SplitNode,
  targetLeafId: string,
  leaf: SplitNode,
  edge: PaneEdge,
  splitId: string,
): SplitNode | null {
  const target = findLeaf(root, targetLeafId);
  if (!target || leaf.kind !== "leaf" || findLeaf(root, leaf.id)) return null;
  const before = edge === "left" || edge === "top";
  const wrapped: SplitNode = {
    kind: "split",
    id: splitId,
    axis: edgeAxis(edge),
    ratio: 0.5,
    first: before ? leaf : target,
    second: before ? target : leaf,
  };
  return balanceAxisGroup(replaceNode(root, targetLeafId, wrapped), splitId);
}

/**
 * 같은 트리 안에서 leaf를 target 옆(edge)으로 옮긴다. 떼어 낸 자리는 닫기와 같은
 * 규칙으로 접어 남은 축 그룹을 균등화하고(탭 사이 이동과 같다) target 옆에 붙인다.
 * 결과가 지금과 같은 모양이면(이미 그 자리) 원래 root를 그대로 돌려준다 — 조정해
 * 둔 비율을 괜히 되돌리지 않는다. 자기 자신 옆·혼자 있는 leaf·모르는 id는 null.
 */
export function dockLeaf(
  root: SplitNode,
  leafId: string,
  targetLeafId: string,
  edge: PaneEdge,
  splitId: string,
): SplitNode | null {
  if (leafId === targetLeafId || !findLeaf(root, targetLeafId)) return null;
  const detached = detachLeaf(root, leafId);
  if (!detached.leaf || !detached.root) return null;
  const next = insertBeside(balanceAllAxisGroups(detached.root), targetLeafId, detached.leaf, edge, splitId);
  if (!next) return null;
  return layoutShape(next) === layoutShape(root) ? root : next;
}

/**
 * 두 leaf의 자리를 맞바꾼다(같은 트리). split id·비율은 그대로이고 leaf 객체를
 * 그대로 옮긴다(view/session 보존). 둘 중 하나라도 없거나 같은 id면 null.
 */
export function swapLeaves(root: SplitNode, a: string, b: string): SplitNode | null {
  if (a === b) return null;
  const leafA = findLeaf(root, a);
  const leafB = findLeaf(root, b);
  if (!leafA || !leafB) return null;
  const walk = (node: SplitNode): SplitNode => {
    if (node.kind === "leaf") return node.id === a ? leafB : node.id === b ? leafA : node;
    const first = walk(node.first);
    const second = walk(node.second);
    return first === node.first && second === node.second ? node : { ...node, first, second };
  };
  return walk(root);
}

/**
 * leaf 하나를 다른 leaf 노드로 바꿔 끼운다(탭 사이 자리 바꾸기의 한쪽). 바꿀 leaf가
 * 없거나, 끼울 노드가 leaf가 아니거나, 그 노드가 이미 이 트리에 있으면 null.
 */
export function replaceLeaf(root: SplitNode, leafId: string, next: SplitNode): SplitNode | null {
  if (next.kind !== "leaf" || !findLeaf(root, leafId)) return null;
  if (next.id !== leafId && findLeaf(root, next.id)) return null;
  return replaceNode(root, leafId, next);
}
