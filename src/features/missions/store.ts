/**
 * missionStore (05-ui §2): daemon mission snapshot의 normalized map과
 * mission별 atSeq/revision/loading/error/dirty를 관리한다.
 *
 * 정본은 daemon의 불변 snapshot이다 — 이 store는 참조만 담는다. 대화 본문
 * (Message.body_ref의 bytes)과 artifact bytes는 절대 저장하지 않고, 필요한
 * 화면이 ref로 다시 읽는다. snapshot 적용은 pagination 전체를 모은 뒤
 * 한 번의 setState로 원자 교체한다(05 §10) — 중간 상태를 그리지 않는다.
 */

import { create } from "zustand";
import type { DaemonEventStream } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import type { MissionListParams } from "../../generated/MissionListParams";
import type { MissionListResult } from "../../generated/MissionListResult";
import type { MissionSnapshotParams } from "../../generated/MissionSnapshotParams";
import type { SnapshotPage } from "../../generated/SnapshotPage";
import type { Candidate } from "../../generated/Candidate";
import type { Decision } from "../../generated/Decision";
import type { Finding } from "../../generated/Finding";
import type { Message } from "../../generated/Message";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import type { ExecRecord } from "../../generated/ExecRecord";
import type { Task } from "../../generated/Task";
import type { Verification } from "../../generated/Verification";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { missionsAvailable } from "./capability";
import { newerMission } from "./missionStatus";
import { startMissionNotifications } from "../notifications/missionNotifications";

/** mission 동기화에 필요한 최소 client 표면(시험 seam). */
export interface MissionSyncClient {
  readonly events: DaemonEventStream;
  missionList(params: MissionListParams): Promise<MissionListResult>;
  missionSnapshot(params: MissionSnapshotParams): Promise<SnapshotPage>;
}

export interface MissionSyncStatus {
  /** 적용 완료된 snapshot의 watermark(U64 wire string). */
  atSeq: string;
  revision: string;
  loading: boolean;
  error: string | null;
  /** mission.changed hint가 atSeq보다 새 도착했음 — 재동기화 대상(05 §10). */
  dirty: boolean;
  /** 본 적 있는 hint 중 최대값(atSeq 이하면 null로 정규화). */
  hintSeq: string | null;
}

export interface MissionStoreState {
  missions: Record<string, Mission>;
  tasks: Record<string, Task>;
  runs: Record<string, Run>;
  execs: Record<string, ExecRecord>;
  decisions: Record<string, Decision>;
  messages: Record<string, Message>;
  candidates: Record<string, Candidate>;
  verifications: Record<string, Verification>;
  /** 결과 화면(05 §9 findings 섹션)용 snapshot Finding 투영. */
  findings: Record<string, Finding>;
  /** mission별 동기화 상태. 목록에만 있고 snapshot을 안 뽑은 mission은 없다. */
  sync: Record<string, MissionSyncStatus>;
  listLoading: boolean;
  listError: string | null;

  /** mission.snapshot pagination을 모두 모아 원자 적용한다. 만료되면 처음부터 다시. */
  syncMission(missionId: string): Promise<void>;
  /** mission.changed hint — atSeq보다 새로울 때만 dirty를 켠다. */
  applyEventHint(missionId: string, latestSeq: string): void;
  /** mission.list pagination(보관 제외) — missions map에 upsert. */
  loadList(): Promise<Mission[]>;
  /**
   * 백그라운드 요약 갱신: mission.list 요약(보관 제외)으로 missions map만 고친다.
   * snapshot을 뽑지 않고, 목록 화면의 loading/error도 건드리지 않는다. 이미 더
   * 새로운(revision) mission은 되돌리지 않는다. 진행 중이면 "한 번 더" 표시를 남기고
   * 같은 약속을 돌려준다 — 진행 중인 조회는 그 뒤의 변경을 담지 못했을 수 있으므로,
   * 끝나면 한 번 더 읽고 나서 약속을 푼다. 마지막 조회가 성공하면 true.
   */
  refreshSummaries(): Promise<boolean>;
  /**
   * 탭 없는 작업을 한 번만 읽는다(후속 작업의 이전 결과 확인 등): snapshot을 한 번 모아 그 작업의
   * Mission(더 새로운 것은 지킨다)과 Candidate만 넣는다. 동기화 기록(sync)을 만들지 않으므로
   * 이후 mission.changed 힌트가 이 작업을 전체 재동기화 대상으로 삼지 않는다. 성공하면 true.
   */
  peekMission(missionId: string): Promise<boolean>;
}

