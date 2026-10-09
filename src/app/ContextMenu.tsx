/**
 * 범용 컨텍스트 메뉴 렌더러.
 *
 * 항목의 뜻은 모른다 — contextMenuTypes의 항목을 그리고, 고른 id만
 * onSelect로 돌려준다. 무엇을 보일지·활성 여부는 모델이 정한다.
 *
 * - 열리면 첫 항목이 초점을 받는다. 화살표·Home/End·Enter/Space·Esc·Tab,
 *   첫 글자 이동(type-ahead). 비활성 항목은 보이되 이동·선택에서 빠진다.
 * - 먼저 화면 왼쪽 위에 숨긴 채 그려 실제 크기를 재고, 커서 자리에서 화면
 *   밖으로 넘치면 커서 반대쪽으로 뒤집은 뒤 보인다.
 * - 바깥 mousedown·창 blur·resize·wheel이면 닫힌다. 앱 단축키(Cmd/Ctrl 조합)는
 *   흘려보내되 메뉴는 닫는다. 메뉴는 React 트리상 pane 안에 있으므로 메뉴
 *   안의 마우스 이벤트는 pane으로 새지 않게 막는다.
 * - onClose는 닫힌 까닭을 함께 알린다 — 부모가 초점을 어디로 돌려줄지 정한다.
 */

import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type Ref,
} from "react";
import { createPortal } from "react-dom";
import type { ContextMenuEntry, ContextMenuItem } from "./contextMenuTypes";

const usePrePaintEffect = typeof window === "undefined" ? useEffect : useLayoutEffect;

/** 화면 가장자리와 메뉴 사이 최소 여백(px). */
const MENU_MARGIN_PX = 8;

/** 수정키 단독 keydown — 조합을 누르는 도중이라 메뉴를 닫지 않는다. */
const MODIFIER_KEYS: ReadonlySet<string> = new Set(["Control", "Meta", "Shift", "Alt", "OS"]);

/**
 * 메뉴가 닫힌 까닭.
 * - select: 항목을 골랐다(onSelect가 곧이어 불린다 — 초점은 동작이 정한다).
 * - escape: Esc·Tab으로 물렸다(메뉴를 연 자리로 초점을 돌려줄 때).
 * - outside: 바깥 누름·창 blur·resize·wheel·앱 단축키(초점은 사용자가 옮긴 곳에 둔다).
 */
export type ContextMenuCloseReason = "select" | "escape" | "outside";

export interface ContextMenuProps {
  entries: readonly ContextMenuEntry[];
  /** viewport 좌표(clientX). */
  x: number;
  /** viewport 좌표(clientY). */
  y: number;
  ariaLabel: string;
  /** 항목을 고르면 onClose("select")가 먼저, 그다음 onSelect(id)가 불린다. */
  onSelect(id: string): void;
  onClose(reason: ContextMenuCloseReason): void;
}

/** 키보드로 옮겨 다닐 수 있는 항목(줄 안의 버튼 포함, 구분선·비활성 제외) — 보이는 순서. */
export function navigableItems(entries: readonly ContextMenuEntry[]): ContextMenuItem[] {
  const items: ContextMenuItem[] = [];
  for (const entry of entries) {
    if (entry.kind === "item") {
      if (!entry.disabled) items.push(entry);
    } else if (entry.kind === "row") {
      for (const item of entry.items) if (!item.disabled) items.push(item);
    }
  }
  return items;
}

/**
 * 한 칸 이동한 항목 id. 끝에서는 반대쪽 끝으로 돈다. 현재 값이 없거나 더는
 * 고를 수 없는 항목이면 delta 방향의 첫 항목(1 → 맨 앞, -1 → 맨 뒤).
 */
export function nextNavigableId(
  entries: readonly ContextMenuEntry[],
  currentId: string | null,
  delta: 1 | -1,
): string | null {
  const ids = navigableItems(entries).map((item) => item.id);
  if (ids.length === 0) return null;
  const index = currentId === null ? -1 : ids.indexOf(currentId);
  if (index < 0) return delta === 1 ? ids[0] : ids[ids.length - 1];
  return ids[(index + delta + ids.length) % ids.length];
}

/** 현재 항목 다음부터 돌며 label이 그 글자로 시작하는 첫 항목(대소문자 무시). */
export function typeAheadId(
  entries: readonly ContextMenuEntry[],
  currentId: string | null,
  char: string,
): string | null {
  const items = navigableItems(entries);
  if (items.length === 0 || char.length === 0) return null;
  const needle = char.toLocaleLowerCase();
  const start = currentId === null ? -1 : items.findIndex((item) => item.id === currentId);
  for (let step = 1; step <= items.length; step++) {
    const item = items[(start + step + items.length) % items.length];
    if (item.label.toLocaleLowerCase().startsWith(needle)) return item.id;
  }
  return null;
}

