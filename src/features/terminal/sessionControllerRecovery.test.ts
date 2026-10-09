/**
 * 재생 막힘 회복(recoverStalledReplay)·수동 재시도(retryPane)·미룬 연결
 * 배수(flushDeferredAttaches)의 회귀 시험 — 어느 경로가 조용히 실패해도
 * pane이 "기록 재생 중…" 오버레이에 영원히 남지 않는지를 본다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import type { DaemonClient, DaemonEvent, DaemonEventListener } from "../daemon/client";
import { MockDaemonClient } from "../daemon/mockClient";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type RegistryDom, type TerminalLike } from "./registry";

async function flush(times = 16): Promise<void> {
  for (let i = 0; i < times; i++) await Promise.resolve();
}

function fakeTerminal(): TerminalLike {
  return {
    open: () => undefined,
    write: (_data, callback) => callback?.(),
    resize: () => undefined,
    reset: () => undefined,
    dispose: () => undefined,
    onData: () => ({ dispose: () => undefined }),
    hasSelection: () => false,
    getSelection: () => "",
    attachCustomKeyEventHandler: () => undefined,
    loadAddon: () => undefined,
    element: null,
  };
}

function fakeDom(): RegistryDom {
  return { className: "", parentElement: null, remove: () => undefined };
}

/** 시험이 건드리는 private 상태(맵·메서드) — 다른 시험 파일과 같은 방식의 cast. */
function internals(controller: SessionController): {
  pipelines: Map<string, { dispose(): void }>;
  sessionIndex: Map<string, { viewId: string; leafId: string }>;
  replayStallStrikes: Map<string, number>;
  deferredAttaches: Map<string, { leafId: string; sessionId: string; workloadId: string }>;
  attachDeferred: (item: { leafId: string; sessionId: string; workloadId: string }) => Promise<void>;
  flushDeferredAttaches: () => Promise<void>;
} {
  return controller as unknown as ReturnType<typeof internals>;
}

interface AttachReply {
  epoch: string;
  replay_from_seq: string;
  last_seq: string;
  cols: number;
  rows: number;
}

interface StallHarness {
  controller: SessionController;
  sessionUnsubscribe: ReturnType<typeof vi.fn>;
  attachCount: () => number;
  /** hangFrom 번째 attach부터 응답을 쥐고 있다가 releaseAttach로 풀어준다. */
  releaseAttach: (reply: AttachReply) => void;
  pendingAttachCount: () => number;
  /** 데몬 이벤트를 컨트롤러에 흘려 넣는다(`session.replay_required` 등). */
  emit: (event: DaemonEvent) => void;
}

/**
 * leaf-1(view-1)이 session-1에 붙어 저널 끝(last_seq 3)에 닿지 못하는 재생을
 * 하는 harness. 레코드를 하나도 흘려 주지 않아 재생은 곧 막히고, 와치독
 * 알림(onReplayStalled → recoverStalledReplay)이 반복된다.
 */
async function stalledHarness(options: { hangFrom?: number; lastSeq?: string } = {}): Promise<StallHarness> {
  vi.useFakeTimers();
  const baseSnapshot = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
  const listeners = new Set<DaemonEventListener>();
  const sessionUnsubscribe = vi.fn();
  let attachCount = 0;
  const pendingReplies: Array<(reply: AttachReply) => void> = [];
  const hangFrom = options.hangFrom ?? Number.POSITIVE_INFINITY;
  const client = {
    events: {
      subscribe(listener: DaemonEventListener) {
        listeners.add(listener);
        return { dispose: () => listeners.delete(listener) };
      },
    },
    systemSnapshot: async () => ({
      ...baseSnapshot,
      workloads: [{
        workload_id: "workload-1",
        session_id: "session-1",
        state: "RUNNING",
        last_error_code: null,
        title: "shell",
        cwd: "/tmp",
      }],
    }),
    interventionList: async () => [],
    sessionAttach: () => {
      attachCount += 1;
      const epoch = `epoch-${attachCount}`;
      const reply: AttachReply = {
        epoch,
        replay_from_seq: "1",
        last_seq: options.lastSeq ?? "3",
        cols: 80,
        rows: 24,
      };
      if (attachCount >= hangFrom) {
        return new Promise<AttachReply>((resolve) => pendingReplies.push(resolve));
      }
      return Promise.resolve(reply);
    },
    sessionResize: async () => ({ cols: 80, rows: 24 }),
    sessionAck: () => undefined,
    sessionFocus: async () => ({ focused_session_ids: [] }),
    sessionUnsubscribe,
  } as unknown as DaemonClient;
  const registry = new TerminalRegistry({
    createTerminal: () => fakeTerminal(),
    createDom: () => fakeDom(),
  });
  useWorkbenchStore.setState({
    tabs: [{
      kind: "terminal",
      id: "tab-1",
      title: "tab",
      root: { kind: "leaf", id: "leaf-1", view_id: "view-1", session_id: "session-1" },
    }],
    activeTabId: "tab-1",
    focusedLeafId: "leaf-1",
    panes: {
      "leaf-1": {
        leafId: "leaf-1",
        viewId: "view-1",
        sessionId: "session-1",
        workloadId: "workload-1",
        title: "shell",
        cwd: "/tmp",
        phase: "replaying",
        error: null,
        usage: null,
        flowBlocked: false,
      },
    },
    revision: 0,
  });
  const controller = new SessionController({ client, registry, platform: "darwin" });
  controller.start();
  await flush();
  return {
    controller,
    sessionUnsubscribe,
    attachCount: () => attachCount,
    releaseAttach: (reply) => {
      pendingReplies.shift()?.(reply);
    },
    pendingAttachCount: () => pendingReplies.length,
    emit: (event) => {
      for (const listener of [...listeners]) listener(event);
    },
  };
}

