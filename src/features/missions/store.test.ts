/**
 * missionStore 계약(05-ui §2·§10): snapshot pagination의 원자 적용,
 * 만료 재시도, hint dirty, loadList pagination, startMissionSync 연결.
 * 대화 본문/artifact bytes는 저장하지 않는다 — 참조만 흐른다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DecisionState } from "../../generated/DecisionState";
import type { Entity } from "../../generated/Entity";
import type { Mission } from "../../generated/Mission";
import type { MissionListResult } from "../../generated/MissionListResult";
import type { MissionSnapshotParams } from "../../generated/MissionSnapshotParams";
import type { RunState } from "../../generated/RunState";
import type { SnapshotPage } from "../../generated/SnapshotPage";
import type { TaskState } from "../../generated/TaskState";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import { useWorkbenchStore, type WorkbenchState } from "../../store/workbenchStore";
import {
  openFirstMissionTab,
  resetMissionStoreForTests,
  startMissionSync,
  SUMMARY_REFRESH_DELAY_MS,
  useMissionStore,
  type MissionSyncClient,
} from "./store";

type Emit = (event: unknown) => void;

class FakeMissionClient {
  private listeners: Emit[] = [];
  readonly snapshotCalls: MissionSnapshotParams[] = [];
  materializations = 0;
  /** 현재 snapshot에 담겨 있는 entity(테스트가 갈아끼운다). */
  entities: Entity[];
  /** 페이지 크기(기본 2 — pagination 경로를 강제한다). */
  pageSize = 2;
  /** 페이지 공급 규칙(테스트마다 덮어쓴다). */
  script: (params: MissionSnapshotParams) => Promise<SnapshotPage> = (params) =>
    Promise.resolve(this.defaultScript(params));
  listScript: () => MissionListResult = () => ({ items: [], next_cursor: null });

  constructor(entities: Entity[] = []) {
    this.entities = entities;
  }

  private defaultScript(params: MissionSnapshotParams): SnapshotPage {
    return this.pageFrom(params);
  }

  /** 커서 위치에 따라 entities를 자른 페이지(새 materialize면 카운트). */
  pageFrom(params: MissionSnapshotParams, snapshotId = "snap-A"): SnapshotPage {
    if (params.cursor === null) this.materializations += 1;
    const start = params.cursor ? Number(params.cursor) : 0;
    const slice = this.entities.slice(start, start + this.pageSize);
    const end = start + slice.length;
    return page(slice, end < this.entities.length ? String(end) : null, snapshotId);
  }

  emit(event: unknown): void {
    for (const listener of [...this.listeners]) listener(event);
  }

  readonly events = {
    subscribe: (listener: Emit) => {
      this.listeners.push(listener);
      return {
        dispose: () => {
          this.listeners = this.listeners.filter((l) => l !== listener);
        },
      };
    },
  };

  async missionSnapshot(params: MissionSnapshotParams): Promise<SnapshotPage> {
    this.snapshotCalls.push(params);
    return this.script(params);
  }

  async missionList(): Promise<MissionListResult> {
    return this.listScript();
  }
}

const clientOf = (fake: FakeMissionClient): MissionSyncClient => fake as unknown as MissionSyncClient;

function page(entities: Entity[], nextCursor: string | null, snapshotId = "snap-A", atSeq = "10"): SnapshotPage {
  return {
    snapshot_id: snapshotId,
    mission_id: "m-1",
    at_seq: atSeq,
    revision: "5",
    entities,
    next_cursor: nextCursor,
    expires_at: "2026-01-01T00:00:00.000Z",
  };
}

const missionEntity = (id: string, title: string): Entity =>
  ({ kind: "mission", value: { id, revision: "5", state: "running", title } }) as unknown as Entity;
const taskEntity = (id: string, ordinal: number, state: TaskState): Entity =>
  ({ kind: "task", value: { id, mission_id: "m-1", ordinal, state, title: `task-${id}` } }) as unknown as Entity;
const runEntity = (id: string, state: RunState): Entity =>
  ({ kind: "run", value: { id, mission_id: "m-1", task_id: "t-1", state } }) as unknown as Entity;
