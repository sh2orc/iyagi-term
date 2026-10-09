import { capabilityBlocked, isExperimentalBinding } from "./bindingSupport";
/**
 * TeamList(05-ui §5): 오른쪽 참여 작업 목록.
 *
 * - 행 key는 task_id, 정렬은 plan ordinal(상태 변화로 재정렬하지 않는다).
 * - 상태는 아이콘 글리프 + 텍스트로 표시(색 외 수단 — 05 §7).
 * - 12행 초과는 `더보기`로 노출을 늘린다(virtual list 대신 상한+확장).
 * - 필터 전체/실행 중/응답 필요/대기/완료, 검색은 title/role/model.
 *   `응답 필요`는 열린 결정이 영향을 주는 할 일이다(결정 N과 같은 기준).
 * - `실행 N`(live Run) `완료 N`(succeeded Task) `결정 N`(open Decision)
 *   카운터 — mission 최종 검증과 구분되는 문구(05 §5).
 * - 목록은 button 행의 list: arrow 탐색/Enter 선택/Escape 닫기.
 * - 재시도는 같은 행에서 attempt history를 펼친다. 남은 재시도는 작업 정책의
 *   할 일당 시도 한도로 센다.
 * - 좁은 목록(≤320px)에서는 제목을 첫 줄, 모델·상태를 둘째 줄에 둔다(CSS).
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import type { Task } from "../../generated/Task";
import type { Run } from "../../generated/Run";
import { useI18n } from "../../i18n";
import { useMissionStore } from "./store";
import {
  selectLiveRunCount,
  selectOpenDecisionCount,
  selectOpenDecisionTaskKey,
  selectPrimaryRun,
  selectSucceededTaskCount,
  selectTaskList,
  selectTaskRuns,
  taskKeySet,
} from "./selectors";
import { blockedReasonLabel, roleLabel, runStateLabel, taskMatchesFilter, taskStatusDisplay } from "./labels";
import type { TeamFilter } from "./uiStore";
import "./integration.css";

/** 12개 초과부터 단순 상한+더보기(05 §5). */
export const TEAM_LIST_PAGE = 12;
/** 이 시간 동안 활동이 없어야 `상태 확인 중`으로 표시한다. */
export const STALE_ACTIVITY_MS = 3 * 60_000;
/** 경과 시간 표시 갱신 주기. */
const CLOCK_TICK_MS = 30_000;

export interface TeamListProps {
  missionId: string;
  selectedTaskId: string | null;
  filter: TeamFilter;
  search: string;
  onSelectTask: (taskId: string) => void;
  onFilterChange: (filter: TeamFilter) => void;
  onSearchChange: (search: string) => void;
  /** Escape — 중간 폭 side sheet 닫기(05 §7). */
  onRequestCloseSheet?: () => void;
}

const FILTERS: readonly TeamFilter[] = ["all", "running", "awaiting", "waiting", "done"];

function useClock(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);
  return now;
}

