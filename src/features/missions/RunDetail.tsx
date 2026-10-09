/**
 * RunDetail(05-ui §6): 선택한 task/실행의 상세.
 *
 * - header: 작업명/역할/모델/시도/실행 상태 + 상태 안내(한 줄 + 행동 + 자세히)
 *   + task 제어(할 일 취소/담당 실행 중단, 다시 시도, 모델 변경). 비활성 버튼은
 *   아래 한 줄로 이유를 보여 준다.
 * - 탭 활동/실행/변경/검증(tablist·arrow 탐색). `실행` 탭은 검증된 native
 *   terminal attach가 있을 때만 보인다 — 가짜 terminal 금지(05 §6).
 * - 활동: mission.activity 꼬리를 읽어 새 출력을 따라간다(위로 스크롤하면 멈춤).
 * - 변경/검증: 작업 전체 결과(candidate)를 보여 주므로 탭 이름에 "전체"를 붙이고,
 *   이 할 일에 해당하는 항목을 먼저 보여 준다.
 * - `이 담당에게 추가 지시`: task명·모델이 고정된 별도 입력창(별도 draft).
 * - mutation은 mutateWithResync, 오류는 MissionErrorNotice. 낭독 영역 없음.
 */

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { Binding } from "../../generated/Binding";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import type { Verification } from "../../generated/Verification";
import { useI18n } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { bindingSupportsTask, capabilityBlocked, isExperimentalBinding } from "./bindingSupport";
import { readArtifactText } from "./bodyCache";
import { CapabilityNotice } from "./CapabilityNotice";
import { getMissionClient } from "./clientAccess";
import { MissionErrorNotice, missionError, withSupportedActions, type MissionUiError } from "./errors";
import { taskRunsInPhase } from "./executionKind";
import {
  blockedReasonLabel,
  inputIntegrityNoteKey,
  roleLabel,
  runStateLabel,
  taskStatusDisplay,
  verificationStatusLabelKey,
} from "./labels";
import { MANIFEST_VISIBLE_LIMIT, manifestChangeLabelKey, parseCandidateManifest, type ManifestEntry } from "./manifest";
import { MessageComposer } from "./MessageComposer";
import { ModelReassignPicker, type ReassignAction } from "./ModelReassignPicker";
import { mutateWithResync } from "./mutation";
import { PlanRepairNotice } from "./PlanRepairNotice";
import { RateLimitNotice, RetryNotice } from "./RateLimitNotice";
import { RecoveryNotice } from "./RecoveryNotice";
import { RequiredRepairNotice, requiredRepairOwner } from "./RequiredRepairNotice";
import { RunAttestedNotice, RunExitAttestation } from "./RunExitAttestation";
import { runUserAttested } from "./runReconciliation";
import {
  runHoldsTask,
  selectCandidateVerifications,
  selectPrimaryRun,
  selectTaskActionGuard,
  selectTaskRuns,
  taskActionGuardHolds,
  type TaskActionGuard,
} from "./selectors";
import { useMissionStore } from "./store";
import type { DetailTab } from "./uiStore";
import { formatElapsedTime, isDeterministicIntegration, newRequestId, useArtifactText } from "./viewUtils";
import "./decisions.css";

const TERMINAL_TASK_STATES = new Set(["succeeded", "failed", "cancelled", "superseded"]);

export interface RunDetailProps {
  mission: Mission;
  task: Task;
  detailTab: DetailTab;
  onDetailTabChange: (tab: DetailTab) => void;
  /** 비활성 사유의 "Lead 대화에서 대체 계획 요청" 링크 대상(없으면 링크 없이 문구만). */
  onOpenLead?: () => void;
}

const ALL_TABS: readonly DetailTab[] = ["activity", "exec", "changes", "verification"];

/** 검증된 native terminal attach가 있는 실행인가(없으면 `실행` 탭을 숨긴다). */
function hasNativeAttach(run: Run | null): boolean {
  return run?.binding_snapshot?.capabilities?.native_terminal_attach?.supported === true;
}