const decisionEntity = (id: string, state: DecisionState): Entity =>
  ({ kind: "decision", value: { id, mission_id: "m-1", state } }) as unknown as Entity;

/** mission.list가 돌려주는 최소 Mission DTO(참조 필드만 시험에 쓴다). */
const missionDto = (id: string, title: string): Mission =>
  ({ id, revision: "1", state: "running", title, created_at: `2026-01-0${id === "m-1" ? 1 : 2}T00:00:00Z` }) as unknown as Mission;

function resetWorkbench(): void {
  useWorkbenchStore.setState({
    tabs: [],
    activeTabId: null,
    focusedLeafId: null,
    panes: {},
    modal: null,
    toast: null,
    hiddenMissions: new Set<string>(),
  } satisfies Partial<WorkbenchState>);
}

describe("missionStore — syncMission", () => {
  let fake: FakeMissionClient;
  let stop: () => void;

  beforeEach(() => {
    resetMissionStoreForTests();
    fake = new FakeMissionClient([
      missionEntity("m-1", "로그인 기능"),
      taskEntity("t-1", 1, "succeeded"),
      taskEntity("t-2", 2, "running"),
      runEntity("r-1", "running"),
      decisionEntity("d-1", "open"),
    ]);
    stop = startMissionSync(clientOf(fake));
  });

  afterEach(() => {
    stop();
    resetMissionStoreForTests();
  });

  it("pagination을 모두 모은 뒤 한 번의 setState로 원자 적용한다", async () => {
    let missionSwaps = 0;
    const unsubscribe = useMissionStore.subscribe((s, prev) => {
      if (s.missions !== prev.missions) missionSwaps += 1;
    });
    await useMissionStore.getState().syncMission("m-1");
    expect(missionSwaps).toBe(1); // 부분 적용 없음 — 페이지 3개(2+2+1)여도 1회 교체
    const state = useMissionStore.getState();
    expect(state.missions["m-1"]).toMatchObject({ id: "m-1", title: "로그인 기능" });
    expect(Object.keys(state.tasks).sort()).toEqual(["t-1", "t-2"]);
    expect(state.runs["r-1"]).toMatchObject({ id: "r-1", state: "running" });
    expect(state.decisions["d-1"]).toMatchObject({ id: "d-1", state: "open" });
    expect(state.sync["m-1"]).toMatchObject({ atSeq: "10", revision: "5", loading: false, error: null, dirty: false });
    unsubscribe();
  });

  it("페이지가 기다리는 동안에는 부분 상태가 없다(로딩만 참)", async () => {
    const gate1 = defer<SnapshotPage>();
    const gate2 = defer<SnapshotPage>();
    fake.script = (params) => (params.cursor === null ? gate1.promise : gate2.promise);
    const syncing = useMissionStore.getState().syncMission("m-1");
    expect(useMissionStore.getState().sync["m-1"]?.loading).toBe(true);
    expect(Object.keys(useMissionStore.getState().missions)).toHaveLength(0);
    gate1.resolve(page([missionEntity("m-1", "x"), taskEntity("t-1", 1, "planned")], "2"));
    await flushMicrotasks();
    expect(useMissionStore.getState().sync["m-1"]?.loading).toBe(true); // 아직 완료 아님
    expect(Object.keys(useMissionStore.getState().missions)).toHaveLength(0); // 1페이지만으로 적용 금지
    gate2.resolve(page([taskEntity("t-2", 2, "running")], null));
    await syncing;
    expect(useMissionStore.getState().missions["m-1"]).toMatchObject({ id: "m-1" });
    expect(Object.keys(useMissionStore.getState().tasks)).toHaveLength(2);
  });

  it("snapshot 만료(SNAPSHOT_EXPIRED)면 처음부터 다시 모은다", async () => {
    let expired = false;
    let missionSwaps = 0;
    const unsubscribe = useMissionStore.subscribe((s, prev) => {
      if (s.missions !== prev.missions) missionSwaps += 1;
    });
    fake.script = (params) => {
      // 첫 시도의 두 번째 페이지에서 만료 → 재시도는 새 materialize부터.
      if (!expired && params.cursor !== null) {
        expired = true;
        return Promise.reject(new RpcClientError("SNAPSHOT_EXPIRED", "스냅샷이 만료되었습니다."));
      }
      return Promise.resolve(fake.pageFrom(params, "snap-B"));
    };
    await useMissionStore.getState().syncMission("m-1");
    expect(expired).toBe(true);
    expect(fake.materializations).toBe(2); // 재시도는 새 materialize
    expect(missionSwaps).toBe(1); // 결국 한 번만 적용
    expect(useMissionStore.getState().sync["m-1"]).toMatchObject({ loading: false, error: null, atSeq: "10" });
    unsubscribe();
  });

  it("만료가 반복되면 오류를 남긴다(부분 적용 없음)", async () => {
    fake.script = (params) =>
      params.cursor === null
        ? Promise.resolve(page([missionEntity("m-1", "x")], "1"))
        : Promise.reject(new RpcClientError("SNAPSHOT_EXPIRED", "스냅샷이 만료되었습니다."));
    await useMissionStore.getState().syncMission("m-1");
    const status = useMissionStore.getState().sync["m-1"];
    expect(status?.loading).toBe(false);
    expect(status?.error).toContain("SNAPSHOT_EXPIRED");
    expect(useMissionStore.getState().missions["m-1"]).toBeUndefined();
    expect(useMissionStore.getState().sync["m-1"]?.dirty).toBe(false);
  });

  it("이전 동기화의 entity는 새 snapshot 적용 때 정리된다(스케일 되지 않는다)", async () => {
    await useMissionStore.getState().syncMission("m-1");
    expect(Object.keys(useMissionStore.getState().tasks)).toHaveLength(2);
    // 다음 snapshot에서 t-2가 사라진 형태.
    fake.entities = [
      missionEntity("m-1", "로그인 기능"),
      taskEntity("t-1", 1, "succeeded"),
      runEntity("r-1", "succeeded"),
    ];
    await useMissionStore.getState().syncMission("m-1");
    expect(Object.keys(useMissionStore.getState().tasks)).toEqual(["t-1"]);
  });
});

