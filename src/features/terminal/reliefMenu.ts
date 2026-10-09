/**
 * pane 메뉴의 수동 압력 완화 항목(08-pressure-relief §2).
 *
 * 무엇을 보일지만 정하는 순수 함수다 — 실행은 runPaneMenuAction이, 데몬 호출은
 * 컨트롤러가 맡는다(paneContextMenu 모델과 같은 나눔). 지금 할 수 있는 것만
 * 보인다: 양보 중이면 "양보 해제", 아니면(보호 중이 아닐 때) "이 세션 양보",
 * 그리고 보호 표시를 켜고 끄는 항목 하나. 되돌릴 수 없는 플랫폼(08 §0-4)에서는
 * 항목을 숨기지 않고 흐리게 두고 데몬이 준 사유를 툴팁에 싣는다 — 왜 못 하는지가
 * 사라지지 않게. 세션이 아직(또는 이미) 없는 pane에는 완화할 프로세스가 없다.
 */

import type { ActionMenuItem } from "../../app/ActionMenu";
import type { LimitSupport } from "../../generated/LimitSupport";
import type { PaneMeta } from "../../store/workbenchStore";
import {
  EMPTY_PANE_MENU_VALUES,
  runPaneMenuAction,
  type PaneMenuController,
  type PaneReliefMenuAction,
} from "./paneMenuActions";

/** snapshot capabilities의 `scheduling_yield` 거울(구 데몬은 둘 다 null). */
export interface ReliefCapability {
  support: LimitSupport | null;
  reason: string | null;
}

type Translate = (key: string, params?: Record<string, string | number>) => string;

export function reliefMenuItems(
  controller: PaneMenuController,
  leafId: string,
  pane: PaneMeta,
  capability: ReliefCapability,
  t: Translate,
): ActionMenuItem[] {
  if (!pane.sessionId) return [];
  const yielded = (pane.relief ?? { kind: "NONE" }).kind === "YIELDED";
  const isProtected = pane.protected ?? false;
  // 구 데몬은 capability를 보내지 않는다 — 그때도 "지원하지 않음"이다.
  const supported = capability.support === "supported";
  const title = supported ? undefined : capability.reason ?? t("terminal.relief.unsupported");
  const item = (action: PaneReliefMenuAction): ActionMenuItem => ({
    label: t(`app.paneMenu.${action}`),
    onSelect: () => runPaneMenuAction(controller, leafId, action, EMPTY_PANE_MENU_VALUES),
    disabled: !supported,
    title,
  });
  const items: ActionMenuItem[] = [];
  if (yielded) items.push(item("reliefRestore"));
  else if (!isProtected) items.push(item("reliefYield"));
  items.push(item(isProtected ? "reliefUnprotect" : "reliefProtect"));
  return items;
}
