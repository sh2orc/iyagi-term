/**
 * 끌어 놓기 미리보기(04-ui §2-5): 끌고 있는 것(흐리게), 놓으면 바뀔 자리(pane의
 * 반쪽·전체, 탭 사이 삽입 막대, 대상 탭 테두리), 탭 위 머무르기 진행 막대, 커서
 * 옆 안내 말풍선을 그린다.
 *
 * 모두 pointer-events 없는 고정 위치 조각이다 — 좌표 판정(elementFromPoint)을
 * 가리지 않고, 끄는 동안 다시 그려지는 것은 이 층뿐이다.
 */

import type { CSSProperties } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { useWorkbenchStore } from "../store/workbenchStore";
import { layoutDropText, type Rect } from "../features/terminal/layoutDrag";
import { TAB_DWELL_MS, useLayoutDragStore, type LayoutDragPreview, type LayoutDragView } from "./layoutDragSession";

/** 말풍선을 커서에서 띄우는 거리와, 화면 끝에서 뒤집을지 가늠하는 어림 크기. */
const GHOST_OFFSET_PX = 16;
const GHOST_FLIP_WIDTH_PX = 300;
const GHOST_FLIP_HEIGHT_PX = 72;

export function LayoutDragLayer(): JSX.Element | null {
  const view = useLayoutDragStore((s) => s.view);
  if (!view) return null;
  const surface = <LayoutDragSurface view={view} />;
  // document가 없는 환경(node 시험)에서는 포털 없이 낸다 — ActionMenu와 같은 갈래.
  if (typeof document === "undefined") return surface;
  return createPortal(surface, document.body);
}

/** 끌기 상태 하나를 그린다(층이 끌기 중일 때만 부른다 — 시험은 상태를 직접 넘긴다). */
export function LayoutDragSurface({ view }: { view: LayoutDragView }): JSX.Element {
  const { t } = useI18n();
  const tabs = useWorkbenchStore((s) => s.tabs);
  const { resolution, preview, dwell } = view;
  const blocked = resolution.blocked !== null;
  // 배치 편집에서는 탭을 "그룹"이라 부른다(그룹 하나가 탭 하나).
  const editor = view.area === "editor";
  const actionText = layoutDropText(tabs, resolution, editor ? "group" : "tab");
  const idleKey = editor
    ? `app.drag.group.idle.${view.source.kind}`
    : view.source.kind === "tab"
      ? "app.drag.idle.tab"
      : "app.drag.idle.pane";
  // 머무르는 중이면 곧 일어날 일을, 놓아도 아무 일이 없으면 놓을 수 있는 곳을 알려 준다.
  const hint = dwell
    ? t(dwell.purpose === "merge" ? "app.drag.dwellMerge" : "app.drag.dwellOpen", { title: dwell.title })
    : actionText === null
      ? t(idleKey)
      : null;
  const viewport = typeof window === "undefined" ? null : { width: window.innerWidth, height: window.innerHeight };
  const flipX = viewport !== null && view.x + GHOST_OFFSET_PX + GHOST_FLIP_WIDTH_PX > viewport.width;
  const flipY = viewport !== null && view.y + GHOST_OFFSET_PX + GHOST_FLIP_HEIGHT_PX > viewport.height;
  const ghostStyle: CSSProperties = {
    left: flipX ? view.x - GHOST_OFFSET_PX : view.x + GHOST_OFFSET_PX,
    top: flipY ? view.y - GHOST_OFFSET_PX : view.y + GHOST_OFFSET_PX,
    transform: `translate(${flipX ? "-100%" : "0"}, ${flipY ? "-100%" : "0"})`,
  };
  const ghostState = blocked ? " blocked" : actionText !== null ? " ready" : "";

  return (
    <div className="layout-drag-layer" aria-hidden="true">
      {view.sourceRect ? <div className="layout-drag-source" style={rectStyle(view.sourceRect)} /> : null}
      {dwell ? (
        <div key={`dwell-${dwell.tabId}`} className={`layout-drop-dwell ${dwell.purpose}`} style={rectStyle(dwell.rect)}>
          <span className="layout-drop-dwell-bar" style={{ animationDuration: `${TAB_DWELL_MS}ms` }} />
        </div>
      ) : null}
      {preview ? <DropPreview preview={preview} blocked={blocked} /> : null}
      <div className={`layout-drag-ghost${ghostState}`} style={ghostStyle}>
        <span className="layout-drag-ghost-title">{view.title}</span>
        {actionText !== null ? <span className="layout-drag-ghost-action">{actionText}</span> : null}
        {hint ? <span className="layout-drag-ghost-hint">{hint}</span> : null}
      </div>
    </div>
  );
}

function DropPreview({ preview, blocked }: { preview: LayoutDragPreview; blocked: boolean }): JSX.Element {
  const state = blocked ? " blocked" : "";
  switch (preview.kind) {
    case "insert":
      return (
        <div
          key="insert"
          className={`layout-drop-insert${state}`}
          style={{ left: preview.x, top: preview.top, height: preview.height }}
        />
      );
    case "tab":
      return <div key={`tab-${preview.tabId}`} className={`layout-drop-tab${state}`} style={rectStyle(preview.rect)} />;
    case "zone":
      // 같은 pane 안에서 자리를 바꾸면 미끄러지듯 옮겨 가고, 다른 pane으로 넘어가면 새로 그린다.
      return (
        <div
          key={`zone-${preview.leafId}`}
          className={`layout-drop-zone zone-${preview.zone}${state}`}
          style={rectStyle(preview.rect)}
        />
      );
  }
}

function rectStyle(rect: Rect): CSSProperties {
  return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
}