afterEach(() => {
  vi.useRealTimers();
  useWorkbenchStore.setState({ tabs: [], activeTabId: null, focusedLeafId: null, panes: {}, workloads: [], revision: 0 });
  vi.restoreAllMocks();
});

describe("SessionController replay stall recovery", () => {
  it("막힘이 거듭되면 세 번째 strike에서 실패로 끝내고 pipeline·세션 경로를 치운다", async () => {
    const h = await stalledHarness();
    try {
      // 초기 attach — 레코드가 오지 않아 재생이 끝나지 않는다.
      expect(h.attachCount()).toBe(1);
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("replaying");

      // 1차 막힘(t≈15s) → 재접속, 2차 막힘(t≈30s) → 재접속.
      await vi.advanceTimersByTimeAsync(20000);
      expect(h.attachCount()).toBe(2);
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("replaying");
      await vi.advanceTimersByTimeAsync(20000);
      expect(h.attachCount()).toBe(3);

      // 3차 막힘(t≈45s) → 실패 오버레이. 붙어 있던 pipeline과 데몬 쪽 view
      // 경로도 치운다(흐름 크레딧을 붙잡은 반쪽짜리 연결을 남기지 않는다).
      await vi.advanceTimersByTimeAsync(20000);
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.phase).toBe("failed");
      expect(pane.error).toBe(t("terminal.session.replayStalled"));
      const state = internals(h.controller);
      expect(state.pipelines.has("view-1")).toBe(false);
      expect(state.sessionIndex.has("session-1")).toBe(false);
      expect(state.replayStallStrikes.has("leaf-1")).toBe(false); // 수동 재시도는 깨끗하게 시작
      expect(h.sessionUnsubscribe).toHaveBeenCalledWith({ session_id: "session-1", view_id: "view-1" });
      expect(h.attachCount()).toBe(3); // 실패 뒤로는 더 재접속하지 않는다
    } finally {
      h.controller.dispose();
    }
  });

  it("재접속이 이미 진행 중일 때 온 막힘 알림은 strikes를 중복으로 올리지 않는다", async () => {
    // 초기 attach는 끝나고, 첫 막힘 회복 attach(2번째)는 응답이 묶인다.
    const h = await stalledHarness({ hangFrom: 2 });
    try {
      expect(h.attachCount()).toBe(1);
      // t≈15s 막힘 → strike 1 → 재접속 attach가 대기한다.
      await vi.advanceTimersByTimeAsync(16000);
      expect(h.attachCount()).toBe(2);
      expect(h.pendingAttachCount()).toBe(1);
      expect(internals(h.controller).replayStallStrikes.get("leaf-1")).toBe(1);

      // 와치독이 다시 알려도(5s마다) attach가 진행 중이다 — strike는 그대로.
      await vi.advanceTimersByTimeAsync(10000);
      expect(internals(h.controller).replayStallStrikes.get("leaf-1")).toBe(1);
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.phase).toBe("replaying");
      expect(pane.error).toBeNull();
      expect(h.sessionUnsubscribe).not.toHaveBeenCalled();
      expect(h.attachCount()).toBe(2);

      // 대기 중이던 attach가 끝나면(새 epoch) 다음 막힘부터 다시 센다.
      h.releaseAttach({ epoch: "epoch-2", replay_from_seq: "1", last_seq: "3", cols: 80, rows: 24 });
      await flush();
      await vi.advanceTimersByTimeAsync(20000);
      expect(internals(h.controller).replayStallStrikes.get("leaf-1")).toBe(2);
      expect(h.attachCount()).toBe(3);
    } finally {
      h.controller.dispose();
    }
  });

  it("pane이 살아 있는데 pipeline·경로를 잃은 막힘은 실패 오버레이로 끝낸다", async () => {
    const h = await stalledHarness();
    try {
      expect(h.attachCount()).toBe(1);
      // 세션 경로가 사라졌다(다른 창이 가로챈 등) — 예전에는 조용히 돌아가
      // "기록 재생 중…"에 영원히 남았다.
      internals(h.controller).sessionIndex.delete("session-1");
      await vi.advanceTimersByTimeAsync(20000);
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.phase).toBe("failed");
      expect(pane.error).toBe(t("terminal.session.replayStalled"));
      expect(h.attachCount()).toBe(1); // 재접속을 시도하지 않는다
    } finally {
      h.controller.dispose();
    }
  });
});