export function TeamList(props: TeamListProps): JSX.Element {
  const { t } = useI18n();
  const tasks = useMissionStore(useShallow((s) => selectTaskList(s, props.missionId)));
  const liveRuns = useMissionStore((s) => selectLiveRunCount(s, props.missionId));
  const doneTasks = useMissionStore((s) => selectSucceededTaskCount(s, props.missionId));
  const openDecisions = useMissionStore((s) => selectOpenDecisionCount(s, props.missionId));
  const awaitingKey = useMissionStore((s) => selectOpenDecisionTaskKey(s, props.missionId));
  const missionState = useMissionStore((s) => s.missions[props.missionId]?.state ?? null);
  const maxAttempts = useMissionStore((s) => s.missions[props.missionId]?.policy.max_attempts_per_task ?? null);
  const [visible, setVisible] = useState(TEAM_LIST_PAGE);
  const listRef = useRef<HTMLUListElement | null>(null);
  const now = useClock(CLOCK_TICK_MS);

  const modelByTask = useMissionModelIndex(props.missionId);
  const awaitingIds = useMemo(() => taskKeySet(awaitingKey), [awaitingKey]);
  const taskById = useMemo(() => new Map(tasks.map((task) => [task.id, task])), [tasks]);
  const missionPaused = missionState === "paused" || missionState === "pausing";

  const filtered = useMemo(() => {
    const needle = props.search.trim().toLowerCase();
    return tasks.filter((task) => {
      const matches = props.filter === "awaiting" ? awaitingIds.has(task.id) : taskMatchesFilter(task.state, props.filter);
      if (!matches) return false;
      if (needle.length === 0) return true;
      const model = modelByTask[task.id] ?? "";
      const role = task.role ?? "";
      return (
        task.title.toLowerCase().includes(needle) ||
        role.toLowerCase().includes(needle) ||
        roleLabel(t, task.role).toLowerCase().includes(needle) ||
        model.toLowerCase().includes(needle)
      );
    });
  }, [tasks, props.filter, props.search, modelByTask, awaitingIds, t]);

  const rows = filtered.slice(0, visible);

  // keyboard: arrow로 선택 이동, Enter는 선택(이미 button이 처리), Escape 닫기.
  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Escape" && props.onRequestCloseSheet) {
      props.onRequestCloseSheet();
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const index = rows.findIndex((task) => task.id === props.selectedTaskId);
    const delta = event.key === "ArrowDown" ? 1 : -1;
    const next = rows.length === 0 ? -1 : Math.min(rows.length - 1, Math.max(0, index + delta));
    if (next >= 0 && rows[next]) {
      props.onSelectTask(rows[next].id);
      const node = listRef.current?.querySelector<HTMLButtonElement>(
        `[data-task-id="${rows[next].id}"]`,
      );
      node?.focus();
    }
  };

  return (
    <section className="mission-team" aria-label={t("missions.team.listAria")}>
      <header className="mission-team-header">
        <div className="mission-team-title-row">
          <h3 className="mission-team-title">{t("missions.page.teamTitle")}</h3>
          <span className="mission-team-counters" data-testid="team-counters">
            <span className="counter-live">{t("missions.team.liveCount", { count: liveRuns })}</span>
            <span className="counter-done">{t("missions.team.doneCount", { count: doneTasks })}</span>
            <span className="counter-decisions">{t("missions.team.decisionCount", { count: openDecisions })}</span>
          </span>
        </div>
        <div className="mission-team-filters" role="group" aria-label={t("missions.team.listAria")}>
          {FILTERS.map((filter) => (
            <button
              key={filter}
              type="button"
              className={`mission-filter-chip${props.filter === filter ? " active" : ""}`}
              aria-pressed={props.filter === filter}
              data-testid={`filter-${filter}`}
              onClick={() => {
                props.onFilterChange(filter);
                setVisible(TEAM_LIST_PAGE);
              }}
            >
              {t(`missions.team.filter.${filter}`)}
            </button>
          ))}
        </div>
        <input
          type="search"
          className="mission-team-search"
          placeholder={t("missions.team.searchPlaceholder")}
          aria-label={t("missions.team.searchLabel")}
          value={props.search}
          data-testid="team-search"
          onChange={(e) => {
            props.onSearchChange(e.target.value);
            setVisible(TEAM_LIST_PAGE);
          }}
        />
      </header>
      {rows.length === 0 ? (
        <p className="mission-team-empty muted">{t("missions.team.empty")}</p>
      ) : (
        <ul
          className="mission-team-list"
          ref={listRef}
          onKeyDown={onKeyDown}
          data-testid="team-list"
        >
          {rows.map((task) => (
            <TeamRow
              key={task.id}
              task={task}
              missionId={props.missionId}
              selected={task.id === props.selectedTaskId}
              model={modelByTask[task.id] ?? null}
              missionPaused={missionPaused}
              maxAttempts={maxAttempts}
              now={now}
              taskById={taskById}
              onSelect={props.onSelectTask}
            />
          ))}
        </ul>
      )}
      {filtered.length > rows.length ? (
        <button
          type="button"
          className="mission-team-more"
          onClick={() => setVisible((v) => v + TEAM_LIST_PAGE)}
          data-testid="team-more"
        >
          {t("missions.team.more")}
        </button>
      ) : null}
    </section>
  );
}

const LIVE_STATES: readonly Run["state"][] = ["prepared", "starting", "running", "awaiting_input", "stopping"];