/** 요약 갱신(mission.list) 한 번에 읽는 페이지 크기. */
const SUMMARY_PAGE_LIMIT = 50;
/**
 * 동기화 기록이 없는 작업의 mission.changed 힌트를 모으는 창. 실행 중인 작업은
 * 이벤트마다 힌트를 보내므로, 창 안의 힌트들은 목록 요약 한 번으로 합친다.
 */
export const SUMMARY_REFRESH_DELAY_MS = 1000;

/** snapshot 만료(SNAPSHOT_EXPIRED) 시 처음부터 다시 모을 수 있는 최대 시도. */
const SNAPSHOT_MAX_ATTEMPTS = 3;

/**
 * startMissionSync가 연결한 client. store 동작은 이것만 쓴다 — 컴포넌트가
 * client를 갖고 다니지 않게 한다(Workbench 마운트가 한 번만 연결).
 */
let syncClient: MissionSyncClient | null = null;
/** mission별 진행 중 sync — 중복 호출은 같은 약속으로 합친다. */
const inFlight = new Map<string, Promise<void>>();
/** 진행 중인 요약 갱신(하나만). */
let summaryInFlight: Promise<boolean> | null = null;
/** 요약 갱신이 진행 중일 때 새 요청이 왔다 — 끝나면 한 번 더 읽는다. */
let summaryAgain = false;

async function collectListPages(client: Pick<MissionSyncClient, "missionList">, archived: boolean): Promise<Mission[]> {
  const items: Mission[] = [];
  let cursor: string | null = null;
  do {
    const page = await client.missionList({ cursor, limit: SUMMARY_PAGE_LIMIT, archived });
    items.push(...page.items);
    cursor = page.next_cursor;
  } while (cursor !== null);
  return items;
}

/** 목록 요약을 missions map에 넣는다 — 더 새로운 snapshot mission은 지킨다. 바뀐 것이 없으면 상태도 그대로. */
function upsertSummaries(state: MissionStoreState, items: readonly Mission[]): Record<string, Mission> {
  let missions = state.missions;
  for (const mission of items) {
    const next = newerMission(missions[mission.id], mission);
    if (next === missions[mission.id]) continue;
    if (missions === state.missions) missions = { ...state.missions };
    missions[mission.id] = next;
  }
  return missions;
}

