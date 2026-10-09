/**
 * 포털 컨텍스트 메뉴(04-ui.md §2-5) — 탭 메뉴와 pane 메뉴가 함께 쓴다.
 *
 * body로 포털을 내보내는 이유는 탭 바·pane 헤더 둘 다 `overflow: hidden`이라
 * 제자리에 그리면 메뉴가 잘리기 때문이다. 바깥을 누르거나 창이 흐려지거나
 * 크기가 바뀌면 닫는다 — 열려 있는 메뉴가 좌표만 맞지 않은 채 떠 있는 편이
 * 사라지는 것보다 나쁘다.
 *
 * 항목은 데이터(items)로 받는다: 열기 조건·비활성 사유는 호출한 쪽이 알고,
 * 여기서는 초점·키보드 이동·닫기만 책임진다. 키는 04-ui.md §3-1의 메뉴 규칙을
 * 따른다 — 화살표·Home/End·Enter/Space·첫 글자 이동·Esc/Tab.
 */

import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";

export interface ActionMenuItem {
  label: string;
  onSelect: () => void;
  /** 되돌릴 수 없는 항목(모두 닫기 등) — 빨간 글자. */
  danger?: boolean;
  /** 지금은 고를 수 없는 항목 — 자리는 지키되 초점·선택에서 빠진다. */
  disabled?: boolean;
  /** 왜 흐린지·무엇을 하는지 한 줄 설명(툴팁). 없으면 붙이지 않는다. */
  title?: string;
}

export interface ActionMenuProps {
  x: number;
  y: number;
  /** 메뉴 자체의 aria-label(어떤 대상의 메뉴인지). */
  label: string;
  items: ActionMenuItem[];
  onClose: () => void;
}

/** 화면 밖으로 나가지 않게 자를 때 쓰는 어림 크기(실측 대신 상수 — 열자마자 정해져야 한다). */
export const ACTION_MENU_WIDTH_PX = 188;
export const ACTION_MENU_ITEM_PX = 30;
export const ACTION_MENU_PADDING_PX = 12;
const VIEWPORT_MARGIN_PX = 8;

/**
 * 클릭 지점을 뷰포트 안으로 자른 메뉴 좌표. 높이는 항목 수로 계산한다 —
 * 항목이 늘어난 메뉴(pane/탭)가 아래로 잘려 나가지 않게.
 */
export function actionMenuPosition(
  clientX: number,
  clientY: number,
  itemCount: number,
  viewport: { width: number; height: number },
): { x: number; y: number } {
  const height = itemCount * ACTION_MENU_ITEM_PX + ACTION_MENU_PADDING_PX;
  return {
    x: Math.max(VIEWPORT_MARGIN_PX, Math.min(clientX, viewport.width - ACTION_MENU_WIDTH_PX)),
    y: Math.max(VIEWPORT_MARGIN_PX, Math.min(clientY, viewport.height - height)),
  };
}

/** 지금 고를 수 있는 항목들(비활성은 disabled라 선택자에서 빠진다). */
function enabledItems(root: HTMLElement | null): HTMLButtonElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]:not(:disabled)'));
}

/** 키보드로 옮겨 다닐 수 있는 항목 index(비활성은 이동·선택에서 빠진다) — 보이는 순서. */
export function navigableMenuIndexes(items: readonly ActionMenuItem[]): number[] {
  const indexes: number[] = [];
  for (let index = 0; index < items.length; index += 1) {
    if (!items[index].disabled) indexes.push(index);
  }
  return indexes;
}

/**
 * 한 칸 이동한 항목 index. 끝에서는 반대쪽 끝으로 돈다. 현재 값이 없거나 더는
 * 고를 수 없는 항목이면 delta 방향의 첫 항목(1 → 맨 앞, -1 → 맨 뒤) —
 * ContextMenu.nextNavigableId와 같은 규칙이다.
 */
export function nextMenuIndex(
  items: readonly ActionMenuItem[],
  currentIndex: number | null,
  delta: 1 | -1,
): number | null {
  const indexes = navigableMenuIndexes(items);
  if (indexes.length === 0) return null;
  const position = currentIndex === null ? -1 : indexes.indexOf(currentIndex);
  if (position < 0) return delta === 1 ? indexes[0] : indexes[indexes.length - 1];
  return indexes[(position + delta + indexes.length) % indexes.length];
}

/** 현재 항목 다음부터 돌며 label이 그 글자로 시작하는 첫 항목(대소문자 무시). */
export function menuTypeAheadIndex(
  items: readonly ActionMenuItem[],
  currentIndex: number | null,
  char: string,
): number | null {
  const indexes = navigableMenuIndexes(items);
  if (indexes.length === 0 || char.length === 0) return null;
  const needle = char.toLocaleLowerCase();
  const position = currentIndex === null ? -1 : indexes.indexOf(currentIndex);
  for (let step = 1; step <= indexes.length; step += 1) {
    const index = indexes[(position + step + indexes.length) % indexes.length];
    if (items[index].label.toLocaleLowerCase().startsWith(needle)) return index;
  }
  return null;
}