export function RunDetail(props: RunDetailProps): JSX.Element {
  const { t } = useI18n();
  const task = props.task;
  const mission = props.mission;
  const primaryRun = useMissionStore((s) => selectPrimaryRun(s, task));
  const liveRunState = primaryRun && ["prepared", "starting", "running", "awaiting_input", "stopping"].includes(primaryRun.state)
    ? primaryRun.state
    : null;
  const display = taskStatusDisplay(task.state, liveRunState);
  const model = primaryRun?.observed_model ?? primaryRun?.requested_model ?? primaryRun?.binding_snapshot?.model_id ?? null;
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});
  const [instructOpen, setInstructOpen] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const controls = useTaskControls(mission, task, primaryRun);
  const candidateId = mission.candidate_id;
  const hasCandidate = useMissionStore((s) => (candidateId ? s.candidates[candidateId] !== undefined : false));
  const hasVerifications = useMissionStore((s) =>
    candidateId ? Object.values(s.verifications).some((verification) => verification.candidate_id === candidateId) : false);
  const tabs = ALL_TABS.filter((tab) => tab !== "exec" || hasNativeAttach(primaryRun));
  // 이 할 일에 필요한 기능을 사용자 동의로 연 연결(11 §8 — 선택 기능은 배지를 만들지 않는다).
  const experimental = isExperimentalBinding(primaryRun?.binding_snapshot, task.kind);
  // 사용자 확인으로 정리된 실행은 데몬이 종료를 관측했다는 안내 대신 미확인 안내를 보인다(계약 C).
  const userAttested = runUserAttested(primaryRun);
  const activeTab: DetailTab = tabs.includes(props.detailTab) ? props.detailTab : "activity";

  useEffect(() => {
    setPickerOpen(false);
  }, [task.id]);

  const changeModel = controls.reassign.visible && !controls.reassign.disabled ? () => setPickerOpen(true) : undefined;
  const openSettings = () => useWorkbenchStore.getState().openSettings("missions");

  const tabLabel = (tab: DetailTab): string => {
    if (tab === "changes" && hasCandidate) return t("missions.detail.tab.changesAll");
    if (tab === "verification" && hasVerifications) return t("missions.detail.tab.verificationAll");
    return t(`missions.detail.tab.${tab}`);
  };

  const onTabKeyDown = (event: React.KeyboardEvent) => {
    if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
    event.preventDefault();
    const index = tabs.indexOf(activeTab);
    const delta = event.key === "ArrowRight" ? 1 : -1;
    const next = tabs[(index + delta + tabs.length) % tabs.length];
    props.onDetailTabChange(next);
    tabRefs.current[next]?.focus();
  };

  return (
    <section className="mission-detail" data-testid="run-detail">
      <header className="mission-detail-header">
        <div className="mission-detail-title-row">
          <h3 className="mission-detail-title">{task.title}</h3>
          <span className={`mission-task-state-label state-${task.state}`}>{t(display.labelKey)}</span>
          {experimental ? (
            <span className="mission-experimental-badge" title={t("missions.experimentalRun.tooltip")} data-testid="detail-experimental-badge">
              {t("missions.experimental.badge")}
              <span className="mission-sr-only"> · {t("missions.experimentalRun.tooltip")}</span>
            </span>
          ) : null}
        </div>
        <dl className="mission-detail-meta">
          <div>
            <dt>{t("missions.detail.role")}</dt>
            <dd data-testid="detail-role">{roleLabel(t, task.role)}</dd>
          </div>
          <div>
            <dt>{t("missions.detail.model")}</dt>
            <dd data-testid="detail-model">{model ?? t("missions.detail.noModel")}</dd>
          </div>
          <div>
            <dt>{t("missions.detail.attemptOf", { n: task.attempt_count, limit: mission.policy.max_attempts_per_task })}</dt>
            <dd data-testid="detail-run-state">{primaryRun ? runStateLabel(t, primaryRun.state) : "—"}</dd>
          </div>
          <div>
            <dt>{t("missions.detail.elapsed")}</dt>
            <dd data-testid="detail-elapsed">{primaryRun ? formatElapsedTime(primaryRun.active_time_ms) : "—"}</dd>
            <span
              className="mission-info-icon"
              role="img"
              tabIndex={0}
              title={t("missions.time.savedNote")}
              aria-label={`${t("missions.detail.savedNoteLabel")}: ${t("missions.time.savedNote")}`}
              data-testid="detail-saved-note"
            >
              ⓘ
            </span>
          </div>
        </dl>
        {task.integration ? <IntegratorAssignment task={task} /> : null}
        <BlockedReason task={task} run={primaryRun} />
        <RateLimitNotice task={task} onChangeModel={changeModel} />
        <RetryNotice task={task} onChangeModel={changeModel} />
        <PlanRepairNotice task={task} run={primaryRun} />
        <RequiredRepairNotice mission={mission} task={task} />
        {userAttested ? <RunAttestedNotice /> : <RecoveryNotice run={primaryRun} />}
        <RunExitAttestation mission={mission} run={primaryRun} />
        <CapabilityNotice task={task} run={primaryRun} onChangeModel={changeModel} onOpenSettings={openSettings} />
        <TaskActions
          mission={mission}
          task={task}
          controls={controls}
          pickerOpen={pickerOpen}
          onPickerOpenChange={setPickerOpen}
          onOpenLead={props.onOpenLead}
          onOpenSettings={openSettings}
        />
        {isDeterministicIntegration(task) ? (
          <p className="muted" data-testid="integration-instructions">
            {t("missions.detail.integrationInstructions")}
          </p>
        ) : (
          <>
            <button
              type="button"
              className="mission-instruct-toggle"
              aria-expanded={instructOpen}
              onClick={() => setInstructOpen(!instructOpen)}
              data-testid="instruct-toggle"
            >
              {t("missions.detail.instruct")}
            </button>
            {instructOpen ? (
              <TaskComposer mission={mission} task={task} model={model} />
            ) : null}
          </>
        )}
      </header>
      <div className="mission-detail-tabs" role="tablist" aria-label={task.title} onKeyDown={onTabKeyDown}>
        {tabs.map((tab) => (
          <button
            key={tab}
            ref={(node) => {
              tabRefs.current[tab] = node;
            }}
            type="button"
            role="tab"
            id={`mission-detail-tab-${tab}`}
            aria-selected={activeTab === tab}
            aria-controls={`mission-detail-panel-${tab}`}
            tabIndex={activeTab === tab ? 0 : -1}
            className={`mission-detail-tab${activeTab === tab ? " active" : ""}`}
            data-testid={`detail-tab-${tab}`}
            onClick={() => props.onDetailTabChange(tab)}
          >
            {tabLabel(tab)}
          </button>
        ))}
      </div>
      <div className="mission-detail-body">
        {activeTab === "activity" ? (
          <div role="tabpanel" id="mission-detail-panel-activity" aria-labelledby="mission-detail-tab-activity">
            <ActivityTab missionId={mission.id} runId={primaryRun?.id ?? null} />
          </div>
        ) : null}
        {activeTab === "exec" ? (
          <div role="tabpanel" id="mission-detail-panel-exec" aria-labelledby="mission-detail-tab-exec">
            <p className="mission-exec-note">{t("missions.detail.execUnsupported")}</p>
            <p className="muted">{t("missions.detail.execNoAttachNote")}</p>
            <button type="button" onClick={() => props.onDetailTabChange("activity")} data-testid="open-activity">
              {t("missions.detail.execOpenActivity")}
            </button>
          </div>
        ) : null}
        {activeTab === "changes" ? (
          <div role="tabpanel" id="mission-detail-panel-changes" aria-labelledby="mission-detail-tab-changes">
            <ChangesTab mission={mission} task={task} />
          </div>
        ) : null}
        {activeTab === "verification" ? (
          <div role="tabpanel" id="mission-detail-panel-verification" aria-labelledby="mission-detail-tab-verification">
            <VerificationTab mission={mission} task={task} />
          </div>
        ) : null}
      </div>
    </section>
  );
}