describe("SessionController retryPane", () => {
  it("재생 중(replaying) pane의 수동 재시도는 허용된다 — 떼어 내고 다시 붙인다", async () => {
    const sessionAttach = vi.fn(async () => ({
      epoch: "epoch-1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24,
    }));
    const sessionUnsubscribe = vi.fn();
    const client = {
      events: { subscribe: () => ({ dispose: () => undefined }) },
      sessionAttach,
      sessionResize: async () => ({ cols: 80, rows: 24 }),
      sessionAck: () => undefined,
      sessionUnsubscribe,
    } as unknown as DaemonClient;
    const registry = new TerminalRegistry({
      createTerminal: () => fakeTerminal(),
      createDom: () => fakeDom(),
    });
    useWorkbenchStore.setState({
      tabs: [{
        kind: "terminal",
        id: "tab-1",
        title: "tab",
        root: { kind: "leaf", id: "leaf-1", view_id: "view-1", session_id: "session-1" },
      }],
      activeTabId: "tab-1",
      focusedLeafId: "leaf-1",
      panes: {
        "leaf-1": {
          leafId: "leaf-1",
          viewId: "view-1",
          sessionId: "session-1",
          workloadId: "workload-1",
          title: "shell",
          cwd: "/tmp",
          phase: "replaying",
          error: null,
          usage: null,
          flowBlocked: false,
        },
      },
      workloads: [],
    });
    const controller = new SessionController({ client, registry, platform: "darwin" });
    const stub = { dispose: vi.fn() };
    internals(controller).pipelines.set("view-1", stub);
    try {
      controller.retryPane("leaf-1");

      // 막힌 재생을 푸는 탈출구: 옛 pipeline을 버리고 새 view로 다시 붙는다.
      expect(stub.dispose).toHaveBeenCalledTimes(1);
      expect(internals(controller).pipelines.has("view-1")).toBe(false);
      expect(sessionUnsubscribe).toHaveBeenCalledWith({ session_id: "session-1", view_id: "view-1" });
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.viewId).not.toBe("view-1");
      expect(pane.phase).toBe("replaying"); // 다시 붙는 중

      await flush();
      const reattached = useWorkbenchStore.getState().panes["leaf-1"];
      expect(sessionAttach).toHaveBeenCalledTimes(1);
      expect(reattached.phase).toBe("live"); // 빈 저널 — attach와 함께 live
      expect(internals(controller).pipelines.has(reattached.viewId)).toBe(true);
    } finally {
      controller.dispose();
    }
  });

  it("재생 중 다시 시도한 뒤 늦게 온 옛 attach의 실패가 새로 붙은 view를 실패로 덮지 않는다", async () => {
    let rejectFirst: ((error: Error) => void) | undefined;
    let attachCount = 0;
    const sessionAttach = vi.fn(() => {
      attachCount += 1;
      // 첫 시도는 응답이 없다가(막힌 재생) 나중에 시간 초과로 거절된다.
      if (attachCount === 1) return new Promise((_resolve, reject) => { rejectFirst = reject; });
      return Promise.resolve({ epoch: "epoch-2", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 });
    });
    const client = {
      events: { subscribe: () => ({ dispose: () => undefined }) },
      sessionAttach,
      sessionDetach: async () => ({ detached: true as const }),
      sessionResize: async () => ({ cols: 80, rows: 24 }),
      sessionAck: () => undefined,
      sessionUnsubscribe: () => undefined,
    } as unknown as DaemonClient;
    const registry = new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: () => fakeDom() });
    useWorkbenchStore.setState({
      tabs: [{
        kind: "terminal", id: "tab-1", title: "tab",
        root: { kind: "leaf", id: "leaf-1", view_id: "view-1", session_id: "session-1" },
      }],
      activeTabId: "tab-1",
      focusedLeafId: "leaf-1",
      panes: {
        "leaf-1": {
          leafId: "leaf-1", viewId: "view-1", sessionId: "session-1", workloadId: "workload-1",
          title: "shell", cwd: "/tmp", phase: "replaying", error: null, usage: null, flowBlocked: false,
        },
      },
      workloads: [],
    });
    const controller = new SessionController({ client, registry, platform: "darwin" });
    internals(controller).pipelines.set("view-1", { dispose: () => undefined });
    try {
      controller.retryPane("leaf-1");
      await flush();
      controller.retryPane("leaf-1"); // 첫 재시도의 재생이 막혀 한 번 더 누른다
      await flush();
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("live");

      rejectFirst!(new Error("session attach timed out"));
      await flush();
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.phase).toBe("live");
      expect(pane.error).toBeNull();
    } finally {
      controller.dispose();
    }
  });

  it("버리는 view는 데몬에서도 뗀다 — 재시도마다 세션 view 상한(2)을 먹지 않는다", async () => {
    // 컨트롤 연결이 살아 있는 동안 데몬은 view를 스스로 치우지 않는다. 떼지 않고
    // 새 view로만 붙으면 세 번째 재시도부터 INVALID_STATE("session already has the
    // maximum number of views")로 영영 붙지 못한다.
    const calls: string[] = [];
    const sessionAttach = vi.fn(async (params: { view_id: string }) => {
      calls.push(`attach:${params.view_id}`);
      return { epoch: "epoch-1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 };
    });
    const sessionDetach = vi.fn(async (params: { session_id: string; view_id: string }) => {
      calls.push(`detach:${params.view_id}`);
      return { detached: true as const };
    });
    const client = {
      events: { subscribe: () => ({ dispose: () => undefined }) },
      sessionAttach,
      sessionDetach,
      sessionResize: async () => ({ cols: 80, rows: 24 }),
      sessionAck: () => undefined,
      sessionUnsubscribe: () => undefined,
    } as unknown as DaemonClient;
    const registry = new TerminalRegistry({
      createTerminal: () => fakeTerminal(),
      createDom: () => fakeDom(),
    });
    useWorkbenchStore.setState({
      tabs: [{
        kind: "terminal", id: "tab-1", title: "tab",
        root: { kind: "leaf", id: "leaf-1", view_id: "view-1", session_id: "session-1" },
      }],
      activeTabId: "tab-1",
      focusedLeafId: "leaf-1",
      panes: {
        "leaf-1": {
          leafId: "leaf-1", viewId: "view-1", sessionId: "session-1", workloadId: "workload-1",
          title: "shell", cwd: "/tmp", phase: "replaying", error: null, usage: null, flowBlocked: false,
        },
      },
      workloads: [],
    });
    const controller = new SessionController({ client, registry, platform: "darwin" });
    internals(controller).pipelines.set("view-1", { dispose: () => undefined });
    try {
      controller.retryPane("leaf-1");
      await flush();
      const second = useWorkbenchStore.getState().panes["leaf-1"].viewId;
      controller.retryPane("leaf-1");
      await flush();
      const third = useWorkbenchStore.getState().panes["leaf-1"].viewId;

      // 매번 옛 view를 먼저 떼고 나서 새 view로 붙는다 — 데몬에는 항상 view 하나.
      expect(calls).toEqual([
        "detach:view-1", `attach:${second}`,
        `detach:${second}`, `attach:${third}`,
      ]);
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("live");
    } finally {
      controller.dispose();
    }
  });
});

