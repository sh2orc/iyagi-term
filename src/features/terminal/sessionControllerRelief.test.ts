/**
 * 압력 완화 UI 배선(08-pressure-relief §2).
 *
 * - 데몬 요약의 relief/protected는 usage와 같은 자리에서 pane으로 내려오고,
 *   값이 그대로면 pane 상태를 갈아 끼우지 않는다(헤더가 초마다 다시 그려지지
 *   않게).
 * - pane 메뉴의 수동 액션은 그 pane의 세션 id로 session.relief를 부르고 응답을
 *   곧바로 pane에 반영한다. 실패는 다른 작업 동작과 같은 자리(toast)로 샌다.
 * - 정책 토글은 relief.set_policy의 응답값을 스토어의 정본으로 삼는다.
 */

import { afterEach, describe, expect, it } from "vitest";
import type { DaemonClient, DaemonEvent, DaemonEventListener } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import type { ReliefPolicyParams } from "../../generated/ReliefPolicyParams";
import type { SessionReliefParams } from "../../generated/SessionReliefParams";
import type { SessionReliefResult } from "../../generated/SessionReliefResult";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
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

async function flush(times = 8): Promise<void> {
  for (let i = 0; i < times; i++) await Promise.resolve();
}

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
    guard: { kind: "NONE" } as const,
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

interface Harness {
  controller: SessionController;
  emit(event: DaemonEvent): void;
  reliefCalls: SessionReliefParams[];
  policyCalls: ReliefPolicyParams[];
}

function harness(options: {
  relief?: (params: SessionReliefParams) => Promise<SessionReliefResult>;
  policy?: (params: ReliefPolicyParams) => Promise<{ auto_yield: boolean }>;
} = {}): Harness {
  const listeners = new Set<DaemonEventListener>();
  const reliefCalls: SessionReliefParams[] = [];
  const policyCalls: ReliefPolicyParams[] = [];
  const client = {
    events: {
      subscribe: (listener: DaemonEventListener) => {
        listeners.add(listener);
        return { dispose: () => void listeners.delete(listener) };
      },
    },
    // 복원 경로는 이 시험의 대상이 아니다 — 영원히 보류시켜 조용히 둔다.
    systemSnapshot: () => new Promise<never>(() => undefined),
    interventionList: async () => [],
    agentSessionList: async () => [],
    sessionFocus: async () => ({ focused_session_ids: [] }),
    sessionRelief: async (params: SessionReliefParams) => {
      reliefCalls.push(params);
      return options.relief
        ? await options.relief(params)
        : ({ relief: { kind: "NONE" }, protected: false } satisfies SessionReliefResult);
    },
    reliefSetPolicy: async (params: ReliefPolicyParams) => {
      policyCalls.push(params);
      return options.policy ? await options.policy(params) : { auto_yield: params.auto_yield };
    },
  } as unknown as DaemonClient;
  const controller = new SessionController({
    client,
    registry: new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom }),
    platform: "darwin",
  });
  controller.start();
  return {
    controller,
    emit: (event) => {
      for (const listener of [...listeners]) listener(event);
    },
    reliefCalls,
    policyCalls,
  };
}

let active: SessionController | null = null;
afterEach(() => {
  active?.dispose();
  active = null;
  useWorkbenchStore.setState({
    tabs: [],
    activeTabId: null,
    focusedLeafId: null,
    panes: {},
    workloads: [],
    toast: null,
    reliefPolicy: { auto_yield: true },
    schedulingYield: null,
  });
});

describe("SessionController relief mirror", () => {
  it("펼쳐진 요약의 relief/protected를 pane으로 내리고, 같은 값이면 다시 쓰지 않는다", () => {
    const h = harness();
    active = h.controller;
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() }, workloads: [workload()] });

    const yielded = workload({
      relief: { kind: "YIELDED", since_ms: "1000", manual: false, partial: true },
      protected: false,
    });
    h.emit({ kind: "workload.changed", payload: yielded });
    expect(useWorkbenchStore.getState().panes["leaf-1"].relief).toEqual({
      kind: "YIELDED",
      sinceMs: 1000,
      manual: false,
      partial: true,
    });

    // 같은 상태가 다시 와도 pane 객체는 그대로다(헤더 리렌더 없음).
    const panes = useWorkbenchStore.getState().panes;
    h.emit({ kind: "workload.changed", payload: { ...yielded } });
    expect(useWorkbenchStore.getState().panes).toBe(panes);

    // 보호 표시만 바뀌어도 반영한다.
    h.emit({ kind: "workload.changed", payload: { ...yielded, protected: true } });
    expect(useWorkbenchStore.getState().panes["leaf-1"].protected).toBe(true);
    expect(useWorkbenchStore.getState().panes).not.toBe(panes);
  });

  it("작업이 요약에서 사라지면 완화 표시도 지운다", () => {
    const h = harness();
    active = h.controller;
    useWorkbenchStore.setState({
      panes: {
        "leaf-1": pane({
          relief: { kind: "YIELDED", sinceMs: 1000, manual: true, partial: false },
          protected: true,
        }),
      },
      workloads: [],
    });

    h.emit({ kind: "workload.changed", payload: workload({ workload_id: "other", session_id: "s9" }) });
    expect(useWorkbenchStore.getState().panes["leaf-1"].relief).toEqual({ kind: "NONE" });
    expect(useWorkbenchStore.getState().panes["leaf-1"].protected).toBe(false);
  });
});

