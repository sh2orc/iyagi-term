/**
 * pane 배경 테마의 참조 안정성 — xterm은 `options.theme`에 새 객체가 들어올 때마다
 * 테마 전체를 다시 적용한다(WebGL 글리프 아틀라스를 비우고 다시 그린다).
 * workload.changed마다 tint pane 전부의 배경을 다시 맞추므로, 같은 (기본 테마, 색)은
 * 같은 참조여야 하고 같은 참조면 아예 대입하지 않는다.
 */

import { describe, expect, it } from "vitest";
import { TerminalRegistry, withBackground, type RegistryDom, type TerminalLike } from "./registry";

const BASE = { background: "#101010", foreground: "#e0e0e0", cursor: "#ffffff" };
const TINT = "#2a1f3d";
const OTHER_TINT = "#1f3d2a";

function makeDom(): RegistryDom {
  return { className: "", parentElement: null, remove: () => undefined };
}

/** theme 대입을 세는 터미널 — 접근자로 xterm의 options 표면을 흉내 낸다. */
function countingTerminal(initialTheme: unknown = undefined): { terminal: TerminalLike; assignments: unknown[] } {
  let theme: unknown = initialTheme;
  const assignments: unknown[] = [];
  const options = {
    fontSize: 13,
    get theme(): unknown {
      return theme;
    },
    set theme(value: unknown) {
      assignments.push(value);
      theme = value;
    },
  };
  const terminal: TerminalLike = {
    open: () => undefined,
    write: () => undefined,
    resize: () => undefined,
    dispose: () => undefined,
    onData: () => ({ dispose: () => undefined }),
    hasSelection: () => false,
    getSelection: () => "",
    attachCustomKeyEventHandler: () => undefined,
    loadAddon: () => undefined,
    element: null,
    options,
  };
  return { terminal, assignments };
}

describe("withBackground — (기본 테마, 색)별 같은 참조", () => {
  it("같은 입력은 같은 객체, 다른 색은 다른 객체를 돌려준다", () => {
    const first = withBackground(BASE, TINT);
    expect(withBackground(BASE, TINT)).toBe(first);
    expect(first).toMatchObject({ background: TINT, cursorAccent: TINT, cursor: "#ffffff" });
    expect(withBackground(BASE, OTHER_TINT)).not.toBe(first);
    // 같은 내용의 다른 base 객체는 다른 캐시다(참조 기준).
    expect(withBackground({ ...BASE }, TINT)).not.toBe(first);
  });

  it("이미 칠한 테마를 넘기면 원래 base에서 다시 계산하고, null은 base로 돌아간다", () => {
    const tinted = withBackground(BASE, TINT);
    expect(withBackground(tinted, TINT)).toBe(tinted);
    expect(withBackground(tinted, OTHER_TINT)).toBe(withBackground(BASE, OTHER_TINT));
    expect(withBackground(tinted, null)).toBe(BASE);
    expect(withBackground(BASE, null)).toBe(BASE);
  });

  it("base가 없어도(전역 테마 적용 전) 같은 색은 같은 참조다", () => {
    expect(withBackground(null, TINT)).toBe(withBackground(null, TINT));
    expect(withBackground(undefined, TINT)).toBe(withBackground(null, TINT));
    expect(withBackground(null, null)).toBeNull();
  });

  it("색을 많이 지나가도 base 하나당 기억은 유계다 — 오래 안 쓴 색부터 다시 만든다", () => {
    const base = { ...BASE };
    const first = withBackground(base, "#000001");
    for (let i = 2; i <= 40; i += 1) withBackground(base, `#0000${i.toString(16).padStart(2, "0")}`);
    // 처음 색은 밀려났다 — 다시 계산하면 새 객체지만 내용은 같다.
    const again = withBackground(base, "#000001");
    expect(again).not.toBe(first);
    expect(again).toEqual(first);
    // 방금 쓴 색은 그대로 기억한다.
    expect(withBackground(base, "#000001")).toBe(again);
  });
});