function TeamRow(props: {
  task: Task;
  missionId: string;
  selected: boolean;
  model: string | null;
  missionPaused: boolean;
  maxAttempts: number | null;
  now: number;
  taskById: ReadonlyMap<string, Task>;
  onSelect: (taskId: string) => void;
}): JSX.Element {
  const { t } = useI18n();
  const task = props.task;
  const primaryRun = useMissionStore((s) => selectPrimaryRun(s, task));
  const runs = useMissionStore(useShallow((s) => selectTaskRuns(s, task.id)));
  const liveRunState: Run["state"] | null =
    primaryRun && LIVE_STATES.includes(primaryRun.state) ? primaryRun.state : null;
  const display = taskStatusDisplay(task.state, liveRunState);
  const [historyOpen, setHistoryOpen] = useState(false);
  const showHistory = task.attempt_count > 1 || runs.length > 1;
  // 어느 시도든 이 할 일에 필요한 기능을 사용자 동의로 연 연결로 실행했으면 표시한다(11 §8).
  const experimental = runs.some((run) => isExperimentalBinding(run.binding_snapshot, task.kind));

  return (
    <li className="mission-team-row-wrap">
      <button
        type="button"
        className={`mission-team-row${props.selected ? " selected" : ""} task-state-${task.state}`}
        data-task-id={task.id}
        aria-current={props.selected ? "true" : undefined}
        title={`${task.title} · ${roleLabel(t, task.role)}`}
        onClick={() => props.onSelect(task.id)}
        data-testid="team-row"
      >
        <span className={`mission-task-status status-${task.state}`} aria-hidden="true">
          {display.glyph}
        </span>
        <span className="mission-task-title">{task.title}</span>
        <span className="mission-task-meta">
          {props.model ? <span className="mission-task-model">{props.model}</span> : null}
          {experimental ? (
            <span className="mission-experimental-badge" title={t("missions.experimentalRun.tooltip")} data-testid="team-row-experimental">
              {t("missions.experimental.badge")}
              <span className="mission-sr-only"> · {t("missions.experimentalRun.tooltip")}</span>
            </span>
          ) : null}
          {task.attempt_count > 1 ? (
            <span className="mission-task-attempt">{t("missions.team.attempt", { n: task.attempt_count })}</span>
          ) : null}
        </span>
        <span className={`mission-task-state-label state-${task.state}`}>
          {t(display.labelKey)}
        </span>
      </button>
      {/* 보조 정보(05 §5 상태표의 추가 정보 열) */}
      <div className="mission-task-extra" data-testid="team-row-extra">
        {task.state === "blocked" ? (
          <BlockedReason task={task} taskById={props.taskById} onSelect={props.onSelect} />
        ) : null}
        {task.failure_repair_run_ids?.length ? <span className="muted">{t("missions.requiredRepair.short", { cycle: task.repair_cycle })}</span> : null}
        {task.state === "planned" && task.depends_on.length > 0 ? (
          <span className="muted">{t("missions.team.dependsOn", { count: task.depends_on.length })}</span>
        ) : null}
        {task.state === "ready" ? (
          <span className="muted" data-testid="ready-reason">
            {t(props.missionPaused ? "missions.team.pausedReady" : "missions.team.readyReason")}
          </span>
        ) : null}
        {task.state === "running" && primaryRun ? (
          <RunningActivity run={primaryRun} now={props.now} />
        ) : null}
        {task.state === "failed" && task.attempt_count > 0 && props.maxAttempts !== null ? (
          <span className="muted" data-testid="retry-left">
            {t("missions.team.retryLeft", { count: Math.max(0, props.maxAttempts - task.attempt_count) })}
          </span>
        ) : null}
        {showHistory ? (
          <button
            type="button"
            className="mission-team-history-toggle"
            aria-expanded={historyOpen}
            onClick={() => setHistoryOpen(!historyOpen)}
            data-testid="attempt-toggle"
          >
            {t("missions.team.attemptHistory")}
          </button>
        ) : null}
      </div>
      {historyOpen && showHistory ? (
        <ul className="mission-attempt-list" data-testid="attempt-list">
          {runs.map((run) => (
            <li key={run.id} className={`run-state-${run.state}`}>
              <span aria-hidden="true">
                {run.state === "succeeded" ? "✓" : run.state === "failed" ? "✗" : run.state === "running" ? "▶" : "·"}
              </span>{" "}
              {t("missions.team.attempt", { n: run.attempt })}
              <span className="muted"> · {runStateLabel(t, run.state)}</span>
            </li>
          ))}
        </ul>
      ) : null}
    </li>
  );
}

