/**
 * 배치 편집 화면 조각: 블록(끌기 id·제목·상태·경로)과 오른쪽 목록(배치 안 됨 먼저, 끌기 id).
 * node 환경이라 끌어 놓기는 순수 모델 시험이, 실제 포인터 흐름은 브라우저 시험대가 맡는다.
 */

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { renderToStaticMarkup } from "react-dom/server";
import { t } from "../i18n";
import type { PaneMeta } from "../store/workbenchStore";
import { LayoutBlockView, LayoutRosterView } from "./LayoutEditorPage";
import { tailPath } from "./layoutPath";

// vitest(node env)에서 ?raw가 빈 문자열로 변환되므로 파일을 직접 읽는다(cssClasses.test.ts와 같다).
const css = readFileSync(fileURLToPath(new URL("./layoutEditor.css", import.meta.url)), "utf8");

function pane(extra: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: "a",
    viewId: "v-a",
    sessionId: "s-a",
    workloadId: "w-a",
    title: "zsh",
    cwd: "/Users/me/project/api",
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    ...extra,
  };
}

describe("LayoutEditorPage pieces", () => {
  it("draws a block with its drag id, title, and trimmed path", () => {
    const html = renderToStaticMarkup(<LayoutBlockView leafId="a" pane={pane()} focused highlighted={false} />);
    expect(html).toContain('data-layout-leaf-id="a"');
    expect(html).toContain("layout-block phase-live focused");
    expect(html).toContain("zsh");
    expect(html).toContain("…/project/api");
    expect(html).not.toContain("layout-block-state");
  });

  it("shows the state of a block that is not live", () => {
    const html = renderToStaticMarkup(
      <LayoutBlockView leafId="a" pane={pane({ phase: "exited" })} focused={false} highlighted />,
    );
    expect(html).toContain("layout-block phase-exited highlighted");
    expect(html).toContain("layout-block-state state-exited");
  });

  it("lists unplaced terminals first with their session drag id, then placed ones with their group", () => {
    const html = renderToStaticMarkup(
      <LayoutRosterView
        roster={{
          unplaced: [{ sessionId: "s-x", workloadId: "w-x", title: "npm test", cwd: "/work/web", state: "RUNNING", agent: null }],
          placed: [{ leafId: "a", tabId: "t1", tabTitle: "api", title: "zsh", cwd: null, phase: "live", agent: null }],
        }}
      />,
    );
    expect(html.indexOf(t("layoutEditor.roster.unplaced"))).toBeLessThan(html.indexOf(t("layoutEditor.roster.placed")));
    expect(html).toContain('data-layout-roster-session="s-x"');
    expect(html).toContain('data-layout-roster-leaf="a"');
    expect(html).toContain("layout-roster-item unplaced");
    expect(html).toContain("layout-roster-chip");
    expect(html).toContain("npm test");
  });

  it("says so when nothing is unplaced or placed", () => {
    const html = renderToStaticMarkup(<LayoutRosterView roster={{ unplaced: [], placed: [] }} />);
    expect(html).toContain(t("layoutEditor.roster.unplacedEmpty"));
    expect(html).toContain(t("layoutEditor.roster.placedEmpty"));
  });

  it("trims long paths to their last two segments", () => {
    expect(tailPath("/Users/me/project/api")).toBe("…/project/api");
    expect(tailPath("/work")).toBe("/work");
    expect(tailPath("C:\\Users\\me\\repo")).toBe("…/me/repo");
  });

  it("defines the classes the page draws, with the card gap the drag session expects", () => {
    for (const selector of [
      ".layout-editor-page {",
      ".layout-board {",
      ".layout-group {",
      ".layout-group-head {",
      ".layout-mini-split {",
      ".layout-mini-split.column {",
      ".layout-block {",
      ".layout-block.highlighted {",
      ".layout-group-new {",
      ".layout-roster {",
      ".layout-roster-item.unplaced {",
    ]) {
      expect(css.includes(selector), `${selector} missing from layoutEditor.css`).toBe(true);
    }
    // 순서 삽입 막대가 카드 사이 한가운데에 서려면 gap이 layoutDragSession의 CARD_GAP_PX(16)와 같아야 한다.
    expect(css).toMatch(/\.layout-board\s*\{[^}]*gap: 16px;/);
  });
});
