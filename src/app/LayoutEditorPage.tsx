/**
 * 배치 편집 화면(04-ui §2-6, Cmd+Shift+G / Ctrl+Shift+G): 그룹(= 탭) 카드 보드와 실행 중인
 * 터미널 목록.
 *
 * 무엇이 어디에 있는지를 한눈에 보고 끌어 놓아 바꾸는 화면이다. 카드 안의 블록은 그 탭의 실제
 * 분할 모양을 줄인 미니맵이라, 블록 가장자리에 놓으면 그 옆에 분할, 가운데에 놓으면 자리
 * 바꾸기 — 탭 화면의 끌어 놓기와 같은 모델(layoutDrag)과 같은 store 동작을 거친다. 놓는 즉시
 * 실제 탭에 반영되고, 그룹을 지워도 터미널은 끄지 않는다(배치 안 됨으로 옮긴다).
 *
 * 끌기 판정은 data 속성으로 한다: 카드 data-layout-tab-id, 블록 data-layout-leaf-id,
 * "+ 새 그룹" data-layout-new-group, 목록 항목 data-layout-roster-session/-leaf.
 */

import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useI18n } from "../i18n";
import { useController } from "./controllerContext";
import { startLayoutDrag } from "./layoutDragSession";
import { tailPath } from "./layoutPath";
import { useWorkbenchStore, type PaneMeta, type TabState } from "../store/workbenchStore";
import { usePreferences } from "../store/preferences";
import { AgentIcon } from "../features/terminal/AgentIcon";
import { layoutTabName } from "../features/terminal/layoutDrag";
import { layoutRoster, type LayoutRoster, type RosterUnplaced } from "../features/terminal/layoutRoster";
import { leafCount, MAX_PANES_PER_TAB, type SplitNode } from "../features/terminal/splitTree";
import { shortcutHint, type Platform } from "../features/terminal/shortcuts";
import { terminalDisplayTitle } from "../features/terminal/shellEnvironment";
import { panePhaseText } from "../features/monitor/statusStrings";
import "./layoutEditor.css";

export function LayoutEditorPage({ platform }: { platform: Platform }): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const tabs = useWorkbenchStore((s) => s.tabs);
  const activeTabId = useWorkbenchStore((s) => s.activeTabId);
  const overrides = usePreferences((s) => s.shortcutOverrides);
  // 목록 항목에 손을 올리면 보드에서 그 블록을 짚어 준다.
  const [highlighted, setHighlighted] = useState<string | null>(null);
  // 이름을 고치고 있는 그룹 — "+ 새 그룹"을 누르면 새 그룹이 곧바로 이름 칸을 연다.
  const [renamingGroupId, setRenamingGroupId] = useState<string | null>(null);
  const endRename = useCallback(() => setRenamingGroupId(null), []);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const hint = shortcutHint("layout-editor", platform, overrides);
  // 화면을 열면 제목에 초점을 둔다 — 어느 화면인지 읽히고, 숨은 터미널에 입력이 가지 않는다.
  useEffect(() => headingRef.current?.focus(), []);

  return (
    <main className="layout-editor-page" aria-labelledby="layout-editor-title">
      <header className="layout-editor-header">
        <div>
          <h1 id="layout-editor-title" ref={headingRef} tabIndex={-1}>
            {t("layoutEditor.title")}
          </h1>
          <p>{t("layoutEditor.description")}</p>
        </div>
        <div className="layout-editor-actions">
          <button type="button" title={t("layoutEditor.regroupHint")} onClick={() => controller.regroupByProject()}>
            {t("layoutEditor.regroup")}
          </button>
          <button
            type="button"
            className="layout-editor-return"
            onClick={() => useWorkbenchStore.getState().setPage("terminal")}
          >
            {t("layoutEditor.return")}
            {hint ? <kbd>{hint}</kbd> : null}
          </button>
        </div>
      </header>
      <div className="layout-editor-body">
        <section className="layout-board" aria-label={t("layoutEditor.groups")}>
          {tabs.map((tab) => (
            <LayoutGroupCard
              key={tab.id}
              tab={tab}
              tabs={tabs}
              current={tab.id === activeTabId}
              highlighted={highlighted}
              renaming={renamingGroupId === tab.id}
              onRenameStart={setRenamingGroupId}
              onRenameEnd={endRename}
            />
          ))}
          <button
            type="button"
            className="layout-group-new"
            data-layout-new-group=""
            title={t("layoutEditor.newGroupHint")}
            onClick={() => setRenamingGroupId(controller.newGroup())}
          >
            {t("layoutEditor.newGroup")}
          </button>
        </section>
        <LayoutRosterPanel onHighlight={setHighlighted} />
      </div>
    </main>
  );
}