export const useMissionStore = create<MissionStoreState>((set, get) => ({
  missions: {},
  tasks: {},
  runs: {},
  execs: {},
  decisions: {},
  messages: {},
  candidates: {},
  verifications: {},
  findings: {},
  sync: {},
  listLoading: false,
  listError: null,

  syncMission: (missionId) => {
    const running = inFlight.get(missionId);
    if (running) return running;
    const task = (async (): Promise<void> => {
      const client = syncClient;
      if (!client) {
        set((s) => ({ sync: { ...s.sync, [missionId]: syncErrorStatus(s.sync[missionId], t("missions.sync.noClient")) } }));
        return;
      }
      set((s) => ({
        sync: {
          ...s.sync,
          [missionId]: {
            atSeq: s.sync[missionId]?.atSeq ?? "0",
            revision: s.sync[missionId]?.revision ?? "0",
            loading: true,
            error: null,
            dirty: s.sync[missionId]?.dirty ?? false,
            hintSeq: s.sync[missionId]?.hintSeq ?? null,
          },
        },
      }));
      try {
        const pages = await collectSnapshotPages(client, missionId);
        applySnapshotPages(set, get, missionId, pages);
      } catch (error) {
        const message =
          error instanceof RpcClientError
            ? `${error.code}: ${error.message}`
            : error instanceof Error && error.message
              ? error.message
              : t("missions.sync.failed");
        set((s) => ({
          sync: {
            ...s.sync,
            [missionId]: syncErrorStatus(s.sync[missionId], message),
          },
        }));
      }
    })().finally(() => {
      inFlight.delete(missionId);
    });
    inFlight.set(missionId, task);
    return task;
  },

  applyEventHint: (missionId, latestSeq) =>
    set((s) => {
      const status = s.sync[missionId];
      // 아직 snapshot을 뽑지 않은 mission: dirty의 기준인 atSeq가 없다 —
      // loadList/syncMission이 항상 최신을 가져오므로 여기선 무시한다.
      if (!status) return s;
      const at = BigInt(status.atSeq);
      const hint = BigInt(latestSeq);
      const prevHint = status.hintSeq !== null ? BigInt(status.hintSeq) : at;
      // dirty는 "본 적 있는 hint의 최대값이 atSeq보다 새로운가"로 판정한다 —
      // 새 힌트 뒤에 늦게 도착한 오래된 힌트가 dirty를 꺼버리지 않게.
      const maxHint = hint > prevHint ? hint : prevHint;
      const nextHintSeq = maxHint > at ? maxHint.toString() : null;
      const nextDirty = maxHint > at;
      if (status.dirty === nextDirty && status.hintSeq === nextHintSeq) return s;
      return { sync: { ...s.sync, [missionId]: { ...status, dirty: nextDirty, hintSeq: nextHintSeq } } };
    }),

  loadList: async () => {
    const client = syncClient;
    if (!client) {
      set({ listError: t("missions.sync.noClient") });
      return [];
    }
    set({ listLoading: true, listError: null });
    try {
      const items = await collectListPages(client, false);
      set((s) => ({ missions: upsertSummaries(s, items), listLoading: false }));
      return items;
    } catch (error) {
      const message =
        error instanceof RpcClientError
          ? `${error.code}: ${error.message}`
          : error instanceof Error && error.message
            ? error.message
            : t("missions.sync.failed");
      set({ listLoading: false, listError: message });
      return [];
    }
  },

  peekMission: async (missionId) => {
    const client = syncClient;
    if (!client) return false;
    try {
      const pages = await collectSnapshotPages(client, missionId);
      // 기다리는 사이 연결이 바뀌었으면 이전 데몬의 내용을 쓰지 않는다.
      if (syncClient !== client) return false;
      let mission: Mission | null = null;
      const candidates: Candidate[] = [];
      for (const page of pages) {
        for (const entity of page.entities) {
          // applySnapshotPages와 같은 인접 태깅 판정을 쓴다.
          if (entity.kind === "mission") {
            if (entity.value.id === missionId) mission = entity.value;
          } else if (entity.kind === "candidate" && entity.value.mission_id === missionId) {
            candidates.push(entity.value);
          }
        }
      }
      set((s) => {
        const missions = mission ? upsertSummaries(s, [mission]) : s.missions;
        // 동기화 기록이 있는 작업(탭이 열림)은 snapshot 적용이 entity를 맡는다 — 한 번 읽은 값으로 덮지 않는다.
        let nextCandidates = s.candidates;
        if (!s.sync[missionId]) {
          for (const candidate of candidates) {
            if (nextCandidates[candidate.id] === candidate) continue;
            if (nextCandidates === s.candidates) nextCandidates = { ...s.candidates };
            nextCandidates[candidate.id] = candidate;
          }
        }
        return missions === s.missions && nextCandidates === s.candidates ? s : { missions, candidates: nextCandidates };
      });
      return true;
    } catch {
      return false;
    }
  },

  refreshSummaries: () => {
    if (summaryInFlight) {
      // 진행 중인 조회가 이 요청의 원인(새 힌트)보다 먼저 읽었을 수 있다 — 끝나면 한 번 더.
      summaryAgain = true;
      return summaryInFlight;
    }
    const client = syncClient;
    if (!client) return Promise.resolve(false);
    const task = (async (): Promise<boolean> => {
      let ok = false;
      try {
        do {
          summaryAgain = false;
          try {
            const items = await collectListPages(client, false);
            // 기다리는 사이 연결이 바뀌었으면(Workbench 재마운트) 이전 데몬의 요약을 쓰지 않는다.
            if (syncClient !== client) return false;
            set((s) => {
              const missions = upsertSummaries(s, items);
              return missions === s.missions ? s : { missions };
            });
            ok = true;
          } catch {
            // 배지 요약은 조용한 신호다 — 실패는 다음 힌트·탭 열기가 다시 채운다(실패 뒤 연달아 재시도하지 않는다).
            return false;
          }
        } while (summaryAgain);
        return ok;
      } finally {
        // await 없이 곧바로 비운다 — 루프 판정과 해제 사이에 끼어든 요청이 사라지지 않게.
        summaryInFlight = null;
        summaryAgain = false;
      }
    })();
    summaryInFlight = task;
    return task;
  },
}));