describe("TerminalRegistry — 같은 테마면 options.theme에 대입하지 않는다", () => {
  it("같은 색의 배경 덮어쓰기를 반복해도 한 번만 대입하고, null은 기본 테마로 되돌린다", () => {
    const { terminal, assignments } = countingTerminal();
    const registry = new TerminalRegistry({ createTerminal: () => terminal, createDom: makeDom });
    registry.acquire("v1");
    registry.applyGlobalPreferences({ theme: BASE });
    expect(assignments).toEqual([BASE]);

    registry.setBackgroundOverride("v1", TINT);
    registry.setBackgroundOverride("v1", TINT);
    registry.setBackgroundOverride("v1", TINT);
    expect(assignments).toHaveLength(2);
    expect(terminal.options?.theme).toBe(withBackground(BASE, TINT));

    registry.setBackgroundOverride("v1", null);
    expect(assignments).toHaveLength(3);
    expect(terminal.options?.theme).toBe(BASE);
    registry.disposeAll();
  });

  it("테마가 그대로인 전역 적용(글꼴·스크롤백 변경)은 테마를 다시 대입하지 않는다", () => {
    const { terminal, assignments } = countingTerminal();
    const registry = new TerminalRegistry({ createTerminal: () => terminal, createDom: makeDom });
    registry.acquire("v1");
    registry.applyGlobalPreferences({ theme: BASE });
    registry.setBackgroundOverride("v1", TINT);
    expect(assignments).toHaveLength(2);

    registry.applyGlobalPreferences({ theme: BASE, scrollback: 5000 });
    registry.applyGlobalPreferences({ theme: BASE, cursorStyle: "bar" });
    expect(assignments).toHaveLength(2);
    expect(terminal.options?.theme).toBe(withBackground(BASE, TINT));

    // 기본 테마가 바뀌면 그 위에 다시 얹는다(한 번).
    const light = { background: "#fafafa", foreground: "#202020" };
    registry.applyGlobalPreferences({ theme: light });
    expect(assignments).toHaveLength(3);
    expect(terminal.options?.theme).toBe(withBackground(light, TINT));
    registry.disposeAll();
  });

  it("전역 테마 적용 전에도 반복 적용이 멱등이고, null은 터미널의 원래 테마로 돌아간다", () => {
    const initial = { background: "#000000", foreground: "#ffffff" };
    const { terminal, assignments } = countingTerminal(initial);
    const registry = new TerminalRegistry({ createTerminal: () => terminal, createDom: makeDom });
    registry.acquire("v1");

    registry.setBackgroundOverride("v1", TINT);
    registry.setBackgroundOverride("v1", TINT);
    expect(assignments).toHaveLength(1);
    // 칠한 테마가 base 자리에 들어와도 원래 테마 위에서 다시 계산한다.
    registry.setBackgroundOverride("v1", OTHER_TINT);
    expect(terminal.options?.theme).toBe(withBackground(initial, OTHER_TINT));
    registry.setBackgroundOverride("v1", null);
    expect(terminal.options?.theme).toBe(initial);
    expect(assignments).toHaveLength(3);
    registry.disposeAll();
  });

  it("뷰가 생기기 전에 정한 덮어쓰기는 acquire 때 한 번만 대입한다", () => {
    const { terminal, assignments } = countingTerminal();
    const registry = new TerminalRegistry({ createTerminal: () => terminal, createDom: makeDom });
    registry.applyGlobalPreferences({ theme: BASE });
    registry.setBackgroundOverride("later", TINT);
    expect(assignments).toEqual([]);

    registry.acquire("later");
    expect(assignments).toHaveLength(1);
    expect(terminal.options?.theme).toBe(withBackground(BASE, TINT));
    // 같은 값을 다시 정해도 대입하지 않는다.
    registry.setBackgroundOverride("later", TINT);
    expect(assignments).toHaveLength(1);
    registry.disposeAll();
  });
});