describe("SessionController manual relief", () => {
  it("pane의 세션으로 session.relief를 부르고 응답을 곧바로 pane에 반영한다", async () => {
    const h = harness({
      relief: async () => ({
        relief: { kind: "YIELDED", since_ms: "4200", manual: true, partial: false },
        protected: false,
      }),
    });
    active = h.controller;
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() }, workloads: [workload()] });

    await h.controller.paneRelief("leaf-1", "yield");
    expect(h.reliefCalls).toEqual([{ session_id: "s1", action: "yield" }]);
    expect(useWorkbenchStore.getState().panes["leaf-1"].relief).toEqual({
      kind: "YIELDED",
      sinceMs: 4200,
      manual: true,
      partial: false,
    });
  });

  it("세션이 없는 pane은 데몬에 물어볼 대상이 없어 아무것도 보내지 않는다", async () => {
    const h = harness();
    active = h.controller;
    useWorkbenchStore.setState({ panes: { "leaf-1": pane({ sessionId: null, phase: "exited" }) } });

    await h.controller.paneRelief("leaf-1", "restore");
    expect(h.reliefCalls).toEqual([]);
  });

  it("데몬이 거절하면 toast로 알리고 pane 상태는 건드리지 않는다", async () => {
    const h = harness({
      relief: async () => {
        throw new RpcClientError("INVALID_ARGUMENT", "unknown session");
      },
    });
    active = h.controller;
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() }, workloads: [workload()] });

    await h.controller.paneRelief("leaf-1", "protect");
    expect(useWorkbenchStore.getState().panes["leaf-1"].protected).toBe(false);
    expect(useWorkbenchStore.getState().toast).toContain("INVALID_ARGUMENT");
  });
});

describe("SessionController auto-yield policy", () => {
  it("relief.set_policy의 응답값을 스토어에 반영한다", async () => {
    const h = harness();
    active = h.controller;

    await h.controller.setAutoYield(false);
    expect(h.policyCalls).toEqual([{ auto_yield: false }]);
    expect(useWorkbenchStore.getState().reliefPolicy).toEqual({ auto_yield: false });
  });

  it("데몬이 요청과 다른 값을 돌려주면 그 값을 따른다", async () => {
    const h = harness({ policy: async () => ({ auto_yield: true }) });
    active = h.controller;

    await h.controller.setAutoYield(false);
    expect(useWorkbenchStore.getState().reliefPolicy).toEqual({ auto_yield: true });
  });

  it("실패하면 정책을 바꾸지 않고 toast로 알린다", async () => {
    const h = harness({
      policy: async () => {
        throw new RpcClientError("CAPABILITY_UNAVAILABLE", "no scheduling yield here");
      },
    });
    active = h.controller;

    await h.controller.setAutoYield(false);
    await flush();
    expect(useWorkbenchStore.getState().reliefPolicy).toEqual({ auto_yield: true });
    expect(useWorkbenchStore.getState().toast).toContain("CAPABILITY_UNAVAILABLE");
  });
});

describe("workbench store paneRelief", () => {
  it("값이 바뀔 때만 쓰고, 모르는 pane은 무시한다", () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() } });
    const store = useWorkbenchStore.getState();

    const before = useWorkbenchStore.getState().panes;
    store.paneRelief("leaf-1", { kind: "NONE" }, false);
    expect(useWorkbenchStore.getState().panes).toBe(before);

    store.paneRelief("leaf-1", { kind: "YIELDED", sinceMs: 7, manual: false, partial: false }, true);
    expect(useWorkbenchStore.getState().panes["leaf-1"]).toMatchObject({
      relief: { kind: "YIELDED", sinceMs: 7, manual: false, partial: false },
      protected: true,
    });

    // 같은 값을 다시 넣어도 새 객체를 만들지 않는다.
    const after = useWorkbenchStore.getState().panes;
    store.paneRelief("leaf-1", { kind: "YIELDED", sinceMs: 7, manual: false, partial: false }, true);
    expect(useWorkbenchStore.getState().panes).toBe(after);

    // partial만 달라도 다른 상태다.
    store.paneRelief("leaf-1", { kind: "YIELDED", sinceMs: 7, manual: false, partial: true }, true);
    expect(useWorkbenchStore.getState().panes).not.toBe(after);

    store.paneRelief("no-such-leaf", { kind: "NONE" }, false);
    expect(Object.keys(useWorkbenchStore.getState().panes)).toEqual(["leaf-1"]);
  });
});
