/**
 * 컨텍스트 메뉴 동작 배분: 오른쪽 클릭한 pane을 대상으로 삼고, 메뉴가 보여 준
 * 값을 복사하며, 새 pane·대화상자가 초점을 가져가는 동작 외에는 입력 초점을
 * 터미널로 돌려준다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { useWorkbenchStore } from "../../store/workbenchStore";
import type { ReliefAction } from "../../generated/ReliefAction";
import type { ShortcutAction } from "./shortcuts";
import type { PaneMenuAction } from "./paneContextMenu";
import { runPaneMenuAction, type PaneReliefMenuAction } from "./paneMenuActions";

function fakeController() {
  return {
    copySelection: vi.fn(async () => true),
    pasteFromClipboard: vi.fn(async () => undefined),
    selectAllInPane: vi.fn((_leafId: string) => undefined),
    dispatchShortcut: vi.fn((_action: ShortcutAction) => undefined),
    clearPaneScrollback: vi.fn((_leafId: string) => undefined),
    toggleBroadcast: vi.fn((_on?: boolean) => undefined),
    copyText: vi.fn(async (_text: string) => true),
    retryPane: vi.fn((_leafId: string) => undefined),
    requestClosePanes: vi.fn((_leafIds: string[], _tabId?: string) => undefined),
    focusPaneTerminal: vi.fn((_leafId: string) => undefined),
    detachPaneToNewTab: vi.fn((_leafId: string) => true),
    paneRelief: vi.fn(async (_leafId: string, _action: ReliefAction) => undefined),
    setPaneBackgroundColor: vi.fn((_leafId: string, _color: string | null) => undefined),
  };
}

const values = {
  link: "https://example.com/a",
  cwd: "/work/app/src",
  repository: "/work/app",
  resumeCommand: "claude --resume abc",
  backgroundColor: null,
};

afterEach(() => useWorkbenchStore.setState({ focusedLeafId: null, modal: null }));

describe("runPaneMenuAction", () => {
  it("targets the right-clicked pane before running focused-pane shortcuts", () => {
    const controller = fakeController();
    useWorkbenchStore.setState({ focusedLeafId: "other" });
    runPaneMenuAction(controller, "leaf-1", "zoom-in", values);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-1");
    expect(controller.dispatchShortcut).toHaveBeenCalledWith("zoom-in");
    expect(controller.focusPaneTerminal).toHaveBeenCalledWith("leaf-1");
  });

  it.each([
    ["copy-link", "https://example.com/a"],
    ["copy-cwd", "/work/app/src"],
    ["copy-resume", "claude --resume abc"],
  ] as const)("%s copies the value the menu showed", (action, text) => {
    const controller = fakeController();
    runPaneMenuAction(controller, "leaf-1", action, values);
    expect(controller.copyText).toHaveBeenCalledWith(text);
  });

  it("skips copy and open actions whose value is missing", () => {
    const controller = fakeController();
    const open = vi.fn(async (_url: string) => true);
    const empty = { link: null, cwd: null, repository: null, resumeCommand: null, backgroundColor: null };
    for (const action of ["open-link", "copy-link", "copy-cwd", "copy-resume"] as PaneMenuAction[]) {
      runPaneMenuAction(controller, "leaf-1", action, empty, open);
    }
    expect(open).not.toHaveBeenCalled();
    expect(controller.copyText).not.toHaveBeenCalled();
    runPaneMenuAction(controller, "leaf-1", "new-mission-here", empty, open);
    expect(useWorkbenchStore.getState().modal).toBeNull();
  });

  it("opens links through the injected opener", () => {
    const controller = fakeController();
    const open = vi.fn(async (_url: string) => true);
    runPaneMenuAction(controller, "leaf-1", "open-link", values, open);
    expect(open).toHaveBeenCalledWith("https://example.com/a");
  });

  it("sends pane-scoped actions to the clicked pane", () => {
    const controller = fakeController();
    runPaneMenuAction(controller, "leaf-1", "select-all", values);
    runPaneMenuAction(controller, "leaf-1", "clear", values);
    runPaneMenuAction(controller, "leaf-1", "restart", values);
    runPaneMenuAction(controller, "leaf-1", "close", values);
    expect(controller.selectAllInPane).toHaveBeenCalledWith("leaf-1");
    expect(controller.clearPaneScrollback).toHaveBeenCalledWith("leaf-1");
    expect(controller.retryPane).toHaveBeenCalledWith("leaf-1");
    expect(controller.requestClosePanes).toHaveBeenCalledWith(["leaf-1"]);
  });

  it("maps find and splits to their shortcuts and leaves focus to the new pane or dialog", () => {
    const expected: Array<[PaneMenuAction, ShortcutAction]> = [
      ["find", "search"],
      ["split-row", "split-row"],
      ["split-column", "split-column"],
    ];
    for (const [action, shortcut] of expected) {
      const controller = fakeController();
      runPaneMenuAction(controller, "leaf-1", action, values);
      expect(controller.dispatchShortcut, action).toHaveBeenCalledWith(shortcut);
      expect(controller.focusPaneTerminal, action).not.toHaveBeenCalled();
    }
    const controller = fakeController();
    runPaneMenuAction(controller, "leaf-1", "close", values);
    expect(controller.focusPaneTerminal).not.toHaveBeenCalled();
  });

  it("returns focus to the terminal after clipboard and view actions", () => {
    for (const action of ["copy", "paste", "select-all", "clear", "broadcast", "zoom-reset"] as PaneMenuAction[]) {
      const controller = fakeController();
      runPaneMenuAction(controller, "leaf-1", action, values);
      expect(controller.focusPaneTerminal, action).toHaveBeenCalledWith("leaf-1");
    }
  });

  it("detaches the pane to a new tab and returns focus once the pane remounts", () => {
    // 테스트(node 환경)엔 requestAnimationFrame이 없어 곧바로(else 분기) 불린다.
    const controller = fakeController();
    runPaneMenuAction(controller, "leaf-1", "detach", values);
    expect(controller.detachPaneToNewTab).toHaveBeenCalledWith("leaf-1");
    expect(controller.focusPaneTerminal).toHaveBeenCalledWith("leaf-1");
    expect(controller.focusPaneTerminal).toHaveBeenCalledTimes(1);
  });

  it("opens the new AI mission dialog with the pane's repository and leaves focus to it", () => {
    const controller = fakeController();
    runPaneMenuAction(controller, "leaf-1", "new-mission-here", values);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-1");
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: "/work/app" });
    expect(controller.focusPaneTerminal).not.toHaveBeenCalled();
  });

  // 08-pressure-relief §2: 메뉴 id → 계약의 session.relief 액션. 배분만
  // 확인한다(데몬 호출·상태 반영은 컨트롤러의 몫).
  it.each([
    ["reliefYield", "yield"],
    ["reliefRestore", "restore"],
    ["reliefProtect", "protect"],
    ["reliefUnprotect", "unprotect"],
  ] as Array<[PaneReliefMenuAction, ReliefAction]>)(
    "%s sends the %s relief action for the clicked pane",
    (menuAction, reliefAction) => {
      const controller = fakeController();
      useWorkbenchStore.setState({ focusedLeafId: "other" });
      runPaneMenuAction(controller, "leaf-1", menuAction, values);
      expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-1");
      expect(controller.paneRelief).toHaveBeenCalledWith("leaf-1", reliefAction);
      // 완화는 대화상자를 열지 않는다 — 입력 초점은 터미널로 돌아간다.
      expect(controller.focusPaneTerminal).toHaveBeenCalledWith("leaf-1");
    },
  );

  it("opens the move-pane dialog for move-to and leaves focus to it", () => {
    const controller = fakeController();
    runPaneMenuAction(controller, "leaf-1", "move-to", values);
    expect(useWorkbenchStore.getState().modal).toEqual({ kind: "move-pane", leafId: "leaf-1" });
    expect(controller.focusPaneTerminal).not.toHaveBeenCalled();
  });
});
