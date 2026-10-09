/**
 * 오른쪽 클릭의 주인: 기본은 컨텍스트 메뉴, 마우스 보고 중인 앱에는
 * Shift+오른쪽 클릭만 보낸다(에이전트 pane에서도 메뉴가 떠야 한다).
 */

import { describe, expect, it } from "vitest";
import { rightClickRouting } from "./paneContextMenu";

describe("rightClickRouting", () => {
  it("opens the menu in panes without mouse reporting, with or without Shift", () => {
    expect(rightClickRouting(false, false)).toBe("menu");
    expect(rightClickRouting(false, true)).toBe("menu");
  });

  it("keeps the menu for agent TUIs that leave mouse reporting on", () => {
    // Claude Code·OpenCode는 실행 내내 마우스 보고를 켜 둔다 — 그래도 오른쪽 클릭은 메뉴다.
    expect(rightClickRouting(true, false)).toBe("menu");
  });

  it("sends only Shift+right-click to an app that reports the mouse", () => {
    expect(rightClickRouting(true, true)).toBe("app");
  });
});
