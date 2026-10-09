import { AgentIcon } from "../features/terminal/AgentIcon";
/**
 * 탭 하나(04-ui.md §1 상단 바): 평소에는 제목 + 닫기 버튼, 이름 편집
 * 중에는 입력만 보여 준다.
 *
 * 편집 상태를 props로 받는 순수 컴포넌트다. 편집을 시작하는 경로가 셋
 * (더블클릭·F2·명령 팔레트)이라 상태 자체는 store가 쥐고, 여기서는
 * "편집 중인가"만 알면 된다.
 */

import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { useI18n } from "../i18n";
import { ActionMenu, actionMenuPosition, type ActionMenuItem } from "./ActionMenu";
import { flashToast, useWorkbenchStore, type TabState } from "../store/workbenchStore";
import type { SplitNode } from "../features/terminal/splitTree";
import { listLeaves } from "../features/terminal/splitTree";
import { isFinishedWorkload } from "../store/workloadState";
import { useTerminalActivity } from "../features/terminal/activity";
import { useNotificationStore } from "../features/notifications/notificationStore";
import { terminalDisplayTitle } from "../features/terminal/shellEnvironment";
import { agentActivityLabel, resolveAgentActivity, type AgentActivity } from "../features/terminal/agentNames";
import { formatCores, formatGiB } from "../features/monitor/format";
import { useMissionStore } from "../features/missions/store";
import { selectMissionTitle } from "../features/missions/selectors";
import {
  decodeMissionTabBadge,
  encodeMissionTabBadge,
  selectMissionTabBadge,
  type MissionTabBadge,
} from "../features/missions/missionStatus";
import "../features/missions/missionShell.css";

export interface TabItemProps {
  tab: TabState;
  index: number;
  /** 전체 탭 수 — 메뉴의 "왼쪽/오른쪽으로 이동·합치기"를 끌 자리를 판단한다. */
  tabCount: number;
  /**
   * terminal 탭 수 — 합치기·다시 묶기는 pane 트리를 가진 terminal 탭끼리의
   * 일이다(05 §2). 생략하면 tabCount와 같다고 본다.
   */
  terminalTabCount?: number;
  active: boolean;
  running?: boolean;
  /** 끝난/실패 pane이 있어 아직 사용자가 안 본 탭(W1-3 조용한 배지). */
  finished?: boolean;
  /** 대기 중(QUEUED) 작업이 있는 탭. */
  queued?: boolean;
  /** 미확인 개입 신호가 있는 탭(W1-5). */
  intervention?: boolean;
  /** 살아 있는 pane의 자원 요약(W2: 상태 스트립의 최소 증명). */
  usage?: { cpuCores: number | null; residentBytes: number | null } | null;
  /** 자동 감지된 에이전트의 활동 상태(타이틀 앞 마커). 에이전트가 없으면 null. */
  agentActivity?: AgentActivity | null;
  /** 그 에이전트 id(마커 문구용). */
  agentId?: string | null;
  /**
   * mission 탭 배지(상태 우선): 결정 필요 N > 확정 대기 > 실패 > 실행 중 N > 완료.
   * terminal 탭에는 없고, 보일 상태가 없으면 null.
   */
  missionBadge?: MissionTabBadge | null;
  /** 표시 제목 override(mission 탭 — missionStore 제목을 우선한다). */
  titleText?: string | null;
  renaming: boolean;
  onSelect: () => void;
  onClose: () => void;
  onCloseAll: () => void;
  onRenameStart: () => void;
  onRenameCommit: (title: string) => void;
  onRenameCancel: () => void;
  /**
   * 탭 재배치(04-ui §2-5). 기본값은 no-op이라 이 기능을 쓰지 않는 호출자
   * (미리보기·시험)는 그대로 둘 수 있다.
   */
  onMoveLeft?: () => void;
  onMoveRight?: () => void;
  onMergeInto?: () => void;
  /** 프로젝트별로 다시 묶기(모든 탭 대상) — 미리보기를 연다. */
  onRegroup?: () => void;
  /**
   * 끌어 놓기 시작(04-ui §2-5) — 탭을 누른 pointerdown을 넘긴다. 순서 바꾸기·다른 탭에
   * 합치기는 끌기 세션이 정한다. 생략하면 끌 수 없다(미리보기·시험).
   */
  onLayoutDragStart?: (event: ReactPointerEvent<HTMLDivElement>) => void;
}