interface GroupCardProps {
  tab: TabState;
  tabs: readonly TabState[];
  /** 터미널 화면에서 보고 있는 탭. */
  current: boolean;
  highlighted: string | null;
  /** 이 그룹의 이름 칸이 열려 있는가(화면이 쥔다 — 새 그룹은 만들자마자 연다). */
  renaming: boolean;
  onRenameStart: (tabId: string) => void;
  onRenameEnd: () => void;
}

const LayoutGroupCard = memo(function LayoutGroupCard({
  tab,
  tabs,
  current,
  highlighted,
  renaming,
  onRenameStart,
  onRenameEnd,
}: GroupCardProps): JSX.Element {
  const { t } = useI18n();
  const controller = useController();
  const name = layoutTabName(tabs, tab.id);
  const startCardDrag = (event: ReactPointerEvent<HTMLElement>) =>
    startLayoutDrag(event, { kind: "tab", tabId: tab.id }, controller, "editor");
  const open = () => {
    const store = useWorkbenchStore.getState();
    store.setActiveTab(tab.id);
    store.setPage("terminal");
  };

  // mission·agent-view 탭은 순서만 바꾼다 — 터미널을 담거나 내보내지 않는다(05 §2).
  if (tab.kind !== "terminal") {
    return (
      <article className={`layout-group layout-group-mission${current ? " current" : ""}`} data-layout-tab-id={tab.id}>
        <header className="layout-group-head" onPointerDown={startCardDrag}>
          <span className="layout-group-grip" aria-hidden="true" />
          <span className="layout-group-name">
            <span className="layout-group-title">{name}</span>
          </span>
          <button type="button" className="layout-group-open" title={t("layoutEditor.group.openHint")} onClick={open}>
            {t("layoutEditor.group.open")}
          </button>
        </header>
        <p className="layout-group-note">{t("layoutEditor.group.mission")}</p>
      </article>
    );
  }

  const count = leafCount(tab.root);
  return (
    <article className={`layout-group${current ? " current" : ""}`} data-layout-tab-id={tab.id}>
      <header
        className="layout-group-head"
        title={renaming ? undefined : t("layoutEditor.group.renameHint")}
        onPointerDown={renaming ? undefined : startCardDrag}
      >
        <span className="layout-group-grip" aria-hidden="true" />
        {renaming ? (
          <GroupNameEditor
            value={name}
            label={t("layoutEditor.group.rename")}
            onCommit={(title) => {
              // 공백뿐인 이름은 store가 거절해 원래 이름을 지킨다(탭 이름 편집과 같다).
              useWorkbenchStore.getState().renameTab(tab.id, title);
              onRenameEnd();
            }}
            onCancel={onRenameEnd}
          />
        ) : (
          <span className="layout-group-name">
            <span className="layout-group-title" onDoubleClick={() => onRenameStart(tab.id)}>
              {name}
            </span>
            <button
              type="button"
              className="layout-group-rename"
              aria-label={t("layoutEditor.group.renameButton")}
              title={t("layoutEditor.group.renameButton")}
              onClick={() => onRenameStart(tab.id)}
            >
              ✎
            </button>
          </span>
        )}
        {current ? <span className="layout-group-current">{t("layoutEditor.group.current")}</span> : null}
        <span className={`layout-group-count${count >= MAX_PANES_PER_TAB ? " full" : ""}`}>
          {count}/{MAX_PANES_PER_TAB}
        </span>
        <button type="button" className="layout-group-open" title={t("layoutEditor.group.openHint")} onClick={open}>
          {t("layoutEditor.group.open")}
        </button>
        <button
          type="button"
          className="layout-group-delete"
          aria-label={t("layoutEditor.group.delete")}
          title={t("layoutEditor.group.delete")}
          onClick={() => void controller.ungroupTab(tab.id)}
        >
          ×
        </button>
      </header>
      <div className="layout-group-body">
        {tab.root ? (
          <LayoutMiniNode node={tab.root} tabId={tab.id} highlighted={highlighted} />
        ) : (
          <p className="layout-group-empty">{t("layoutEditor.group.empty")}</p>
        )}
      </div>
    </article>
  );
});