describe("missionStore — applyEventHint / startMissionSync", () => {
  let fake: FakeMissionClient;
  let stop: () => void;

  beforeEach(() => {
    resetMissionStoreForTests();
    fake = new FakeMissionClient([missionEntity("m-1", "로그인 기능")]);
    stop = startMissionSync(clientOf(fake));
  });

  afterEach(() => {
    stop();
    resetMissionStoreForTests();
  });

  it("atSeq보다 새로운 hint만 dirty를 켠다(오래된 hint는 무시, 같은 값은 무상태)", async () => {
    await useMissionStore.getState().syncMission("m-1"); // atSeq 10
    useMissionStore.getState().applyEventHint("m-1", "12");
    expect(useMissionStore.getState().sync["m-1"]).toMatchObject({ dirty: true, hintSeq: "12" });
    useMissionStore.getState().applyEventHint("m-1", "3"); // 오래된 hint
    expect(useMissionStore.getState().sync["m-1"]).toMatchObject({ dirty: true, hintSeq: "12" });
    const before = useMissionStore.getState();
    useMissionStore.getState().applyEventHint("m-1", "10"); // watermark와 동일
    expect(useMissionStore.getState()).toBe(before);
    // 동기화한 적 없는 mission에는 hint 상태를 만들지 않는다.
    useMissionStore.getState().applyEventHint("m-9", "5");
    expect(useMissionStore.getState().sync["m-9"]).toBeUndefined();
  });

  it("재동기화하면 dirty가 풀린다", async () => {
    await useMissionStore.getState().syncMission("m-1");
    useMissionStore.getState().applyEventHint("m-1", "11");
    fake.script = () => Promise.resolve(page([missionEntity("m-1", "로그인 기능")], null, "snap-C", "11"));
    await useMissionStore.getState().syncMission("m-1");
    expect(useMissionStore.getState().sync["m-1"]).toMatchObject({ dirty: false, atSeq: "11", hintSeq: null });
  });

  it("mission.changed 이벤트를 hint로 연결하고, 해제되면 더 반응하지 않는다", async () => {
    await useMissionStore.getState().syncMission("m-1");
    fake.emit({ kind: "mission.changed", payload: { mission_id: "m-1", latest_seq: "15" } });
    expect(useMissionStore.getState().sync["m-1"]?.dirty).toBe(true);
    stop();
    const before = useMissionStore.getState();
    fake.emit({ kind: "mission.changed", payload: { mission_id: "m-1", latest_seq: "20" } });
    expect(useMissionStore.getState()).toBe(before);
  });

  it("peekMission은 한 번 읽어 작업·결과만 채우고 동기화 기록을 남기지 않는다(이후 힌트가 전체 동기화를 부르지 않는다)", async () => {
    const candidate = { kind: "candidate", value: { id: "c-1", mission_id: "m-1", commit_oid: "a".repeat(40) } } as unknown as Entity;
    fake.entities = [missionEntity("m-1", "로그인 기능"), taskEntity("t-1", 0, "succeeded"), candidate];
    await expect(useMissionStore.getState().peekMission("m-1")).resolves.toBe(true);
    const state = useMissionStore.getState();
    expect(state.missions["m-1"]).toMatchObject({ title: "로그인 기능" });
    expect(state.candidates["c-1"]).toMatchObject({ commit_oid: "a".repeat(40) });
    // 상세 entity와 동기화 기록은 만들지 않는다.
    expect(state.tasks["t-1"]).toBeUndefined();
    expect(state.sync["m-1"]).toBeUndefined();
    const snapshotCalls = fake.snapshotCalls.length;
    fake.emit({ kind: "mission.changed", payload: { mission_id: "m-1", latest_seq: "15" } });
    await flushMicrotasks();
    expect(fake.snapshotCalls).toHaveLength(snapshotCalls);
    expect(useMissionStore.getState().sync["m-1"]).toBeUndefined();
  });

  it("peekMission 실패는 false만 돌려주고 상태를 건드리지 않는다", async () => {
    fake.script = () => Promise.reject(new RpcClientError("DAEMON_UNAVAILABLE", "down"));
    const before = useMissionStore.getState();
    await expect(useMissionStore.getState().peekMission("m-1")).resolves.toBe(false);
    expect(useMissionStore.getState()).toBe(before);
  });

  it("client가 연결되지 않았으면 오류만 남긴다(빈 상태 유지)", async () => {
    stop();
    resetMissionStoreForTests();
    await useMissionStore.getState().syncMission("m-1");
    expect(useMissionStore.getState().sync["m-1"]).toMatchObject({
      loading: false,
      error: t("missions.sync.noClient"),
    });
  });
});