/**
 * 메뉴 왼쪽 위 좌표. 오른쪽으로 넘치면 커서 왼쪽에, 아래로 넘치면 커서 위에
 * 붙인 뒤, 어느 쪽이든 여백 안으로 당긴다(화면이 메뉴보다 작아도 여백 밑으로는
 * 가지 않는다 — 앞머리가 보여야 닫고 다시 열 수 있다).
 */
export function clampMenuPosition(
  anchor: { x: number; y: number },
  size: { width: number; height: number },
  viewport: { width: number; height: number },
  margin: number = MENU_MARGIN_PX,
): { left: number; top: number } {
  let left = anchor.x;
  let top = anchor.y;
  if (left + size.width > viewport.width - margin) left = anchor.x - size.width;
  if (top + size.height > viewport.height - margin) top = anchor.y - size.height;
  left = Math.max(margin, Math.min(left, viewport.width - size.width - margin));
  top = Math.max(margin, Math.min(top, viewport.height - size.height - margin));
  return { left, top };
}

export interface ContextMenuListProps extends ContextMenuProps {
  /** roving tabindex의 주인(초점을 가질 항목). */
  activeId: string | null;
  onActiveChange?(id: string | null): void;
  style?: CSSProperties;
  menuRef?: Ref<HTMLDivElement>;
}

/** 포털 없는 메뉴 마크업 — 상태는 props로 받는다(SSR로 검증 가능). */
export function ContextMenuList(props: ContextMenuListProps): JSX.Element {
  const { entries, ariaLabel, activeId, onActiveChange, style, menuRef, onSelect, onClose } = props;
  // 켜고 끄는 항목이 하나라도 있으면 모든 일반 항목에 체크 칸을 둬 글자 줄을 맞춘다.
  const checkColumn = entries.some((entry) => entry.kind === "item" && typeof entry.checked === "boolean");

  const activate = (item: ContextMenuItem) => {
    if (item.disabled) return;
    onClose("select");
    onSelect(item.id);
  };

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    let handled = true;
    switch (event.key) {
      case "ArrowDown":
      case "ArrowRight":
        onActiveChange?.(nextNavigableId(entries, activeId, 1));
        break;
      case "ArrowUp":
      case "ArrowLeft":
        onActiveChange?.(nextNavigableId(entries, activeId, -1));
        break;
      case "Home":
        onActiveChange?.(nextNavigableId(entries, null, 1));
        break;
      case "End":
        onActiveChange?.(nextNavigableId(entries, null, -1));
        break;
      case "Enter":
      case " ": {
        const item = navigableItems(entries).find((candidate) => candidate.id === activeId);
        if (item) activate(item);
        break;
      }
      case "Escape":
      case "Tab":
        onClose("escape");
        break;
      default:
        // 수정키 없는 글자 하나만 type-ahead다.
        if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
          const id = typeAheadId(entries, activeId, event.key);
          if (id) onActiveChange?.(id);
        } else {
          handled = false;
          // 앱 단축키(Cmd+D·Cmd+C 등)는 그대로 흘려보내되 메뉴는 닫는다 — 메뉴 뒤에서
          // 분할·붙여넣기가 일어나면 메뉴가 보여 주던 상태는 이미 낡았다.
          if ((event.ctrlKey || event.metaKey) && !MODIFIER_KEYS.has(event.key)) onClose("outside");
        }
    }
    if (handled) {
      event.preventDefault();
      event.stopPropagation();
    }
  };

  const itemButton = (item: ContextMenuItem, inRow: boolean) => {
    const checkable = typeof item.checked === "boolean";
    const className = [
      "context-menu-item",
      inRow ? "context-menu-row-button" : null,
      item.danger ? "danger" : null,
      item.disabled ? "disabled" : null,
    ]
      .filter(Boolean)
      .join(" ");
    // 줄 안의 작은 버튼은 단축키 칸이 없다 — 대신 툴팁에 이름과 단축키를 싣는다.
    const title = inRow
      ? item.detail ?? (item.shortcut ? `${item.ariaLabel ?? item.label} (${item.shortcut})` : item.ariaLabel)
      : item.detail;
    return (
      <button
        key={item.id}
        type="button"
        className={className}
        role={checkable ? "menuitemcheckbox" : "menuitem"}
        aria-checked={checkable ? item.checked : undefined}
        aria-disabled={item.disabled ? true : undefined}
        aria-label={item.ariaLabel}
        title={title}
        tabIndex={item.id === activeId ? 0 : -1}
        data-id={item.id}
        onMouseEnter={item.disabled ? undefined : () => onActiveChange?.(item.id)}
        onClick={() => activate(item)}
      >
        {!inRow && checkColumn ? (
          <span className="context-menu-check" aria-hidden="true">
            {item.checked ? "✓" : ""}
          </span>
        ) : null}
        <span className="context-menu-label">{item.label}</span>
        {!inRow && item.shortcut ? (
          <span className="context-menu-shortcut" aria-hidden="true">
            {item.shortcut}
          </span>
        ) : null}
      </button>
    );
  };

  return (
    <div
      ref={menuRef}
      className="context-menu"
      role="menu"
      aria-label={ariaLabel}
      tabIndex={-1}
      style={style}
      onKeyDown={onKeyDown}
      onMouseDown={(event) => event.stopPropagation()}
      onClick={(event) => event.stopPropagation()}
      onContextMenu={(event) => {
        event.preventDefault();
        event.stopPropagation();
      }}
    >
      {entries.map((entry) => {
        if (entry.kind === "separator") {
          return <div key={entry.id} className="context-menu-separator" role="separator" />;
        }
        if (entry.kind === "row") {
          return (
            <div key={entry.id} className="context-menu-row" role="group" aria-label={entry.label}>
              <span className="context-menu-row-label">{entry.label}</span>
              <div className="context-menu-row-buttons">{entry.items.map((item) => itemButton(item, true))}</div>
            </div>
          );
        }
        return itemButton(entry, false);
      })}
    </div>
  );
}