/** 분할 미니맵: 탭의 실제 분할 모양·비율을 그대로 줄인다. */
function LayoutMiniNode(props: { node: SplitNode; tabId: string; highlighted: string | null }): JSX.Element {
  const { node, tabId, highlighted } = props;
  if (node.kind === "leaf") {
    return <LayoutBlock leafId={node.id} tabId={tabId} highlighted={highlighted === node.id} />;
  }
  const firstGrow = Math.max(1, Math.round(node.ratio * 1000));
  const secondGrow = Math.max(1, Math.round((1 - node.ratio) * 1000));
  return (
    <div className={`layout-mini-split ${node.axis}`}>
      <div className="layout-mini-slot" style={{ flexGrow: firstGrow }}>
        <LayoutMiniNode node={node.first} tabId={tabId} highlighted={highlighted} />
      </div>
      <div className="layout-mini-slot" style={{ flexGrow: secondGrow }}>
        <LayoutMiniNode node={node.second} tabId={tabId} highlighted={highlighted} />
      </div>
    </div>
  );
}

function LayoutBlock(props: { leafId: string; tabId: string; highlighted: boolean }): JSX.Element {
  const { leafId, tabId, highlighted } = props;
  const controller = useController();
  const pane = useWorkbenchStore((s) => s.panes[leafId]);
  const focused = useWorkbenchStore((s) => s.focusedLeafId === leafId);
  return (
    <LayoutBlockView
      leafId={leafId}
      pane={pane ?? null}
      focused={focused}
      highlighted={highlighted}
      onPointerDown={(event) => startLayoutDrag(event, { kind: "pane", leafId }, controller, "editor")}
      onOpen={() => {
        // 두 번 누르면 그 터미널로 바로 간다.
        const store = useWorkbenchStore.getState();
        store.setActiveTab(tabId);
        store.focusPane(leafId);
        store.setPage("terminal");
      }}
    />
  );
}

export interface LayoutBlockViewProps {
  leafId: string;
  pane: PaneMeta | null;
  focused: boolean;
  highlighted: boolean;
  onPointerDown?: (event: ReactPointerEvent<HTMLDivElement>) => void;
  onOpen?: () => void;
}

/** 블록 하나(시험이 store 없이 그릴 수 있게 값만 받는다). */
export function LayoutBlockView({ leafId, pane, focused, highlighted, onPointerDown, onOpen }: LayoutBlockViewProps): JSX.Element {
  const { t } = useI18n();
  const phase = pane?.phase ?? "starting";
  const title = pane ? terminalDisplayTitle(pane.title) : leafId;
  return (
    <div
      className={`layout-block phase-${phase}${focused ? " focused" : ""}${highlighted ? " highlighted" : ""}`}
      data-layout-leaf-id={leafId}
      title={t("layoutEditor.block.hint")}
      onPointerDown={onPointerDown}
      onDoubleClick={onOpen}
    >
      <span className="layout-block-title">
        {pane?.agent ? <AgentIcon agent={pane.agent.agent} /> : null}
        {title}
      </span>
      <span className="layout-block-meta">
        {phase !== "live" ? <span className={`layout-block-state state-${phase}`}>{panePhaseText(phase)}</span> : null}
        {pane?.cwd ? <span className="layout-block-cwd">{tailPath(pane.cwd)}</span> : null}
      </span>
    </div>
  );
}

function LayoutRosterPanel({ onHighlight }: { onHighlight: (leafId: string | null) => void }): JSX.Element {
  const controller = useController();
  const tabs = useWorkbenchStore((s) => s.tabs);
  const panes = useWorkbenchStore((s) => s.panes);
  const workloads = useWorkbenchStore((s) => s.workloads);
  const workloadMemory = useWorkbenchStore((s) => s.workloadMemory);
  const roster = useMemo(
    () => layoutRoster({ tabs, panes, workloads, workloadMemory }),
    [tabs, panes, workloads, workloadMemory],
  );
  return (
    <LayoutRosterView
      roster={roster}
      onHighlight={onHighlight}
      onPlacedPointerDown={(leafId, event) => startLayoutDrag(event, { kind: "pane", leafId }, controller, "editor")}
      onUnplacedPointerDown={(item, event) =>
        startLayoutDrag(event, { kind: "session", sessionId: item.sessionId, workloadId: item.workloadId }, controller, "editor")
      }
    />
  );
}

export interface LayoutRosterViewProps {
  roster: LayoutRoster;
  onHighlight?: (leafId: string | null) => void;
  onPlacedPointerDown?: (leafId: string, event: ReactPointerEvent<HTMLLIElement>) => void;
  onUnplacedPointerDown?: (item: RosterUnplaced, event: ReactPointerEvent<HTMLLIElement>) => void;
}

/**
 * 오른쪽 목록: 배치 안 됨(끌어 넣으면 다시 붙는다)을 먼저, 그다음 배치됨(끌면 원래 자리에서
 * 옮겨진다). 둘 다 끌 수 있고, 같은 터미널이 두 목록에 겹치지 않는다(layoutRoster).
 */
