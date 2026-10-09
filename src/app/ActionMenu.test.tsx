/**
 * 컨텍스트 메뉴(탭·pane 공용) 마크업·좌표 계약.
 *
 * node 환경이라 초점 이동·바깥 클릭 닫기는 여기서 확인할 수 없다(브라우저
 * 이벤트). 대신 "무엇이 그려지고 무엇이 꺼져 있는가"와, 화면 밖으로 나가지
 * 않게 자르는 계산을 본다 — 항목 수가 늘어난 메뉴가 아래로 잘려 나가던
 * 자리(하드코딩된 높이 116)를 다시 만들지 않기 위한 회귀 시험이다.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import {
  ACTION_MENU_ITEM_PX,
  ACTION_MENU_PADDING_PX,
  ACTION_MENU_WIDTH_PX,
  ActionMenu,
  actionMenuPosition,
  menuKeyAction,
  menuTypeAheadIndex,
  navigableMenuIndexes,
  nextMenuIndex,
  type ActionMenuItem,
} from "./ActionMenu";

const noop = () => undefined;

const items: ActionMenuItem[] = [
  { label: "이름 바꾸기", onSelect: noop },
  { label: "왼쪽으로 이동", onSelect: noop, disabled: true },
  { label: "모두 닫기", onSelect: noop, danger: true },
];

function render(): string {
  return renderToStaticMarkup(
    <ActionMenu x={12} y={34} label="탭 메뉴" items={items} onClose={noop} />,
  );
}

describe("ActionMenu markup", () => {
  it("메뉴 역할·이름과 항목을 순서대로 낸다", () => {
    const html = render();
    expect(html).toContain('role="menu"');
    expect(html).toContain('aria-label="탭 메뉴"');
    expect(html.match(/role="menuitem"/g)).toHaveLength(3);
    expect(html.indexOf("이름 바꾸기")).toBeLessThan(html.indexOf("왼쪽으로 이동"));
    expect(html.indexOf("왼쪽으로 이동")).toBeLessThan(html.indexOf("모두 닫기"));
  });

  it("좌표는 style로, 되돌릴 수 없는 항목은 danger로 표시한다", () => {
    const html = render();
    expect(html).toContain("left:12px");
    expect(html).toContain("top:34px");
    expect(html).toContain('class="danger"');
  });

  it("비활성 항목은 aria-disabled와 disabled 둘 다 — 보조기술에도, 초점 순회에도 빠진다", () => {
    const html = render();
    expect(html).toMatch(/왼쪽으로 이동/);
    expect(html).toContain('aria-disabled="true"');
    expect(html).toContain("disabled=\"\"");
    // 고를 수 있는 항목에는 붙지 않는다.
    expect(html.match(/aria-disabled="true"/g)).toHaveLength(1);
  });
});

describe("keyboard navigation targets", () => {
  // 초점 이동 자체는 브라우저 이벤트라 node에서 확인할 수 없다 — 대신 어느 항목으로
  // 옮기고 어느 항목을 실행할지의 규칙(navigableMenuIndexes·menuKeyAction)을 본다.
  // 규칙은 ContextMenu(04-ui.md §3-1)와 같다: 화살표·Home/End·Enter/Space·첫 글자
  // 이동·Esc/Tab, 비활성 항목은 이동·선택에서 빠진다.
  const menu: ActionMenuItem[] = [
    { label: "Copy", onSelect: noop },
    { label: "Paste", onSelect: noop, disabled: true },
    { label: "Clear", onSelect: noop },
    { label: "close", onSelect: noop, danger: true },
  ];

  it("이동 대상 목록은 비활성 항목을 뺀다", () => {
    expect(navigableMenuIndexes(menu)).toEqual([0, 2, 3]);
    expect(navigableMenuIndexes([{ label: "X", onSelect: noop, disabled: true }])).toEqual([]);
  });

  it("한 칸씩 움직이고 양 끝에서는 반대쪽 끝으로 돈다 — 현재 항목이 없으면 방향의 끝에서 시작", () => {
    expect(nextMenuIndex(menu, null, 1)).toBe(0);
    expect(nextMenuIndex(menu, null, -1)).toBe(3);
    expect(nextMenuIndex(menu, 0, 1)).toBe(2);
    expect(nextMenuIndex(menu, 3, 1)).toBe(0);
    expect(nextMenuIndex(menu, 0, -1)).toBe(3);
    // 비활성·모르는 index는 현재 항목이 없는 것과 같다.
    expect(nextMenuIndex(menu, 1, 1)).toBe(0);
    expect(nextMenuIndex(menu, 99, 1)).toBe(0);
  });

  it("옮겨 다닐 항목이 없으면 null을 돌려준다", () => {
    expect(nextMenuIndex([], null, 1)).toBeNull();
  });

  it("첫 글자 이동은 그 글자로 시작하는 다음 항목을 찾아 현재 항목을 지나 돈다", () => {
    expect(menuTypeAheadIndex(menu, null, "c")).toBe(0);
    expect(menuTypeAheadIndex(menu, 0, "C")).toBe(2);
    expect(menuTypeAheadIndex(menu, 2, "c")).toBe(3);
    expect(menuTypeAheadIndex(menu, 3, "c")).toBe(0);
    // 비활성 항목(Paste)은 후보에서 빠진다.
    expect(menuTypeAheadIndex(menu, null, "p")).toBeNull();
    expect(menuTypeAheadIndex(menu, null, "z")).toBeNull();
  });

  it("키마다 반응을 정한다 — 화살표·Home/End는 이동, Enter/Space는 실행, Esc/Tab은 닫기", () => {
    expect(menuKeyAction(menu, 0, "ArrowDown")).toEqual({ kind: "focus", index: 2 });
    expect(menuKeyAction(menu, 2, "ArrowUp")).toEqual({ kind: "focus", index: 0 });
    expect(menuKeyAction(menu, 2, "Home")).toEqual({ kind: "focus", index: 0 });
    expect(menuKeyAction(menu, 0, "End")).toEqual({ kind: "focus", index: 3 });
    expect(menuKeyAction(menu, 2, "Enter")).toEqual({ kind: "activate", index: 2 });
    expect(menuKeyAction(menu, 0, " ")).toEqual({ kind: "activate", index: 0 });
    expect(menuKeyAction(menu, 0, "Escape")).toEqual({ kind: "close" });
    expect(menuKeyAction(menu, 0, "Tab")).toEqual({ kind: "close" });
    // 첫 글자 이동 — 수정키 없는 글자 하나뿐이다.
    expect(menuKeyAction(menu, 0, "c")).toEqual({ kind: "focus", index: 2 });
    expect(menuKeyAction(menu, 0, "z")).toEqual({ kind: "unhandled" });
    expect(menuKeyAction(menu, 0, "c", { ctrlKey: true })).toEqual({ kind: "unhandled" });
  });

  it("비활성 항목이나 없는 항목에서는 실행하지 않는다", () => {
    expect(menuKeyAction(menu, 1, "Enter")).toEqual({ kind: "unhandled" });
    expect(menuKeyAction(menu, null, " ")).toEqual({ kind: "unhandled" });
  });
});

describe("actionMenuPosition", () => {
  const viewport = { width: 800, height: 600 };

  it("클릭 지점을 그대로 쓰되 가장자리 여백을 지킨다", () => {
    expect(actionMenuPosition(120, 80, 3, viewport)).toEqual({ x: 120, y: 80 });
    expect(actionMenuPosition(1, 2, 3, viewport)).toEqual({ x: 8, y: 8 });
  });

  it("오른쪽·아래로는 메뉴 크기만큼 당겨 화면 안에 둔다", () => {
    const at = actionMenuPosition(795, 595, 6, viewport);
    expect(at.x).toBe(viewport.width - ACTION_MENU_WIDTH_PX);
    expect(at.y).toBe(viewport.height - (6 * ACTION_MENU_ITEM_PX + ACTION_MENU_PADDING_PX));
  });

  it("높이는 항목 수를 따른다 — 항목이 늘면 더 위에서 연다", () => {
    const short = actionMenuPosition(400, 595, 3, viewport);
    const tall = actionMenuPosition(400, 595, 6, viewport);
    expect(tall.y).toBeLessThan(short.y);
    expect(short.y - tall.y).toBe(3 * ACTION_MENU_ITEM_PX);
  });
});