type WorkbenchSnapshot = ReturnType<typeof useWorkbenchStore.getState>;
type TerminalTab = TabState & { kind: "terminal" };

/** 이 탭의 살아 있는 pane 중 최대 CPU·RAM(없으면 null). */
function maxPaneUsage(
  panes: WorkbenchSnapshot["panes"],
  root: SplitNode | null,
): { cpu: number | null; ram: number | null } {
  let cpu: number | null = null;
  let ram: number | null = null;
  for (const leaf of listLeaves(root)) {
    const pane = panes[leaf.id];
    if (!pane?.usage || pane.phase !== "live") continue;
    if (pane.usage.cpuCores !== null && (cpu === null || pane.usage.cpuCores > cpu)) {
      cpu = pane.usage.cpuCores;
    }
    if (pane.usage.residentBytes !== null && (ram === null || pane.usage.residentBytes > ram)) {
      ram = pane.usage.residentBytes;
    }
  }
  return { cpu, ram };
}

/**
 * 이 탭의 살아 있는 에이전트 pane 중 마커에 실을 활동 상태와 그 에이전트.
 * 자체 보고(Claude 레지스트리)가 있으면 그것을, 없으면 출력 활동을 근거로
 * 삼고, pane이 여럿이면 확인 대기 > 작업 중 > idle — 사용자의 손이 필요한
 * 쪽이 먼저다.
 */
function tabAgentActivity(
  panes: WorkbenchSnapshot["panes"],
  root: SplitNode | null,
  activeSessions: ReadonlySet<string>,
): { activity: AgentActivity; agent: string } | null {
  let best: { activity: AgentActivity; agent: string } | null = null;
  for (const leaf of listLeaves(root)) {
    const pane = panes[leaf.id];
    if (!pane?.agent || pane.phase !== "live") continue;
    const activity = resolveAgentActivity(
      pane.agent,
      typeof pane.sessionId === "string" && activeSessions.has(pane.sessionId),
    );
    const candidate = { activity, agent: pane.agent.agent };
    if (activity === "waiting") return candidate;
    if (activity === "working" && best?.activity !== "working") best = candidate;
    else if (best === null) best = candidate;
  }
  return best;
}

/**
 * Subscribe only to the resulting boolean, not every resource sample.
 * kind별로 갈라진다(05 §2): terminal 배지는 pane 상태에서, mission 배지는
 * missionStore 개수 selector에서 나온다 — 어느 쪽이든 원시값만 구독한다.
 */
export function LiveTabItem(props: TabItemProps): JSX.Element {
  if (props.tab.kind === "terminal") return <TerminalTabItem {...props} tab={props.tab} />;
  if (props.tab.kind === "mission") return <MissionTabItem {...props} />;
  return <TabItem {...props} />;
}

function TerminalTabItem(props: Omit<TabItemProps, "tab"> & { tab: TerminalTab }): JSX.Element {
  const root = props.tab.root;
  const activeSessions = useTerminalActivity(s => s.sessions);
  const running = useWorkbenchStore(s => listLeaves(root).some(leaf => {
    const pane = s.panes[leaf.id];
    if (!pane?.sessionId || pane.phase === "exited") return false;
    const workload = pane.workloadId === null ? undefined : s.workloadById.get(pane.workloadId);
    if (workload && (isFinishedWorkload(workload.state) || workload.state === "QUEUED")) return false;
    if (pane.phase !== "live") return false;
    return activeSessions.has(pane.sessionId);
  }));
  const finished = useWorkbenchStore(s => listLeaves(root).some(leaf => {
    const pane = s.panes[leaf.id];
    return pane?.phase === "exited" || pane?.phase === "failed";
  }));
  // 미확인 개입 신호가 이 탭의 세션에 붙어 있으면 배지(W1-5).
  const intervention = useNotificationStore(s => s.items.some(item => {
    const sessionId = item.sessionId;
    if (item.kind !== "intervention" || item.acknowledged || sessionId === null) return false;
    return listLeaves(root).some((leaf) => s2SessionMatch(leaf.id, sessionId));
  }));
  const queued = useWorkbenchStore(s => listLeaves(root).some(leaf => {
    const pane = s.panes[leaf.id];
    if (!pane?.workloadId) return false;
    return s.workloadById.get(pane.workloadId)?.state === "QUEUED";
  }));
  // 자원 배지(W2): 이 탭의 살아 있는 pane 중 최대 CPU·RAM — 자원 관리가
  // "살아 있는 에이전트"에 상시 보이는 최소 증명이다.
  // 객체를 새로 만드는 selector는 Object.is 비교를 항상 실패시켜 모든 탭이
  // 매 스토어 갱신마다 다시 그려진다 — 원시값 둘로 구독한다.
  const usageCpu = useWorkbenchStore((s) => maxPaneUsage(s.panes, root).cpu);
  const usageRam = useWorkbenchStore((s) => maxPaneUsage(s.panes, root).ram);
  const usage = usageCpu === null && usageRam === null ? null : { cpuCores: usageCpu, residentBytes: usageRam };
  // 타이틀 앞 활동 마커(원시값 둘로 구독 — 위 usage와 같은 이유).
  const activity = useWorkbenchStore((s) => tabAgentActivity(s.panes, root, activeSessions)?.activity ?? null);
  const activityAgent = useWorkbenchStore((s) => tabAgentActivity(s.panes, root, activeSessions)?.agent ?? null);
  return (
    <TabItem
      {...props}
      running={running}
      finished={finished}
      queued={queued}
      intervention={intervention}
      usage={usage}
      agentActivity={activity}
      agentId={activityAgent}
    />
  );
}