describe("SessionController deferred attach drain", () => {
  it("미룬 연결 하나가 실패해도 나머지는 계속 붙는다", async () => {
    const registry = new TerminalRegistry({
      createTerminal: () => fakeTerminal(),
      createDom: () => fakeDom(),
    });
    const controller = new SessionController({
      client: {} as unknown as DaemonClient,
      registry,
      platform: "darwin",
    });
    const state = internals(controller);
    state.deferredAttaches.set("leaf-1", { leafId: "leaf-1", sessionId: "s1", workloadId: "w1" });
    state.deferredAttaches.set("leaf-2", { leafId: "leaf-2", sessionId: "s2", workloadId: "w2" });
    const attachDeferred = vi.fn()
      .mockRejectedValueOnce(new Error("attach blew up"))
      .mockResolvedValue(undefined);
    state.attachDeferred = attachDeferred;
    try {
      await state.flushDeferredAttaches();
      expect(attachDeferred).toHaveBeenCalledTimes(2);
      expect(state.deferredAttaches.size).toBe(0);
    } finally {
      controller.dispose();
    }
  });
});

describe("SessionController forced re-attach backoff", () => {
  const shed = (h: StallHarness) =>
    h.emit({
      kind: "session.replay_required",
      payload: { session_id: "session-1", view_id: "view-1", epoch: "stale", first_seq: "1" },
    });
  const phase = () => useWorkbenchStore.getState().panes["leaf-1"].phase;

  it("replay_required가 잇달으면 backoff로 합치고 상한을 넘기면 실패로 끝낸다", async () => {
    // 빈 저널(last_seq 0): 붙자마자 live라 막힘 감시가 끼어들지 않는다.
    const h = await stalledHarness({ lastSeq: "0" });
    try {
      expect(h.attachCount()).toBe(1);
      expect(phase()).toBe("live");

      // 첫 알림은 즉시 다시 붙는다.
      shed(h);
      await flush();
      expect(h.attachCount()).toBe(2);
      expect(phase()).toBe("live");

      // 잇단 알림: 1초 뒤 한 번만 — 그 사이 알림은 합쳐지고 pane은 재생 중으로 보인다.
      shed(h);
      shed(h);
      await flush();
      expect(h.attachCount()).toBe(2);
      expect(phase()).toBe("replaying");
      await vi.advanceTimersByTimeAsync(999);
      expect(h.attachCount()).toBe(2);
      await vi.advanceTimersByTimeAsync(1);
      await flush();
      expect(h.attachCount()).toBe(3);
      expect(phase()).toBe("live");

      // 2초 → 4초 → 8초 → 15초(상한)로 늘어난다.
      for (const delay of [2000, 4000, 8000, 15000]) {
        const before = h.attachCount();
        shed(h);
        await vi.advanceTimersByTimeAsync(delay - 1);
        expect(h.attachCount()).toBe(before);
        await vi.advanceTimersByTimeAsync(1);
        await flush();
        expect(h.attachCount()).toBe(before + 1);
        expect(phase()).toBe("live");
      }
      expect(h.attachCount()).toBe(7);

      // 한 창(60초) 안의 일곱 번째: 더 재생하지 않고 실패 오버레이로 끝낸다 —
      // pipeline과 데몬 쪽 view 경로도 치운다(수동 재시도는 깨끗하게 시작).
      shed(h);
      await vi.advanceTimersByTimeAsync(20000);
      await flush();
      expect(h.attachCount()).toBe(7);
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      expect(pane.phase).toBe("failed");
      expect(pane.error).toBe(t("terminal.session.replayLooping"));
      const state = internals(h.controller);
      expect(state.pipelines.has("view-1")).toBe(false);
      expect(state.sessionIndex.has("session-1")).toBe(false);
      expect(h.sessionUnsubscribe).toHaveBeenCalledWith({ session_id: "session-1", view_id: "view-1" });
    } finally {
      h.controller.dispose();
    }
  });

  it("live로 10초를 버티면 backoff 횟수가 지워져 다음 알림은 즉시 다시 붙는다", async () => {
    const h = await stalledHarness({ lastSeq: "0" });
    try {
      shed(h);
      await flush();
      shed(h);
      await vi.advanceTimersByTimeAsync(1000);
      await flush();
      expect(h.attachCount()).toBe(3);
      // 건강한 시간: 알림 없이 live 유지.
      await vi.advanceTimersByTimeAsync(10000);
      shed(h);
      await flush();
      expect(h.attachCount()).toBe(4); // backoff 없이 즉시
    } finally {
      h.controller.dispose();
    }
  });
});
