/** Team navigation and a single, full-size execution workspace. */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import type { Decision } from "../../generated/Decision";
import type { Mission } from "../../generated/Mission";
import type { MissionControlAction } from "../../generated/MissionControlAction";
import type { Task } from "../../generated/Task";
import { useI18n, type MessageParams } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { useMissionStore } from "./store";
import {
  selectAutoSelectTaskId,
  selectLiveRunCount,
  selectOpenDecisions,
  selectTaskList,
} from "./selectors";
import {
  defaultMissionUi,
  defaultRightPane,
  useMissionUiStore,
  type RightPane,
} from "./uiStore";
import { LeadConversation } from "./LeadConversation";
import { TeamList } from "./TeamList";
import { RunDetail } from "./RunDetail";
import {
  DecisionPanel,
  isTaskFailureDecision,
  isReconciledDecision,
  isReviewRepairDecision,
  isIntegrationConflictDecision,
} from "./DecisionPanel";
import { ResultReview } from "./ResultReview";
import { PolicyLimitsEditor } from "./CostPolicyEditor";
import { MissionTeamEditor } from "./MissionTeamEditor";
import { getMissionClient } from "./clientAccess";
import { openNewMissionFrom } from "./followUp";
import { missionStateLabelKey, phaseLabelKey } from "./labels";
import { isCostDecision } from "./costs";
import { approvalText, fileChangeRequest } from "./FileChangeApproval";
import { MissionErrorNotice, missionError, type MissionErrorAction, type MissionUiError } from "./errors";
import { mutateWithResync } from "./mutation";
import {
  CONNECTION_POLL_MS,
  checkMissionConnection,
  classifySyncError,
  syncErrorCause,
  syncRetryDelayMs,
  useMissionConnection,
} from "./missionConnection";
import { showArchiveUndoToast } from "./ArchiveUndoToast";
import { selectFirstRunAwaitingExit, selectSlotHoldingRunCount } from "./runReconciliation";
import { formatElapsedTime, newRequestId, useArtifactText } from "./viewUtils";
import "./missions.css";
import "./missionPage.css";

const TERMINAL_MISSION_STATES = new Set(["completed", "failed", "cancelled"]);
const WIDE_MIN = 1100;
const MEDIUM_MIN = 760;
/** `답변을 저장했습니다`를 보여 주는 시간. */
export const DECISION_SAVED_NOTICE_MS = 4000;

type Translate = (key: string, params?: MessageParams) => string;

export function MissionPage(props: { missionId: string }): JSX.Element {
  // Workbench는 mission 탭을 바꿔도 같은 자리에 이 컴포넌트를 그린다. 결정 패널·
  // 확인창 같은 지역 상태가 다른 mission으로 새지 않게 mission마다 다시 마운트한다.
  return <MissionPageView key={props.missionId} missionId={props.missionId} />;
}

/** 재연결 뒤 강제 동기화 — 끊김 중에 시작된 동기화에 합류해 실패했으면 한 번 더 뽑는다. */
async function resyncAfterReconnect(missionId: string): Promise<void> {
  await useMissionStore.getState().syncMission(missionId);
  if (useMissionStore.getState().sync[missionId]?.error) await useMissionStore.getState().syncMission(missionId);
}

/** 모델 변경 행동의 대상 할 일: 막히거나 실패한 첫 할 일, 없으면 현재 선택. */
function modelChangeTaskId(missionId: string, selectedTaskId: string | null): string | null {
  const stuck = selectTaskList(useMissionStore.getState(), missionId).find(
    (task) => task.state === "blocked" || task.state === "failed",
  );
  return stuck?.id ?? selectedTaskId;
}

function closeMissionTab(missionId: string): void {
  const workbench = useWorkbenchStore.getState();
  const tab = workbench.tabs.find((candidate) => candidate.kind === "mission" && candidate.missionId === missionId);
  if (tab) workbench.closeTab(tab.id);
}

/** 배너의 짧은 제목 — 질문 원문(JSON일 수 있다) 대신 결정 종류별 문구. */
export function decisionBannerTitle(t: Translate, decision: Decision, isFileChange: boolean): string {
  if (isFileChange) return t("missions.decision.fileChange");
  if (isIntegrationConflictDecision(decision)) return t("missions.integration.summary");
  if (isReviewRepairDecision(decision)) return t("missions.requiredRepair.reviewSummary");
  if (isReconciledDecision(decision)) return t("missions.recovery.summary");
  if (isTaskFailureDecision(decision)) return t("missions.decision.failureSummary");
  if (isCostDecision(decision)) return t("missions.cost.summary");
  return t(`missions.banner.kind.${decision.kind}`);
}

