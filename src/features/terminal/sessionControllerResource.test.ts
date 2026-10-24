/**
 * 자원 계약 배선(SessionController):
 * - resource.snapshot 표본 — host·압력은 스토어와 렌더러 예산으로, 워크로드별 cpu·rss는
 *   요약을 거쳐 pane 배지까지(값이 같으면 아무것도 갈아 끼우지 않는다).
 * - 스냅샷 다시 받기 안전망 — 표본 revision이 앞서면 간격을 지켜 한 번만 다시 받고,
 *   받아 봐도 바뀐 게 없으면 간격을 늘린다. 창이 숨은 동안·첫 스냅샷 전에는 받지 않는다.
 * - 자원 정책 — 붙을 때 저장된 변경분을 다시 적용하고(복원이 기다린다), 설정의 변경은
 *   범위를 먼저 검사해 받아들여진 항목만 저장한다. 설정 전체 되돌리기는 데몬도 되돌린다.
 * - 자동 일시정지 알림 — 가드가 스스로 정지시킨 순간만 한 번, "보호"는 보호 + 재개.
 * - 전송 점검 — 창이 숨은 동안 10초에 한 번, 결과는 공유 소식으로.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DaemonClient, DaemonEvent, DaemonEventListener } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import { MockDaemonClient } from "../daemon/mockClient";
import type { GuardPolicy } from "../../generated/GuardPolicy";
import type { HostSample } from "../../generated/HostSample";
import type { Snapshot } from "../../generated/Snapshot";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { WorkloadUsageBrief } from "../../generated/WorkloadUsageBrief";
import { DEFAULT_PREFERENCES, usePreferences } from "../../store/preferences";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { useNotificationStore } from "../notifications/notificationStore";
import { resetTransportFeedForTests, subscribeTransportStatus } from "../daemon/transportFeed";
import { t } from "../../i18n";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type RegistryDom, type TerminalLike } from "./registry";

function fakeTerminal(): TerminalLike {
  return {
    open: () => undefined,
    write: () => undefined,
    resize: () => undefined,
    dispose: () => undefined,
    onData: () => ({ dispose: () => undefined }),
    hasSelection: () => false,
    getSelection: () => "",
    attachCustomKeyEventHandler: () => undefined,
    loadAddon: () => undefined,
    element: null,
    options: { fontSize: 13 },
  } satisfies TerminalLike;
}

function fakeDom(): RegistryDom {
  return { className: "", parentElement: null, remove: () => undefined };
}

async function flush(times = 12): Promise<void> {
  for (let i = 0; i < times; i++) await Promise.resolve();
}

let baseSnapshot: Snapshot;
beforeEach(async () => {
  baseSnapshot = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
});

const DEFAULT_GUARD: GuardPolicy = {
  auto_suspend: true,
  cpu_cores_limit: 6,
  rss_limit_bytes: String(8 * 1024 ** 3),
  sustain_ms: "20000",
  rss_sustain_ms: "5000",
  auto_resume: false,
};

function workload(overrides: Partial<WorkloadSummary> = {}): WorkloadSummary {
  return {
    workload_id: "w1",
    session_id: "s1",
    mode: "shell",
    state: "RUNNING",
    priority: 1,
    title: "zsh",
    cwd: "/work",
    program: "/bin/zsh",
    reservation_bytes: "0",
    cpu_slots: 1,
    enforcement: "observe",
    root_exited: false,
    cancel_requested: false,
    connection: "attached",
    relief: { kind: "NONE" },
    guard: { kind: "NONE" },
    guard_warning: null,
    protected: false,
    ...overrides,
  };
}

function pane(overrides: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: "leaf-1",
    viewId: "view-1",
    sessionId: "s1",
    workloadId: "w1",
    title: "zsh",
    cwd: "/work",
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    relief: { kind: "NONE" },
    protected: false,
    ...overrides,
  };
}

function brief(workloadId: string, cpu: number, rss: string): WorkloadUsageBrief {
  return {
    workload_id: workloadId,
    cpu_cores: { value: cpu, source: "sysinfo", quality: "measured", reason: null },
    resident_bytes: { value: rss, source: "sysinfo", quality: "measured", reason: null },
  };
}

function sample(revision: number, usage: WorkloadUsageBrief[] = [], host: Partial<HostSample> = {}): DaemonEvent {
  const merged = { ...baseSnapshot.host, ...host };
  return {
    kind: "resource.snapshot",
    payload: { revision, host: merged, pressure: merged.pressure, cpu_pressure: merged.cpu_pressure, usage },
  };
}

interface HarnessOptions {
  snapshot?: () => Snapshot | Promise<Snapshot>;
  guardSetPolicy?: (policy: GuardPolicy) => Promise<GuardPolicy>;
  reliefSetPolicy?: (autoYield: boolean) => Promise<{ auto_yield: boolean }>;
  transport?: {
    status: () => Promise<{ controlAlive: boolean; dataAlive: boolean; generation: number; daemonOutdated: boolean }>;
  };
  start?: boolean;
}

function harness(options: HarnessOptions = {}) {
  const listeners = new Set<DaemonEventListener>();
  const calls = {
    snapshot: 0,
    guard: [] as GuardPolicy[],
    relief: [] as boolean[],
    reliefActions: [] as Array<{ session_id: string; action: string }>,
    resume: [] as string[],
    transport: 0,
  };
  const client = {
    events: {
      subscribe: (listener: DaemonEventListener) => {
        listeners.add(listener);
        return { dispose: () => void listeners.delete(listener) };
      },
    },
    systemSnapshot: async () => {
      calls.snapshot += 1;
      return options.snapshot ? options.snapshot() : { ...baseSnapshot, workloads: [], queue: [] };
    },
    interventionList: async () => [],
    agentSessionList: async () => [],
    sessionFocus: async () => ({ focused_session_ids: [] }),
    gatewaySetPolicy: async () => ({ enabled: false, blocked: false, pool_strategy: "round_robin" }),
    guardSetPolicy: async ({ policy }: { policy: GuardPolicy }) => {
      calls.guard.push(policy);
      return options.guardSetPolicy ? options.guardSetPolicy(policy) : { ...policy };
    },
    reliefSetPolicy: async ({ auto_yield }: { auto_yield: boolean }) => {
      calls.relief.push(auto_yield);
      return options.reliefSetPolicy ? options.reliefSetPolicy(auto_yield) : { auto_yield };
    },
    sessionRelief: async (params: { session_id: string; action: string }) => {
      calls.reliefActions.push(params);
      return { relief: { kind: "NONE" }, protected: params.action === "protect" };
    },
    workloadResume: async ({ workload_id }: { workload_id: string }) => {
      calls.resume.push(workload_id);
      return { guard: { kind: "NONE" } };
    },
    sessionAttach: async () => {
      throw new Error("no transport in this test");
    },
    ...(options.transport
      ? {
          transportStatus: async () => {
            calls.transport += 1;
            return options.transport!.status();
          },
          reconnectTransport: async () => undefined,
        }
      : {}),
  } as unknown as DaemonClient;
  const registry = new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom });
  const controller = new SessionController({ client, registry, platform: "darwin" });
  if (options.start !== false) controller.start();
  return {
    controller,
    registry,
    calls,
    emit: (event: DaemonEvent) => {
      for (const listener of [...listeners]) listener(event);
    },
  };
}

let active: SessionController | null = null;
afterEach(() => {
  active?.dispose();
  active = null;
  vi.useRealTimers();
  vi.unstubAllGlobals();
  resetTransportFeedForTests();
  usePreferences.setState({ ...DEFAULT_PREFERENCES });
  useNotificationStore.getState().clear();
  useWorkbenchStore.setState({
    tabs: [],
    activeTabId: null,
    focusedLeafId: null,
    panes: {},
    workloads: [],
    workloadById: new Map(),
    workloadBySession: new Map(),
    queue: [],
    host: null,
    revision: 0,
    toast: null,
    page: "terminal",
    reliefPolicy: { auto_yield: true },
    guardPolicy: null,
    guardPolicyDefault: null,
  });
});

describe("resource.snapshot 표본", () => {
  it("host·압력은 스토어와 렌더러 예산으로, 워크로드별 사용량은 요약을 거쳐 pane 배지까지 내린다", () => {
    const h = harness({ snapshot: () => new Promise<never>(() => undefined) });
    active = h.controller;
    const pressure = vi.spyOn(h.registry, "setMemoryPressure");
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() } });
    useWorkbenchStore.getState().upsertWorkload(workload({ usage: null }));

    h.emit(sample(0, [brief("w1", 1.5, "1048576"), brief("unknown", 9, "1")], { pressure: "WARNING" }));
    const state = useWorkbenchStore.getState();
    expect(state.host?.pressure).toBe("WARNING");
    expect(pressure).toHaveBeenLastCalledWith("WARNING");
    expect(state.workloadById.get("w1")?.usage?.cpu_cores.value).toBe(1.5);
    expect(state.workloadById.has("unknown")).toBe(false);
    expect(state.panes["leaf-1"].usage).toEqual({ cpuCores: 1.5, residentBytes: 1048576 });

    // 같은 값이 다시 오면 요약도 pane도 갈아 끼우지 않는다(헤더·탭이 다시 그려지지 않게).
    const { workloads, panes } = useWorkbenchStore.getState();
    h.emit(sample(0, [brief("w1", 1.5, "1048576")]));
    expect(useWorkbenchStore.getState().workloads).toBe(workloads);
    expect(useWorkbenchStore.getState().panes).toBe(panes);

    h.emit(sample(0, [brief("w1", 0.25, "2097152")]));
    expect(useWorkbenchStore.getState().panes["leaf-1"].usage).toEqual({ cpuCores: 0.25, residentBytes: 2097152 });
  });
});

describe("스냅샷 다시 받기 안전망", () => {
  it("표본 revision이 앞서면 간격을 지켜 한 번만 다시 받고, 바뀐 게 없으면 간격을 늘리며, 바뀐 것을 찾으면 되돌린다", async () => {
    vi.useFakeTimers();
    let revision = 10;
    let workloads: WorkloadSummary[] = [workload()];
    const h = harness({ snapshot: () => ({ ...baseSnapshot, revision, workloads: workloads.map((w) => ({ ...w })), queue: [] }) });
    active = h.controller;
    await h.controller.restoreWorkspace();
    expect(useWorkbenchStore.getState().revision).toBe(10);
    const restored = h.calls.snapshot;

    // 같은 revision은 아무것도 하지 않는다.
    h.emit(sample(10));
    await vi.advanceTimersByTimeAsync(10_000);
    expect(h.calls.snapshot).toBe(restored);

    // 앞선 revision: 곧바로 한 번(겹친 표본은 합친다).
    revision = 11;
    h.emit(sample(11));
    h.emit(sample(11));
    await vi.advanceTimersByTimeAsync(0);
    expect(h.calls.snapshot).toBe(restored + 1);

    // 바뀐 게 없었다 → 다음은 3초가 아니라 6초 간격.
    revision = 12;
    h.emit(sample(12));
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.calls.snapshot).toBe(restored + 1);
    h.emit(sample(12));
    await vi.advanceTimersByTimeAsync(1100);
    expect(h.calls.snapshot).toBe(restored + 2);

    // 이번에는 구 데몬처럼 이벤트 없이 자동 정지가 일어났다 — 다시 받은 스냅샷이 알아챈다.
    revision = 13;
    workloads = [workload({ guard: { kind: "SUSPENDED", since_ms: "77", reason: "memory_limit", manual: false, partial: false } })];
    h.emit(sample(13));
    await vi.advanceTimersByTimeAsync(12_100);
    expect(h.calls.snapshot).toBe(restored + 3);
    expect(useWorkbenchStore.getState().workloadById.get("w1")?.guard.kind).toBe("SUSPENDED");
    expect(useNotificationStore.getState().items.map((item) => item.id)).toEqual(["guard:w1:77"]);

    // 바뀐 것을 찾았으니 간격은 다시 3초다.
    revision = 14;
    h.emit(sample(14));
    await vi.advanceTimersByTimeAsync(3100);
    expect(h.calls.snapshot).toBe(restored + 4);
  });

  it("첫 스냅샷 전(복원 중)과 창이 숨은 동안에는 다시 받지 않는다", async () => {
    vi.useFakeTimers();
    const fakeDocument = { hidden: false, addEventListener: () => undefined, removeEventListener: () => undefined };
    vi.stubGlobal("document", fakeDocument);
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const h = harness({
      snapshot: async () => {
        await gate;
        return { ...baseSnapshot, revision: 5, workloads: [], queue: [] };
      },
    });
    active = h.controller;
    // 복원이 아직 첫 스냅샷을 기다린다(스토어 revision 0) — 앞선 표본이 와도 따로 받지 않는다.
    h.emit(sample(50));
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.calls.snapshot).toBe(1);

    release();
    await h.controller.restoreWorkspace();
    const restored = h.calls.snapshot;
    fakeDocument.hidden = true;
    h.emit(sample(6));
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.calls.snapshot).toBe(restored);
    fakeDocument.hidden = false;
    h.emit(sample(6));
    await vi.advanceTimersByTimeAsync(0);
    expect(h.calls.snapshot).toBe(restored + 1);
  });
});

describe("자원 정책 재적용", () => {
  it("저장된 변경분은 붙을 때 지금 정책 위에 얹어 전체로 보내고, 복원은 그 적용을 기다려 스토어에 반영한다", async () => {
    usePreferences.setState({ resourceGuardOverrides: { cpu_cores_limit: 3 }, reliefAutoYield: false });
    const h = harness({
      snapshot: () => ({ ...baseSnapshot, revision: 3, guard_policy: { ...DEFAULT_GUARD }, guard_policy_default: { ...DEFAULT_GUARD }, workloads: [], queue: [] }),
    });
    active = h.controller;
    await h.controller.restoreWorkspace();
    expect(h.calls.guard).toEqual([{ ...DEFAULT_GUARD, cpu_cores_limit: 3 }]);
    expect(h.calls.relief).toEqual([false]);
    // 복원이 쓴 스냅샷은 적용 전 값이었지만 적용된 값으로 덧댔다.
    expect(useWorkbenchStore.getState().guardPolicy?.cpu_cores_limit).toBe(3);
    expect(useWorkbenchStore.getState().guardPolicyDefault).toEqual(DEFAULT_GUARD);
    expect(useWorkbenchStore.getState().reliefPolicy).toEqual({ auto_yield: false });
  });

  it("변경분이 없으면 보내지 않고, 데몬이 이미 같은 값이면 guard.set_policy를 생략한다", async () => {
    const h = harness({ snapshot: () => ({ ...baseSnapshot, guard_policy: { ...DEFAULT_GUARD, cpu_cores_limit: 3 }, workloads: [], queue: [] }) });
    active = h.controller;
    await h.controller.restoreWorkspace();
    expect(h.calls.guard).toEqual([]);
    expect(h.calls.relief).toEqual([]);
    h.controller.dispose();

    usePreferences.setState({ resourceGuardOverrides: { cpu_cores_limit: 3 } });
    const again = harness({ snapshot: () => ({ ...baseSnapshot, guard_policy: { ...DEFAULT_GUARD, cpu_cores_limit: 3 }, workloads: [], queue: [] }) });
    active = again.controller;
    await again.controller.restoreWorkspace();
    expect(again.calls.guard).toEqual([]);
    expect(useWorkbenchStore.getState().guardPolicy?.cpu_cores_limit).toBe(3);
  });

  it("구 데몬이 거절해도 복원은 계속된다(실패는 삼킨다)", async () => {
    usePreferences.setState({ resourceGuardOverrides: { auto_resume: true }, reliefAutoYield: false });
    const h = harness({
      guardSetPolicy: async () => {
        throw new RpcClientError("INVALID_ARGUMENT", "unknown method \"guard.set_policy\"");
      },
      reliefSetPolicy: async () => {
        throw new RpcClientError("INVALID_ARGUMENT", "unknown method");
      },
    });
    active = h.controller;
    await expect(h.controller.restoreWorkspace()).resolves.toBeUndefined();
    expect(h.calls.guard).toHaveLength(1);
    expect(useWorkbenchStore.getState().toast).toBeNull();
  });

  it("데몬 세대가 바뀌면(재시작) 붙기 전에 다시 적용한다", async () => {
    vi.useFakeTimers();
    usePreferences.setState({ resourceGuardOverrides: { sustain_ms: "30000" } });
    let generation = 1;
    const h = harness({
      snapshot: () => ({ ...baseSnapshot, guard_policy: { ...DEFAULT_GUARD }, workloads: [], queue: [] }),
      transport: { status: async () => ({ controlAlive: true, dataAlive: true, generation, daemonOutdated: false }) },
    });
    active = h.controller;
    await h.controller.restoreWorkspace();
    await vi.advanceTimersByTimeAsync(2000);
    expect(h.calls.guard).toHaveLength(1);
    generation = 2;
    await vi.advanceTimersByTimeAsync(2000);
    expect(h.calls.guard).toHaveLength(2);
    expect(h.calls.guard[1]).toEqual({ ...DEFAULT_GUARD, sustain_ms: "30000" });
  });

  it("설정의 변경은 범위를 먼저 검사하고, 받아들여진 항목만(기본값과 다를 때) 변경분으로 저장한다", async () => {
    const h = harness({ start: false });
    active = h.controller;
    useWorkbenchStore.setState({ guardPolicy: { ...DEFAULT_GUARD }, guardPolicyDefault: { ...DEFAULT_GUARD } });

    await expect(h.controller.setGuardPolicy({ cpu_cores_limit: 0 })).rejects.toThrow(t("settings.resources.error.cpuCores"));
    await expect(h.controller.setGuardPolicy({ rss_limit_bytes: "1024" })).rejects.toThrow(t("settings.resources.error.rssLimit"));
    expect(h.calls.guard).toEqual([]);

    await h.controller.setGuardPolicy({ cpu_cores_limit: 4 });
    expect(h.calls.guard).toEqual([{ ...DEFAULT_GUARD, cpu_cores_limit: 4 }]);
    expect(useWorkbenchStore.getState().guardPolicy?.cpu_cores_limit).toBe(4);
    expect(usePreferences.getState().resourceGuardOverrides).toEqual({ cpu_cores_limit: 4 });

    await h.controller.setGuardPolicy({ rss_sustain_ms: "8000" });
    expect(h.calls.guard.at(-1)).toEqual({ ...DEFAULT_GUARD, cpu_cores_limit: 4, rss_sustain_ms: "8000" });
    expect(usePreferences.getState().resourceGuardOverrides).toEqual({ cpu_cores_limit: 4, rss_sustain_ms: "8000" });

    // 기본값과 같아진 항목은 지운다 — 호스트 기본값이 바뀌면 따라가게.
    await h.controller.setGuardPolicy({ cpu_cores_limit: 6 });
    expect(usePreferences.getState().resourceGuardOverrides).toEqual({ rss_sustain_ms: "8000" });

    // 데몬이 거절하면 저장하지 않고 그대로 던진다.
    const rejecting = harness({
      start: false,
      guardSetPolicy: async () => {
        throw new RpcClientError("INVALID_ARGUMENT", "rss_limit_bytes out of range");
      },
    });
    await expect(rejecting.controller.setGuardPolicy({ auto_resume: true })).rejects.toBeInstanceOf(RpcClientError);
    expect(usePreferences.getState().resourceGuardOverrides).toEqual({ rss_sustain_ms: "8000" });
    rejecting.controller.dispose();
  });

  it("기본값으로: 호스트 기본값으로 보내고 변경분에서 지운다", async () => {
    const h = harness({ start: false });
    active = h.controller;
    useWorkbenchStore.setState({
      guardPolicy: { ...DEFAULT_GUARD, cpu_cores_limit: 2, auto_resume: true },
      guardPolicyDefault: { ...DEFAULT_GUARD },
    });
    usePreferences.setState({ resourceGuardOverrides: { cpu_cores_limit: 2, auto_resume: true } });

    await h.controller.resetGuardPolicy(["cpu_cores_limit"]);
    expect(h.calls.guard.at(-1)).toEqual({ ...DEFAULT_GUARD, auto_resume: true });
    expect(usePreferences.getState().resourceGuardOverrides).toEqual({ auto_resume: true });

    await h.controller.resetGuardPolicy();
    expect(h.calls.guard.at(-1)).toEqual(DEFAULT_GUARD);
    expect(usePreferences.getState().resourceGuardOverrides).toEqual({});
  });

  it("대기열의 자동 양보 토글도 변경분으로 저장하고, 기본값(켬)으로 돌리면 지운다", async () => {
    const h = harness({ start: false });
    active = h.controller;
    await h.controller.setAutoYield(false);
    expect(usePreferences.getState().reliefAutoYield).toBe(false);
    await h.controller.setAutoYield(true);
    expect(usePreferences.getState().reliefAutoYield).toBeNull();
    expect(h.calls.relief).toEqual([false, true]);
  });

  it("설정 전체 되돌리기로 변경분이 사라지면 데몬도 기본값으로 맞춘다", async () => {
    usePreferences.setState({ resourceGuardOverrides: { cpu_cores_limit: 2 }, reliefAutoYield: false });
    const h = harness({
      snapshot: () => ({ ...baseSnapshot, guard_policy: { ...DEFAULT_GUARD }, guard_policy_default: { ...DEFAULT_GUARD }, workloads: [], queue: [] }),
    });
    active = h.controller;
    await h.controller.restoreWorkspace();
    expect(useWorkbenchStore.getState().guardPolicy?.cpu_cores_limit).toBe(2);

    usePreferences.getState().resetAll();
    await flush();
    expect(h.calls.guard.at(-1)).toEqual(DEFAULT_GUARD);
    expect(h.calls.relief.at(-1)).toBe(true);
    expect(useWorkbenchStore.getState().guardPolicy).toEqual(DEFAULT_GUARD);
    expect(useWorkbenchStore.getState().reliefPolicy).toEqual({ auto_yield: true });
  });
});

describe("자동 일시정지 알림", () => {
  const suspended = (since: string, manual = false): WorkloadSummary["guard"] =>
    ({ kind: "SUSPENDED", since_ms: since, reason: "host_memory_pressure", manual, partial: false });

  it("가드가 스스로 정지시킨 순간만 한 번 알리고, 수동 정지는 알리지 않는다", async () => {
    const h = harness({ snapshot: () => ({ ...baseSnapshot, workloads: [workload()], queue: [] }) });
    active = h.controller;
    await h.controller.restoreWorkspace();
    useWorkbenchStore.setState({ panes: { "leaf-1": pane({ terminalTitle: "build" }) } });

    h.emit({ kind: "workload.changed", payload: workload({ guard: suspended("100") }) });
    h.emit({ kind: "workload.changed", payload: workload({ guard: suspended("100") }) });
    const items = useNotificationStore.getState().items;
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      id: "guard:w1:100",
      kind: "resource",
      workloadId: "w1",
      sessionId: "s1",
      title: t("notifications.resource.suspended", { title: "build" }),
      detail: t("queue.guard.reason.host_memory_pressure"),
      acknowledged: false,
    });

    // 풀렸다가 다시 정지되면 새 정지로 알린다. 수동 정지는 알리지 않는다.
    h.emit({ kind: "workload.changed", payload: workload() });
    h.emit({ kind: "workload.changed", payload: workload({ guard: suspended("200") }) });
    h.emit({ kind: "workload.changed", payload: workload({ workload_id: "w2", session_id: "s2", guard: suspended("300", true) }) });
    expect(useNotificationStore.getState().items.map((item) => item.id)).toEqual(["guard:w1:200", "guard:w1:100"]);
  });

  it("복원 때 이미 정지돼 있던 작업은 기준으로만 삼는다", async () => {
    const h = harness({ snapshot: () => ({ ...baseSnapshot, workloads: [workload({ guard: suspended("50") })], queue: [] }) });
    active = h.controller;
    await h.controller.restoreWorkspace();
    h.emit({ kind: "workload.changed", payload: workload({ guard: suspended("50") }) });
    expect(useNotificationStore.getState().items).toEqual([]);
  });

  it("보호는 자동 완화·정지에서 빼고(session.relief protect) 정지돼 있으면 재개한다", async () => {
    const h = harness({ start: false });
    active = h.controller;
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() } });
    useWorkbenchStore.getState().upsertWorkload(workload({ guard: suspended("10") }));

    await h.controller.protectWorkload("w1");
    expect(h.calls.reliefActions).toEqual([{ session_id: "s1", action: "protect" }]);
    expect(h.calls.resume).toEqual(["w1"]);
    expect(useWorkbenchStore.getState().panes["leaf-1"].protected).toBe(true);

    // 정지돼 있지 않으면 보호만 한다.
    useWorkbenchStore.getState().upsertWorkload(workload());
    await h.controller.protectWorkload("w1");
    expect(h.calls.resume).toEqual(["w1"]);
  });
});

describe("전송 점검", () => {
  it("창이 숨은 동안은 10초에 한 번만 점검하고, 돌아오면 곧바로 점검하며, 결과를 공유 소식으로 알린다", async () => {
    vi.useFakeTimers();
    const listeners = new Set<() => void>();
    const fakeDocument = {
      hidden: false,
      addEventListener: (_type: string, listener: () => void) => listeners.add(listener),
      removeEventListener: (_type: string, listener: () => void) => listeners.delete(listener),
    };
    vi.stubGlobal("document", fakeDocument);
    const observed: Array<{ connected: boolean; generation: number | null }> = [];
    subscribeTransportStatus((observation) => observed.push(observation));
    const h = harness({
      transport: { status: async () => ({ controlAlive: true, dataAlive: true, generation: 4, daemonOutdated: false }) },
    });
    active = h.controller;
    await vi.advanceTimersByTimeAsync(4000);
    expect(h.calls.transport).toBe(2);
    expect(observed.at(-1)).toEqual({ connected: true, generation: 4 });

    fakeDocument.hidden = true;
    for (const listener of listeners) listener();
    await vi.advanceTimersByTimeAsync(8000);
    expect(h.calls.transport).toBe(2);
    await vi.advanceTimersByTimeAsync(4000);
    expect(h.calls.transport).toBe(3);

    fakeDocument.hidden = false;
    for (const listener of listeners) listener();
    await vi.advanceTimersByTimeAsync(0);
    expect(h.calls.transport).toBe(4);
  });
});
