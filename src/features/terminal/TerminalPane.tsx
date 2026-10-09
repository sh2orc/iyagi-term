import { AgentIcon } from "./AgentIcon";
/**
 * TerminalPane (04-ui.md §1·§2): 24px header(title/state/CPU/RAM badge),
 * xterm container, STARTING placeholder, retry/close on failure.
 */

import { memo, useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { TerminalContextMenu, type PaneMenuAnchor } from "./TerminalContextMenu";
import { rightClickRouting } from "./paneContextMenu";
import { reliefMenuItems } from "./reliefMenu";
import { useI18n } from "../../i18n";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { useController } from "../../app/controllerContext";
import { formatCores, formatGiB } from "../monitor/format";
import { exitReasonText, isAbnormalExit, panePhaseText } from "../monitor/statusStrings";
import { findLeaf, leafCount, PANE_HEADER_PX } from "./splitTree";
import { ActionMenu, actionMenuPosition, type ActionMenuItem } from "../../app/ActionMenu";
import { startLayoutDrag } from "../../app/layoutDragSession";
import { TerminalView } from "./TerminalView";
import { terminalDisplayTitle } from "./shellEnvironment";
import {
  agentActivityLabel,
  agentBadgeTooltip,
  agentDisplayName,
  agentDisplayId,
  agentSessionBadgeLabel,
  resolveAgentActivity,
} from "./agentNames";
import { useTerminalActivity, sessionLastOutputAt } from "./activity";
import { useInputHealth } from "./inputHealth";
import { elapsedPhrase, showsLastOutput, staleClass, useNow } from "./lastOutput";
import { agentModelLabel, isAgentModelProvisional } from "./agentModel";
import { resumeCommand, sessionDisplayLabel, type AgentResumeInfo } from "../agentSessions/types";
import { abbreviateHome } from "./displayPath";
import type { AgentStatus } from "../../generated/AgentStatus";
import { queryGitBranch, type GitBranchInfo } from "./gitBranch";
import { missionEntryState } from "../missions/capability";
import { openMissionCreate, paneRepositoryHint } from "../missions/entry";

interface PaneProps {
  leafId: string;
}

/** 흐름 제어 배지가 켜지거나 꺼지기까지 그 상태가 이어져야 하는 시간. */
const FLOW_BADGE_SETTLE_MS = 400;

/**
 * `value`가 `delayMs` 동안 유지된 뒤에만 따라가는 값 — 초당 여러 번 뒤집히는
 * 플래그(흐름 제어)로 배지가 깜빡이며 헤더를 흔들지 않게 한다.
 */
function useSettledFlag(value: boolean, delayMs: number): boolean {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    if (value === settled) return;
    const timer = setTimeout(() => setSettled(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, settled, delayMs]);
  return settled;
}

export const TerminalPane = memo(function TerminalPane({ leafId }: PaneProps): JSX.Element {
  const homeDir = useWorkbenchStore((s) => s.homeDir);
  const { t } = useI18n();
  const controller = useController();
  const pane = useWorkbenchStore((s) => s.panes[leafId]);
  const focused = useWorkbenchStore((s) => s.focusedLeafId === leafId);
  // 동기 입력은 렌더링 중인 pane(= 활성 탭) 전부에 적용된다.
  const broadcasting = useWorkbenchStore((s) => s.broadcastInput);
  const openModal = useWorkbenchStore((s) => s.openModal);
  // pane 메뉴(04-ui §2-5)의 두 조건 — 객체가 아니라 boolean으로 구독해야
  // 탭 배열이 새로 만들어질 때마다 모든 pane이 다시 그려지지 않는다.
  // pane은 terminal 탭 사이에서만 옮긴다(05 §2) — mission/agent-view 탭은 트리가 없다.
  const aloneInTab = useWorkbenchStore((s) => {
    const tab = s.tabs.find((t) => t.kind === "terminal" && findLeaf(t.root, leafId) !== null);
    return tab?.kind === "terminal" ? leafCount(tab.root) === 1 : false;
  });
  const hasOtherTab = useWorkbenchStore((s) => s.tabs.filter((t) => t.kind === "terminal").length > 1);
  // 완화 메뉴의 가용성(08 §2) — 되돌릴 수 없는 플랫폼은 항목을 흐리게 두고
  // 데몬이 준 사유를 툴팁에 싣는다. 객체 참조는 snapshot마다 새로 오므로
  // 두 원시값으로만 구독한다(헤더가 초마다 다시 그려지지 않게).
  const yieldSupport = useWorkbenchStore((s) => s.schedulingYield?.support ?? null);
  const yieldReason = useWorkbenchStore((s) => s.schedulingYield?.reason ?? null);
  const schedulingYield = { support: yieldSupport, reason: yieldReason };
  // 에이전트 pane의 "AI 팀에 맡기기" 항목 가용성(숨김·비활성·사유) — 원시값만 구독한다.
  const missionProtocol = useWorkbenchStore((s) => s.missionProtocol);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  // 활동 마커의 출력 활동 근거 — 자체 상태를 알리지 않는 에이전트(Codex·opencode)용.
  const outputActive = useTerminalActivity(
    (s) => typeof pane?.sessionId === "string" && s.sessions.has(pane.sessionId),
  );
  // 오른쪽 클릭 메뉴가 열린 자리(닫혀 있으면 null) — pane 안의 UI 상태라 store에 두지 않는다.
  const [menuAnchor, setMenuAnchor] = useState<PaneMenuAnchor | null>(null);
  // 흐름 제어 배지는 출력이 많을 때 초당 여러 번 켜졌다 꺼진다 — 잠시 이어진
  // 상태만 보여 헤더가 깜빡이지 않게 한다.
  const flowPaused = useSettledFlag(pane?.flowBlocked ?? false, FLOW_BADGE_SETTLE_MS);

  if (!pane) return <section className="terminal-pane pane-empty" />;

  const onFocus = () => {
    useWorkbenchStore.getState().focusPane(leafId);
    // 사용자가 이 pane을 보고 있다 — 패턴 배지를 끈다(W3-5).
    controller.clearInterventionBadge(leafId);
  };
  const onClose = () => controller.requestClosePanes([leafId]);
  /**
   * pane 메뉴: 분리 → 이동 → 닫기. 할 수 없는 자리는 숨기지 않고 흐리게
   * 남긴다(혼자 있는 창은 이미 탭 하나를 쓰고 있고, 탭이 하나뿐이면 옮길
   * 곳이 없다).
   */
  const menuItems: ActionMenuItem[] = [
    ...reliefMenuItems(controller, leafId, pane, schedulingYield, t),
    ...agentMissionMenuItems(pane, missionProtocol, t),
    { label: t("app.paneMenu.detach"), onSelect: () => controller.detachPaneToNewTab(leafId), disabled: aloneInTab },
    { label: t("app.paneMenu.moveTo"), onSelect: () => openModal({ kind: "move-pane", leafId }), disabled: !hasOtherTab },
    { label: t("terminal.pane.close"), onSelect: onClose },
  ];
  const openMenuAt = (x: number, y: number) =>
    setMenu(actionMenuPosition(x, y, menuItems.length, { width: window.innerWidth, height: window.innerHeight }));
  // 메뉴가 초점을 가져갔으니 닫을 때 터미널에 돌려준다 — 안 그러면 초점이
  // 사라진 버튼 자리(body)에 남아 다음 키 입력이 셸에 닿지 않는다. 항목이
  // 모달을 여는 경우는 모달의 autoFocus가 그 뒤에 실행되어 이긴다.
  const closeMenu = () => {
    setMenu(null);
    const entry = controller.registry.get(pane.viewId);
    (entry?.terminal.element as HTMLElement | null)?.querySelector<HTMLTextAreaElement>("textarea")?.focus();
  };
  const onRetry = () => controller.retryPane(leafId);
  const onNewShell = () => controller.retryPane(leafId, { newShell: true });
  const onResume = (resume: AgentResumeInfo) => {
    void controller.resumeAgentSession(resume, { leafId });
  };
  // 오른쪽 클릭의 주인(rightClickRouting): 기본은 메뉴, 마우스 보고 중인 앱에는 Shift+오른쪽 클릭만.
  const rightClickGoesToApp = (shiftKey: boolean) =>
    rightClickRouting(controller.paneMouseTrackingActive(leafId), shiftKey) === "app";
  // 마우스 보고 중이면 xterm이 오른쪽 누름·뗌을 앱으로 보낸다 — 메뉴가 그 클릭을 가질 때는
  // xterm에 닿기 전(capture)에 멈춰 앱에 짝 없는 오른쪽 클릭이 가지 않게 한다. 보고가 꺼진
  // pane은 건드리지 않는다(xterm의 오른쪽 클릭 단어 선택을 살린다).
  const onRightButtonCapture = (event: ReactMouseEvent<HTMLElement>) => {
    if (event.button !== 2 || !controller.paneMouseTrackingActive(leafId) || rightClickGoesToApp(event.shiftKey)) {
      return;
    }
    event.stopPropagation();
    if (event.type === "mousedown") onFocus();
  };
  const onContextMenu = (event: ReactMouseEvent<HTMLElement>) => {
    // 웹뷰 기본 메뉴(새로고침·요소 검사)는 터미널 위에 띄우지 않는다.
    event.preventDefault();
    if (rightClickGoesToApp(event.shiftKey)) return;
    onFocus();
    setMenuAnchor({ x: event.clientX, y: event.clientY, link: controller.paneHoveredLink(leafId) });
  };

  return (
    <section
      className={`terminal-pane${focused ? " focused" : ""}${broadcasting ? " broadcasting" : ""}`}
      data-leaf-id={leafId}
      onMouseDown={onFocus}
      onMouseDownCapture={onRightButtonCapture}
      onMouseUpCapture={onRightButtonCapture}
      onContextMenu={onContextMenu}
      aria-label={t("terminal.pane.aria", { title: terminalDisplayTitle(pane.title) })}
    >
      <header
        className="pane-header"
        style={{ height: PANE_HEADER_PX }}
        // 헤더를 잡고 끌면 창을 옮긴다(04-ui §2-5). 버튼 위에서 누른 것은 끌기가 아니다.
        onPointerDown={(event) => startLayoutDrag(event, { kind: "pane", leafId }, controller)}
      >
        <span className="pane-grip" aria-hidden="true" title={t("app.drag.paneHint")} />
        {pane.agent && pane.phase === "live" ? (
          // 자리를 고정한다: 마커가 나타났다 사라질 때(작업 중 ↔ idle, 몇 초마다)
          // 제목과 배지가 좌우로 밀리며 헤더가 떨리지 않게.
          <span className="pane-marker-slot">
            <AgentActivityMarker agent={pane.agent} outputActive={outputActive} />
          </span>
        ) : null}
        <span className="pane-title" title={pane.cwd ?? undefined}>
          {terminalDisplayTitle(pane.title)}
          {pane.cwd ? <span className="pane-cwd">{abbreviateHome(pane.cwd, homeDir)}</span> : null}
        </span>
        <GitBranchBadge leafId={leafId} cwd={pane.cwd} />
        {pane.agent ? <AgentBadge agent={pane.agent} /> : null}
        {pane.agent && pane.phase === "live" && typeof pane.sessionId === "string" ? (
          <LastOutputBadge sessionId={pane.sessionId} />
        ) : null}
        {broadcasting ? <span className="pane-broadcast">{t("app.broadcast.pane")}</span> : null}
        {/* 정상 상태(live)의 "실행 중"은 정보가 없어 적지 않는다 — 시작·재생·종료·실패·연결 해제만 적는다. */}
        {pane.phase !== "live" ? (
          <span className={`pane-state state-${pane.phase}`}>{panePhaseText(pane.phase)}</span>
        ) : null}
        {flowPaused ? <span className="pane-flow">{t("terminal.pane.flowPaused")}</span> : null}
        <ReliefBadges pane={pane} />
        <InputHealthBadges pane={pane} />
        {pane.replayTrimmedBytes ? (
          <span
            className="pane-replay-trimmed"
            title={t("terminal.pane.replayTrimmedDetail", { size: formatGiB(pane.replayTrimmedBytes) })}
          >
            {t("terminal.pane.replayTrimmed", { size: formatGiB(pane.replayTrimmedBytes) })}
          </span>
        ) : null}
        {pane.interventionBadge ? (
          <span className="pane-intervention-badge" role="status">{t("terminal.pane.interventionBadge")}</span>
        ) : null}
        {pane.usage ? (
          <span className="pane-usage" title={t("terminal.pane.usageTitle")}>
            CPU {formatCores(pane.usage.cpuCores)} · RAM {formatGiB(pane.usage.residentBytes)}
          </span>
        ) : null}
        {/* 실행 위치 배지("로컬")는 R2 원격 실행이 붙을 때 "원격일 때만" 되살린다 — 지금은 항상 로컬이라 정보가 없다. */}
        <button
          type="button"
          className="pane-menu"
          aria-label={t("app.paneMenu.aria")}
          aria-haspopup="menu"
          aria-expanded={menu !== null}
          // 버튼 자체는 초점을 가져가지 않는다 — 초점은 곧 열릴 메뉴의 것이다.
          onMouseDown={(event) => event.preventDefault()}
          onClick={(event) => {
            // 메뉴가 가리키는 창을 먼저 초점으로 — 어떤 창의 메뉴인지 눈으로 보이게.
            onFocus();
            const rect = event.currentTarget.getBoundingClientRect();
            openMenuAt(rect.left, rect.bottom);
          }}
        >
          ⋯
        </button>
        {menu ? (
          <ActionMenu
            x={menu.x}
            y={menu.y}
            label={t("app.paneMenu")}
            items={menuItems}
            onClose={closeMenu}
          />
        ) : null}
        <button type="button" className="pane-close" aria-label={t("terminal.pane.close")} onClick={onClose}>
          ×
        </button>
      </header>
      <div
        className="pane-body"
        data-phase={pane.phase}
        aria-busy={pane.phase === "replaying" ? true : undefined}
      >
        <TerminalView viewId={pane.viewId} />
        <PaneOverlay
          pane={pane}
          focused={focused}
          onRetry={onRetry}
          onNewShell={onNewShell}
          onClose={onClose}
          onResume={onResume}
        />
      </div>
      {menuAnchor ? (
        <TerminalContextMenu leafId={leafId} anchor={menuAnchor} onClose={() => setMenuAnchor(null)} />
      ) : null}
    </section>
  );
});

/**
 * 완화 배지(08-pressure-relief §2): 양보 중이면 "양보 중", 사용자가 자동
 * 완화에서 빼 두었으면 "보호". 배지는 상태를 알릴 뿐이고 해제는 pane 메뉴에
 * 있다. 툴팁이 자동/수동·경과·부분 적용을 한 줄로 설명한다.
 *
 * 경과는 데몬의 monotonic 시계로만 뜻이 있어(벽시계가 아니다) 마지막 host
 * sample과 비교해 구한다 — 초마다 오는 그 값을 구독하면 pane 헤더 전부가
 * 1초마다 다시 그려지므로, 툴팁용으로 렌더 시점에 한 번만 읽는다.
 */
function ReliefBadges({ pane }: { pane: PaneMeta }): JSX.Element | null {
  const { t } = useI18n();
  const relief = pane.relief ?? { kind: "NONE" as const };
  const isProtected = pane.protected ?? false;
  if (relief.kind !== "YIELDED" && !isProtected) return null;

  let yieldedTitle = "";
  if (relief.kind === "YIELDED") {
    const parts = [t(relief.manual ? "terminal.pane.yieldedManual" : "terminal.pane.yieldedAuto")];
    const nowMs = useWorkbenchStore.getState().host?.monotonic_ms ?? null;
    if (relief.sinceMs !== null && nowMs !== null && nowMs >= relief.sinceMs) {
      const phrase = elapsedPhrase(nowMs - relief.sinceMs);
      parts.push(t("terminal.pane.yieldedSince", { elapsed: t(phrase.key, phrase.params) }));
    }
    if (relief.partial) parts.push(t("terminal.pane.yieldedPartial"));
    yieldedTitle = parts.join(" · ");
  }

  return (
    <>
      {relief.kind === "YIELDED" ? (
        <span className="pane-relief" role="status" title={yieldedTitle}>
          {t("terminal.pane.yielded")}
        </span>
      ) : null}
      {isProtected ? (
        <span className="pane-protected" title={t("terminal.pane.protectedDetail")}>
          {t("terminal.pane.protected")}
        </span>
      ) : null}
    </>
  );
}

/**
 * 입력이 닿지 않는 이유(좀비 pane 진단). 자원 가드 일시정지는 데몬 스냅샷을
 * 거울로 보이고 그 자리에서 재개할 수 있다. 입력 막힘(프로그램이 tty 입력을
 * 읽지 않음)은 데몬이 입력을 거절할 때 켜진다. 예전에는 둘 다 큐 서랍에만
 * 있거나 아예 보이지 않아, pane은 멀쩡한데 키만 사라지는 것처럼 보였다.
 */
function InputHealthBadges({ pane }: { pane: PaneMeta }): JSX.Element | null {
  const { t } = useI18n();
  const controller = useController();
  const sessionId = pane.sessionId;
  // 가드 상태를 파생 인덱스 한 번의 조회로 문자열 프리미티브에 담는다 — 객체/배열
  // 셀렉터는 스냅샷마다 참조가 새로 만들어져 헤더를 다시 그리고, 배열 스캔은
  // pane 수만큼 반복된다. ""=이 pane의 워크로드가 목록에 없음, "r"=정지 아님,
  // "s:<workload_id>:<reason>:<manual>"=일시정지.
  const guardKey = useWorkbenchStore((s) => {
    const workload = pane.workloadId !== null
      ? s.workloadById.get(pane.workloadId)
      : pane.sessionId !== null
        ? s.workloadBySession.get(pane.sessionId)
        : undefined;
    if (workload === undefined) return "";
    if (workload.guard?.kind !== "SUSPENDED") return "r";
    return `s:${workload.workload_id}:${workload.guard.reason}:${workload.guard.manual}`;
  });
  const issue = useInputHealth((s) => (typeof sessionId === "string" ? s.issues.get(sessionId) ?? null : null));

  const guard = guardKey.startsWith("s:")
    ? (() => {
        const [, workloadId, reason, manual] = guardKey.split(":");
        return { workloadId, reason, manual: manual === "true" };
      })()
    : null;
  const suspended = guard !== null || issue === "suspended";
  if (!suspended && issue !== "stalled") return null;

  const suspendedTitle = [
    guard !== null ? t(`queue.guard.reason.${guard.reason}`) : null,
    guard?.manual ? null : t("terminal.pane.suspendedAutoHint"),
  ]
    .filter((part): part is string => part !== null)
    .join(" · ");
  return (
    <>
      {suspended ? (
        <span className="pane-guard" role="status" title={suspendedTitle}>
          {t("terminal.pane.suspended")}
          {guard !== null ? (
            <button
              type="button"
              className="pane-guard-resume"
              onClick={() => void controller.resumeWorkload(guard.workloadId)}
            >
              {t("queue.guard.resume")}
            </button>
          ) : null}
        </span>
      ) : null}
      {!suspended && issue === "stalled" ? (
        <span className="pane-input-stalled" role="status" title={t("terminal.pane.inputStalledDetail")}>
          {t("terminal.pane.inputStalled")}
        </span>
      ) : null}
    </>
  );
}

/** 폴링 결과가 같으면 상태를 갈아 끼우지 않는다(4초마다 헤더 리렌더 방지). */
function sameBranch(a: GitBranchInfo | null, b: GitBranchInfo | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return a.name === b.name && a.detached === b.detached;
}

/**
 * 브랜치 배지 겸 pane의 git 최상위 경로(project 메타)의 출처.
 *
 * 조회할 때마다 결과를 pane 메타(project)로 밀어 넣는다 — mission 생성
 * 대화상자의 저장소 경로 제안 등이 이 값을 쓴다. 재그룹핑(04-ui §2-5)은
 * 이 값이 아니라 cwd로 묶는다. 저장소 밖이거나 cwd를 모르면 null이다.
 */
function GitBranchBadge({ leafId, cwd }: { leafId: string; cwd: string | null }): JSX.Element | null {
  const { t } = useI18n();
  const [branch, setBranch] = useState<GitBranchInfo | null>(null);

  useEffect(() => {
    let active = true;
    setBranch(null);
    if (!cwd) {
      useWorkbenchStore.getState().paneProject(leafId, null);
      return () => { active = false; };
    }
    const refresh = () => {
      // 트레이에 숨어 있는 동안은 폴링하지 않는다 — 보이지 않는 배지를 위해
      // 4초마다 브리지를 깨울 이유가 없다. 다시 보이면 곧바로 갱신한다.
      if (typeof document !== "undefined" && document.hidden) return;
      void queryGitBranch(cwd).then((next) => {
        if (!active) return;
        // 같은 값이면 스토어가 스스로 무시한다(paneProject) — 매 폴링마다
        // 밀어 넣어도 다시 그리지 않는다.
        useWorkbenchStore.getState().paneProject(leafId, next?.top_level ?? null);
        setBranch((previous) => (sameBranch(previous, next) ? previous : next));
      });
    };
    refresh();
    const timer = window.setInterval(refresh, 4_000);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      active = false;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [cwd, leafId]);

  if (!branch) return null;
  const label = t(branch.detached ? "terminal.git.detached" : "terminal.git.branch", {
    branch: branch.name,
  });
  return <span className="pane-git-branch" title={label} aria-label={label}>git:{branch.name}</span>;
}

/**
 * 에이전트가 떠 있는 pane의 ⋯ 메뉴 진입점: 같은 저장소(git 최상위 → cwd)로 새 AI 작업 대화상자를 연다.
 * 지금 대화는 옮겨지지 않는다는 점을 툴팁으로 먼저 알린다. 프로덕션 빌드에서 데몬이 프로토콜을
 * 선언하지 않으면 숨기고, 그 밖에 쓸 수 없으면 사유와 함께 흐리게 둔다.
 */
export function agentMissionMenuItems(
  pane: Pick<PaneMeta, "agent" | "project" | "cwd">,
  missionProtocol: number | null,
  t: (key: string) => string,
): ActionMenuItem[] {
  if (!pane.agent) return [];
  const entry = missionEntryState(missionProtocol);
  if (entry.hidden) return [];
  const repository = paneRepositoryHint(pane);
  return [
    {
      label: t("terminal.pane.missionFromAgent"),
      onSelect: () => openMissionCreate(repository),
      disabled: !entry.enabled,
      title: entry.enabled || !entry.reasonKey ? t("terminal.pane.missionFromAgentHint") : t(entry.reasonKey),
    },
  ];
}

/**
 * 타이틀 앞 활동 마커 — iTerm2에서 CLI가 제목에 스피너를 넣는 그 자리다.
 * 작업 중이면 숨 쉬듯 밝아졌다 흐려지는 초록 점, 확인 대기면 노란 점(정지),
 * idle이면 아무것도 없다.
 */
function AgentActivityMarker({ agent, outputActive }: { agent: AgentStatus; outputActive: boolean }): JSX.Element | null {
  const activity = resolveAgentActivity(agent, outputActive);
  const label = agentActivityLabel(activity, agent.agent);
  if (!label) return null;
  return <span className={`pane-agent-marker ${activity}`} role="img" aria-label={label} title={label} />;
}

/**
 * 감지된 에이전트 배지(04-ui §5): 이름 뒤에 세션 표시(이름 또는 짧은 id)를
 * 붙이고, 툴팁에 전체 세션 id와 그 CLI의 재개 명령을 담는다.
 */
function AgentBadge({ agent }: { agent: AgentStatus }): JSX.Element {
  const session = agentSessionBadgeLabel(agent);
  // 현재 모델·effort(/model·/effort 즉시 반영). 잠정값은 흐리게 구분한다.
  const model = agentModelLabel(agent);
  return (
    <span
      className={`pane-agent-badge agent-${agentDisplayId(agent.agent, agent.model)}`}
      role="status"
      title={agentBadgeTooltip(agent)}
    >
      <AgentIcon agent={agent.agent} />
      {agentDisplayName(agent.agent, agent.model)}
      {session ? <span className="pane-agent-session">{` \u00b7 ${session}`}</span> : null}
      {model ? (
        <span className={`pane-agent-model${isAgentModelProvisional(agent) ? " provisional" : ""}`}>
          {` \u00b7 ${model}`}
        </span>
      ) : null}
    </span>
  );
}

/**
 * 마지막 출력 경과(에이전트 pane 한정): "마지막 출력 3분" — 여러 에이전트를
 * 돌려 둘 때 방치된 터미널을 골라내는 단서다. 시각 자체가 흘러야 갱신되므로
 * 공유 티커(useNow)로 다시 그리고, 문구는 분 단위 이상에서만 바뀐다. 출력이
 * 한 번도 없었던 세션(배지를 띄울 근거가 없다)은 아무것도 보이지 않는다.
 */
function LastOutputBadge({ sessionId }: { sessionId: string }): JSX.Element | null {
  const { t } = useI18n();
  useNow();
  const at = sessionLastOutputAt(sessionId);
  if (at === null) return null;
  const elapsed = Math.max(0, Date.now() - at);
  // 10초 이내는 띄우지 않는다 — 방금 출력한 터미널은 활동 점이 알려 준다.
  if (!showsLastOutput(elapsed)) return null;
  const phrase = elapsedPhrase(elapsed);
  return (
    <span
      className={`pane-last-output${staleClass(elapsed)}`}
      title={t("terminal.pane.lastOutput.tooltip", { time: new Date(at).toLocaleTimeString() })}
    >
      {t("terminal.pane.lastOutput.label", { elapsed: t(phrase.key, phrase.params) })}
    </span>
  );
}

function PaneOverlay(props: {
  pane: PaneMeta;
  /** 이 pane이 초점 pane인가 — 이어서 열기 버튼에 초점을 옮길지 정한다. */
  focused: boolean;
  onRetry: () => void;
  onNewShell: () => void;
  onClose: () => void;
  onResume: (resume: AgentResumeInfo) => void;
}): JSX.Element | null {
  const { t } = useI18n();
  const { pane, focused, onRetry, onNewShell, onClose, onResume } = props;
  if (pane.phase === "starting") {
    return (
      <div className="pane-overlay" role="status">
        {t("terminal.overlay.starting")}
      </div>
    );
  }
  if (pane.phase === "replaying") {
    // 재생 중에는 터미널을 가리지 않고 위에 반투명 배지만 얹는다 — 이전 화면이
    // 비치므로 재생이 살아 보이고(tmux attach), 화면을 숨겼다 드러내는 깜빡임도 없다.
    return (
      <div className="pane-overlay pane-overlay-replay" role="status">
        <span className="pane-replay-badge">{t("terminal.overlay.replaying")}</span>
      </div>
    );
  }
  if (pane.phase === "failed") {
    return (
      <div className="pane-overlay pane-overlay-error" role="alert">
        <p>{pane.error ?? t("terminal.overlay.failed")}</p>
        <div className="overlay-actions">
          <button type="button" onClick={onRetry}>
            {t("terminal.overlay.retry")}
          </button>
          {pane.resume ? (
            <button type="button" onClick={onNewShell}>
              {t("terminal.resume.newShell")}
            </button>
          ) : null}
          <button type="button" onClick={onClose}>
            {t("terminal.overlay.close")}
          </button>
        </div>
      </div>
    );
  }
  if (pane.phase === "exited" && pane.resume) {
    return (
      <ResumeOverlay
        pane={pane}
        resume={pane.resume}
        focused={focused}
        onNewShell={onNewShell}
        onClose={onClose}
        onResume={onResume}
      />
    );
  }
  if (pane.phase === "exited") {
    return (
      <div className="pane-overlay pane-overlay-exited" role="status">
        <p>{pane.error ?? t("terminal.overlay.exited")}</p>
        {pane.exit ? <p className="pane-exit-reason">{exitReasonText(pane.exit)}</p> : null}
        <div className="overlay-actions">
          <button type="button" onClick={onRetry}>
            {t("terminal.overlay.restart")}
          </button>
          <button type="button" onClick={onClose}>
            {t("terminal.overlay.close")}
          </button>
        </div>
      </div>
    );
  }
  return null;
}

/**
 * 이어서 열 수 있는 종료 pane(04-ui §5-1): 제목 / 세션 · 경로 / 선택지 세 줄.
 * 비정상 종료(코드≠0·시그널·OOM·저널 한도)일 때만 그 사이에 사유 한 줄을
 * 더한다(W1-1) — 정상 종료·사용자 취소는 세 줄 그대로다.
 */
function ResumeOverlay(props: {
  pane: PaneMeta;
  resume: AgentResumeInfo;
  focused: boolean;
  onNewShell: () => void;
  onClose: () => void;
  onResume: (resume: AgentResumeInfo) => void;
}): JSX.Element {
  const { t } = useI18n();
  const { pane, resume, focused, onNewShell, onClose, onResume } = props;
  const homeDir = useWorkbenchStore((s) => s.homeDir);
  const primaryRef = useRef<HTMLButtonElement>(null);
  // 초점 pane의 프로세스가 끝나 이 오버레이가 떴으면 Enter 한 번으로 이어
  // 열 수 있게 버튼에 초점을 준다. 다른 pane·팔레트·모달에 초점이 가 있으면
  // 빼앗지 않는다(초점이 이 pane 안이거나 아무 데도 없을 때만).
  useEffect(() => {
    if (!focused) return;
    const button = primaryRef.current;
    if (!button) return;
    const active = document.activeElement;
    const section = button.closest(".terminal-pane");
    if (active && active !== document.body && !(section?.contains(active) ?? false)) return;
    button.focus();
  }, [focused]);

  const command = resumeCommand(resume.agent, resume.agentSessionId);
  const sessionTooltip = [
    t("terminal.agent.sessionId", { id: resume.agentSessionId }),
    ...(command ? [t("terminal.agent.sessionResume", { command })] : []),
  ].join("\n");
  const abnormal = pane.exit && isAbnormalExit(pane.exit) ? pane.exit : null;

  return (
    <div className="pane-overlay pane-overlay-exited pane-overlay-resume" role="status">
      <p className="pane-resume-title">
        {t("terminal.resume.title", { agent: agentDisplayName(resume.agent) })}
      </p>
      {abnormal ? <p className="pane-exit-reason">{exitReasonText(abnormal)}</p> : null}
      {/* 세션 이름과 작업 경로를 한 줄에 둔다 — 오버레이를 세 줄(제목·위치·선택지)로 유지한다. */}
      <p className="pane-resume-where">
        <span className="pane-resume-session" title={sessionTooltip}>
          {sessionDisplayLabel(resume.title, resume.agentSessionId)}
        </span>
        <span className="pane-resume-sep" aria-hidden="true">
          {" · "}
        </span>
        <span className="pane-resume-cwd" title={`${t("terminal.resume.cwdLabel")}: ${resume.cwd}`}>
          {abbreviateHome(resume.cwd, homeDir)}
        </span>
      </p>
      <div className="overlay-actions">
        <button type="button" className="primary" ref={primaryRef} onClick={() => onResume(resume)}>
          {t("terminal.resume.open")}
        </button>
        <button type="button" onClick={onNewShell}>
          {t("terminal.resume.newShell")}
        </button>
        <button type="button" onClick={onClose}>
          {t("terminal.overlay.close")}
        </button>
      </div>
    </div>
  );
}