// ---------------------------------------------------------------- 헤더 보조

function IntegratorAssignment({ task }: { task: Task }): JSX.Element {
  const { t } = useI18n();
  const [binding, setBinding] = useState<Binding | null>(null);
  useEffect(() => {
    let alive = true;
    setBinding(null);
    void getMissionClient()?.bindingList().then(result => {
      if (alive) setBinding(result.bindings.find(b => b.id === task.binding_id) ?? null);
    }, () => { if (alive) setBinding(null); });
    return () => { alive = false; };
  }, [task.binding_id]);
  return <p className="muted" data-testid="integration-assignment">{t("missions.integration.assignment", {
    model: binding ? `${binding.label} · ${binding.model_id}` : t("missions.integration.bindingUnavailable"),
  })}</p>;
}

/** 전용 안내가 따로 있는 blocked code — 이유 줄을 중복해 보여 주지 않는다. */
function blockedReasonCoveredByNotice(task: Task, run: Run | null): boolean {
  const code = task.blocked_code;
  if (code === null) return false;
  if (["provider_rate_limited", "transient_retry", "plan_format_repair"].includes(code) || capabilityBlocked(code)) return true;
  return (code === "outcome_unknown" || code === "outcome_unknown_ended") && run !== null && ["unknown", "interrupted"].includes(run.state);
}

