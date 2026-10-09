/**
 * 터미널 pane 컨텍스트 메뉴(오른쪽 클릭).
 *
 * 무엇을 보일지는 buildPaneMenu(순수 모델)가, 배치·키보드 조작은 ContextMenu가
 * 맡는다. 여기서는 오른쪽 클릭한 pane의 지금 상태를 모아 모델에 넘기고, 고른
 * 동작을 runPaneMenuAction으로 보낸다.
 *
 * (파일 이름이 모델 paneContextMenu.ts와 대소문자만 다르면 macOS의 대소문자
 * 무시 파일 시스템에서 모듈 해석이 엇갈린다 — 그래서 Terminal로 시작한다.)
 */

import { ContextMenu } from "../../app/ContextMenu";
import { useController } from "../../app/controllerContext";
import { useI18n } from "../../i18n";
import { usePreferences } from "../../store/preferences";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { paneResumeCommand } from "../agentSessions/types";
import { missionEntryState } from "../missions/capability";
import { paneRepositoryHint } from "../missions/entry";
import { buildPaneMenu, isPaneMenuAction } from "./paneContextMenu";
import { runPaneMenuAction } from "./paneMenuActions";
import { currentPlatform } from "./shortcuts";
import { canAddPane, findLeaf, leafCount } from "./splitTree";

/** 메뉴를 연 자리(viewport 좌표)와 그때 마우스 아래에 있던 링크. */
export interface PaneMenuAnchor {
  x: number;
  y: number;
  link: string | null;
}

export function TerminalContextMenu(props: {
  leafId: string;
  anchor: PaneMenuAnchor;
  onClose: () => void;
}): JSX.Element | null {
  const { leafId, anchor, onClose } = props;
  const { t } = useI18n();
  const controller = useController();
  const pane = useWorkbenchStore((s) => s.panes[leafId]);
  const broadcast = useWorkbenchStore((s) => s.broadcastInput);
  const canSplit = useWorkbenchStore((s) => {
    // 분할 트리는 터미널 탭에만 있다(미션·에이전트 보기 탭에는 root가 없다).
    const tab = s.tabs.find((candidate) => candidate.kind === "terminal" && findLeaf(candidate.root, leafId) !== null);
    return tab?.kind === "terminal" ? canAddPane(tab.root) : false;
  });
  // 탭 재배치(04-ui §2-5)의 두 조건 — 헤더 ⋯ 메뉴(TerminalPane)와 같은 로직.
  // 객체가 아니라 boolean으로 구독해야 탭 배열이 새로 만들어질 때마다
  // 다시 그려지지 않는다. pane은 terminal 탭 사이에서만 옮긴다 —
  // mission/agent-view 탭은 트리가 없다.
  const canDetach = useWorkbenchStore((s) => {
    const tab = s.tabs.find((candidate) => candidate.kind === "terminal" && findLeaf(candidate.root, leafId) !== null);
    return tab?.kind === "terminal" ? leafCount(tab.root) > 1 : false;
  });
  const canMoveToTab = useWorkbenchStore((s) => s.tabs.filter((t) => t.kind === "terminal").length > 1);
  const missionProtocol = useWorkbenchStore((s) => s.missionProtocol);
  const overrides = usePreferences((s) => s.shortcutOverrides);
  const baseFontSize = usePreferences((s) => s.baseFontSize);
  if (!pane) return null;

  // 메뉴가 보여 준 값 그대로 동작한다(열린 사이 cwd·에이전트가 바뀌어도).
  const values = {
    link: anchor.link,
    cwd: pane.cwd,
    repository: paneRepositoryHint(pane),
    resumeCommand: paneResumeCommand(pane),
    backgroundColor: pane.backgroundColor ?? null,
  };
  const entries = buildPaneMenu({
    platform: currentPlatform(),
    overrides,
    phase: pane.phase,
    hasSelection: controller.paneHasSelection(leafId),
    link: values.link,
    cwd: values.cwd,
    project: pane.project ?? null,
    missionEntry: missionEntryState(missionProtocol),
    broadcast,
    canSplit,
    canDetach,
    canMoveToTab,
    fontSize: controller.paneFontSize(leafId),
    baseFontSize,
    resumeCommand: values.resumeCommand,
    backgroundColor: values.backgroundColor,
  });

  return (
    <ContextMenu
      entries={entries}
      x={anchor.x}
      y={anchor.y}
      ariaLabel={t("terminal.contextMenu.aria")}
      onClose={(reason) => {
        onClose();
        // Esc·Tab으로 물리면 입력 초점을 이 터미널로 돌려준다. 항목을 고른 경우는
        // 동작이, 바깥을 누른 경우는 사용자가 옮긴 곳이 초점을 정한다.
        if (reason === "escape") controller.focusPaneTerminal(leafId);
      }}
      onSelect={(id) => {
        if (isPaneMenuAction(id)) runPaneMenuAction(controller, leafId, id, values);
      }}
    />
  );
}
