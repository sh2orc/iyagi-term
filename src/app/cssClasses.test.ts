import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

// vitest(node env)에서 ?raw가 빈 문자열로 변환되므로 파일을 직접 읽는다.
const css = readFileSync(fileURLToPath(new URL("./workbench.css", import.meta.url)), "utf8");

/**
 * 동적 클래스명-CSS 패리티 회귀 방지.
 *
 * SplitContainer는 `split-${axis}` / `divider-${axis}`를 렌더링한다(axis는
 * "row" | "column"). 과거 CSS가 `.split-col`로 정의되어 상하 분할이 좌우로
 * 렌더링되던 실제 버그(2026-09-06 브라우저 실증에서 발견)를 다시 내보내지
 * 않도록, 컴포넌트가 만들 수 있는 모든 클래스가 스타일시트에 존재해야 한다.
 */
describe("dynamic class ↔ CSS parity", () => {
  const axes = ["row", "column"] as const;
  const dynamicClasses = [
    ...axes.map((axis) => `split-${axis}`),
    ...axes.map((axis) => `divider-${axis}`),
    // .split-root는 `.split-root, .split {` 그룹 셀렉터로 정의된다.
    "split-root,",
    "pane-slot",
    "terminal-pane",
    "pane-header",
    // 범용 컨텍스트 메뉴(ContextMenu.tsx)가 만드는 클래스.
    "context-menu",
    "context-menu-item",
    "context-menu-separator",
    "context-menu-shortcut",
    "context-menu-row",
  ];

  it.each(dynamicClasses)(".%s 규칙이 workbench.css에 정의되어 있다", (cls) => {
    // 끝이 쉼표면 그룹 셀렉터 형태(split-root, split 공유 규칙) 허용.
    const needle = cls.endsWith(",") ? cls.slice(0, -1) : `.${cls} {`;
    expect(css.includes(needle), `${cls} rule missing from workbench.css`).toBe(true);
  });

  it("상하 분할은 flex-direction: column, 가로 분할선은 높이 4px", () => {
    expect(css.includes(".split-column")).toBe(true);
    expect(css.includes("flex-direction: column")).toBe(true);
    expect(css.includes(".divider-column")).toBe(true);
    expect(css.includes("height: 4px")).toBe(true);
    expect(css.includes(".divider-row")).toBe(true);
    expect(css.includes("width: 4px")).toBe(true);
  });

  it("split 노드는 부모를 채운다(전체 창 분할 회귀 방지)", () => {
    // `.split { flex: 1 1 auto; }` — 없으면 콘텐츠 폭에 머물러 창의 나머지가
    // 비는 버그(2026-09-06 실증).
    expect(css.includes(".split {")).toBe(true);
    expect(css.includes("flex: 1 1 auto")).toBe(true);
  });

  it("저널 재생 중에도 xterm 화면은 그대로 비친다 — 상태는 반투명 배지가 알린다", () => {
    // 화면을 통째로 숨기면 재생이 멈춘 듯 보이고 다시 나타날 때 깜빡인다.
    // 터미널을 가리는 규칙이 다시 들어오지 않게 못 박는다.
    expect(css).not.toMatch(/data-phase="replaying"\] \.terminal-view/);
    expect(css).toMatch(/\.pane-overlay-replay\s*\{[^}]*background: transparent;/);
    expect(css).toMatch(/\.pane-replay-badge\s*\{/);
  });

  it("IME 조합 표시는 불투명 배경으로 고스트 텍스트를 덮고 캐럿을 숨긴다(밑줄 없음)", () => {
    // TUI placeholder가 조합 글자 뒤로 비치지 않도록 투명이 아닌 불투명.
    expect(css).toMatch(/\.xterm \.composition-view\s*\{[^}]*background: var\(--bg-panel\);/);
    expect(css).not.toMatch(/\.xterm \.composition-view\s*\{[^}]*text-decoration:/);
    // 조합 중에는 블록 캐럿을 숨긴다.
    expect(css).toMatch(/\.xterm\.ime-composing \.xterm-cursor\s*\{[^}]*visibility: hidden/);
  });

  it("시작 터미널 선택 대화상자(ShellSelectDialog) 클래스가 정의되어 있다", () => {
    for (const cls of ["shell-select {", "shell-select-list", "shell-select-item", "shell-select-label", "shell-select-detail", "shell-select-save", "shell-select-empty"]) {
      expect(css.includes(`.${cls}`), `${cls} rule missing from workbench.css`).toBe(true);
    }
  });

  it("구독 사용량 게이지(QuotaGauges)는 막대로 그려지고 상태 바 오른쪽에 정렬된다", () => {
    for (const cls of ["strip-quota {", "strip-quota-bar {", "strip-quota-fill {", "strip-quota-value {", "strip-quota-low .strip-quota-fill {"]) {
      expect(css.includes(`.${cls}`), `${cls} rule missing from workbench.css`).toBe(true);
    }
    expect(css).toMatch(/\.strip-subscriptions\s*\{[^}]*margin-left: auto;/);
    expect(css).toMatch(/\.strip-quota-value\s*\{[^}]*text-align: right;/);
    // WebKit은 내용 폭 계산에서 flex-basis를 무시한다 — 막대 폭이 width가 아니면
    // 퍼센트가 다음 라벨과 겹친다(2026-09-11 WKWebView 실증).
    expect(css).toMatch(/\.strip-quota-bar\s*\{[^}]*width: 44px;/);
    expect(css).not.toMatch(/\.strip-quota-bar\s*\{[^}]*flex: 0 0 44px/);
  });

  it("에이전트 세션 배지·재개 오버레이·목록 클래스가 정의되어 있다(04-ui §5)", () => {
    for (const cls of [
      "pane-agent-session {",
      "pane-overlay-resume {",
      "agent-sessions-dialog {",
      "agent-session-list {",
      "agent-session-row {",
      "agent-session-actions {",
    ]) {
      expect(css.includes(`.${cls}`), `${cls} rule missing from workbench.css`).toBe(true);
    }
    // busy 점은 배지에서 물러났다 — 활동은 타이틀 앞 마커(pane-agent-marker)로만
    // 알린다. 배지에 상태 수식 클래스가 다시 붙지 않는지도 지킨다.
    expect(css).not.toMatch(/\.pane-agent-badge\.agent-busy/);
  });
});

