/**
 * 컨텍스트 메뉴 항목 계약.
 *
 * 무엇을 보일지(모델 — 예: terminal/paneContextMenu.ts)와 어떻게 보이고
 * 조작되는지(렌더러 — app/ContextMenu.tsx)를 나눈다. 렌더러는 항목의 뜻을
 * 모르고, 고른 항목의 id만 돌려준다.
 */

export interface ContextMenuItem {
  kind: "item";
  /** 선택 시 onSelect로 돌려주는 id — 메뉴 안에서 유일하다. */
  id: string;
  /** 보이는 문구. */
  label: string;
  /** 글자 대신 기호(−·+)를 보일 때의 접근성 이름. 없으면 label. */
  ariaLabel?: string;
  /** 오른쪽에 흐리게 붙는 단축키 표시(없으면 표시하지 않는다). */
  shortcut?: string | null;
  /** 지금은 할 수 없는 동작 — 보이되 고를 수 없고, 키보드 이동도 건너뛴다. */
  disabled?: boolean;
  /** boolean이면 켜고 끄는 항목(menuitemcheckbox). 생략하면 일반 항목. */
  checked?: boolean;
  /** 되돌리기 어려운 동작(창 닫기 등) — 경고색. */
  danger?: boolean;
  /** 긴 값(링크·경로·명령)의 전체 텍스트 — 툴팁. */
  detail?: string;
}

export interface ContextMenuSeparator {
  kind: "separator";
  id: string;
}

/**
 * 한 줄에 나란히 놓는 작은 버튼 묶음(예: 글꼴 크기 − 기본 +). 왼쪽에 label,
 * 오른쪽에 버튼들. 키보드 이동은 줄 단위가 아니라 버튼마다 한 칸씩이다.
 */
export interface ContextMenuRow {
  kind: "row";
  id: string;
  label: string;
  items: ContextMenuItem[];
}

export type ContextMenuEntry = ContextMenuItem | ContextMenuSeparator | ContextMenuRow;
