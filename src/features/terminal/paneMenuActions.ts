/**
 * 터미널 컨텍스트 메뉴에서 고른 동작을 controller로 보낸다.
 *
 * 메뉴는 오른쪽 클릭한 pane을 가리킨다. 단축키 경로(분할·확대·붙여넣기·
 * 찾기)는 초점 pane을 대상으로 하므로 먼저 초점을 그 pane에 맞춘다. 동작이
 * 끝나면 입력 초점을 터미널로 돌려준다 — 새 pane·대화상자가 초점을 가져가는
 * 동작(분할·찾기·닫기·분리·이동·새 AI 작업)은 예외다.
 */

import { useWorkbenchStore } from "../../store/workbenchStore";
import { openMissionCreate } from "../missions/entry";
import { openExternal } from "../security/linkOpener";
import type { PaneMenuAction } from "./paneContextMenu";
import type { SessionController } from "./sessionController";

export type PaneMenuController = Pick<
  SessionController,
  | "copySelection"
  | "pasteFromClipboard"
  | "selectAllInPane"
  | "dispatchShortcut"
  | "clearPaneScrollback"
  | "toggleBroadcast"
  | "copyText"
  | "retryPane"
  | "requestClosePanes"
  | "focusPaneTerminal"
  | "detachPaneToNewTab"
  | "paneRelief"
  | "setPaneBackgroundColor"
>;

/**
 * 수동 압력 완화(08-pressure-relief §2)의 메뉴 항목 id. 오른쪽 클릭 메뉴
 * 모델(paneContextMenu)이 아니라 pane 헤더 ⋯ 메뉴가 쓰므로 PaneMenuAction과
 * 따로 둔다 — 배분은 여기 한 곳이다.
 */
export type PaneReliefMenuAction = "reliefYield" | "reliefRestore" | "reliefProtect" | "reliefUnprotect";

/** 메뉴 id → 계약의 `session.relief` 액션. */
const RELIEF_ACTIONS: Record<PaneReliefMenuAction, "yield" | "restore" | "protect" | "unprotect"> = {
  reliefYield: "yield",
  reliefRestore: "restore",
  reliefProtect: "protect",
  reliefUnprotect: "unprotect",
};

/** 값이 필요 없는 동작(완화 메뉴 등)이 쓰는 빈 값 묶음. */
export const EMPTY_PANE_MENU_VALUES: PaneMenuValues = {
  link: null,
  cwd: null,
  repository: null,
  resumeCommand: null,
  backgroundColor: null,
};

/** 메뉴를 연 순간의 값 — 동작 시점에 다시 읽지 않는다(메뉴가 보여 준 그대로). */
export interface PaneMenuValues {
  link: string | null;
  cwd: string | null;
  /** 새 AI 작업에 채울 저장소 경로(git 최상위 → cwd). */
  repository: string | null;
  resumeCommand: string | null;
  /** 이 pane에 사용자가 지정한 배경색(색 고르기의 초기값). */
  backgroundColor: string | null;
}

export function runPaneMenuAction(
  controller: PaneMenuController,
  leafId: string,
  action: PaneMenuAction | PaneReliefMenuAction,
  values: PaneMenuValues,
  open: (url: string) => Promise<boolean> = openExternal,
): void {
  useWorkbenchStore.getState().focusPane(leafId);
  switch (action) {
    // 수동 완화(08 §2): 데몬에 보내고 응답을 pane에 반영하는 일은 컨트롤러가
    // 한다 — 실패도 거기서 toast로 알린다. 초점은 터미널로 돌아간다.
    case "reliefYield":
    case "reliefRestore":
    case "reliefProtect":
    case "reliefUnprotect":
      void controller.paneRelief(leafId, RELIEF_ACTIONS[action]);
      break;
    case "open-link":
      if (values.link) void open(values.link);
      break;
    case "copy-link":
      if (values.link) void controller.copyText(values.link);
      break;
    case "copy":
      void controller.copySelection();
      break;
    case "paste":
      void controller.pasteFromClipboard();
      break;
    case "select-all":
      controller.selectAllInPane(leafId);
      break;
    case "find":
      // 검색 대화상자가 입력 초점을 가져간다.
      controller.dispatchShortcut("search");
      return;
    case "clear":
      controller.clearPaneScrollback(leafId);
      break;
    case "bg-color": {
      // OS 색 고르기: 화면에 보이지 않는 color input을 한 번 클릭해
      // 네이티브 피커를 연다(메뉴 안에 폼을 두지 않아도 되게). 취소도 정리한다.
      if (typeof document === "undefined") break;
      const input = document.createElement("input");
      input.type = "color";
      if (values.backgroundColor) input.value = values.backgroundColor;
      input.style.position = "fixed";
      input.style.opacity = "0";
      input.style.pointerEvents = "none";
      document.body.appendChild(input);
      const cleanup = (): void => input.remove();
      input.addEventListener("change", () => {
        controller.setPaneBackgroundColor(leafId, input.value);
        cleanup();
      });
      input.addEventListener("cancel", cleanup);
      input.click();
      break;
    }
    case "bg-reset":
      controller.setPaneBackgroundColor(leafId, null);
      break;
    case "zoom-out":
    case "zoom-reset":
    case "zoom-in":
      controller.dispatchShortcut(action);
      break;
    case "split-row":
    case "split-column":
      // 새 pane이 초점을 가져간다.
      controller.dispatchShortcut(action);
      return;
    case "broadcast":
      controller.toggleBroadcast();
      break;
    case "detach": {
      controller.detachPaneToNewTab(leafId);
      // pane이 새 탭에서 다시 그려지므로 지금 초점을 주면 옛 DOM 자리를 잡는다
      // — 다음 애니메이션 프레임에 그 터미널로 돌려준다(테스트 환경엔 rAF가 없다).
      if (typeof requestAnimationFrame === "function") {
        requestAnimationFrame(() => controller.focusPaneTerminal(leafId));
      } else {
        controller.focusPaneTerminal(leafId);
      }
      return;
    }
    case "move-to":
      // 대화상자가 입력 초점을 가져간다.
      useWorkbenchStore.getState().openModal({ kind: "move-pane", leafId });
      return;
    case "copy-cwd":
      if (values.cwd) void controller.copyText(values.cwd);
      break;
    case "copy-resume":
      if (values.resumeCommand) void controller.copyText(values.resumeCommand);
      break;
    case "new-mission-here":
      if (!values.repository) break;
      // 대화상자가 입력 초점을 가져간다.
      openMissionCreate(values.repository);
      return;
    case "restart":
      controller.retryPane(leafId);
      break;
    case "close":
      // 확인 대화상자·다음 pane이 초점을 정한다.
      controller.requestClosePanes([leafId]);
      return;
  }
  controller.focusPaneTerminal(leafId);
}
