/** 배치 편집 화면의 키·명령 규칙(04-ui §2-6): Esc, 통과시킬 단축키, 터미널 전용 팔레트 명령. */

import { afterEach, describe, expect, it } from "vitest";
import { useWorkbenchStore } from "../store/workbenchStore";
import { layoutPageAllowsAction, leavesLayoutPage, runOnTerminalPage } from "./layoutPage";

afterEach(() => {
  useWorkbenchStore.setState({ page: "terminal", modal: null, renamingTabId: null });
});

describe("layout editor page rules", () => {
  it("Esc leaves the layout page only when no dialog was open and nothing is composing", () => {
    expect(leavesLayoutPage({ key: "Escape", isComposing: false }, { page: "layout", modalOpen: false })).toBe(true);
    // 대화상자를 닫은 Esc(누른 순간 모달이 떠 있었다)는 화면까지 닫지 않는다.
    expect(leavesLayoutPage({ key: "Escape", isComposing: false }, { page: "layout", modalOpen: true })).toBe(false);
    expect(leavesLayoutPage({ key: "Escape", isComposing: true }, { page: "layout", modalOpen: false })).toBe(false);
    expect(leavesLayoutPage({ key: "Escape", isComposing: false }, { page: "terminal", modalOpen: false })).toBe(false);
    expect(leavesLayoutPage({ key: "Escape", isComposing: false }, { page: "settings", modalOpen: false })).toBe(false);
    expect(leavesLayoutPage({ key: "Enter", isComposing: false }, { page: "layout", modalOpen: false })).toBe(false);
  });

  it("only the page toggle and the palette pass through while terminals are hidden", () => {
    expect(layoutPageAllowsAction("layout-editor")).toBe(true);
    expect(layoutPageAllowsAction("palette")).toBe(true);
    for (const action of ["paste", "copy", "split-row", "close-pane", "search", "zoom-in", "broadcast-toggle", "queue-toggle"] as const) {
      expect(layoutPageAllowsAction(action), action).toBe(false);
    }
  });

  it("runs terminal-only palette commands on the terminal page, clearing a stuck tab rename", () => {
    useWorkbenchStore.setState({ page: "layout", renamingTabId: "stale" });
    const seen: Array<[string, string | null]> = [];
    const record = () => {
      const state = useWorkbenchStore.getState();
      seen.push([state.page, state.renamingTabId]);
    };
    runOnTerminalPage(record)();
    runOnTerminalPage(record)();
    expect(seen).toEqual([
      ["terminal", null],
      ["terminal", null],
    ]);
  });
});