function syncErrorStatus(previous: MissionSyncStatus | undefined, message: string): MissionSyncStatus {
  return {
    atSeq: previous?.atSeq ?? "0",
    revision: previous?.revision ?? "0",
    loading: false,
    error: message,
    dirty: previous?.dirty ?? false,
    hintSeq: previous?.hintSeq ?? null,
  };
}

/**
 * mission.snapshot pagination 수집(§4). 페이지 사이에 snapshot이 만료되면
 * SNAPSHOT_EXPIRED로 실패하는데, 그때는 커서를 버리고 새 materialize부터
 * 처음부터 다시 모은다 — 부분 페이지를 적용하지 않는다.
 */
async function collectSnapshotPages(client: MissionSyncClient, missionId: string): Promise<SnapshotPage[]> {
  for (let attempt = 1; ; attempt += 1) {
    try {
      const pages: SnapshotPage[] = [];
      let snapshotId: string | null = null;
      let cursor: string | null = null;
      do {
        const page = await client.missionSnapshot({ mission_id: missionId, snapshot_id: snapshotId, cursor });
        pages.push(page);
        snapshotId = page.snapshot_id;
        cursor = page.next_cursor;
      } while (cursor !== null);
      return pages;
    } catch (error) {
      if (
        error instanceof RpcClientError &&
        error.code === "SNAPSHOT_EXPIRED" &&
        attempt < SNAPSHOT_MAX_ATTEMPTS
      ) {
        continue; // 처음부터(새 snapshot_id) 다시
      }
      throw error;
    }
  }
}