describe("missionStore — loadList / openFirstMissionTab", () => {
  let fake: FakeMissionClient;
  let stop: () => void;

  beforeEach(() => {
    resetMissionStoreForTests();
    resetWorkbench();
    fake = new FakeMissionClient([missionEntity("m-1", "로그인 기능")]);
    stop = startMissionSync(clientOf(fake));
  });

  afterEach(() => {
    stop();
    resetMissionStoreForTests();
    resetWorkbench();
  });

  it("mission.list pagination을 모아 missions map에 upsert한다", async () => {
    let call = 0;
    fake.listScript = () => {
      call += 1;
      return call === 1
        ? { items: [missionDto("m-1", "A")], next_cursor: "2" }
        : { items: [missionDto("m-2", "B")], next_cursor: null };
    };
    const items = await useMissionStore.getState().loadList();
    expect(items).toHaveLength(2);
    expect(useMissionStore.getState().missions["m-1"]).toMatchObject({ id: "m-1", title: "A" });
    expect(useMissionStore.getState().missions["m-2"]).toMatchObject({ id: "m-2", title: "B" });
    expect(useMissionStore.getState().listLoading).toBe(false);
    expect(useMissionStore.getState().listError).toBeNull();
  });

  it("openFirstMissionTab은 첫 mission 탭을 열고 snapshot을 뽑는다", async () => {
    fake.listScript = () => ({ items: [missionDto("m-1", "로그인 기능")], next_cursor: null });
    await openFirstMissionTab();
    const workbench = useWorkbenchStore.getState();
    expect(workbench.tabs).toHaveLength(1);
    expect(workbench.tabs[0]).toMatchObject({ kind: "mission", missionId: "m-1", title: "로그인 기능" });
    expect(workbench.activeTabId).toBe(workbench.tabs[0].id);
    // 탭을 열며 snapshot 동기화도 시작했다(불리듯 완료된다).
    await vi.waitFor(() => {
      expect(useMissionStore.getState().sync["m-1"]?.loading).toBe(false);
    });
    expect(useMissionStore.getState().missions["m-1"]).toMatchObject({ id: "m-1", title: "로그인 기능" });
    expect(useMissionStore.getState().sync["m-1"]?.error).toBeNull();
  });

  it("mission이 없으면 05 §2의 빈 상태 문구를 toast로 알린다(탭 없음)", async () => {
    fake.listScript = () => ({ items: [], next_cursor: null });
    await openFirstMissionTab();
    expect(useWorkbenchStore.getState().toast).toBe(t("missions.emptyList"));
    expect(useWorkbenchStore.getState().tabs).toHaveLength(0);
  });
});