/**
 * mission 탭: 제목은 missionStore가 정본(저장된 탭 제목은 폴백), 배지는 상태 우선 한 개
 * (missionStatus.missionTabBadge). snapshot을 뽑지 않은 백그라운드 탭은 `mission.list`
 * 요약으로 채워진다. selector가 원시 문자열을 반환하므로 탭은 배지가 바뀔 때만 다시
 * 그려진다 — missionStore의 다른 갱신이 터미널 렌더를 반복시키지 않는다(U15).
 *
 * 스크린리더: 배지 자체는 낭독 영역이 아니다(탭마다 role="status"면 갱신마다 읽힌다).
 * 결정 필요로 "바뀔 때만" 앱의 polite 알림 영역(토스트, role="status")으로 한 번 알린다.
 */
function MissionTabItem(props: TabItemProps): JSX.Element {
  const tab = props.tab;
  if (tab.kind !== "mission") throw new Error("mission tab required");
  const { t } = useI18n();
  const badgeCode = useMissionStore((s) => encodeMissionTabBadge(selectMissionTabBadge(s, tab.missionId)));
  const known = useMissionStore((s) => s.missions[tab.missionId] !== undefined);
  const missionTitle = useMissionStore((s) => selectMissionTitle(s, tab.missionId));
  // 정본은 missionStore 제목 → 저장된 탭 제목 → 기본 문구 순서의 폴백이다.
  const title = missionTitle || tab.title || t("missions.tabTitle");
  const badge = decodeMissionTabBadge(badgeCode);
  const previousKind = useRef<string | null>(null);
  const active = props.active;
  useEffect(() => {
    // 작업 요약이 오기 전(모름)은 기준이 아니다 — 앱을 켜고 처음 채워지는 배지는 알리지 않는다.
    if (!known) return;
    const kind = badge?.kind ?? "none";
    const before = previousKind.current;
    previousKind.current = kind;
    if (before === null || before === "decision" || kind !== "decision") return;
    // 보고 있는 탭은 화면의 결정 배너가 스스로 낭독한다.
    if (active) return;
    flashToast(t("missions.tabBadge.announce", { title }));
    // 배지 종류가 바뀔 때만 판단한다(제목·언어 변경으로 다시 알리지 않는다).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [badgeCode, known]);
  return (
    <TabItem
      {...props}
      titleText={title}
      missionBadge={badge}
    />
  );
}

/** 탭 배지 문구(상태 우선 한 개). */
export function missionTabBadgeText(
  t: (key: string, params?: Record<string, string | number>) => string,
  badge: MissionTabBadge,
): string {
  switch (badge.kind) {
    case "decision":
      return t("missions.tabBadge.decision", { count: badge.count });
    case "acceptance":
      return t("missions.tabBadge.acceptance");
    case "failed":
      return t("missions.tabBadge.failed");
    case "running":
      return badge.count === null ? t("missions.tabBadge.running") : t("missions.tabBadge.runningCount", { count: badge.count });
    case "done":
      return t("missions.tabBadge.done");
  }
}

function s2SessionMatch(leafId: string, sessionId: string): boolean {
  const pane = useWorkbenchStore.getState().panes[leafId];
  return pane?.sessionId === sessionId;
}

export function TabItem({
  tab,
  index,
  tabCount,
  active,
  running = false,
  finished = false,
  queued = false,
  intervention = false,
  usage = null,
  agentActivity = null,
  agentId = null,
  missionBadge = null,
  titleText = null,
  renaming,
  onSelect,
  onClose,
  onCloseAll,
  onRenameStart,
  onRenameCommit,
  onRenameCancel,
  onMoveLeft = () => undefined,
  onMoveRight = () => undefined,
  onMergeInto = () => undefined,
  onRegroup = () => undefined,
  onLayoutDragStart,
  terminalTabCount = tabCount,
}: TabItemProps): JSX.Element {
  const { t } = useI18n();
  // mission 탭은 missionStore 제목(호출자가 titleText로 전달)을, terminal
  // 탭은 저장된 이름을 쓴다. 이름 편집은 terminal 전용(05 §2).
  const canRename = tab.kind === "terminal";
  const title =
    titleText ?? terminalDisplayTitle(tab.title ?? t("app.tabTitle", { index: index + 1 }));
  // 에이전트가 있는 탭은 타이틀 앞 활동 마커가 출력 활동 점을 대신한다 — 둘 다
  // 보이면 무엇이 "작업 중"인지 헷갈리고, 입력 상자 다시 그리기만으로 점이
  // 깜빡인다. 마커 문구는 pane 헤더와 같은 키를 쓴다.
  const activityLabel = agentId && agentActivity ? agentActivityLabel(agentActivity, agentId) : null;
  const showRunningDot = running && agentActivity === null;
  const tabTooltip = activityLabel ?? (showRunningDot ? t("app.tabRunning") : undefined);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  /**
   * 탭 메뉴(04-ui §2-5): 이름 → 자리 옮기기 → 합치기 → 닫기 순서다. 지금
   * 할 수 없는 자리(맨 왼쪽에서 더 왼쪽으로, 탭이 하나뿐인데 합치기)는
   * 숨기지 않고 흐리게 남긴다 — 항목 자리가 상황마다 바뀌면 손이 기억한
   * 위치를 못 믿게 된다. 이름 편집은 terminal 전용이라(05 §2) mission 계열
   * 탭에는 그 항목이 없고, pane 트리가 없는 탭은 다른 탭에 합칠 것도 없다.
   */
  const menuItems: ActionMenuItem[] = [
    ...(canRename ? [{ label: t("app.tabMenu.rename"), onSelect: onRenameStart }] : []),
    { label: t("app.tabMenu.moveLeft"), onSelect: onMoveLeft, disabled: index <= 0 },
    { label: t("app.tabMenu.moveRight"), onSelect: onMoveRight, disabled: index >= tabCount - 1 },
    { label: t("app.tabMenu.mergeInto"), onSelect: onMergeInto, disabled: tab.kind !== "terminal" || terminalTabCount <= 1 },
    { label: t("app.tabMenu.regroup"), onSelect: onRegroup, disabled: terminalTabCount <= 1 },
    { label: t("app.tabMenu.close"), onSelect: onClose },
    { label: t("app.tabMenu.closeAll"), onSelect: onCloseAll, danger: true },
  ];

  return (
    <>
      <div
        className={`tab${active ? " active" : ""}${running ? " running" : ""}${renaming ? " renaming" : ""}`}
        role="tab"
        data-tab-id={tab.id}
        aria-selected={active}
        title={tabTooltip}
        tabIndex={renaming ? -1 : 0}
        onClick={renaming ? undefined : onSelect}
        // 누른 채 움직이면 끌기(순서 바꾸기·합치기) — 임계값 전에 떼면 위의 클릭 그대로다.
        onPointerDown={renaming ? undefined : onLayoutDragStart}
        onDoubleClick={renaming || !canRename ? undefined : onRenameStart}
        onContextMenu={renaming ? undefined : (event) => {
          event.preventDefault();
          onSelect();
          setContextMenu(actionMenuPosition(event.clientX, event.clientY, menuItems.length, {
            width: window.innerWidth,
            height: window.innerHeight,
          }));
        }}
        onKeyDown={
          renaming
            ? undefined
            : (e) => {
              if (e.key === "Enter" || e.key === " ") onSelect();
              else if (e.key === "F2" && canRename) {
                e.preventDefault();
                onRenameStart();
              }
            }
        }
      >
        {renaming ? (
          <TabTitleEditor
            value={title}
            label={t("app.renameTab")}
            onCommit={onRenameCommit}
            onCancel={onRenameCancel}
          />
        ) : (
          <>
            {/* 마커 자리를 고정한다(작업 중 ↔ idle로 점이 나타났다 사라져도 탭이 좌우로 떨리지 않게). */}
            {running || agentId ? (
              <span className="tab-marker-slot">
                {activityLabel && agentActivity ? (
                  <span className={`tab-agent-marker ${agentActivity}`} role="img" aria-label={activityLabel} />
                ) : showRunningDot ? (
                  <span className="tab-running-dot" role="img" aria-label={t("app.tabRunning")} />
                ) : null}
              </span>
            ) : null}
            <span className={`tab-title${agentId ? ` agent-${agentId}` : ""}`}>
              {agentId ? <AgentIcon agent={agentId} /> : null}
              {title}
            </span>
            {/* mission 탭 배지: 상태 우선 한 개. 낭독 영역이 아니다(결정 필요 전환만 MissionTabItem이 알린다). */}
            {missionBadge ? (
              <span
                className={`tab-badge tab-badge-mission tab-badge-mission-${missionBadge.kind}`}
                data-badge={missionBadge.kind}
                // 좁은 배지에서 잘린 문구(영문 "Awaiting confirmation" 등)를 툴팁으로 끝까지 읽는다.
                title={missionTabBadgeText(t, missionBadge)}
              >
                {missionTabBadgeText(t, missionBadge)}
              </span>
            ) : null}
            {usage ? (
              <span
                className="tab-usage"
                title={t("app.tabUsage")}
              >
                {formatCores(usage.cpuCores)} · {formatGiB(usage.residentBytes)}
              </span>
            ) : null}
            {/* 조용한 배지(W1-3): 사용자가 보고 있는(active) 탭에서는 내비둔다. */}
            {!active && intervention ? (
              <span className="tab-badge tab-badge-intervention" role="status">{t("app.tabIntervention")}</span>
            ) : null}
            {!active && queued ? (
              <span className="tab-badge tab-badge-queued" role="status">{t("app.tabQueued")}</span>
            ) : null}
            {!active && finished ? (
              <span className="tab-badge tab-badge-finished" role="status">{t("app.tabFinished")}</span>
            ) : null}
            <button
              type="button"
              className="tab-close"
              aria-label={t("app.closeTab", { index: index + 1 })}
              onClick={(e) => {
                e.stopPropagation();
                onClose();
              }}
            >
              ×
            </button>
          </>
        )}
      </div>
      {contextMenu ? (
        <ActionMenu
          x={contextMenu.x}
          y={contextMenu.y}
          label={t("app.tabMenu")}
          items={menuItems}
          onClose={() => setContextMenu(null)}
        />
      ) : null}
    </>
  );
}

interface TabTitleEditorProps {
  value: string;
  label: string;
  onCommit: (next: string) => void;
  onCancel: () => void;
}

function TabTitleEditor({ value, label, onCommit, onCancel }: TabTitleEditorProps): JSX.Element {
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement>(null);
  // 편집을 시작하면 기존 이름을 통째로 선택해 바로 덮어쓸 수 있게 한다.
  useEffect(() => inputRef.current?.select(), []);
  return (
    <input
      ref={inputRef}
      className="tab-title-input"
      autoFocus
      value={draft}
      aria-label={label}
      size={Math.max(draft.length, 8)}
      onChange={(e) => setDraft(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onBlur={() => onCommit(draft)}
      onKeyDown={(e) => {
        // 탭 활성화 핸들러와 window 단축키 양쪽으로 새지 않게 막는다.
        e.stopPropagation();
        // 한글 조합을 확정하는 Enter는 이름 확정이 아니다(04 §3 IME 규칙). WebKit은 후보를 고르는
        // Enter를 compositionend 뒤에 keyCode 229로 보내므로 그것도 조합으로 본다.
        if (e.key === "Enter" && !e.nativeEvent.isComposing && e.nativeEvent.keyCode !== 229) onCommit(draft);
        else if (e.key === "Escape") onCancel();
      }}
    />
  );
}