/** 모은 페이지를 한 번의 setState로 원자 적용한다 — mission의 entity 전체 교체. */
function applySnapshotPages(
  set: (partial: Partial<MissionStoreState>) => void,
  get: () => MissionStoreState,
  missionId: string,
  pages: SnapshotPage[],
): void {
  const state = get();
  const missions = { ...state.missions };
  const tasks: Record<string, Task> = {};
  const runs: Record<string, Run> = {};
  const execs: Record<string, ExecRecord> = {};
  const decisions: Record<string, Decision> = {};
  const messages: Record<string, Message> = {};
  const candidates: Record<string, Candidate> = {};
  const verifications: Record<string, Verification> = {};
  const findings: Record<string, Finding> = {};
  for (const [id, task] of Object.entries(state.tasks)) {
    if (task.mission_id !== missionId) tasks[id] = task;
  }
  for (const [id, run] of Object.entries(state.runs)) {
    if (run.mission_id !== missionId) runs[id] = run;
  }
  for (const [id, exec] of Object.entries(state.execs)) {
    if (exec.mission_id !== missionId) execs[id] = exec;
  }
  for (const [id, decision] of Object.entries(state.decisions)) {
    if (decision.mission_id !== missionId) decisions[id] = decision;
  }
  for (const [id, message] of Object.entries(state.messages)) {
    if (message.mission_id !== missionId) messages[id] = message;
  }
  for (const [id, candidate] of Object.entries(state.candidates)) {
    if (candidate.mission_id !== missionId) candidates[id] = candidate;
  }
  for (const [id, verification] of Object.entries(state.verifications)) {
    if (verification.mission_id !== missionId) verifications[id] = verification;
  }
  for (const [id, finding] of Object.entries(state.findings)) {
    if (finding.mission_id !== missionId) findings[id] = finding;
  }
  for (const page of pages) {
    for (const entity of page.entities) {
      // entity는 인접 태깅(`{kind, value}`)으로 온다 — 데몬의 Serialize 구현이 계약이다.
      // 이 판정이 어긋나면 한 건도 남지 않고 조용히 버려져, 데몬이 정상으로 일하는 동안
      // 화면만 빈 채로 굳는다(할 일 0·실행 0·결정 0). 태그 문자열은 EntityKind와 같다.
      switch (entity.kind) {
        case "mission":
          if (entity.value.id === missionId) missions[entity.value.id] = entity.value;
          break;
        case "task": tasks[entity.value.id] = entity.value; break;
        case "run": runs[entity.value.id] = entity.value; break;
        case "exec": execs[entity.value.id] = entity.value; break;
        case "decision": decisions[entity.value.id] = entity.value; break;
        case "message": messages[entity.value.id] = entity.value; break;
        case "candidate": candidates[entity.value.id] = entity.value; break;
        case "verification": verifications[entity.value.id] = entity.value; break;
        case "finding": findings[entity.value.id] = entity.value; break;
        // Workspace/Knowledge는 상세 화면이 필요할 때 같은 방식으로 늘린다.
      }
    }
  }
  const last = pages[pages.length - 1];
  const previous = state.sync[missionId];
  const at = BigInt(last.at_seq);
  const hintSeq = previous?.hintSeq !== null && previous?.hintSeq !== undefined ? BigInt(previous.hintSeq) : at;
  set({
    missions,
    tasks,
    runs,
    execs,
    decisions,
    messages,
    candidates,
    verifications,
    findings,
    sync: {
      ...state.sync,
      [missionId]: {
        atSeq: last.at_seq,
        revision: last.revision,
        loading: false,
        error: null,
        dirty: hintSeq > at,
        hintSeq: hintSeq > at ? hintSeq.toString() : null,
      },
    },
  });
}

/**
 * client 이벤트의 mission.changed hint를 store에 연결한다(05 §10 hint).
 * Workbench가 마운트될 때 한 번 호출하고, 언마운트 시 반환된 해제 함수를
 * 부른다. 다른 탭에서 badge만 갱신한다 — 강제 전환/모달은 없다(05 §8).
 * hint가 at_seq보다 새면 곧바로 재동기화한다: 활성 탭이 아닌 mission의
 * 배지(결정 N)도 갱신되어야 하기 때문이다(MissionPage는 마운트된
 * mission만 스스로 다시 뽑는다).
 */