/** 제공자 결과(AgentResult JSON)가 blocked면 그 보고 artifact. */
function blockedReportRef(text: string | null): ArtifactRef | null {
  if (!text) return null;
  try {
    const value: unknown = JSON.parse(text);
    if (!value || typeof value !== "object") return null;
    const record = value as { kind?: unknown; report_ref?: unknown };
    const ref = record.report_ref as Partial<ArtifactRef> | undefined;
    return record.kind === "blocked" && ref && typeof ref.id === "string" && typeof ref.bytes === "string" ? (ref as ArtifactRef) : null;
  } catch {
    return null;
  }
}

function BlockedReason({ task, run }: { task: Task; run: Run | null }): JSX.Element | null {
  const { t } = useI18n();
  const blocked = task.state === "blocked";
  const providerBlocked = blocked && (task.blocked_code?.startsWith("provider_blocked:") ?? false);
  const result = useArtifactText(providerBlocked && run?.result_ref ? run.result_ref : null);
  const report = useArtifactText(providerBlocked ? blockedReportRef(result.text) : null);
  if (!blocked || blockedReasonCoveredByNotice(task, run)) return null;
  return (
    <div data-testid="blocked-reason">
      <p className="mission-blocked-reason">{t("missions.detail.blockedReason", { reason: blockedReasonLabel(t, task.blocked_code) })}</p>
      {providerBlocked && report.text ? (
        <details open>
          <summary>{t("missions.detail.blockedReport")}</summary>
          <pre className="mission-blocked-report" data-testid="blocked-report">{report.text}</pre>
        </details>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------- 활동 탭

function ActivityTab(props: { missionId: string; runId: string | null }): JSX.Element {
  const { t } = useI18n();
  const [activityText, setActivityText] = useState("");
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState(false);
  const [refreshKey, setRefreshKey] = useState(0);
  const [following, setFollowing] = useState(true);
  const followingRef = useRef(true);
  const logRef = useRef<HTMLPreElement | null>(null);

  useEffect(() => {
    setActivityText("");
    setLoaded(false);
    setError(false);
    followingRef.current = true;
    setFollowing(true);
    if (props.runId === null) return;
    let alive = true;
    const client = getMissionClient();
    if (!client) {
      setError(true);
      return;
    }
    let offset = "0";
    let retained = "";
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async (): Promise<void> => {
      try {
        let complete = false;
        // Drain several immutable pages without waiting for another timer.
        for (let i = 0; i < 4 && alive; i++) {
          const page = await client.missionActivity({ mission_id: props.missionId, run_id: props.runId!, after_offset: offset, max_bytes: 65536 });
          if (!alive) return;
          if (page.body_ref) {
            const text = await readArtifactText(client, page.body_ref.id, Number(page.body_ref.bytes));
            if (!alive) return;
            retained = (retained + text).split("\n").slice(-200).join("\n").slice(-262144);
            setActivityText(retained);
          }
          const advanced = page.next_offset !== offset;
          offset = page.next_offset;
          complete = page.complete;
          if (complete || !advanced || !page.body_ref) break;
        }
        if (alive) {
          setError(false);
          setLoaded(true);
          if (!complete) timer = setTimeout(() => { void poll(); }, 1000);
        }
      } catch {
        if (alive) setError(true);
      }
    };
    void poll();
    return () => { alive = false; if (timer) clearTimeout(timer); };
  }, [props.missionId, props.runId, refreshKey]);

  // 새 출력은 사용자가 맨 아래를 보고 있을 때만 따라간다.
  useLayoutEffect(() => {
    const log = logRef.current;
    if (log && followingRef.current) log.scrollTop = log.scrollHeight;
  }, [activityText]);

  const onScroll = () => {
    const log = logRef.current;
    if (!log) return;
    const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight <= 24;
    if (atBottom !== followingRef.current) {
      followingRef.current = atBottom;
      setFollowing(atBottom);
    }
  };

  const jumpToLatest = () => {
    followingRef.current = true;
    setFollowing(true);
    const log = logRef.current;
    if (log) log.scrollTop = log.scrollHeight;
  };

  if (props.runId === null) {
    return <p className="muted" data-testid="activity-empty">{t("missions.detail.activityNoRun")}</p>;
  }
  if (error) {
    return (
      <div>
        <p className="mission-area-error">{t("missions.detail.activityError")}</p>
        <button type="button" onClick={() => setRefreshKey((k) => k + 1)}>
          {t("missions.detail.activityRefresh")}
        </button>
      </div>
    );
  }
  if (!loaded && !activityText) return <p className="muted" data-testid="activity-loading">{t("missions.detail.activityLoading")}</p>;
  if (!activityText) return <p className="muted" data-testid="activity-empty">{t("missions.detail.activityEmpty")}</p>;
  return (
    <div className="mission-activity">
      <pre ref={logRef} className="mission-activity-log" data-testid="activity-log" tabIndex={0} onScroll={onScroll}>
        {activityText}
      </pre>
      {!following ? (
        <button type="button" className="mission-activity-latest" onClick={jumpToLatest} data-testid="activity-latest">
          {t("missions.detail.activityLatest")}
        </button>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------- 변경 탭

/** allowed_paths 한 항목이 path를 덮는가(`dir`, `dir/`, `dir/**` 형식). */
function pathInScope(path: string, scope: string): boolean {
  const base = scope.replace(/\/\*\*?$/, "").replace(/\*\*?$/, "").replace(/\/+$/, "");
  if (base === "" || base === ".") return true;
  return path === base || path.startsWith(`${base}/`);
}

function ManifestEntryList({ entries }: { entries: readonly ManifestEntry[] }): JSX.Element {
  const { t } = useI18n();
  const visible = entries.slice(0, MANIFEST_VISIBLE_LIMIT);
  return (
    <>
      <ul className="mission-changes-list">
        {visible.map((entry) => {
          const kind = entry.change === "added" ? "add" : entry.change === "deleted" ? "delete" : "modify";
          const labelKey = manifestChangeLabelKey(entry.change);
          return (
            <li key={entry.path} className={`change-${kind}`} data-testid="changes-entry">
              <span aria-hidden="true" title={labelKey ? t(labelKey) : entry.change}>
                {kind === "add" ? "+" : kind === "delete" ? "-" : "~"}
              </span>{" "}
              {entry.path}
            </li>
          );
        })}
      </ul>
      {entries.length > visible.length ? (
        <p className="muted">{t("missions.detail.changes.more", { count: entries.length - visible.length })}</p>
      ) : null}
    </>
  );
}

function ChangesTab(props: { mission: Mission; task: Task }): JSX.Element {
  const { t } = useI18n();
  const candidateId = props.mission.candidate_id;
  const candidate = useMissionStore((s) => (candidateId ? s.candidates[candidateId] ?? null : null));
  const manifest = useArtifactText(candidate ? candidate.manifest_ref : null);
  const taskRunIds = useMissionStore(useShallow((s) => selectTaskRuns(s, props.task.id).map((run) => run.id)));

  const inFlight =
    props.mission.phase === "implementing" || props.mission.phase === "integrating";

  if (!candidate) {
    return (
      <p className={inFlight ? "mission-changes-progress" : "muted"} data-testid="changes-state">
        {inFlight ? t("missions.detail.changesInProgress") : t("missions.detail.noChanges")}
      </p>
    );
  }
  const entries = parseCandidateManifest(manifest.text).entries;
  const scopes = props.task.contract.allowed_paths;
  const scoped = scopes.length > 0 ? entries.filter((entry) => scopes.some((scope) => pathInScope(entry.path, scope))) : [];
  const others = scoped.length > 0 ? entries.filter((entry) => !scoped.includes(entry)) : entries;
  const included = candidate.source_run_ids.some((id) => taskRunIds.includes(id));
  return (
    <div>
      <p data-testid="changes-state">{t("missions.detail.changesCaptured", { count: entries.length })}</p>
      {taskRunIds.length > 0 ? (
        <p className="muted" data-testid="changes-task-inclusion">
          {t(included ? "missions.detail.changes.taskIncluded" : "missions.detail.changes.taskNotIncluded")}
        </p>
      ) : null}
      {scoped.length > 0 ? (
        <div data-testid="changes-task-scope">
          <p className="mission-detail-section-title">{t("missions.detail.changes.taskScope")}</p>
          <ManifestEntryList entries={scoped} />
        </div>
      ) : null}
      {others.length > 0 ? (
        <div data-testid="changes-other">
          {scoped.length > 0 ? <p className="mission-detail-section-title">{t("missions.detail.changes.otherFiles")}</p> : null}
          <ManifestEntryList entries={others} />
        </div>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------- 검증 탭

function VerificationRow({ verification }: { verification: Verification }): JSX.Element {
  const { t } = useI18n();
  return (
    <li className={`verification-${verification.status}`} data-testid="verification-row">
      <span aria-hidden="true">{verification.status === "passed" ? "✓" : verification.status === "failed" ? "✗" : "?"}</span>{" "}
      <span data-testid="verification-status">{t(verificationStatusLabelKey(verification.status))}</span>
      {/* 입력 무결성은 결과와 별개의 보장 수준이다 — "통과"를 섞지 않는다. */}
      <span className="muted" data-testid="verification-integrity"> · {t(inputIntegrityNoteKey(verification.input_integrity))}</span>
      {verification.exit_code !== null ? (
        <span className="muted"> · {t("missions.detail.verification.exit", { code: verification.exit_code })}</span>
      ) : null}
    </li>
  );
}

function VerificationTab(props: { mission: Mission; task: Task }): JSX.Element {
  const { t } = useI18n();
  const candidateId = props.mission.candidate_id;
  const verifications = useMissionStore(useShallow((s) =>
    candidateId ? selectCandidateVerifications(s, candidateId) : []));
  if (verifications.length === 0) {
    return <p className="muted" data-testid="verification-empty">{t("missions.detail.verificationEmpty")}</p>;
  }
  const requirementIds = props.task.contract.requirement_ids;
  const related = verifications.filter((verification) => verification.task_id === props.task.id
    || verification.requirement_ids.some((id) => requirementIds.includes(id)));
  const others = verifications.filter((verification) => !related.includes(verification));
  return (
    <div data-testid="verification-list">
      {related.length > 0 ? (
        <>
          <p className="mission-detail-section-title">{t("missions.detail.verification.task")}</p>
          <ul className="mission-verification-list" data-testid="verification-task">
            {related.map((verification) => <VerificationRow key={verification.id} verification={verification} />)}
          </ul>
        </>
      ) : null}
      {others.length > 0 ? (
        <>
          {related.length > 0 ? <p className="mission-detail-section-title">{t("missions.detail.verification.other")}</p> : null}
          <ul className="mission-verification-list" data-testid="verification-other">
            {others.map((verification) => <VerificationRow key={verification.id} verification={verification} />)}
          </ul>
        </>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------- task 제어

interface DisabledReason {
  key: string;
  /** 링크 "Lead 대화에서 대체 계획 요청"을 붙인다. */
  askLead?: boolean;
}

interface ControlState {
  hasLiveRun: boolean;
  cancel: { visible: boolean; labelKey: string };
  retry: { visible: boolean; disabled: boolean; reason: DisabledReason | null };
  reassign: { visible: boolean; disabled: boolean; reason: DisabledReason | null; action: ReassignAction; labelKey: string };
  requiredCancelled: boolean;
}

/** 할 일 제어 버튼의 표시/활성/비활성 사유(RunDetail 헤더와 notice 행동이 같이 쓴다). */
function useTaskControls(mission: Mission, task: Task, primaryRun: Run | null): ControlState {
  const repairOwner = useMissionStore((s) => requiredRepairOwner(s, task));
  const integrationConflict = useMissionStore((s) => isDeterministicIntegration(task) && Object.values(s.decisions).some((d) =>
    d.mission_id === mission.id && d.kind === "conflict" && d.state === "open"
    && d.requesting_run_id !== null && s.runs[d.requesting_run_id]?.task_id === task.id));
  const hasLiveRun = runHoldsTask(primaryRun);
  const terminal = ["succeeded", "cancelled", "superseded"].includes(task.state);
  const missionActive = ["running", "paused", "pausing"].includes(mission.state);
  const canRetryMission = missionActive && repairOwner === null;
  const cancelledInPhase = task.state === "cancelled" && task.active_run_id === null && taskRunsInPhase(task, mission);
  const canAssignIntegrator = task.integration != null && integrationConflict;
  const canReassignWithoutRetry = canAssignIntegrator || ["planned", "ready"].includes(task.state)
    || (task.state === "blocked" && (capabilityBlocked(task.blocked_code)
      || ["cost_limit", "cost_unknown", "provider_rate_limited", "transient_retry", "plan_format_repair", "outcome_unknown_ended"].includes(task.blocked_code ?? "")));

  const liveReason: DisabledReason | null = hasLiveRun
    ? { key: primaryRun && ["stopping", "unknown", "interrupted"].includes(primaryRun.state) ? "missions.detail.disabled.stopping" : "missions.detail.disabled.running" }
    : null;
  const missionReason: DisabledReason | null = !canRetryMission
    ? { key: repairOwner !== null ? "missions.detail.disabled.repairOwner" : "missions.detail.disabled.missionState" }
    : null;
  const laterPhase: DisabledReason = { key: "missions.detail.disabled.laterPhase", askLead: true };

  const retryVisible = task.state === "failed" || task.state === "cancelled";
  const retryReason: DisabledReason | null = liveReason ?? missionReason
    ?? (integrationConflict ? { key: "missions.detail.disabled.integrationConflict" } : null)
    ?? (task.state === "cancelled" && !cancelledInPhase ? laterPhase : null);

  const reassignVisible = !isDeterministicIntegration(task) || canAssignIntegrator;
  const quiet = task.state === "succeeded" || task.state === "superseded";
  const reassignReason: DisabledReason | null = task.kind === "verify" ? { key: "missions.detail.disabled.verify" }
    : liveReason ?? missionReason
    ?? (!canReassignWithoutRetry && task.state !== "failed" && !cancelledInPhase
      ? (task.state === "cancelled" ? laterPhase : { key: "missions.detail.disabled.notChangeable" })
      : null);

  return {
    hasLiveRun,
    cancel: { visible: !terminal, labelKey: hasLiveRun ? "missions.detail.actionCancel" : "missions.detail.actionCancelTask" },
    retry: { visible: retryVisible, disabled: retryReason !== null, reason: retryReason },
    reassign: {
      visible: reassignVisible,
      disabled: reassignReason !== null,
      reason: quiet ? null : reassignReason,
      action: canReassignWithoutRetry ? "reassign" : "retry",
      labelKey: canReassignWithoutRetry ? "missions.cost.changeModel" : "missions.detail.actionReassign",
    },
    requiredCancelled: task.state === "cancelled" && task.required && canRetryMission,
  };
}

function TaskActions(props: {
  mission: Mission;
  task: Task;
  controls: ControlState;
  pickerOpen: boolean;
  onPickerOpenChange: (open: boolean) => void;
  onOpenLead?: () => void;
  onOpenSettings: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const { controls } = props;
  const failedRun = useMissionStore((state) => selectPrimaryRun(state, props.task));
  const needsInstallationCheck = failedRun?.failure_code === "CAPABILITY_UNSUPPORTED"
    && failedRun.retry_evidence?.basis === "request_not_submitted";
  const controlPending = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<MissionUiError | null>(null);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const lastAction = useRef<"cancel" | "retry" | null>(null);
  /** 취소 확인창을 열 때 본 전제 — 확인을 누른 뒤 보내는 요청도 이 화면 기준이어야 한다. */
  const cancelGuard = useRef<TaskActionGuard | null>(null);
  const captureGuard = () => selectTaskActionGuard(useMissionStore.getState(), props.task);
  const dependentCount = useDependentCount(props.mission.id, props.task.id);

  const control = async (action: "cancel" | "retry", guard: TaskActionGuard = captureGuard()) => {
    if (controlPending.current) return;
    lastAction.current = action;
    const client = getMissionClient();
    if (!client) {
      setError(missionError(t, new Error(t("missions.sync.noClient"))));
      return;
    }
    controlPending.current = true;
    setBusy(true);
    setError(null);
    try {
      if (action === "retry" && needsInstallationCheck) {
        const bindingId = failedRun?.binding_snapshot?.id;
        if (!bindingId) throw new Error(t("missions.compatibility.recheckMissing"));
        const result = await client.bindingProbe({ binding_id: bindingId });
        if (result.installation !== "verified") {
          throw new Error(t(`missions.settings.probeStatus.${result.installation}`));
        }
        if (!bindingSupportsTask(result.binding, props.task.kind)) {
          throw new Error(t("missions.compatibility.recheckUnsupported"));
        }
        // Probe persists fresh daemon-owned evidence. Retry still checks that
        // this is the same failed task and uses the latest mission revision.
        await useMissionStore.getState().syncMission(props.mission.id);
      }
      await mutateWithResync(props.mission.id, (latest) => {
        // 누른 순간과 할 일 상태·배정·실행·시도 수가 달라졌으면 보내지 않는다(재동기화 뒤 재전송 포함).
        if (!taskActionGuardHolds(useMissionStore.getState(), guard)) return null;
        return client.missionTaskControl({
          request_id: newRequestId(),
          mission_id: latest.id,
          expected_revision: latest.revision,
          task_id: props.task.id,
          action,
          binding_id: null,
        });
      });
    } catch (cause) {
      setError(missionError(t, cause));
    } finally {
      controlPending.current = false;
      setBusy(false);
    }
  };

  // 같은 이유는 한 번만 보여 준다(다시 시도·모델 변경이 같은 이유로 막힌 경우).
  const reasons: DisabledReason[] = [];
  for (const [visible, reason] of [
    [controls.retry.visible, controls.retry.reason],
    [controls.reassign.visible, controls.reassign.reason],
  ] as const) {
    if (visible && reason && !reasons.some((existing) => existing.key === reason.key)) reasons.push(reason);
  }

  return (
    <div className="mission-task-actions">
      {controls.requiredCancelled ? (
        <p data-testid="required-cancelled-notice">{t("missions.detail.requiredCancelled")}</p>
      ) : null}
      {controls.cancel.visible ? (
        <button
          type="button"
          className="danger"
          disabled={busy}
          onClick={() => {
            cancelGuard.current = captureGuard();
            setConfirmCancel(true);
          }}
          data-testid="task-cancel"
        >
          {t(controls.cancel.labelKey)}
        </button>
      ) : null}
      {controls.retry.visible ? (
        <button
          type="button"
          disabled={busy || controls.retry.disabled}
          onClick={() => void control("retry")}
          data-testid="task-retry"
        >
          {t(needsInstallationCheck ? (busy ? "missions.compatibility.rechecking" : "missions.compatibility.recheckRetry") : "missions.detail.actionRetry")}
        </button>
      ) : null}
      {controls.reassign.visible ? (
        <button
          type="button"
          disabled={busy || controls.reassign.disabled}
          onClick={() => props.onPickerOpenChange(true)}
          data-testid="task-reassign"
        >
          {t(controls.reassign.labelKey)}
        </button>
      ) : null}
      {reasons.map((reason) => (
        <p key={reason.key} className="mission-disabled-reason" data-testid="task-disabled-reason">
          {t(reason.key)}
          {reason.askLead && props.onOpenLead ? (
            <button type="button" className="link" onClick={props.onOpenLead} data-testid="task-ask-lead">
              {t("missions.detail.askLeadReplan")}
            </button>
          ) : null}
        </p>
      ))}
      {confirmCancel ? (
        <div className="mission-inline-confirm" role="alertdialog" aria-modal={false} data-testid="task-cancel-confirm">
          <p>
            {dependentCount > 0
              ? t("missions.detail.actionCancelConfirm", { count: dependentCount })
              : t("missions.detail.actionCancelConfirmPlain")}
          </p>
          {props.task.required ? <p>{t("missions.detail.requiredCancelled")}</p> : null}
          <button
            type="button"
            className="danger"
            onClick={() => {
              setConfirmCancel(false);
              const guard = cancelGuard.current ?? captureGuard();
              cancelGuard.current = null;
              void control("cancel", guard);
            }}
          >
            {t("missions.common.ok")}
          </button>
          <button type="button" onClick={() => setConfirmCancel(false)}>
            {t("missions.common.cancel")}
          </button>
        </div>
      ) : null}
      {props.pickerOpen ? (
        <ModelReassignPicker
          mission={props.mission}
          task={props.task}
          action={controls.reassign.action}
          onClose={() => props.onPickerOpenChange(false)}
        />
      ) : null}
      {error ? (
        <MissionErrorNotice
          error={withSupportedActions(error, ["resync", "retry", "open_settings", ...(controls.reassign.visible && !controls.reassign.disabled ? ["change_model" as const] : [])])}
          onRetry={() => {
            if (lastAction.current) void control(lastAction.current);
          }}
          onAction={(action) => {
            if (action === "resync") void useMissionStore.getState().syncMission(props.mission.id);
            else if (action === "open_settings") props.onOpenSettings();
            else if (action === "change_model") props.onPickerOpenChange(true);
          }}
        />
      ) : null}
    </div>
  );
}

/** 이 task에 의존하는(아직 끝나지 않은) task 수 — cancel 확인 문구용(05 §8). */
function useDependentCount(missionId: string, taskId: string): number {
  return useMissionStore((s) => {
    let count = 0;
    for (const task of Object.values(s.tasks)) {
      if (task.mission_id !== missionId) continue;
      if (task.id === taskId) continue;
      if (TERMINAL_TASK_STATES.has(task.state)) continue;
      if (task.depends_on.includes(taskId)) count += 1;
    }
    return count;
  });
}

/**
 * `이 담당에게 추가 지시` 입력창 — 수신자(task명·모델)가 고정되고 draft도
 * Lead composer와 분리된다(05 §4).
 */
function TaskComposer(props: { mission: Mission; task: Task; model: string | null }): JSX.Element {
  const { t } = useI18n();
  return <MessageComposer mission={props.mission} task={props.task}
    recipient={t("missions.detail.instructRecipient", { task: props.task.title, model: props.model ?? t("missions.detail.noModel") })}
    placeholder={t("missions.detail.instructPlaceholder")} />;
}