/** 키 하나에 대한 메뉴의 반응 — focus는 초점 이동, activate는 항목 실행, close는 닫기, unhandled는 흘려보내기. */
export type MenuKeyAction =
  | { kind: "focus"; index: number }
  | { kind: "activate"; index: number }
  | { kind: "close" }
  | { kind: "unhandled" };

const UNHANDLED: MenuKeyAction = { kind: "unhandled" };

function focusOrUnhandled(index: number | null): MenuKeyAction {
  return index === null ? UNHANDLED : { kind: "focus", index };
}

/**
 * 메뉴 키보드 규칙(04-ui.md §3-1): 화살표·Home/End·Enter/Space·첫 글자 이동·
 * Esc/Tab. 반응을 값으로 돌려주어 node 시험에서도 키 규칙을 확인할 수 있게 한다.
 */
export function menuKeyAction(
  items: readonly ActionMenuItem[],
  currentIndex: number | null,
  key: string,
  modifiers: { ctrlKey?: boolean; metaKey?: boolean; altKey?: boolean } = {},
): MenuKeyAction {
  switch (key) {
    case "ArrowDown":
      return focusOrUnhandled(nextMenuIndex(items, currentIndex, 1));
    case "ArrowUp":
      return focusOrUnhandled(nextMenuIndex(items, currentIndex, -1));
    case "Home":
      return focusOrUnhandled(nextMenuIndex(items, null, 1));
    case "End":
      return focusOrUnhandled(nextMenuIndex(items, null, -1));
    case "Enter":
    case " ": {
      if (currentIndex === null) return UNHANDLED;
      const item = items[currentIndex];
      return item && !item.disabled ? { kind: "activate", index: currentIndex } : UNHANDLED;
    }
    case "Escape":
    case "Tab":
      return { kind: "close" };
    default:
      // 수정키 없는 글자 하나만 첫 글자 이동이다.
      if (key.length === 1 && !modifiers.ctrlKey && !modifiers.metaKey && !modifiers.altKey) {
        return focusOrUnhandled(menuTypeAheadIndex(items, currentIndex, key));
      }
      return UNHANDLED;
  }
}

export function ActionMenu({ x, y, label, items, onClose }: ActionMenuProps): JSX.Element | null {
  const menuRef = useRef<HTMLDivElement>(null);
  // 닫기 콜백은 호출부에서 인라인으로 만들어지는 일이 잦다 — ref로 받아
  // 아래 효과가 매 렌더마다 다시 붙어 초점을 첫 항목으로 되돌리지 않게 한다.
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  });

  useEffect(() => {
    enabledItems(menuRef.current)[0]?.focus();
    const dismiss = () => onCloseRef.current();
    const dismissOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") dismiss();
    };
    window.addEventListener("mousedown", dismiss);
    window.addEventListener("blur", dismiss);
    window.addEventListener("resize", dismiss);
    window.addEventListener("keydown", dismissOnEscape);
    return () => {
      window.removeEventListener("mousedown", dismiss);
      window.removeEventListener("blur", dismiss);
      window.removeEventListener("resize", dismiss);
      window.removeEventListener("keydown", dismissOnEscape);
    };
  }, []);

  const surface = (
    <div
      ref={menuRef}
      className="tab-context-menu"
      role="menu"
      aria-label={label}
      style={{ left: x, top: y }}
      // 메뉴 안의 mousedown이 window까지 올라가면 자기 자신을 닫는다.
      onMouseDown={(event) => event.stopPropagation()}
      onClick={(event) => event.stopPropagation()}
      onKeyDown={(event) => {
        // 규칙은 항목 index로 굴린다 — 렌더 순서와 버튼 순서가 같으므로 그대로 초점에 옮긴다.
        const buttons = Array.from(
          menuRef.current?.querySelectorAll<HTMLButtonElement>('button[role="menuitem"]') ?? [],
        );
        const active = buttons.indexOf(document.activeElement as HTMLButtonElement);
        const action = menuKeyAction(items, active >= 0 ? active : null, event.key, event);
        if (action.kind === "unhandled") return;
        // 다룬 키는 삼킨다 — Esc가 창 단위 닫기 리스너까지 흘러 두 번 닫게 하지 않기 위해서다.
        event.preventDefault();
        event.stopPropagation();
        if (action.kind === "focus") {
          buttons[action.index]?.focus();
        } else if (action.kind === "activate") {
          const item = items[action.index];
          if (item) {
            // 클릭과 같은 차례 — 먼저 닫는다.
            onClose();
            item.onSelect();
          }
        } else {
          onClose();
        }
      }}
    >
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          className={item.danger ? "danger" : undefined}
          title={item.title}
          // 흐린 표시는 :disabled가 맡는다 — aria-disabled는 보조기술용 중복 선언.
          disabled={item.disabled}
          aria-disabled={item.disabled ? true : undefined}
          onClick={() => {
            // 먼저 닫는다 — 동작이 모달을 열면 그 위에 메뉴가 남으면 안 된다.
            onClose();
            item.onSelect();
          }}
        >
          {item.label}
        </button>
      ))}
    </div>
  );

  // document가 없는 환경(node 시험·SSR)에서는 포털 없이 그대로 낸다 —
  // 마크업 계약을 그 환경에서도 확인할 수 있어야 한다.
  if (typeof document === "undefined") return surface;
  return createPortal(surface, document.body);
}
