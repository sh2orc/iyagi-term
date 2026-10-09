import type { ModalState, WorkbenchState } from "../store/workbenchStore";
import { findLeaf, leafCount, MAX_PANES_PER_TAB } from "../features/terminal/splitTree";
import { terminalRoot } from "../features/terminal/regroup";
import { terminalDisplayTitle } from "../features/terminal/shellEnvironment";

/** 이 대화상자를 여는 두 모달. */
export type MoveTargetModal = Extract<ModalState, { kind: "move-pane" } | { kind: "merge-tab" }>;

/** 고를 수 없는 이유 — 문구는 화면이 옮긴다(순수 함수는 언어를 모른다). */
export type MoveTargetReason =
  | { key: "moveTarget.full"; max: number }
  | { key: "moveTarget.tooMany"; n: number; max: number };

export interface MoveTargetRow {
  /** "new"는 새 탭으로 분리하는 행(pane 이동에만 있다). */
  kind: "tab" | "new";
  /** 대상 탭 id(새 탭 행은 null). */
  tabId: string | null;
  /** 대상 탭 이름(새 탭 행은 빈 문자열 — 문구는 화면이 정한다). */
  title: string;
  /** 대상 탭의 현재 창 수(새 탭 행은 null). */
  paneCount: number | null;
  disabled: boolean;
  reason: MoveTargetReason | null;
}

/** 이 모달이 옮기려는 것이 원래 있던 탭(없으면 null — 사라진 pane). */
function sourceTabId(state: Pick<WorkbenchState, "tabs">, modal: MoveTargetModal): string | null {
  if (modal.kind === "merge-tab") return modal.tabId;
  return state.tabs.find((tab) => findLeaf(terminalRoot(tab), modal.leafId) !== null)?.id ?? null;
}

/**
 * 고를 수 있는 대상 행. 자기 자신(출발 탭)은 빼고, 상한을 넘는 탭은 지우지
 * 않고 이유와 함께 흐리게 남긴다 — "왜 여기로는 못 가는가"가 목록에서
 * 바로 읽혀야 한다. mission/agent-view 탭(05 §2)은 pane 트리가 없어 대상이
 * 될 수 없으므로 목록에 없고, 그런 탭을 합치려 하면 고를 대상도 없다.
 */
export function moveTargets(
  state: Pick<WorkbenchState, "tabs">,
  modal: MoveTargetModal,
  maxPerTab: number = MAX_PANES_PER_TAB,
): MoveTargetRow[] {
  const sourceId = sourceTabId(state, modal);
  const source = state.tabs.find((tab) => tab.id === sourceId) ?? null;
  if (source && source.kind !== "terminal") return [];
  const sourceCount = source ? leafCount(terminalRoot(source)) : 0;
  const rows: MoveTargetRow[] = [];
  // 창이 혼자 쓰는 탭에서 "새 탭으로"는 제자리걸음이다 — 그 행은 아예 없다.
  if (modal.kind === "move-pane" && sourceCount > 1) {
    rows.push({ kind: "new", tabId: null, title: "", paneCount: null, disabled: false, reason: null });
  }
  for (const tab of state.tabs) {
    if (tab.id === sourceId || tab.kind !== "terminal") continue;
    const paneCount = leafCount(tab.root);
    let reason: MoveTargetReason | null = null;
    if (modal.kind === "move-pane") {
      if (paneCount >= maxPerTab) reason = { key: "moveTarget.full", max: maxPerTab };
    } else if (sourceCount + paneCount > maxPerTab) {
      reason = { key: "moveTarget.tooMany", n: sourceCount + paneCount, max: maxPerTab };
    }
    rows.push({
      kind: "tab",
      tabId: tab.id,
      title: terminalDisplayTitle(tab.title),
      paneCount,
      disabled: reason !== null,
      reason,
    });
  }
  return rows;
}
