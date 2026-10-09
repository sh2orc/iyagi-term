/**
 * 끌어 놓기 미리보기 층: 끌기 상태 → 화면 조각. node 환경이라 포인터 조작은 순수 모델
 * 시험(tests/layoutDrag.test.ts)이 맡고, 여기서는 무엇을 그리는지와 그 클래스가
 * 스타일시트에 있는지만 확인한다.
 *
 * 서버 렌더에서 zustand 구독은 처음 상태(getInitialState)를 읽는다 — 그래서 끌기 상태가
 * 있는 화면은 LayoutDragSurface에 직접 넘겨 보고, 층 자체는 "끌기 없음 → 빈 화면"만 본다.
 */

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { renderToStaticMarkup } from "react-dom/server";
import { t } from "../i18n";
import { LayoutDragLayer, LayoutDragSurface } from "./LayoutDragLayer";
import { TAB_DWELL_MS, type LayoutDragView } from "./layoutDragSession";

// vitest(node env)에서 ?raw가 빈 문자열로 변환되므로 파일을 직접 읽는다(cssClasses.test.ts와 같다).
const css = readFileSync(fileURLToPath(new URL("./workbench.css", import.meta.url)), "utf8");

function view(overrides: Partial<LayoutDragView> = {}): LayoutDragView {
  return {
    source: { kind: "pane", leafId: "a" },
    area: "workbench",
    title: "zsh",
    x: 40,
    y: 40,
    resolution: { action: null, blocked: null },
    preview: null,
    sourceRect: null,
    dwell: null,
    ...overrides,
  };
}

function render(next: LayoutDragView): string {
  return renderToStaticMarkup(<LayoutDragSurface view={next} />);
}

describe("LayoutDragLayer", () => {
  it("draws nothing while no drag is in progress", () => {
    expect(renderToStaticMarkup(<LayoutDragLayer />)).toBe("");
  });

  it("previews the pane zone, dims the source, and names the action next to the cursor", () => {
    const html = render(
      view({
        resolution: { action: { kind: "dock-pane", leafId: "a", targetLeafId: "b", edge: "right" }, blocked: null },
        preview: { kind: "zone", leafId: "b", zone: "right", rect: { left: 50, top: 0, width: 50, height: 80 } },
        sourceRect: { left: 0, top: 0, width: 50, height: 80 },
      }),
    );
    expect(html).toContain('aria-hidden="true"');
    expect(html).toContain("layout-drag-source");
    expect(html).toContain("layout-drop-zone zone-right");
    expect(html).toContain("layout-drag-ghost ready");
    expect(html).toContain("zsh");
    expect(html).toContain(t("app.drag.dock.right"));
    expect(html).not.toContain("layout-drag-ghost-hint");
  });

  it("marks a blocked drop in red and states the cap", () => {
    const html = render(
      view({
        resolution: { action: null, blocked: { key: "moveTarget.full", max: 8 } },
        preview: { kind: "tab", tabId: "t2", rect: { left: 0, top: 0, width: 90, height: 24 } },
      }),
    );
    expect(html).toContain("layout-drop-tab blocked");
    expect(html).toContain("layout-drag-ghost blocked");
    expect(html).toContain(t("moveTarget.full", { max: 8 }));
  });

  it("explains where to drop when nothing would happen, and fills the dwell bar over the hovered tab", () => {
    const idle = render(view({ source: { kind: "tab", tabId: "t1" } }));
    expect(idle).toContain(t("app.drag.idle.tab"));
    expect(idle).not.toContain("layout-drag-ghost ready");

    const dwelling = render(
      view({
        source: { kind: "tab", tabId: "t1" },
        resolution: { action: { kind: "reorder-tab", tabId: "t1", toIndex: 1 }, blocked: null },
        preview: { kind: "insert", x: 120, top: 8, height: 24 },
        dwell: { tabId: "t2", title: "web", rect: { left: 100, top: 8, width: 90, height: 24 }, purpose: "merge" },
      }),
    );
    expect(dwelling).toContain("layout-drop-insert");
    expect(dwelling).toContain("layout-drop-dwell merge");
    expect(dwelling).toContain(`animation-duration:${TAB_DWELL_MS}ms`);
    expect(dwelling).toContain(t("app.drag.reorderTab"));
    expect(dwelling).toContain(t("app.drag.dwellMerge", { title: "web" }));
  });

  it("defines every piece it draws in workbench.css and never blocks hit testing", () => {
    for (const selector of [
      ".layout-drag-layer {",
      ".layout-drag-source {",
      ".layout-drop-zone {",
      ".layout-drop-zone.zone-center {",
      ".layout-drop-insert {",
      ".layout-drop-tab {",
      ".layout-drop-dwell {",
      ".layout-drop-dwell-bar {",
      ".layout-drag-ghost {",
      ".pane-grip {",
      "body.layout-dragging",
      ".layout-drop-blocked",
    ]) {
      expect(css.includes(selector), `${selector} missing from workbench.css`).toBe(true);
    }
    // 미리보기 조각이 elementFromPoint를 가리면 놓을 자리를 못 찾는다.
    expect(css).toMatch(/\.layout-drag-layer\s*\{[^}]*pointer-events: none;/);
    expect(css).toMatch(/\.layout-drag-layer > \*\s*\{[^}]*pointer-events: none;/);
    // 탭을 끌기 손잡이로 만들며 막은 글자 선택이 이름 편집 칸까지 막으면 안 된다.
    expect(css).toMatch(/\.tab-title-input\s*\{[^}]*user-select: text;/);
  });
});
