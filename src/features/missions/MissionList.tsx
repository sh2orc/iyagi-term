/**
 * AI 작업 목록(modal kind "mission-list").
 *
 * 탭을 닫아도 작업은 데몬에서 계속 실행된다(05 §8) — 이 목록이 다시 여는 자리다.
 *
 * - `mission.list`(보관 제외)를 모두 읽어 결정 필요 / 확정 대기 / 진행 중 / 끝남으로
 *   묶고, 보관됨은 그 필터를 고를 때 `archived: true`로 따로 읽는다.
 * - 행: 제목 · 저장소 이름 · 상태·단계 · 결정 수 · 마지막 갱신. 검색은 제목·저장소.
 * - 행을 누르면 그 작업 탭을 연다(이미 열려 있으면 이동). 탭 상한이면 이 대화상자
 *   안에 안내한다(토스트로 덮지 않는다).
 * - 보관됨 행에는 보관 해제(mission.control `unarchive`).
 * - 읽은 요약은 missionStore에 넣는다 — 열려 있는 탭의 제목·배지도 함께 채워진다.
 * - 작업 공간(계약 D): 열 때 `workspace.usage`로 총 사용량을 보이고, 끝남·보관됨 행에
 *   정리 버튼(확인창) 또는 막힌 이유를 둔다. 사용량 조회 실패는 조용히 숨긴다.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Mission } from "../../generated/Mission";
import { useI18n } from "../../i18n";
import { MAX_MISSION_TABS, useWorkbenchStore } from "../../store/workbenchStore";
import { getMissionClient } from "./clientAccess";
import { focusedRepositoryHint, openMissionCreate } from "./entry";
import { useMissionEntryState } from "./entryPoints";
import { MissionErrorNotice, missionError, type MissionErrorAction, type MissionUiError } from "./errors";
import { missionStateLabelKey, phaseLabelKey } from "./labels";
import {
  isFinishedMissionState,
  missionListGroup,
  repositoryName,
  type MissionListGroup,
} from "./missionStatus";
import { mutateWithResync } from "./mutation";
import { listAllMissions, openMissionTabAndSync, upsertMissionSummaries, useMissionStore } from "./store";
import { formatClock, newRequestId } from "./viewUtils";
import type { WorkspaceUsageEntry } from "../../generated/WorkspaceUsageEntry";
import { WorkspaceCleanup, useWorkspaceUsage } from "./WorkspaceCleanup";
import { formatStorageSize, workspaceUsageEntry } from "./workspaceUsage";
import "./missionList.css";

export type MissionListFilter = "all" | MissionListGroup;

/** 필터 순서(전체 → 결정 필요 → 확정 대기 → 진행 중 → 끝남 → 보관됨). */
export const MISSION_LIST_FILTERS: readonly MissionListFilter[] = [
  "all",
  "decision",
  "acceptance",
  "active",
  "finished",
  "archived",
];

/** "전체"에서 보이는 묶음(보관됨은 따로 고른다). */
const ALL_GROUPS: readonly MissionListGroup[] = ["decision", "acceptance", "active", "finished"];

type ListKind = "active" | "archived";

/** 마지막 갱신 표기: 오늘이면 HH:MM, 올해면 MM-DD HH:MM, 그 밖은 YYYY-MM-DD HH:MM. */
export function formatMissionUpdatedAt(iso: string, now: Date = new Date()): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  const clock = formatClock(iso);
  const sameYear = date.getFullYear() === now.getFullYear();
  if (sameYear && date.getMonth() === now.getMonth() && date.getDate() === now.getDate()) return clock;
  const mm = String(date.getMonth() + 1).padStart(2, "0");
  const dd = String(date.getDate()).padStart(2, "0");
  return sameYear ? `${mm}-${dd} ${clock}` : `${date.getFullYear()}-${mm}-${dd} ${clock}`;
}

/** 검색어 일치(제목·저장소 경로·저장소 이름, 대소문자 무시). */
export function missionMatchesQuery(mission: Pick<Mission, "title" | "repository_path">, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (needle.length === 0) return true;
  return (
    mission.title.toLowerCase().includes(needle) ||
    mission.repository_path.toLowerCase().includes(needle) ||
    repositoryName(mission.repository_path).toLowerCase().includes(needle)
  );
}

function filterLabelKey(filter: MissionListFilter): string {
  return filter === "all" ? "missions.list.filter.all" : `missions.list.group.${filter}`;
}

