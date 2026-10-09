/**
 * 범용 컨텍스트 메뉴: 마크업(역할·체크·비활성·구분선·줄 버튼·단축키 칸),
 * 키보드 이동 대상 계산, 화면 가장자리 위치 보정.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { ContextMenuEntry } from "./contextMenuTypes";
import {
  ContextMenu,
  ContextMenuList,
  clampMenuPosition,
  navigableItems,
  nextNavigableId,
  typeAheadId,
} from "./ContextMenu";

const noop = () => undefined;

const entries: ContextMenuEntry[] = [
  { kind: "item", id: "copy", label: "복사", shortcut: "⌘C", disabled: true },
  { kind: "item", id: "paste", label: "붙여넣기", shortcut: "⌘V" },
  { kind: "separator", id: "sep-1" },
  {
    kind: "row",
    id: "font",
    label: "글꼴 크기 100%",
    items: [
      { kind: "item", id: "zoom-out", label: "−", ariaLabel: "작게", shortcut: "⌘-" },
      { kind: "item", id: "zoom-reset", label: "기본", disabled: true },
      { kind: "item", id: "zoom-in", label: "+", ariaLabel: "크게" },
    ],
  },
  { kind: "separator", id: "sep-2" },
  { kind: "item", id: "broadcast", label: "동시 입력", checked: true },
  { kind: "item", id: "close", label: "창 닫기", danger: true, detail: "이 창을 닫습니다" },
];

function render(list: readonly ContextMenuEntry[] = entries, activeId: string | null = "paste"): string {
  return renderToStaticMarkup(
    <ContextMenuList
      entries={list}
      x={10}
      y={10}
      ariaLabel="터미널 메뉴"
      activeId={activeId}
      onSelect={noop}
      onClose={noop}
    />,
  );
}

function buttonFor(html: string, id: string): string {
  const match = html.match(new RegExp(`<button[^>]*data-id="${id}"[^>]*>.*?</button>`));
  expect(match, `button ${id}`).not.toBeNull();
  return match?.[0] ?? "";
}

describe("ContextMenuList markup", () => {
  it("renders a labelled menu root", () => {
    const html = render();
    expect(html.startsWith('<div class="context-menu" role="menu" aria-label="터미널 메뉴"')).toBe(true);
  });

  it("marks plain, checkable and disabled items with the right roles", () => {
    const html = render();
    const paste = buttonFor(html, "paste");
    expect(paste).toContain('role="menuitem"');
    expect(paste).not.toContain("aria-checked");
    expect(paste).not.toContain("aria-disabled");

    const copy = buttonFor(html, "copy");
    expect(copy).toContain('class="context-menu-item disabled"');
    expect(copy).toContain('aria-disabled="true"');

    const broadcast = buttonFor(html, "broadcast");
    expect(broadcast).toContain('role="menuitemcheckbox"');
    expect(broadcast).toContain('aria-checked="true"');
    expect(broadcast).toContain('<span class="context-menu-check" aria-hidden="true">✓</span>');

    const unchecked = buttonFor(render([{ kind: "item", id: "b", label: "B", checked: false }]), "b");
    expect(unchecked).toContain('aria-checked="false"');
    expect(unchecked).toContain('<span class="context-menu-check" aria-hidden="true"></span>');
  });

  it("renders separators and the shortcut column only when a shortcut exists", () => {
    const html = render();
    expect(html.match(/<div class="context-menu-separator" role="separator"><\/div>/g)).toHaveLength(2);
    expect(buttonFor(html, "paste")).toContain('<span class="context-menu-shortcut" aria-hidden="true">⌘V</span>');
    expect(buttonFor(html, "broadcast")).not.toContain("context-menu-shortcut");
  });

  it("aligns labels with a check column only when some item is checkable", () => {
    // 체크 가능한 항목이 있으면 일반 항목에도 빈 체크 칸이 붙는다.
    expect(buttonFor(render(), "paste")).toContain('<span class="context-menu-check" aria-hidden="true"></span>');
    const plain: ContextMenuEntry[] = [
      { kind: "item", id: "a", label: "A" },
      { kind: "item", id: "b", label: "B" },
    ];
    expect(render(plain, "a")).not.toContain("context-menu-check");
  });

  it("renders rows as a labelled group of compact buttons without check or shortcut columns", () => {
    const html = render();
    expect(html).toContain(
      '<div class="context-menu-row" role="group" aria-label="글꼴 크기 100%"><span class="context-menu-row-label">글꼴 크기 100%</span><div class="context-menu-row-buttons">',
    );
    const zoomOut = buttonFor(html, "zoom-out");
    expect(zoomOut).toContain('class="context-menu-item context-menu-row-button"');
    expect(zoomOut).toContain('aria-label="작게"');
    expect(zoomOut).toContain('title="작게 (⌘-)"');
    expect(zoomOut).not.toContain("context-menu-check");
    expect(zoomOut).not.toContain("context-menu-shortcut");
    expect(buttonFor(html, "zoom-reset")).toContain('class="context-menu-item context-menu-row-button disabled"');
  });

  it("marks danger items and exposes detail as a tooltip", () => {
    const close = buttonFor(render(), "close");
    expect(close).toContain('class="context-menu-item danger"');
    expect(close).toContain('title="이 창을 닫습니다"');
  });

  it("gives only the active item a tab stop", () => {
    const html = render(entries, "paste");
    expect(buttonFor(html, "paste")).toContain('tabindex="0"');
    expect(buttonFor(html, "close")).toContain('tabindex="-1"');
    expect(html.match(/tabindex="0"/g)).toHaveLength(1);
  });

  it("portal wrapper renders nothing outside the browser", () => {
    expect(
      renderToStaticMarkup(<ContextMenu entries={entries} x={0} y={0} ariaLabel="m" onSelect={noop} onClose={noop} />),
    ).toBe("");
  });
});

describe("keyboard navigation targets", () => {
  it("flattens rows and skips separators and disabled items", () => {
    expect(navigableItems(entries).map((item) => item.id)).toEqual(["paste", "zoom-out", "zoom-in", "broadcast", "close"]);
  });

  it("moves one step, wraps at both ends, and starts from the edge without a current item", () => {
    expect(nextNavigableId(entries, null, 1)).toBe("paste");
    expect(nextNavigableId(entries, null, -1)).toBe("close");
    expect(nextNavigableId(entries, "paste", 1)).toBe("zoom-out");
    expect(nextNavigableId(entries, "zoom-in", 1)).toBe("broadcast");
    expect(nextNavigableId(entries, "close", 1)).toBe("paste");
    expect(nextNavigableId(entries, "paste", -1)).toBe("close");
    // 비활성·모르는 id는 현재 항목이 없는 것과 같다.
    expect(nextNavigableId(entries, "copy", 1)).toBe("paste");
  });

  it("returns null when nothing can be navigated", () => {
    expect(nextNavigableId([], null, 1)).toBeNull();
    expect(nextNavigableId([{ kind: "separator", id: "s" }, { kind: "item", id: "x", label: "X", disabled: true }], null, -1)).toBeNull();
  });

  it("type-ahead finds the next item starting with the letter, cycling past the current one", () => {
    const list: ContextMenuEntry[] = [
      { kind: "item", id: "copy", label: "Copy" },
      { kind: "item", id: "paste", label: "Paste" },
      { kind: "item", id: "clear", label: "Clear" },
      { kind: "item", id: "close", label: "close", disabled: true },
    ];
    expect(typeAheadId(list, null, "c")).toBe("copy");
    expect(typeAheadId(list, "copy", "C")).toBe("clear");
    expect(typeAheadId(list, "clear", "c")).toBe("copy");
    expect(typeAheadId(list, null, "z")).toBeNull();
  });
});

describe("clampMenuPosition", () => {
  const viewport = { width: 1000, height: 800 };
  const size = { width: 200, height: 300 };

  it("keeps the cursor position when the menu fits", () => {
    expect(clampMenuPosition({ x: 100, y: 100 }, size, viewport)).toEqual({ left: 100, top: 100 });
  });

  it("opens to the left of the cursor near the right edge", () => {
    expect(clampMenuPosition({ x: 950, y: 100 }, size, viewport)).toEqual({ left: 750, top: 100 });
  });

  it("flips above the cursor near the bottom edge", () => {
    expect(clampMenuPosition({ x: 100, y: 700 }, size, viewport)).toEqual({ left: 100, top: 400 });
  });

  it("clamps back inside when the cursor sits in the edge margin", () => {
    // 뒤집은 자리(96)가 여백 안쪽 한계(300-200-8=92)를 넘으면 한계로 당긴다.
    expect(clampMenuPosition({ x: 296, y: 396 }, size, { width: 300, height: 400 })).toEqual({ left: 92, top: 92 });
    // 뒤집은 자리가 음수면 여백으로.
    expect(clampMenuPosition({ x: 150, y: 250 }, size, { width: 300, height: 400 })).toEqual({ left: 8, top: 8 });
  });

  it("never goes above or left of the margin on a tiny viewport", () => {
    expect(clampMenuPosition({ x: 50, y: 50 }, { width: 220, height: 400 }, { width: 100, height: 120 })).toEqual({
      left: 8,
      top: 8,
    });
  });
});