export function LayoutRosterView({ roster, onHighlight, onPlacedPointerDown, onUnplacedPointerDown }: LayoutRosterViewProps): JSX.Element {
  const { t } = useI18n();
  const { placed, unplaced } = roster;
  return (
    <aside className="layout-roster" aria-label={t("layoutEditor.roster.title")}>
      <h2>
        {t("layoutEditor.roster.title")}
        <span className="layout-roster-count">{placed.length + unplaced.length}</span>
      </h2>
      <p className="layout-roster-hint">{t("layoutEditor.roster.hint")}</p>
      <section className="layout-roster-section" aria-label={t("layoutEditor.roster.unplaced")}>
        <h3>
          {t("layoutEditor.roster.unplaced")}
          <span className="layout-roster-count">{unplaced.length}</span>
        </h3>
        {unplaced.length === 0 ? (
          <p className="layout-roster-empty">{t("layoutEditor.roster.unplacedEmpty")}</p>
        ) : (
          <>
            <p className="layout-roster-note">{t("layoutEditor.roster.unplacedHint")}</p>
            <ul>
              {unplaced.map((item) => (
                <li
                  key={item.sessionId}
                  className="layout-roster-item unplaced"
                  data-layout-roster-session={item.sessionId}
                  title={item.cwd}
                  onPointerDown={onUnplacedPointerDown ? (event) => onUnplacedPointerDown(item, event) : undefined}
                >
                  <span className="layout-roster-title">
                    {item.agent ? <AgentIcon agent={item.agent.agent} /> : null}
                    {item.title}
                  </span>
                  <span className="layout-roster-meta">{tailPath(item.cwd)}</span>
                </li>
              ))}
            </ul>
          </>
        )}
      </section>
      <section className="layout-roster-section" aria-label={t("layoutEditor.roster.placed")}>
        <h3>
          {t("layoutEditor.roster.placed")}
          <span className="layout-roster-count">{placed.length}</span>
        </h3>
        {placed.length === 0 ? (
          <p className="layout-roster-empty">{t("layoutEditor.roster.placedEmpty")}</p>
        ) : (
          <ul>
            {placed.map((item) => (
              <li
                key={item.leafId}
                className={`layout-roster-item phase-${item.phase}`}
                data-layout-roster-leaf={item.leafId}
                title={item.cwd ?? undefined}
                onPointerDown={onPlacedPointerDown ? (event) => onPlacedPointerDown(item.leafId, event) : undefined}
                onPointerEnter={onHighlight ? () => onHighlight(item.leafId) : undefined}
                onPointerLeave={onHighlight ? () => onHighlight(null) : undefined}
              >
                <span className="layout-roster-title">
                  {item.agent ? <AgentIcon agent={item.agent.agent} /> : null}
                  {item.title}
                </span>
                <span className="layout-roster-meta">
                  <span className="layout-roster-chip">{item.tabTitle}</span>
                  {item.cwd ? tailPath(item.cwd) : null}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
    </aside>
  );
}

interface GroupNameEditorProps {
  value: string;
  label: string;
  onCommit: (next: string) => void;
  onCancel: () => void;
}

/** 그룹(탭) 이름 편집 칸 — 탭 바의 이름 편집과 같은 규칙(전체 선택·Enter 확정·Esc 취소). */
function GroupNameEditor({ value, label, onCommit, onCancel }: GroupNameEditorProps): JSX.Element {
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement>(null);
  // 확정·취소 뒤 칸이 사라지며 오는 blur가 이름을 다시 확정하지 않게 한다.
  const settledRef = useRef(false);
  useEffect(() => inputRef.current?.select(), []);
  const settle = (commit: boolean) => {
    if (settledRef.current) return;
    settledRef.current = true;
    if (commit) onCommit(draft);
    else onCancel();
  };
  return (
    <input
      ref={inputRef}
      className="layout-group-title-input"
      autoFocus
      value={draft}
      aria-label={label}
      onChange={(event) => setDraft(event.target.value)}
      onBlur={() => settle(true)}
      onKeyDown={(event) => {
        // 창 단축키(Esc = 터미널로 돌아가기)로 새지 않게 막는다.
        event.stopPropagation();
        // 한글 조합을 확정하는 Enter는 이름 확정이 아니다(04 §3 IME 규칙). WebKit은 후보를 고르는
        // Enter를 compositionend 뒤에 keyCode 229로 보내므로 그것도 조합으로 본다.
        if (event.key === "Enter" && !event.nativeEvent.isComposing && event.nativeEvent.keyCode !== 229) settle(true);
        else if (event.key === "Escape") settle(false);
      }}
    />
  );
}