export function MissionList(props: { onClose: () => void }): JSX.Element {
  const { t } = useI18n();
  const storeMissions = useMissionStore((s) => s.missions);
  const openMissionIds = useWorkbenchStore((s) =>
    s.tabs.flatMap((tab) => (tab.kind === "mission" ? [tab.missionId] : [])).join("\n"),
  );
  const entry = useMissionEntryState();
  const [filter, setFilter] = useState<MissionListFilter>("all");
  const [query, setQuery] = useState("");
  const [lists, setLists] = useState<Record<ListKind, Mission[] | null>>({ active: null, archived: null });
  const [loading, setLoading] = useState<Record<ListKind, boolean>>({ active: false, archived: false });
  const [loadError, setLoadError] = useState<{ which: ListKind; error: MissionUiError } | null>(null);
  const [actionError, setActionError] = useState<{ missionId: string; error: MissionUiError } | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [tabLimit, setTabLimit] = useState(false);
  const mounted = useRef(true);
  const workspace = useWorkspaceUsage(null);
  // useI18n의 t는 렌더마다 새 함수다 — load를 t에 묶으면 load가 매번 바뀌어 아래 보관됨 effect가
  // 렌더마다 다시 돈다. 최신 t는 ref로 읽고 load 자체는 한 번만 만든다.
  const tRef = useRef(t);
  tRef.current = t;

  const load = useCallback(
    async (which: ListKind): Promise<void> => {
      const t = tRef.current;
      const client = getMissionClient();
      if (!client) {
        setLoadError({
          which,
          error: { code: null, reasonCode: null, message: t("missions.sync.noClient"), detail: null, action: null },
        });
        return;
      }
      setLoading((previous) => ({ ...previous, [which]: true }));
      setLoadError((previous) => (previous?.which === which ? null : previous));
      try {
        const items = await listAllMissions(client, which === "archived");
        if (!mounted.current) return;
        upsertMissionSummaries(items);
        setLists((previous) => ({ ...previous, [which]: items }));
      } catch (cause) {
        if (mounted.current) setLoadError({ which, error: missionError(tRef.current, cause) });
      } finally {
        if (mounted.current) setLoading((previous) => ({ ...previous, [which]: false }));
      }
    },
    [],
  );

  useEffect(() => {
    mounted.current = true;
    void load("active");
    return () => {
      mounted.current = false;
    };
    // 목록은 열릴 때 한 번 읽는다(load는 안정적이다 — 언어 전환으로 다시 읽지 않는다).
  }, [load]);

  // 보관됨은 고를 때만 읽는다(평소 목록을 가볍게).
  useEffect(() => {
    if (filter === "archived" && lists.archived === null && !loading.archived && loadError?.which !== "archived") {
      void load("archived");
    }
  }, [filter, lists.archived, loading.archived, loadError, load]);

  const openIds = useMemo(() => new Set(openMissionIds ? openMissionIds.split("\n") : []), [openMissionIds]);

  const rows = useMemo(() => {
    const byId = new Map<string, Mission>();
    for (const mission of [...(lists.active ?? []), ...(lists.archived ?? [])]) {
      byId.set(mission.id, storeMissions[mission.id] ?? mission);
    }
    return [...byId.values()]
      .filter((mission) => missionMatchesQuery(mission, query))
      .sort((a, b) =>
        a.updated_at !== b.updated_at ? (a.updated_at < b.updated_at ? 1 : -1) : a.id < b.id ? -1 : 1,
      );
  }, [lists, storeMissions, query]);

  const grouped = useMemo(() => {
    const groups: Record<MissionListGroup, Mission[]> = {
      decision: [],
      acceptance: [],
      active: [],
      finished: [],
      archived: [],
    };
    for (const mission of rows) groups[missionListGroup(mission)].push(mission);
    return groups;
  }, [rows]);

  const openMission = (mission: Mission): void => {
    const tabId = openMissionTabAndSync(mission.id, mission.title);
    if (tabId === null) {
      // store가 띄운 상한 토스트 대신 이 대화상자 안에서 안내한다.
      const store = useWorkbenchStore.getState();
      if (store.toast === t("missions.tabLimit", { count: MAX_MISSION_TABS })) store.setToast(null);
      setTabLimit(true);
      return;
    }
    props.onClose();
  };

  const unarchive = async (mission: Mission): Promise<void> => {
    const client = getMissionClient();
    if (!client) {
      setActionError({
        missionId: mission.id,
        error: { code: null, reasonCode: null, message: t("missions.sync.noClient"), detail: null, action: null },
      });
      return;
    }
    setBusyId(mission.id);
    setActionError(null);
    try {
      await mutateWithResync(mission.id, (latest) =>
        latest.archived_at === null
          ? null
          : client.missionControl({
              request_id: newRequestId(),
              mission_id: latest.id,
              expected_revision: latest.revision,
              action: "unarchive",
            }),
      );
      if (!mounted.current) return;
      await Promise.all([load("active"), load("archived")]);
    } catch (cause) {
      if (mounted.current) setActionError({ missionId: mission.id, error: missionError(t, cause) });
    } finally {
      if (mounted.current) setBusyId(null);
    }
  };

  const onRowErrorAction = (mission: Mission, action: MissionErrorAction): void => {
    if (action === "retry") {
      void unarchive(mission);
      return;
    }
    setActionError(null);
    void Promise.all([load("active"), load("archived")]);
  };

  const newMission = (): void => {
    // 새 대화상자가 이 목록을 대신한다(modal은 하나).
    openMissionCreate(focusedRepositoryHint(useWorkbenchStore.getState()));
  };

  const activeLoaded = lists.active !== null;
  const nothingAtAll =
    activeLoaded &&
    (lists.active ?? []).length === 0 &&
    (lists.archived ?? []).length === 0 &&
    query.trim().length === 0 &&
    filter !== "archived";
  const reason = entry.reasonKey ? t(entry.reasonKey) : undefined;
  const newMissionButton = entry.hidden ? null : (
    <button
      type="button"
      className="primary"
      disabled={!entry.enabled}
      title={entry.enabled ? t("missions.newMission.hint") : reason}
      onClick={newMission}
      data-testid="mission-list-new"
    >
      {t("missions.newMission")}
    </button>
  );

  const countFor = (candidate: MissionListFilter): number | null => {
    if (candidate === "all") return ALL_GROUPS.reduce((sum, group) => sum + grouped[group].length, 0);
    if (candidate === "archived" && lists.archived === null) return null;
    return grouped[candidate].length;
  };

  const renderRows = (missions: readonly Mission[]): JSX.Element => (
    <ul className="mission-list-rows">
      {missions.map((mission) => (
        <MissionListRow
          key={mission.id}
          mission={mission}
          tabOpen={openIds.has(mission.id)}
          busy={busyId === mission.id}
          error={actionError?.missionId === mission.id ? actionError.error : null}
          onOpen={openMission}
          onUnarchive={(target) => void unarchive(target)}
          onErrorAction={onRowErrorAction}
          workspaceEntry={workspaceUsageEntry(workspace.usage, mission.id)}
          onWorkspaceRefresh={workspace.refresh}
        />
      ))}
    </ul>
  );

  let body: JSX.Element | null;
  if (nothingAtAll) {
    body = (
      <div className="mission-list-empty" data-testid="mission-list-empty">
        <p className="mission-list-empty-title">{t("missions.list.empty")}</p>
        <p className="muted">{t("missions.newMission.hint")}</p>
        {newMissionButton}
      </div>
    );
  } else if (filter === "all") {
    const sections = ALL_GROUPS.filter((group) => grouped[group].length > 0);
    body =
      !activeLoaded ? null : sections.length === 0 ? (
        <p className="muted mission-list-none" role="status">{t("missions.list.noMatch")}</p>
      ) : (
        <>
          {sections.map((group) => (
            <section key={group} className={`mission-list-section group-${group}`} data-testid={`mission-list-group-${group}`}>
              <h3>
                {t(`missions.list.group.${group}`)} <span className="mission-list-count">{grouped[group].length}</span>
              </h3>
              {renderRows(grouped[group])}
            </section>
          ))}
        </>
      );
  } else {
    const list = grouped[filter];
    const pending = filter === "archived" ? lists.archived === null : !activeLoaded;
    body = pending ? null : list.length === 0 ? (
      <p className="muted mission-list-none" role="status">{t("missions.list.noMatch")}</p>
    ) : (
      <section className={`mission-list-section group-${filter}`} data-testid={`mission-list-group-${filter}`}>
        {renderRows(list)}
      </section>
    );
  }

  const showLoading = filter === "archived" ? loading.archived && lists.archived === null : loading.active && !activeLoaded;

  return (
    <div
      className="mission-list-dialog"
      data-testid="mission-list"
      onKeyDown={(event) => {
        if (event.key === "Escape") props.onClose();
      }}
    >
      <header className="mission-list-header">
        <div className="mission-list-heading">
          <h2 id="mission-list-title">{t("missions.list.title")}</h2>
          {workspace.usage ? (
            <span className="mission-list-workspace-total muted" data-testid="mission-list-workspace-total">
              {t("missions.workspaceCleanup.total", { size: formatStorageSize(workspace.usage.total_bytes) })}
            </span>
          ) : null}
        </div>
        {nothingAtAll ? null : newMissionButton}
      </header>
      <input
        type="search"
        className="palette-input mission-list-search"
        autoFocus
        value={query}
        placeholder={t("missions.list.search")}
        aria-label={t("missions.list.search")}
        onChange={(event) => setQuery(event.target.value)}
        data-testid="mission-list-search"
      />
      <div className="mission-list-filters" role="group" aria-label={t("missions.list.filterAria")}>
        {MISSION_LIST_FILTERS.map((candidate) => {
          const count = countFor(candidate);
          return (
            <button
              key={candidate}
              type="button"
              className={`mission-list-filter${filter === candidate ? " selected" : ""}${candidate === "decision" && (count ?? 0) > 0 ? " attention" : ""}`}
              aria-pressed={filter === candidate}
              onClick={() => setFilter(candidate)}
              data-testid={`mission-list-filter-${candidate}`}
            >
              {t(filterLabelKey(candidate))}
              {count !== null ? <span className="mission-list-count">{count}</span> : null}
            </button>
          );
        })}
      </div>
      {tabLimit ? (
        <div className="mission-list-limit" role="alert" data-testid="mission-list-tab-limit">
          {t("missions.list.tabLimit", { count: MAX_MISSION_TABS })}
        </div>
      ) : null}
      {loadError && (filter === "archived") === (loadError.which === "archived") ? (
        <MissionErrorNotice
          error={loadError.error}
          onRetry={() => void load(loadError.which)}
          onAction={() => void load(loadError.which)}
        />
      ) : null}
      {showLoading ? (
        <p className="muted" role="status">
          {t("missions.list.loading")}
        </p>
      ) : null}
      <div className="mission-list-body">{body}</div>
      <div className="modal-actions">
        <button type="button" onClick={props.onClose}>
          {t("missions.common.close")}
        </button>
      </div>
    </div>
  );
}