describe("missionStore — 백그라운드 탭 요약(mission.list)", () => {
  let fake: FakeMissionClient;
  let stop: (() => void) | null;
  let listCalls: number;

  const summaryDto = (id: string, title: string, patch: Partial<Mission> = {}): Mission =>
    ({
      id,
      revision: "1",
      state: "running",
      phase: "implementing",
      title,
      open_decision_count: 0,
      created_at: "2026-01-01T00:00:00Z",
      ...patch,
    }) as unknown as Mission;

  beforeEach(() => {
    resetMissionStoreForTests();
    resetWorkbench();
    useWorkbenchStore.setState({ missionProtocol: null });
    fake = new FakeMissionClient([missionEntity("m-1", "로그인 기능")]);
    listCalls = 0;
    stop = null;
  });

  afterEach(() => {
    stop?.();
    vi.useRealTimers();
    resetMissionStoreForTests();
    resetWorkbench();
    useWorkbenchStore.setState({ missionProtocol: null });
  });

  it("데몬이 mission 프로토콜을 선언하면(연결 후) 목록 요약으로 저장된 탭의 제목·배지 데이터를 채운다", async () => {
    fake.listScript = () => {
      listCalls += 1;
      return { items: [summaryDto("m-2", "백그라운드 작업", { open_decision_count: 2 })], next_cursor: null };
    };
    stop = startMissionSync(clientOf(fake));
    await flushMicrotasks();
    expect(listCalls).toBe(0); // 연결 전(프로토콜 모름)에는 부르지 않는다
    useWorkbenchStore.setState({ missionProtocol: 1 });
    await vi.waitFor(() => {
      expect(useMissionStore.getState().missions["m-2"]).toMatchObject({ title: "백그라운드 작업", open_decision_count: 2 });
    });
    expect(listCalls).toBe(1);
    // 요약은 snapshot이 아니다 — 동기화 기록을 만들지 않는다.
    expect(useMissionStore.getState().sync["m-2"]).toBeUndefined();
    expect(fake.snapshotCalls).toHaveLength(0);
  });

  it("동기화 기록이 없는 작업의 mission.changed는 창 안에서 모아 요약 한 번으로 갱신한다(snapshot 없음)", async () => {
    vi.useFakeTimers();
    let open = 0;
    fake.listScript = () => {
      listCalls += 1;
      return { items: [summaryDto("m-2", "백그라운드 작업", { revision: String(listCalls + 1), open_decision_count: open })], next_cursor: null };
    };
    stop = startMissionSync(clientOf(fake));
    open = 1;
    fake.emit({ kind: "mission.changed", payload: { mission_id: "m-2", latest_seq: "3" } });
    fake.emit({ kind: "mission.changed", payload: { mission_id: "m-2", latest_seq: "4" } });
    fake.emit({ kind: "mission.changed", payload: { mission_id: "m-3", latest_seq: "9" } });
    expect(listCalls).toBe(0);
    await vi.advanceTimersByTimeAsync(SUMMARY_REFRESH_DELAY_MS);
    await vi.waitFor(() => {
      expect(useMissionStore.getState().missions["m-2"]).toMatchObject({ open_decision_count: 1 });
    });
    expect(listCalls).toBe(1);
    expect(fake.snapshotCalls).toHaveLength(0);
    expect(useMissionStore.getState().sync["m-2"]).toBeUndefined();
  });

  it("늦게 도착한 오래된 요약은 이미 적용된 snapshot mission을 되돌리지 않는다", async () => {
    stop = startMissionSync(clientOf(fake));
    await useMissionStore.getState().syncMission("m-1"); // snapshot mission revision "5"
    fake.listScript = () => ({ items: [summaryDto("m-1", "옛 제목", { revision: "4" })], next_cursor: null });
    await useMissionStore.getState().refreshSummaries();
    expect(useMissionStore.getState().missions["m-1"]).toMatchObject({ title: "로그인 기능", revision: "5" });
    fake.listScript = () => ({ items: [summaryDto("m-1", "새 제목", { revision: "6" })], next_cursor: null });
    await useMissionStore.getState().refreshSummaries();
    expect(useMissionStore.getState().missions["m-1"]).toMatchObject({ title: "새 제목", revision: "6" });
  });

  it("요약 조회가 진행 중일 때 들어온 갱신 요청은 흡수하지 않고 끝난 뒤 한 번 더 읽는다", async () => {
    stop = startMissionSync(clientOf(fake));
    const first = defer<MissionListResult>();
    const responses: Array<Promise<MissionListResult>> = [
      first.promise,
      Promise.resolve({ items: [summaryDto("m-2", "백그라운드 작업", { revision: "3", open_decision_count: 2 })], next_cursor: null }),
    ];
    fake.missionList = () => {
      listCalls += 1;
      return responses.shift() ?? Promise.resolve({ items: [], next_cursor: null });
    };
    const running = useMissionStore.getState().refreshSummaries();
    await flushMicrotasks();
    expect(listCalls).toBe(1);
    // 진행 중에 새 힌트(요청)가 두 번 들어와도 추가 조회는 한 번으로 합친다.
    const again = useMissionStore.getState().refreshSummaries();
    const againTwice = useMissionStore.getState().refreshSummaries();
    expect(again).toBe(running);
    expect(againTwice).toBe(running);
    expect(listCalls).toBe(1);
    // 첫 조회는 변경 전 요약을 돌려준다.
    first.resolve({ items: [summaryDto("m-2", "백그라운드 작업", { revision: "2", open_decision_count: 1 })], next_cursor: null });
    await expect(running).resolves.toBe(true);
    expect(listCalls).toBe(2);
    expect(useMissionStore.getState().missions["m-2"]).toMatchObject({ revision: "3", open_decision_count: 2 });
    // 다 끝난 뒤의 요청은 새 조회를 시작한다.
    await useMissionStore.getState().refreshSummaries();
    expect(listCalls).toBe(3);
  });

  it("요약 갱신 실패는 조용히 넘어간다(오류 상태·목록 로딩 표시를 건드리지 않는다)", async () => {
    stop = startMissionSync(clientOf(fake));
    fake.listScript = () => {
      throw new RpcClientError("DAEMON_UNAVAILABLE", "down");
    };
    await expect(useMissionStore.getState().refreshSummaries()).resolves.toBe(false);
    expect(useMissionStore.getState().listError).toBeNull();
    expect(useMissionStore.getState().listLoading).toBe(false);
  });

  it("해제한 뒤에는 예약된 요약 갱신도 부르지 않는다", async () => {
    vi.useFakeTimers();
    fake.listScript = () => {
      listCalls += 1;
      return { items: [], next_cursor: null };
    };
    stop = startMissionSync(clientOf(fake));
    fake.emit({ kind: "mission.changed", payload: { mission_id: "m-7", latest_seq: "2" } });
    stop();
    stop = null;
    await vi.advanceTimersByTimeAsync(SUMMARY_REFRESH_DELAY_MS * 2);
    expect(listCalls).toBe(0);
  });
});

function defer<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}