/**
 * 막힌 이유. 다음 행동 안내가 붙은 기존 짧은 문구(호환성·제공자 한도·자동 재시도·
 * 계획 형식 수리)는 그대로 쓰고, 나머지는 공용 이유 문구로 바꾼다. 선행 작업
 * 실패는 실패한 선행 할 일 이름을 눌러 그 할 일로 이동한다.
 */
function BlockedReason(props: {
  task: Task;
  taskById: ReadonlyMap<string, Task>;
  onSelect: (taskId: string) => void;
}): JSX.Element {
  const { t } = useI18n();
  const code = props.task.blocked_code;
  if (capabilityBlocked(code)) return <span className="muted">{t("missions.compatibility.short")}</span>;
  if (code === "provider_rate_limited") return <span className="muted">{t("missions.quota.short")}</span>;
  if (code === "transient_retry") return <span className="muted">{t("missions.retry.short")}</span>;
  if (code === "plan_format_repair") return <span className="muted">{t("missions.planRepair.short")}</span>;
  const reason = blockedReasonLabel(t, code);
  if (code !== "dependency_failed") {
    return <span className="muted" data-testid="blocked-reason">{reason}</span>;
  }
  const dependencies = props.task.depends_on
    .map((id) => props.taskById.get(id))
    .filter((dependency): dependency is Task => dependency !== undefined);
  const failed = dependencies.filter((dependency) => dependency.state === "failed" || dependency.state === "cancelled");
  const links = failed.length > 0 ? failed : dependencies;
  return (
    <span className="muted mission-blocked-reason" data-testid="blocked-reason">
      {reason}
      {links.map((dependency, index) => (
        <span key={dependency.id}>
          {index === 0 ? " · " : ", "}
          <button
            type="button"
            className="link mission-task-dependency"
            aria-label={t("missions.team.openDependency", { title: dependency.title })}
            onClick={() => props.onSelect(dependency.id)}
            data-testid="dependency-link"
          >
            {dependency.title}
          </button>
        </span>
      ))}
    </span>
  );
}

/** 경과 시간 짧은 표기(분 단위). */
function elapsedLabel(t: (key: string, params?: Record<string, string | number>) => string, ms: number): string {
  if (!Number.isFinite(ms) || ms < 60_000) return t("missions.team.elapsedUnderMinute");
  return t("missions.team.elapsedMinutes", { minutes: Math.floor(ms / 60_000) });
}

/**
 * 실행 중 표시: 마지막 활동이 3분 이상 없을 때만 `상태 확인 중`, 그 전에는
 * `실행 중 · {경과}`. 시작 전 실행(준비·시작 중)은 상태 문구가 이미 말해 준다.
 */
function RunningActivity(props: { run: Run; now: number }): JSX.Element | null {
  const { t } = useI18n();
  const run = props.run;
  if (!LIVE_STATES.includes(run.state)) return null;
  const lastSeen = run.last_activity_at ?? run.started_at;
  const lastSeenMs = lastSeen ? Date.parse(lastSeen) : Number.NaN;
  const idleMs = Number.isNaN(lastSeenMs) ? null : props.now - lastSeenMs;
  if (idleMs !== null && idleMs >= STALE_ACTIVITY_MS) {
    return (
      <span className="muted" data-testid="running-activity" data-stale="true">
        {t("missions.team.staleActivity", { minutes: Math.floor(idleMs / 60_000) })}
      </span>
    );
  }
  if (run.state !== "running" && run.state !== "awaiting_input") return null;
  const startedMs = run.started_at ? Date.parse(run.started_at) : Number.NaN;
  if (Number.isNaN(startedMs)) return null;
  return (
    <span className="muted" data-testid="running-activity" data-stale="false">
      {t("missions.team.runningFor", { elapsed: elapsedLabel(t, props.now - startedMs) })}
    </span>
  );
}

/** task별 model 문자열 인덱스 — 대표 실행의 요청/관측 모델(검색용). */
function useMissionModelIndex(missionId: string): Record<string, string> {
  return useMissionStore(
    useShallow((s) => {
      const index: Record<string, string> = {};
      for (const task of selectTaskList(s, missionId)) {
        const run = selectPrimaryRun(s, task);
        if (!run) continue;
        const model = run.observed_model ?? run.requested_model ?? run.binding_snapshot?.model_id ?? null;
        if (model) index[task.id] = model;
      }
      return index;
    }),
  );
}