function MissionListRow(props: {
  mission: Mission;
  tabOpen: boolean;
  busy: boolean;
  error: MissionUiError | null;
  onOpen: (mission: Mission) => void;
  onUnarchive: (mission: Mission) => void;
  onErrorAction: (mission: Mission, action: MissionErrorAction) => void;
  /** 이 작업의 작업 공간 사용량(모르면 null). */
  workspaceEntry?: WorkspaceUsageEntry | null;
  onWorkspaceRefresh?: () => void;
}): JSX.Element {
  const { t } = useI18n();
  const { mission } = props;
  const group = missionListGroup(mission);
  const finished = isFinishedMissionState(mission.state);
  return (
    <li className={`mission-list-row group-${group}`} data-testid="mission-list-row" data-mission-id={mission.id}>
      <div className="mission-list-row-main">
        <button type="button" className="mission-list-open" onClick={() => props.onOpen(mission)}>
          <span className="mission-list-title">{mission.title || t("missions.tabTitle")}</span>
          <span className="mission-list-meta">
            <span className="mission-list-repo" title={mission.repository_path}>
              {repositoryName(mission.repository_path)}
            </span>
            <span className="mission-list-status">
              {t(missionStateLabelKey(mission.state))} · {t(phaseLabelKey(mission.phase))}
            </span>
            {!finished && mission.open_decision_count > 0 ? (
              <span className="mission-list-decisions">
                {t("missions.list.decisions", { count: mission.open_decision_count })}
              </span>
            ) : null}
            {props.tabOpen ? <span className="mission-list-tab-open">{t("missions.list.tabOpen")}</span> : null}
            <span className="mission-list-updated" title={mission.updated_at}>
              {t("missions.list.updated", { time: formatMissionUpdatedAt(mission.updated_at) })}
            </span>
          </span>
        </button>
        {group === "archived" ? (
          <button
            type="button"
            className="mission-list-unarchive"
            disabled={props.busy}
            onClick={() => props.onUnarchive(mission)}
            data-testid="mission-list-unarchive"
          >
            {t("missions.list.unarchive")}
          </button>
        ) : null}
      </div>
      {group === "finished" || group === "archived" ? (
        <WorkspaceCleanup
          missionId={mission.id}
          entry={props.workspaceEntry ?? null}
          variant="row"
          onRefresh={props.onWorkspaceRefresh}
        />
      ) : null}
      {props.error ? (
        <MissionErrorNotice
          error={props.error}
          onRetry={() => props.onUnarchive(mission)}
          onAction={(action) => props.onErrorAction(mission, action)}
        />
      ) : null}
    </li>
  );
}