function MissionPageView(props: { missionId: string }): JSX.Element {
  const { t } = useI18n();
  const missionId = props.missionId;
  const mission = useMissionStore((s) => s.missions[missionId] ?? null);
  const syncStatus = useMissionStore((s) => s.sync[missionId] ?? null);
  const syncMission = useMissionStore((s) => s.syncMission);

  // 첫 마운트에 snapshot을 뽑고, hint로 dirty가 되면 다시 뽑는다(05 §10).
  useEffect(() => {
    void syncMission(missionId);
  }, [missionId, syncMission]);
  const dirty = syncStatus?.dirty ?? false;
  const loading = syncStatus?.loading ?? false;
  const syncError = syncStatus?.error ?? null;
  // 실패한 뒤 dirty가 그대로면 곧바로 다시 뽑지 않는다 — loading이 풀릴 때마다 즉시 재시도하면
  // 데몬이 거절하는 동안 요청이 끝없이 이어진다. 1s → 2s → 4s … 최대 30s로 늘리고, 성공하면 처음으로.
  const syncFailures = useRef(0);
  const [syncRetryTick, setSyncRetryTick] = useState(0);
  useEffect(() => {
    if (loading) return;
    if (syncError === null) syncFailures.current = 0;
    if (!dirty) return;
    if (syncError === null) {
      void syncMission(missionId);
      return;
    }
    let current = true;
    const timer = setTimeout(() => {
      syncFailures.current += 1;
      void syncMission(missionId).finally(() => {
        // 같은 오류로 곧바로 다시 실패하면(loading 변화가 한 번에 합쳐져) 관찰하는 값이 그대로라
        // 이 effect가 다시 돌지 않는다 — 그대로면 한 번 더 판단하게 한다.
        if (current) setSyncRetryTick((tick) => tick + 1);
      });
    }, syncRetryDelayMs(syncFailures.current));
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [dirty, loading, syncError, missionId, syncMission, syncRetryTick]);

  // 전송 계층 연결 상태 — 끊김과 동기화 지연을 구분하고, 재연결되면 강제 동기화.
  const connection = useMissionConnection();
  const disconnected = connection.status === "disconnected";
  const reconnectsSeen = useRef(connection.reconnects);
  useEffect(() => {
    if (connection.reconnects === reconnectsSeen.current) return;
    reconnectsSeen.current = connection.reconnects;
    void resyncAfterReconnect(missionId);
  }, [connection.reconnects, missionId]);
  const syncProblem = classifySyncError(syncError);
  const lastConnectionCheck = useRef<number | null>(null);
  useEffect(() => {
    // 지연처럼 보이는 실패는 실제 끊김일 수 있다 — 기다리지 않고 한 번 확인한다.
    // 실패가 반복돼도 주기 확인(CONNECTION_POLL_MS)보다 자주 부르지 않는다.
    if (classifySyncError(syncError) !== "transient") return;
    const now = Date.now();
    if (lastConnectionCheck.current !== null && now - lastConnectionCheck.current < CONNECTION_POLL_MS) return;
    lastConnectionCheck.current = now;
    void checkMissionConnection();
  }, [syncError]);

  // 반응형 폭 측정(05 §7). 로딩 화면 → 본 화면으로 요소가 바뀌어도 다시 관찰한다.
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [containerEl, setContainerEl] = useState<HTMLDivElement | null>(null);
  const attachContainer = useCallback((element: HTMLDivElement | null) => {
    containerRef.current = element;
    setContainerEl(element);
  }, []);
  const [width, setWidth] = useState(() => (typeof window === "undefined" ? 1280 : window.innerWidth));
  useEffect(() => {
    if (!containerEl || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver((entries) => {
      for (const entry of entries) setWidth(entry.contentRect.width);
    });
    observer.observe(containerEl);
    return () => observer.disconnect();
  }, [containerEl]);
  const mode: "wide" | "medium" | "narrow" = width >= WIDE_MIN ? "wide" : width >= MEDIUM_MIN ? "medium" : "narrow";

  const ui = useMissionUiStore((s) => s.perMission[missionId]);
  const patchUi = useMissionUiStore((s) => s.patchUi);
  const selectTaskInUi = useMissionUiStore((s) => s.selectTask);
  const autoSelectTask = useMissionUiStore((s) => s.autoSelectTask);
  const openTaskDetail = useMissionUiStore((s) => s.openTaskDetail);
  const chooseRightPane = useMissionUiStore((s) => s.chooseRightPane);
  const observePhase = useMissionUiStore((s) => s.observePhase);
  const closeSheetInUi = useMissionUiStore((s) => s.closeSheet);
  const effectiveUi = useMemo(() => ({ ...defaultMissionUi(), ...ui }), [ui]);

  // 인수 대기로 "바뀌는 순간" 한 번 결과 화면으로(좁은 폭은 결과 탭으로).
  const phase = mission?.phase ?? null;
  useEffect(() => {
    if (phase !== null) observePhase(missionId, phase);
  }, [missionId, phase, observePhase]);

  // 빈 상태(05 §5): 선택이 없으면 실행 중인 Lead 또는 첫 실행 중 할 일을 고른다.
  const autoTaskId = useMissionStore((s) => selectAutoSelectTaskId(s, missionId));
  const selectedTaskId = effectiveUi.selectedTaskId;
  useEffect(() => {
    if (autoTaskId !== null && selectedTaskId === null) autoSelectTask(missionId, autoTaskId);
  }, [autoTaskId, selectedTaskId, missionId, autoSelectTask]);
  const selectedTaskLive = useMissionStore((s) => (selectedTaskId !== null ? s.tasks[selectedTaskId] ?? null : null));
  const previousSheetOpen = useRef(effectiveUi.sheetOpen);
  useEffect(() => {
    const changed = previousSheetOpen.current !== effectiveUi.sheetOpen;
    previousSheetOpen.current = effectiveUi.sheetOpen;
    if (mode !== "narrow" || !changed) return;
    if (effectiveUi.sheetOpen) {
      containerRef.current?.querySelector<HTMLElement>(".mission-workspace-content")?.focus();
    } else if (selectedTaskId !== null) {
      containerRef.current?.querySelector<HTMLElement>(`[data-task-id="${selectedTaskId}"]`)?.focus();
    }
  }, [mode, effectiveUi.sheetOpen, selectedTaskId]);


  // 결정 배너/패널(05 §8): 배너는 현재 질문을 이 페이지 안에서 연다.
  const openDecisions = useMissionStore(useShallow((s) => selectOpenDecisions(s, missionId)));
  const [decisionPanelId, setDecisionPanelId] = useState<string | null>(null);
  const panelDecision = useMissionStore((s) =>
    decisionPanelId !== null ? s.decisions[decisionPanelId] ?? null : null,
  );
  const nextOpenDecisionId = openDecisions[0]?.id ?? null;
  const panelState = panelDecision?.state ?? null;
  const [savedNotice, setSavedNotice] = useState(0);
  const [savedVisible, setSavedVisible] = useState(false);
  useEffect(() => {
    if (decisionPanelId === null || panelState === null || panelState === "open") return;
    if (panelState === "answered") {
      // 답한 결정에 머물지 않는다: 다음 열린 결정으로, 없으면 패널을 닫는다.
      setSavedNotice((count) => count + 1);
      setDecisionPanelId(nextOpenDecisionId);
    } else if (nextOpenDecisionId === null) {
      setDecisionPanelId(null);
    }
  }, [decisionPanelId, panelState, nextOpenDecisionId]);
  useEffect(() => {
    if (savedNotice === 0) return;
    setSavedVisible(true);
    const timer = setTimeout(() => setSavedVisible(false), DECISION_SAVED_NOTICE_MS);
    return () => clearTimeout(timer);
  }, [savedNotice]);
  const bannerDecision = panelDecision?.state === "open" ? panelDecision : openDecisions[0] ?? null;

  const onErrorAction = (action: MissionErrorAction) => {
    switch (action) {
      case "resync":
        void syncMission(missionId);
        return;
      case "open_settings":
        useWorkbenchStore.getState().openSettings("missions");
        return;
      case "change_model": {
        const taskId = modelChangeTaskId(missionId, selectedTaskId);
        if (taskId !== null) openTaskDetail(missionId, taskId);
        return;
      }
      case "open_list":
        useWorkbenchStore.getState().openModal({ kind: "mission-list" });
        return;
      case "retry":
        void syncMission(missionId);
        return;
      case "login":
        // 안내는 MissionErrorNotice가 펼친다.
        return;
    }
  };

  if (!mission) {
    if (syncProblem === "not_found") {
      return (
        <div className="mission-page mission-page-loading mission-not-found" ref={attachContainer} data-testid="mission-not-found">
          <h2>{t("missions.notFound.title")}</h2>
          <p className="muted">{t("missions.notFound.body")}</p>
          <button type="button" onClick={() => closeMissionTab(missionId)} data-testid="not-found-close">
            {t("missions.notFound.closeTab")}
          </button>
        </div>
      );
    }
    return (
      <div className={`mission-page mission-page-loading${disconnected ? " offline" : ""}`} ref={attachContainer}>
        {syncError === null && !disconnected ? <p className="muted">{t("missions.sync.loading")}</p> : null}
        <SyncNotice
          missionId={missionId}
          error={syncError}
          loading={loading}
          disconnected={disconnected}
          onAction={onErrorAction}
        />
      </div>
    );
  }

  const onOpenTask = (taskId: string) => openTaskDetail(missionId, taskId);
  const onOpenRunId = (runId: string) => {
    const run = useMissionStore.getState().runs[runId];
    if (run) openTaskDetail(missionId, run.task_id);
  };
  const onOpenResult = () => {
    chooseRightPane(missionId, "result");
    patchUi(missionId, { narrowView: "result" });
    // 결과 화면의 `내 브랜치로 가져오기` 안내로 이동한다(그려진 뒤).
    setTimeout(() => {
      containerRef.current?.querySelector<HTMLElement>("[data-testid='result-import']")?.scrollIntoView?.({ block: "nearest" });
    }, 0);
  };
  const onRequestChanges = () => {
    chooseRightPane(missionId, "lead");
    patchUi(missionId, { narrowView: "lead" });
    // 좁은 화면이면 대화 영역이 새로 그려지고, 입력창은 초안 복원이 끝나야 활성화된다 —
    // 잠시 기다렸다가 초점을 준다(끝난 작업처럼 입력창이 없으면 그만둔다).
    const focusComposer = (attempt: number) => {
      const input = containerRef.current?.querySelector<HTMLTextAreaElement>("[data-testid='lead-composer']");
      if (input && !input.disabled) {
        input.focus();
        return;
      }
      if (attempt < 20) setTimeout(() => focusComposer(attempt + 1), 50);
    };
    setTimeout(() => focusComposer(0), 0);
  };
  const onArchived = () => {
    showArchiveUndoToast({ missionId, title: mission.title });
    closeMissionTab(missionId);
  };

  const rightPane: RightPane = effectiveUi.rightPane ?? defaultRightPane(mission.phase, mission.state);
  const resultPending = mission.phase === "awaiting_acceptance";
  const showNavigation = mode !== "narrow" || (rightPane === "detail" && !effectiveUi.sheetOpen);
  const showContent = mode !== "narrow" || !showNavigation;

  // 끊김이면 이 영역들의 변경 요청 버튼을 한꺼번에 막는다(읽기는 그대로).
  const guarded = (content: JSX.Element) => (
    <fieldset className="mission-action-guard" disabled={disconnected}>
      {content}
    </fieldset>
  );
  const runDetail = (task: Task) =>
    guarded(
      <RunDetail
        mission={mission}
        task={task}
        detailTab={effectiveUi.detailTab}
        onDetailTabChange={(tab) => patchUi(missionId, { detailTab: tab })}
        onOpenLead={() => {
          closeSheetInUi(missionId);
          onRequestChanges();
        }}
      />,
    );
  const resultReview = () =>
    guarded(
      <ResultReview
        mission={mission}
        onRefresh={() => void syncMission(missionId)}
        onOpenTask={onOpenTask}
        onOpenDecision={(decisionId) => setDecisionPanelId(decisionId)}
        onRequestChanges={onRequestChanges}
        onArchived={onArchived}
      />,
    );
  const pendingDot = resultPending ? (
    <span className="mission-pending-dot" title={t("missions.page.resultPending")} data-testid="result-pending-dot">
      <span className="mission-visually-hidden">{t("missions.page.resultPending")}</span>
    </span>
  ) : null;

  return (
    <div
      className={`mission-page mission-workspace mode-${mode}${disconnected ? " offline" : ""}`}
      ref={attachContainer}
      data-mode={mode}
      data-connection={connection.status}
      data-testid="mission-page"
    >
      <MissionHeader
        mission={mission}
        disconnected={disconnected}
        onOpenRunId={onOpenRunId}
        onErrorAction={onErrorAction}
        onArchived={onArchived}
      />
      <SyncNotice
        missionId={missionId}
        error={syncError}
        loading={loading}
        disconnected={disconnected}
        onAction={onErrorAction}
      />
      <DecisionBanner
        decision={bannerDecision}
        count={openDecisions.length}
        expanded={bannerDecision !== null && decisionPanelId === bannerDecision.id}
        saved={savedVisible}
        disconnected={disconnected}
        onToggle={() => {
          if (bannerDecision === null) return;
          const id = bannerDecision.id;
          setDecisionPanelId((current) => (current === id ? null : id));
        }}
      />
      {decisionPanelId !== null && panelDecision && !disconnected ? (
        <DecisionPanel
          mission={mission}
          decision={panelDecision}
          currentDecisionId={nextOpenDecisionId}
          onJumpToCurrent={() => setDecisionPanelId(nextOpenDecisionId)}
          onOpenTask={onOpenTask}
        />
      ) : null}
      <nav className="mission-workspace-nav" aria-label={mission.title}>
        <button type="button" aria-pressed={rightPane === "detail"} data-testid="toggle-detail"
          onClick={() => { chooseRightPane(missionId, "detail"); if (mode === "narrow") closeSheetInUi(missionId); }}>
          {t("missions.workspace.team")}
        </button>
        <button type="button" data-testid="toggle-lead" aria-pressed={rightPane === "lead"} onClick={() => chooseRightPane(missionId, "lead")}>
          {t("missions.workspace.lead")}
        </button>
        <button type="button" aria-pressed={rightPane === "result"} data-testid="toggle-result"
          onClick={() => chooseRightPane(missionId, "result")}>
          {t("missions.result.title")}{pendingDot}
        </button>
      </nav>
      <div className="mission-body mission-workspace-body">
        {showNavigation ? (
          <aside className="mission-workspace-team" aria-label={t("missions.workspace.team")}>
            <TeamList
              missionId={missionId}
              selectedTaskId={rightPane === "detail" ? effectiveUi.selectedTaskId : null}
              filter={effectiveUi.filter}
              search={effectiveUi.search}
              onSelectTask={(taskId) => selectTaskInUi(missionId, taskId)}
              onFilterChange={(filter) => patchUi(missionId, { filter })}
              onSearchChange={(search) => patchUi(missionId, { search })}
            />
          </aside>
        ) : null}
        {showContent ? (
          <main className="mission-workspace-content" data-testid="right-bottom" tabIndex={-1}
            onKeyDown={(event) => {
              if (mode === "narrow" && rightPane === "detail" && event.key === "Escape" && !event.defaultPrevented) {
                event.preventDefault();
                closeSheetInUi(missionId);
              }
            }}>
            {rightPane === "lead" ? (
              <LeadConversation
                mission={mission}
                disconnected={disconnected}
                onOpenRun={(run) => openTaskDetail(missionId, run.task_id)}
                onStartFollowUp={() => void openNewMissionFrom(mission, "follow-up")}
                onStartSameGoal={() => void openNewMissionFrom(mission, "same-goal")}
                onOpenSettings={() => useWorkbenchStore.getState().openSettings("missions")}
                onOpenResult={onOpenResult}
              />
            ) : rightPane === "result" ? resultReview() : (
              <>
                <div className="mission-workspace-toolbar">
                  {mode === "narrow" ? (
                    <button type="button" onClick={() => closeSheetInUi(missionId)}>{t("missions.workspace.back")}</button>
                  ) : null}
                  <span className="muted">{t("missions.workspace.record")}</span>
                  {selectedTaskLive ? (
                    <button type="button" onClick={() => useWorkbenchStore.getState().openAgentViewTab(missionId, selectedTaskLive.id, selectedTaskLive.title)}>
                      {t("missions.workspace.openTab")}
                    </button>
                  ) : null}
                </div>
                {selectedTaskLive ? runDetail(selectedTaskLive) : (
                  <p className="muted mission-detail-empty">{t("missions.detail.noSelection")}</p>
                )}
              </>
            )}
          </main>
        ) : null}
      </div>
    </div>
  );
}

/**
 * 연결·동기화 알림(05 §10): 끊김 > 기록 없음 > 일시 지연 > 실패 순으로 하나만.
 * 일시 오류(BUSY·timeout·SNAPSHOT_EXPIRED)는 끊김으로 표시하지 않는다.
 */
function SyncNotice(props: {
  missionId: string;
  error: string | null;
  loading: boolean;
  disconnected: boolean;
  onAction: (action: MissionErrorAction) => void;
}): JSX.Element | null {
  const { t } = useI18n();
  const syncMission = useMissionStore((s) => s.syncMission);
  const retry = () => void syncMission(props.missionId);
  if (props.disconnected) {
    return (
      <div className="mission-connection-lost" role="alert" data-testid="connection-lost">
        <span>{t("missions.disconnected")}</span>
      </div>
    );
  }
  const problem = classifySyncError(props.error);
  if (problem === "none" || props.error === null) return null;
  if (problem === "not_found") {
    return (
      <div className="mission-area-error mission-sync-error" role="alert" data-testid="sync-not-found">
        <span>{t("missions.notFound.title")}</span>
        <button type="button" onClick={() => closeMissionTab(props.missionId)} data-testid="not-found-close">
          {t("missions.notFound.closeTab")}
        </button>
      </div>
    );
  }
  if (problem === "transient") {
    return (
      <div className="mission-sync-delayed" data-testid="sync-delayed">
        <span>{t("missions.sync.delayed")}</span>
        <button type="button" disabled={props.loading} onClick={retry} data-testid="sync-retry">
          {t("missions.sync.tryAgain")}
        </button>
      </div>
    );
  }
  return (
    <div className="mission-sync-error" data-testid="sync-error">
      <MissionErrorNotice error={missionError(t, syncErrorCause(props.error))} onRetry={retry} onAction={props.onAction} />
    </div>
  );
}

type HeaderControl = Extract<MissionControlAction, "start" | "pause" | "resume" | "cancel" | "archive">;

/** 시작이 거절됐을 때 같은 목표로 새 작업을 만들어야 하는 사유(저장소·기준 커밋이 바뀜). */
const RECREATE_REASONS: ReadonlySet<string> = new Set(["base_changed", "repository_changed"]);

function controlAllowed(mission: Mission, action: HeaderControl): boolean {
  switch (action) {
    case "start":
      return mission.state === "draft";
    case "pause":
      return mission.state === "running";
    case "resume":
      return mission.state === "paused";
    case "cancel":
      return !TERMINAL_MISSION_STATES.has(mission.state) && mission.state !== "stopping";
    case "archive":
      return TERMINAL_MISSION_STATES.has(mission.state) && mission.archived_at === null;
  }
}

/** mission header — 제목 · 상태 · phase · 제어 드롭다운(05 §1·§8). */
function MissionHeader(props: {
  mission: Mission;
  disconnected: boolean;
  onOpenRunId: (runId: string) => void;
  onErrorAction: (action: MissionErrorAction) => void;
  onArchived: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const mission = props.mission;
  const liveRuns = useMissionStore((s) => selectLiveRunCount(s, mission.id));
  // 멈추는 중: 실행 자리를 잡은 실행(live + 종료 증거 없는 불확실 실행)을 센다. 첫 실행 열기는
  // 종료 증거가 없는 실행을 먼저 연다 — 상세에서 "프로세스 종료를 직접 확인했습니다" 동선이 보인다.
  const waitingRuns = useMissionStore((s) => selectSlotHoldingRunCount(s, mission.id));
  const firstWaitingRunId = useMissionStore((s) => selectFirstRunAwaitingExit(s, mission.id));
  const [menuOpen, setMenuOpen] = useState(false);
  const [confirm, setConfirm] = useState<"cancel" | "archive" | "delete-draft" | null>(null);
  const [limitsOpen, setLimitsOpen] = useState(false);
  const [teamOpen, setTeamOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const startRequested = useMissionUiStore((s) => s.perMission[mission.id]?.startRequested ?? false);
  const [error, setError] = useState<MissionUiError | null>(null);
  const lastAction = useRef<HeaderControl | "delete-draft" | null>(null);
  const draft = mission.state === "draft";
  const terminal = TERMINAL_MISSION_STATES.has(mission.state);
  const settling = mission.state === "stopping" || mission.state === "pausing";
  const menuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (event: MouseEvent) => {
      if (!menuRef.current?.contains(event.target as globalThis.Node)) setMenuOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  const control = async (action: HeaderControl) => {
    lastAction.current = action;
    setMenuOpen(false);
    const client = getMissionClient();
    if (!client) {
      setConfirm(null);
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await mutateWithResync(mission.id, (current) =>
        controlAllowed(current, action)
          ? client.missionControl({
              request_id: newRequestId(),
              mission_id: current.id,
              expected_revision: current.revision,
              action,
            })
          : null,
      );
      setConfirm(null);
      if (action === "archive") props.onArchived();
    } catch (cause) {
      setConfirm(null);
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  // Creation hands startup to this mounted view so failures use the same
  // retry/recreate controls as any other draft. Consume before dispatching.
  useEffect(() => {
    if (props.disconnected || !startRequested || !useMissionUiStore.getState().perMission[mission.id]?.startRequested) return;
    useMissionUiStore.getState().patchUi(mission.id, { startRequested: false });
    if (mission.state === "draft") void control("start");
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one-shot request, not a render-driven retry
  }, [mission.id, startRequested, props.disconnected]);

  /**
   * 초안 삭제: 초안을 중단(cancel)한 뒤 보관한다. 보관은 목록 분류라 되돌리기 토스트가 뜬다.
   * 중단 직후 store가 아직 draft일 수 있어 한 번 동기화하고, 그래도 늦으면 중단 결과의 revision을 쓴다.
   */
  const deleteDraft = async () => {
    lastAction.current = "delete-draft";
    setMenuOpen(false);
    const client = getMissionClient();
    if (!client) {
      setConfirm(null);
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      let cancelledRevision: string | null = null;
      if ((useMissionStore.getState().missions[mission.id] ?? mission).state === "draft") {
        const cancelled = await mutateWithResync(mission.id, (current) =>
          current.state === "draft"
            ? client.missionControl({
                request_id: newRequestId(),
                mission_id: current.id,
                expected_revision: current.revision,
                action: "cancel",
              })
            : null,
        );
        cancelledRevision = cancelled.revision;
        await useMissionStore.getState().syncMission(mission.id);
      }
      await mutateWithResync(mission.id, (current) => {
        if (current.archived_at !== null) return null;
        const stale = cancelledRevision !== null && BigInt(current.revision) < BigInt(cancelledRevision);
        if (!stale && !TERMINAL_MISSION_STATES.has(current.state)) return null;
        return client.missionControl({
          request_id: newRequestId(),
          mission_id: current.id,
          expected_revision: stale && cancelledRevision !== null ? cancelledRevision : current.revision,
          action: "archive",
        });
      });
      setConfirm(null);
      props.onArchived();
    } catch (cause) {
      setConfirm(null);
      setError(missionError(t, cause));
    } finally {
      setBusy(false);
    }
  };

  const retryLast = () => {
    const action = lastAction.current;
    if (action === null) return;
    if (action === "delete-draft") {
      setError(null);
      setConfirm("delete-draft");
      return;
    }
    // 중단·보관은 다시 확인을 받는다.
    if (action === "cancel" || action === "archive") {
      setError(null);
      setConfirm(action);
      return;
    }
    void control(action);
  };

  const blocked = busy || startRequested || props.disconnected;

  return (
    <header className="mission-header">
      <div className="mission-header-title">
        <h2>{mission.title}</h2>
        <span className={`mission-state state-${mission.state}`}>{t(missionStateLabelKey(mission.state))}</span>
        <span className="muted">· {t(phaseLabelKey(mission.phase))}</span>
        <span className="muted mission-elapsed" data-testid="mission-elapsed" title={t("missions.time.definition")}>
          {t("missions.time.budget", { elapsed: formatElapsedTime(mission.active_time_ms), limit: formatElapsedTime(mission.policy.active_time_limit_ms) })}
        </span>
      </div>
      <div className="mission-header-actions" ref={menuRef}>
        <button
          type="button"
          className="mission-control-button"
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          disabled={terminal || props.disconnected}
          title={terminal ? t("missions.control.disabledTerminal") : props.disconnected ? t("missions.control.offline") : undefined}
          onClick={() => setMenuOpen(!menuOpen)}
          data-testid="mission-control"
        >
          {t("missions.page.controlMenu")} ▾
        </button>
        {menuOpen ? (
          <div className="mission-control-menu" role="menu" data-testid="mission-control-menu">
            {settling ? (
              <>
                <div className="mission-control-waiting" role="menuitem" aria-disabled="true" data-testid="control-waiting">
                  {t("missions.control.waitingRuns", { count: waitingRuns })}
                </div>
                {firstWaitingRunId !== null ? (
                  <button
                    type="button"
                    role="menuitem"
                    onClick={() => {
                      setMenuOpen(false);
                      props.onOpenRunId(firstWaitingRunId);
                    }}
                    data-testid="control-open-first-run"
                  >
                    {t("missions.control.openFirstRun")}
                  </button>
                ) : null}
              </>
            ) : (
              <>
                {mission.state === "running" ? (
                  <button type="button" role="menuitem" disabled={blocked} onClick={() => void control("pause")} data-testid="mission-pause">
                    {t("missions.control.pause")}
                  </button>
                ) : null}
                {mission.state === "paused" ? (
                  <button type="button" role="menuitem" disabled={blocked} onClick={() => void control("resume")} data-testid="mission-resume">
                    {t("missions.control.resume")}
                  </button>
                ) : null}
                <button
                  type="button"
                  role="menuitem"
                  className="danger"
                  disabled={blocked}
                  onClick={() => {
                    setMenuOpen(false);
                    setError(null);
                    setConfirm("cancel");
                  }}
                  data-testid="mission-cancel"
                >
                  {t("missions.control.cancel")}
                </button>
              </>
            )}
            {mission.state !== "stopping" ? (
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setMenuOpen(false);
                  setLimitsOpen(true);
                }}
                data-testid="mission-limits"
              >
                {t("missions.control.limits")}
              </button>
            ) : null}
            {mission.state !== "stopping" ? (
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setMenuOpen(false);
                  setTeamOpen(true);
                }}
                data-testid="mission-team"
              >
                {t("missions.control.team")}
              </button>
            ) : null}
          </div>
        ) : null}
        {confirm === "cancel" ? (
          <div className="mission-inline-confirm" role="alertdialog" data-testid="mission-cancel-confirm">
            <h4>{t("missions.control.cancelTitle")}</h4>
            <p>{t("missions.control.cancelBody", { runs: liveRuns })}</p>
            <button type="button" className="danger" disabled={blocked} onClick={() => void control("cancel")} data-testid="mission-cancel-ok">
              {t("missions.common.ok")}
            </button>
            <button type="button" onClick={() => setConfirm(null)}>
              {t("missions.common.cancel")}
            </button>
          </div>
        ) : null}
        {confirm === "archive" ? (
          <div className="mission-inline-confirm" role="alertdialog" aria-labelledby={`archive-title-${mission.id}`} data-testid="mission-archive-confirm">
            <h4 id={`archive-title-${mission.id}`}>{t("missions.control.archiveTitle")}</h4>
            <p>{t("missions.control.archiveBody")}</p>
            <button type="button" className="primary" disabled={blocked} onClick={() => void control("archive")} data-testid="mission-archive-ok">
              {t("missions.control.archive")}
            </button>
            <button type="button" onClick={() => setConfirm(null)}>
              {t("missions.common.cancel")}
            </button>
          </div>
        ) : null}
        {limitsOpen ? (
          <div
            className="mission-inline-confirm mission-limits-popover"
            role="dialog"
            aria-label={t("missions.control.limitsTitle")}
            data-testid="mission-limits-editor"
            onKeyDown={(event) => {
              if (event.key === "Escape") setLimitsOpen(false);
            }}
          >
            <PolicyLimitsEditor mission={mission} onClose={() => setLimitsOpen(false)} />
          </div>
        ) : null}
        {teamOpen ? (
          <div
            className="mission-inline-confirm mission-limits-popover"
            role="dialog"
            aria-label={t("missions.team.title")}
            data-testid="mission-team-popover"
            onKeyDown={(event) => {
              if (event.key === "Escape") setTeamOpen(false);
            }}
          >
            <MissionTeamEditor mission={mission} onClose={() => setTeamOpen(false)} />
          </div>
        ) : null}
        {draft ? (
          <>
            <button
              type="button"
              className="primary"
              disabled={blocked}
              onClick={() => void control("start")}
              data-testid="mission-start"
            >
              {busy && lastAction.current === "start" ? t("missions.draft.starting") : t("missions.draft.start")}
            </button>
            <button
              type="button"
              disabled={blocked}
              onClick={() => {
                setError(null);
                setConfirm("delete-draft");
              }}
              data-testid="mission-draft-delete"
            >
              {t("missions.draft.delete")}
            </button>
          </>
        ) : null}
        {confirm === "delete-draft" ? (
          <div className="mission-inline-confirm" role="alertdialog" aria-labelledby={`draft-delete-title-${mission.id}`} data-testid="mission-draft-delete-confirm">
            <h4 id={`draft-delete-title-${mission.id}`}>{t("missions.draft.deleteTitle")}</h4>
            <p>{t("missions.draft.deleteBody")}</p>
            <button type="button" className="danger" disabled={blocked} onClick={() => void deleteDraft()} data-testid="mission-draft-delete-ok">
              {t("missions.draft.delete")}
            </button>
            <button type="button" onClick={() => setConfirm(null)}>
              {t("missions.common.cancel")}
            </button>
          </div>
        ) : null}
        {terminal && mission.archived_at === null ? (
          <button
            type="button"
            disabled={blocked}
            onClick={() => {
              setError(null);
              setConfirm("archive");
            }}
            data-testid="mission-archive"
          >
            {t("missions.control.archive")}
          </button>
        ) : null}
        {error ? (
          <div className="mission-control-error" data-testid="mission-control-error">
            <MissionErrorNotice error={error} onRetry={retryLast} onAction={props.onErrorAction} />
            {lastAction.current === "start" && error.reasonCode !== null && RECREATE_REASONS.has(error.reasonCode) ? (
              <button
                type="button"
                onClick={() => {
                  setError(null);
                  void openNewMissionFrom(mission, "same-goal");
                }}
                data-testid="mission-start-recreate"
              >
                {t("missions.outcome.sameGoal")}
              </button>
            ) : null}
            <button type="button" className="link" onClick={() => setError(null)}>
              {t("missions.common.close")}
            </button>
          </div>
        ) : null}
      </div>
    </header>
  );
}

/**
 * 결정 배너(05 §1·§8): 현재 질문의 종류별 짧은 제목 + 열기/접기.
 * 낭독 영역(aria-live polite)은 결정이 없을 때도 자리를 지켜, 답변 저장·새 결정이
 * 같은 영역에서 한 번씩 읽히게 한다.
 */
function DecisionBanner(props: {
  decision: Decision | null;
  count: number;
  expanded: boolean;
  saved: boolean;
  disconnected: boolean;
  onToggle: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const decision = props.decision;
  // 파일 변경 승인인지 알려면 승인 질문 본문이 필요하다 — 다른 종류는 읽지 않는다.
  const question = useArtifactText(decision?.kind === "approval" ? decision.question_ref : null);
  const fileChange = decision?.kind === "approval" ? fileChangeRequest(approvalText(question.text)) : null;
  const bannerText = decision
    ? t("missions.banner.decision", { count: props.count, question: decisionBannerTitle(t, decision, fileChange !== null) })
    : "";
  const text = [props.saved ? t("missions.banner.saved") : "", bannerText].filter((part) => part.length > 0).join(" · ");
  const className = decision
    ? "mission-decision-banner"
    : props.saved
      ? "mission-decision-banner saved"
      : "mission-decision-live-idle";
  return (
    <div className={className} data-testid={decision ? "decision-banner" : props.saved ? "decision-saved" : "decision-live-idle"}>
      <span aria-live="polite" data-testid={decision ? "decision-banner-text" : "decision-live-text"}>
        {text}
      </span>
      {decision ? (
        <button
          type="button"
          onClick={props.onToggle}
          aria-expanded={props.expanded}
          disabled={props.disconnected}
          title={props.disconnected ? t("missions.banner.offline") : undefined}
          data-testid="decision-banner-open"
        >
          {props.expanded ? t("missions.banner.close") : t("missions.banner.open")}
        </button>
      ) : null}
    </div>
  );
}