/** document.body에 포털로 띄우는 메뉴. 브라우저가 아니면(SSR·node) 아무것도 그리지 않는다. */
export function ContextMenu(props: ContextMenuProps): JSX.Element | null {
  const { entries, x, y } = props;
  const menuRef = useRef<HTMLDivElement>(null);
  const [activeId, setActiveId] = useState<string | null>(() => nextNavigableId(entries, null, 1));
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  // 부모가 매 렌더마다 새 onClose를 넘겨도 바깥 리스너를 다시 달지 않는다.
  const onCloseRef = useRef(props.onClose);
  onCloseRef.current = props.onClose;

  // 항목이 바뀌어 활성 항목이 사라지거나 비활성이 되면 첫 항목으로.
  useEffect(() => {
    setActiveId((current) =>
      current !== null && navigableItems(entries).some((item) => item.id === current)
        ? current
        : nextNavigableId(entries, null, 1),
    );
  }, [entries]);

  // 크기를 재서 자리를 정한다. 첫 렌더는 (0,0)에 숨겨 두어 커서가 오른쪽
  // 가장자리 근처일 때 폭이 줄어든 채로 재지 않게 한다.
  usePrePaintEffect(() => {
    const menu = menuRef.current;
    if (!menu) return;
    const rect = menu.getBoundingClientRect();
    const next = clampMenuPosition(
      { x, y },
      { width: rect.width, height: rect.height },
      { width: window.innerWidth, height: window.innerHeight },
    );
    setPosition((previous) =>
      previous && previous.left === next.left && previous.top === next.top ? previous : next,
    );
  }, [x, y, entries]);

  // roving tabindex: 활성 항목이 DOM 초점도 가진다. 숨은 동안은 초점을 줄 수
  // 없으므로 자리가 정해진 뒤에 옮긴다. 고를 항목이 없으면 메뉴 자체(Esc용).
  usePrePaintEffect(() => {
    const menu = menuRef.current;
    if (!menu || !position) return;
    const target =
      activeId === null
        ? menu
        : Array.from(menu.querySelectorAll<HTMLElement>("[data-id]")).find((el) => el.dataset.id === activeId) ??
          menu;
    if (document.activeElement !== target) target.focus({ preventScroll: true });
  }, [activeId, position]);

  useEffect(() => {
    const inside = (target: EventTarget | null) =>
      typeof Node !== "undefined" && target instanceof Node && menuRef.current?.contains(target) === true;
    const onOutsidePointer = (event: Event) => {
      if (!inside(event.target)) onCloseRef.current("outside");
    };
    const onWindowChange = () => onCloseRef.current("outside");
    // capture: 바깥 요소가 mousedown을 삼켜도(pane의 xterm 등) 먼저 닫힌다.
    // 오른쪽 클릭으로 다른 자리를 누르면 여기서 닫히고, 그 자리의
    // contextmenu가 새 메뉴를 연다.
    window.addEventListener("mousedown", onOutsidePointer, true);
    window.addEventListener("wheel", onOutsidePointer, { capture: true, passive: true });
    window.addEventListener("blur", onWindowChange);
    window.addEventListener("resize", onWindowChange);
    return () => {
      window.removeEventListener("mousedown", onOutsidePointer, true);
      window.removeEventListener("wheel", onOutsidePointer, true);
      window.removeEventListener("blur", onWindowChange);
      window.removeEventListener("resize", onWindowChange);
    };
  }, []);

  if (typeof document === "undefined") return null;
  const style: CSSProperties = position
    ? { left: position.left, top: position.top }
    : { left: 0, top: 0, visibility: "hidden" };
  return createPortal(
    <ContextMenuList {...props} activeId={activeId} onActiveChange={setActiveId} style={style} menuRef={menuRef} />,
    document.body,
  );
}