export function startMissionSync(client: MissionSyncClient): () => void {
  syncClient = client;
  const stopNotifications = startMissionNotifications(useMissionStore);
  let stopped = false;
  let summaryTimer: ReturnType<typeof setTimeout> | null = null;
  // 요약 갱신 예약: 창 안의 여러 힌트는 한 번의 mission.list로 합친다.
  const scheduleSummaryRefresh = (delay: number): void => {
    if (stopped || summaryTimer !== null) return;
    summaryTimer = setTimeout(() => {
      summaryTimer = null;
      if (!stopped) void useMissionStore.getState().refreshSummaries();
    }, delay);
  };
  // 앱 시작(데몬 연결 후): 저장된 mission 탭은 snapshot을 뽑기 전이라 제목·배지가 비어 있다 —
  // 데몬이 mission 프로토콜을 선언하면(= 첫 snapshot이 도착하면) 목록 요약으로 채운다.
  const refreshWhenConnected = (): void => {
    if (missionsAvailable({ mission_protocol: useWorkbenchStore.getState().missionProtocol })) {
      scheduleSummaryRefresh(0);
    }
  };
  refreshWhenConnected();
  const stopWorkbench = useWorkbenchStore.subscribe((state, prev) => {
    if (state.missionProtocol !== prev.missionProtocol) refreshWhenConnected();
  });
  const subscription = client.events.subscribe((event) => {
    if (event.kind === "mission.changed") {
      const store = useMissionStore.getState();
      store.applyEventHint(event.payload.mission_id, event.payload.latest_seq);
      const status = useMissionStore.getState().sync[event.payload.mission_id];
      if (!status) {
        // 동기화 기록이 없는 작업(백그라운드 탭·탭 없는 작업): 전체 snapshot 대신 목록 요약만 갱신한다.
        scheduleSummaryRefresh(SUMMARY_REFRESH_DELAY_MS);
        return;
      }
      if (status.dirty && !status.loading) {
        void useMissionStore.getState().syncMission(event.payload.mission_id);
      }
    }
  });
  return () => {
    stopped = true;
    if (summaryTimer !== null) clearTimeout(summaryTimer);
    summaryTimer = null;
    subscription.dispose();
    stopWorkbench();
    stopNotifications();
    if (syncClient === client) syncClient = null;
  };
}

/** mission.list 전체 페이지(보관 여부 지정) — 작업 목록 화면이 쓴다. */
export function listAllMissions(client: Pick<MissionSyncClient, "missionList">, archived: boolean): Promise<Mission[]> {
  return collectListPages(client, archived);
}

/** 목록 요약을 missions map에 반영한다(더 새로운 snapshot mission은 지킨다). */
export function upsertMissionSummaries(items: readonly Mission[]): void {
  useMissionStore.setState((s) => {
    const missions = upsertSummaries(s, items);
    return missions === s.missions ? s : { missions };
  });
}

/**
 * mission 탭을 열고(이미 열려 있으면 그 탭으로 이동) snapshot을 뽑는다. 작업 목록·
 * 알림 센터가 쓴다. 탭 상한에 걸리면 null — 호출자가 그 자리에 안내한다.
 */
export function openMissionTabAndSync(missionId: string, title: string): string | null {
  const tabId = useWorkbenchStore.getState().openMissionTab(missionId, title);
  if (tabId !== null) void useMissionStore.getState().syncMission(missionId);
  return tabId;
}

/**
 * `새 AI 작업` 진입의 하위 유틸(O15 계약, O16 이후에도 빈 목록 문구 시험이
 * 사용): 목록의 첫 mission 탭을 연다(없으면 05 §2의 빈 상태 문구). 실제
 * 진입점은 MissionCreate 대화상자다.
 */
export async function openFirstMissionTab(): Promise<void> {
  const list = await useMissionStore.getState().loadList();
  if (list.length === 0) {
    useWorkbenchStore.getState().setToast(t("missions.emptyList"));
    return;
  }
  const first = list[0];
  const tabId = useWorkbenchStore.getState().openMissionTab(first.id, first.title);
  if (tabId !== null) void useMissionStore.getState().syncMission(first.id);
}

/** 시험 전용: 상태와 진행 중 sync를 초기 상태로 되돌린다. */
export function resetMissionStoreForTests(): void {
  inFlight.clear();
  summaryInFlight = null;
  summaryAgain = false;
  useMissionStore.setState({
    missions: {},
    tasks: {},
    runs: {},
    execs: {},
    decisions: {},
    messages: {},
    candidates: {},
    verifications: {},
    findings: {},
    sync: {},
    listLoading: false,
    listError: null,
  });
}