describe("에이전트 활동 마커(탭·pane 타이틀 앞)", () => {
  it("마커 클래스가 정의되어 있고 작업 중 점은 숨 쉬며, 모션 최소 설정에서는 멈춘다", () => {
    for (const cls of ["tab-agent-marker", "pane-agent-marker"]) {
      expect(css.includes(`.${cls}`), `${cls} rule missing from workbench.css`).toBe(true);
    }
    // 회전이 아니라 불투명도 호흡이다 — 링/스피너로 되돌아가지 않게 못 박는다.
    expect(css).toMatch(/\.tab-agent-marker\.working,\s*\.pane-agent-marker\.working\s*\{[^}]*animation: agent-marker-breathe/);
    expect(css).not.toMatch(/\.tab-agent-marker\.working,\s*\.pane-agent-marker\.working\s*\{[^}]*rotate/);
    expect(css).toMatch(/@keyframes agent-marker-breathe\s*\{[^}]*opacity: 0\.3;/);
    expect(css).toMatch(/\.tab-agent-marker\.waiting,\s*\.pane-agent-marker\.waiting\s*\{[^}]*background: var\(--warn\)/);
    expect(css).toMatch(
      /@media \(prefers-reduced-motion: reduce\)\s*\{\s*\.tab-agent-marker\.working,\s*\.pane-agent-marker\.working\s*\{[^}]*animation: none;/,
    );
  });
});
