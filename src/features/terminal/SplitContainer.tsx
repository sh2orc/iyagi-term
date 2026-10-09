/**
 * SplitContainer: recursive SplitNode rendering with 6px dividers
 * (04-ui.md §2).
 *
 * Divider contract:
 * - role="separator", aria-orientation, aria-valuenow/min/max(비율 %),
 *   keyboard focusable — arrow keys adjust 2%(Shift 10%).
 * - Pointer capture drag with release/cancel cleanup, resize cursor per
 *   axis, rAF-throttled visual updates.
 * - 더블클릭하면 그 divider가 속한 축 그룹 비율을 균등으로 되돌린다
 *   (분할 직후와 같은 기준 — splitTree.balanceAxisGroup).
 * - Ratio clamp uses recursive leaf minimums only.
 */

import { memo, useCallback, useRef, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import {
  DIVIDER_PX,
  DIVIDER_STEP,
  DIVIDER_STEP_LARGE,
  adjustRatio,
  axisGroupMembers,
  clampRatio,
  minSize,
  type SplitNode,
} from "./splitTree";
import { TerminalPane } from "./TerminalPane";

export const SplitContainer = memo(function SplitContainer(props: { tabId: string }): JSX.Element | null {
  // mission/agent-view 탭에는 root가 없다(05 §2 kind guard) — 렌더 자체가
  // 일어나지 않지만, 저장 파일이나 상태 이행 중 만나도 안전해야 한다.
  const root = useWorkbenchStore((s) => {
    const tab = s.tabs.find((t) => t.id === props.tabId);
    return tab?.kind === "terminal" ? tab.root : null;
  });
  if (!root) return null;
  return (
    <div className="split-root">
      <NodeView node={root} />
    </div>
  );
});

function NodeView(props: { node: SplitNode }): JSX.Element {
  const { node } = props;
  const tabId = useWorkbenchStore((s) => s.activeTabId);
  const setRatio = useWorkbenchStore((s) => s.setRatio);
  const balanceRatios = useWorkbenchStore((s) => s.balanceRatios);
  const boxRef = useRef<HTMLDivElement>(null);

  if (node.kind === "leaf") {
    return <TerminalPane leafId={node.id} />;
  }

  const firstGrow = Math.max(1, Math.round(node.ratio * 10000));
  const secondGrow = Math.max(1, Math.round((1 - node.ratio) * 10000));

  const measureParent = () => {
    const rect = boxRef.current?.getBoundingClientRect();
    return { width: rect?.width ?? 0, height: rect?.height ?? 0 };
  };

  const commitRatio = (proposed: number) => {
    const size = measureParent();
    const next = clampRatio(node, size, proposed);
    if (tabId && next !== node.ratio) setRatio(tabId, node.id, next);
  };

  // 더블클릭: 드래그로 옮겨 둔 경계선을 축 그룹 균등 비율로 되돌린다.
  const resetRatio = () => {
    if (tabId) balanceRatios(tabId, node.id);
  };

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const horizontalKeys = node.axis === "row"; // 세로 분할선 → 좌우 조작
    const step = event.shiftKey ? DIVIDER_STEP_LARGE : DIVIDER_STEP;
    let delta = 0;
    if (horizontalKeys && event.key === "ArrowLeft") delta = -step;
    else if (horizontalKeys && event.key === "ArrowRight") delta = step;
    else if (!horizontalKeys && event.key === "ArrowUp") delta = -step;
    else if (!horizontalKeys && event.key === "ArrowDown") delta = step;
    else return;
    event.preventDefault();
    event.stopPropagation();
    commitRatio(adjustRatio(node, measureParent(), delta));
  };

  return (
    <div ref={boxRef} className={`split split-${node.axis}`}>
      <div className="pane-slot" style={{ flexGrow: firstGrow, flexShrink: 1, flexBasis: 0 }}>
        <NodeView node={node.first} />
      </div>
      <Divider
        axis={node.axis}
        ratio={node.ratio}
        minFirst={node.axis === "row" ? minSize(node.first).width : minSize(node.first).height}
        minSecond={node.axis === "row" ? minSize(node.second).width : minSize(node.second).height}
        onCommit={commitRatio}
        onReset={resetRatio}
        onKeyDown={onKeyDown}
        groupSize={axisGroupMembers(node, node.axis).length}
      />
      <div className="pane-slot" style={{ flexGrow: secondGrow, flexShrink: 1, flexBasis: 0 }}>
        <NodeView node={node.second} />
      </div>
    </div>
  );
}

interface DividerProps {
  axis: "row" | "column";
  ratio: number;
  minFirst: number;
  minSecond: number;
  onCommit: (proposed: number) => void;
  /** 더블클릭 시 축 그룹 균등 재조정. */
  onReset: () => void;
  onKeyDown: (event: ReactKeyboardEvent<HTMLDivElement>) => void;
  /** 이 divider가 속한 축 그룹의 pane(member) 수 — 되돌릴 기준 비율 안내. */
  groupSize: number;
}

function Divider(props: DividerProps): JSX.Element {
  const { axis, ratio, minFirst, minSecond, onCommit, onReset, onKeyDown, groupSize } = props;
  const { t } = useI18n();
  const dragRef = useRef<{ pointerId: number; startX: number; startY: number; startRatio: number } | null>(null);
  const rafRef = useRef<number | null>(null);
  const pendingRef = useRef<number | null>(null);

  const clampLocal = useCallback(
    (proposed: number) => {
      // clampRatio와 동일 식이지만 divider 위치 계산에 필요한 최소값만 전달받는다.
      return Math.min(Math.max(proposed, 0), 1);
    },
    [],
  );

  const scheduleCommit = useCallback(
    (proposed: number) => {
      pendingRef.current = proposed;
      if (rafRef.current !== null) return;
      rafRef.current = requestAnimationFrame(() => {
        rafRef.current = null;
        const value = pendingRef.current;
        pendingRef.current = null;
        if (value !== null) onCommit(value);
      });
    },
    [onCommit],
  );

  const endDrag = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    dragRef.current = null;
    const target = event.currentTarget;
    try {
      if (target.hasPointerCapture(event.pointerId)) target.releasePointerCapture(event.pointerId);
    } catch {
      // capture가 이미 풀린 경우 무시.
    }
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    const value = pendingRef.current;
    pendingRef.current = null;
    if (value !== null) onCommit(value);
  }, [onCommit]);

  const onDoubleClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    event.preventDefault();
    event.stopPropagation();
    // pointerup에서 drag가 이미 정리되므로 여기서는 재조정만 요청한다.
    onReset();
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      startRatio: ratio,
    };
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    const box = event.currentTarget.parentElement?.getBoundingClientRect();
    if (!box) return;
    const total = (axis === "row" ? box.width : box.height) - DIVIDER_PX;
    if (total <= 0) return;
    const deltaPx = axis === "row" ? event.clientX - drag.startX : event.clientY - drag.startY;
    const proposed = clampLocal(drag.startRatio + deltaPx / total);
    scheduleCommit(proposed);
  };

  return (
    <div
      role="separator"
      tabIndex={0}
      className={`divider divider-${axis}`}
      aria-orientation={axis === "row" ? "vertical" : "horizontal"}
      aria-label={t(axis === "row" ? "terminal.divider.row" : "terminal.divider.column")}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(ratio * 100)}
      title={t("terminal.divider.reset", { percent: Math.round(100 / groupSize) })}
      data-min-first={minFirst}
      data-min-second={minSecond}
      onKeyDown={onKeyDown}
      onDoubleClick={onDoubleClick}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
    />
  );
}
