/**
 * 설정 화면 클래스 ↔ CSS 패리티 + 라이트 팔레트 존재 확인.
 *
 * cssClasses.test.ts와 같은 이유다: 컴포넌트가 만들어 내는 클래스에 규칙이
 * 없으면 화면은 조용히 무너진다(오류 없이 배치만 깨진다).
 */

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const settingsCss = readFileSync(fileURLToPath(new URL("./settings.css", import.meta.url)), "utf8");
const workbenchCss = readFileSync(fileURLToPath(new URL("./workbench.css", import.meta.url)), "utf8");

const classes = [
  "settings-search",
  "settings-results",
  "settings-nav-footer",
  "settings-reset-all",
  "settings-reset-actions",
  "setting-row",
  "setting-row-inline",
  "setting-row-stacked",
  "setting-row-highlight",
  "setting-text",
  "setting-label",
  "setting-description",
  "setting-control",
  "setting-reset",
  "setting-select",
  "setting-toggle",
  "setting-toggle-state",
  "setting-number",
  "setting-number-input",
  "setting-unit",
  "setting-error",
  "custom-shells",
  "custom-shell-form",
  "custom-shell-list",
  "custom-shell-name",
  "shortcut-table",
];

describe("설정 화면 클래스 ↔ settings.css", () => {
  it.each(classes)(".%s 규칙이 정의되어 있다", (cls) => {
    expect(settingsCss.includes(`.${cls} `), `${cls} rule missing from settings.css`).toBe(true);
  });
});

describe("테마 팔레트(04-ui.md §4: dark/light 두 가지)", () => {
  it("dark가 기본이고 light는 data-theme으로 덮어쓴다", () => {
    expect(workbenchCss.includes(":root {")).toBe(true);
    expect(workbenchCss.includes(':root[data-theme="light"]')).toBe(true);
  });

  it("두 팔레트가 같은 색 토큰 집합을 정의한다(라이트에서 색이 비지 않게)", () => {
    const block = (selector: string): Set<string> => {
      const start = workbenchCss.indexOf(selector);
      const open = workbenchCss.indexOf("{", start);
      const close = workbenchCss.indexOf("}", open);
      const body = workbenchCss.slice(open, close);
      return new Set([...body.matchAll(/--([a-z-]+):/g)].map((m) => m[1]));
    };
    const dark = block(":root {");
    const light = block(':root[data-theme="light"]');
    // --header-h는 색이 아니라 치수라 라이트가 다시 정의하지 않는다.
    dark.delete("header-h");
    expect([...dark].sort()).toEqual([...light].sort());
  });

  it("네이티브 컨트롤이 테마를 따르도록 color-scheme을 선언한다", () => {
    expect(workbenchCss.includes("color-scheme: dark")).toBe(true);
    expect(workbenchCss.includes("color-scheme: light")).toBe(true);
  });
});
