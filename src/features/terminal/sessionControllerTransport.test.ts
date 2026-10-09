import { afterEach, describe, expect, it, vi } from "vitest";
import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import type { AttachParams } from "../../generated/AttachParams";
import type { DaemonClient, DaemonEvent, DaemonEventListener } from "../daemon/client";
import { MockDaemonClient } from "../daemon/mockClient";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type RegistryDom, type TerminalLike } from "./registry";

async function flush(times = 1): Promise<void> {
  for (let i = 0; i < 8 * times; i++) await Promise.resolve();
}

function terminalLike(): TerminalLike {
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

describe("SessionController transport recovery", () => {
  afterEach(() => vi.useRealTimers());

  it("연결이 끊겼다 돌아와도 복구가 시작되기 전에는 pane을 미리 replaying으로 끄지 않는다", async () => {
    vi.useFakeTimers();
    const baseSnapshot = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
    let healthy = true;
    let generation = 0;
    let releaseReconnect: (() => void) | undefined;
    const reconnectTransport = vi.fn(() => new Promise<void>((resolve) => {
      releaseReconnect = () => {
        healthy = true;
        generation += 1;
        resolve();
      };
    }));
    let attachCount = 0;
    const client = {
      events: { subscribe: () => ({ dispose: () => undefined }) },
      systemSnapshot: async () => ({
        ...baseSnapshot,
        workloads: [
          { workload_id: "workload-1", session_id: "session-1", state: "RUNNING", last_error_code: null, title: "shell", cwd: "/tmp" },
          { workload_id: "workload-2", session_id: "session-2", state: "RUNNING", last_error_code: null, title: "shell", cwd: "/tmp" },
        ],
      }),
      interventionList: async () => [],
      // 빈 저널(last_seq 0) — attach가 끝나면 곧바로 live가 된다.
      sessionAttach: async () => {
        attachCount += 1;
        return { epoch: `epoch-${attachCount}`, replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 };
      },
      sessionResize: async () => ({ cols: 80, rows: 24 }),
      sessionAck: () => undefined,
      sessionFocus: async () => ({ focused_session_ids: [] }),
      transportStatus: async () => ({ controlAlive: healthy, dataAlive: healthy, generation }),
      reconnectTransport,
    } as unknown as DaemonClient;
    const registry = new TerminalRegistry({
      createTerminal: () => terminalLike(),
      createDom: () => ({ className: "", parentElement: null, remove: () => undefined } satisfies RegistryDom),
    });
    const pane = (leafId: string, viewId: string, sessionId: string, workloadId: string) => ({
      leafId,
      viewId,
      sessionId,
      workloadId,
      title: "shell",
      cwd: "/tmp",
      phase: "replaying" as const,
      error: null,
      usage: null,
      flowBlocked: false,
    });
    useWorkbenchStore.setState({
      tabs: [
        { kind: "terminal", id: "tab-1", title: "tab1", root: { kind: "leaf", id: "leaf-1", view_id: "view-1", session_id: "session-1" } },
        { kind: "terminal", id: "tab-2", title: "tab2", root: { kind: "leaf", id: "leaf-2", view_id: "view-2", session_id: "session-2" } },
      ],
      activeTabId: "tab-1",
      focusedLeafId: "leaf-1",
      panes: {
        "leaf-1": pane("leaf-1", "view-1", "session-1", "workload-1"),
        "leaf-2": pane("leaf-2", "view-2", "session-2", "workload-2"),
      },
      revision: 0,
    });
    const controller = new SessionController({ client, registry, platform: "darwin" });

    try {
      controller.start();
      await flush(4);
      expect(attachCount).toBe(2);
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("live");
      expect(useWorkbenchStore.getState().panes["leaf-2"].phase).toBe("live");

      // 첫 와치독 턴: 건전한 generation 기준을 세운다.
      await vi.advanceTimersByTimeAsync(2000);
      healthy = false;
      await vi.advanceTimersByTimeAsync(2000);

      // 재접속이 대기 중이다 — 아직 복구가 시작되지 않았으므로 어떤 pane도
      // 미리 "replaying"으로 꺼지지 않는다(한 번에 전부 깜빡이는 회귀 방지).
      expect(reconnectTransport).toHaveBeenCalledTimes(1);
      expect(attachCount).toBe(2);
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("live");
      expect(useWorkbenchStore.getState().panes["leaf-2"].phase).toBe("live");

      // 재접속이 끝나면 pane마다 다시 붙어 live로 돌아간다.
      releaseReconnect!();
      await flush(32);
      expect(attachCount).toBe(4);
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("live");
      expect(useWorkbenchStore.getState().panes["leaf-2"].phase).toBe("live");
    } finally {
      controller.dispose();
    }
  });

  it.each(["claude", "codex", "opencode"].flatMap(agent =>
    ["pipe", "restart", "exit", "replace", "close", "disposed"].map(cause => ({ agent, cause }))
  ))("$agent recovery: $cause", async ({ agent, cause }) => {
    const restarted = cause === "restart";
    const delayed = ["replace", "close", "disposed"].includes(cause);
    vi.useFakeTimers();
    const baseSnapshot = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
    const listeners = new Set<DaemonEventListener>();
    let attachCount = 0;
    let generation = 0;
    let healthy = true;
    const reconnectTransport = vi.fn(async () => {
      healthy = true;
      generation += 1;
    });
    const record = {
      id: "record-1", workload_id: "workload-1", pty_session_id: "session-1",
      agent, agent_session_id: "known-session-1", cwd: "/tmp", program: `/opt/bin/${agent}`, active: false,
    } as AgentSessionRecord;
    let resolveList: ((records: AgentSessionRecord[]) => void) | undefined;
    const agentSessionList = vi.fn(() => delayed
      ? new Promise<AgentSessionRecord[]>(resolve => { resolveList = resolve; })
      : Promise.resolve([record]));
    const workloadLaunch = vi.fn();
    const client = {
      events: {
        subscribe(listener: DaemonEventListener) {
          listeners.add(listener);
          return { dispose: () => listeners.delete(listener) };
        },
      },
      systemSnapshot: async () => ({
        ...baseSnapshot,
        revision: generation ? 1 : 100,
        workloads: [{
          workload_id: "workload-1",
          session_id: "session-1",
          state: restarted && generation ? "INTERRUPTED" : "RUNNING",
          last_error_code: restarted && generation ? "DAEMON_RESTART" : null,
          title: "shell",
          cwd: "/tmp",
        }],
      }),
      interventionList: async () => [],
      agentSessionList,
      workloadLaunch,
      sessionAttach: async (_params: AttachParams) => {
        attachCount += 1;
        const epoch = `epoch-${attachCount}`;
        queueMicrotask(() => {
          const event = {
            kind: "session.output",
            payload: {
              session_id: "session-1",
              epoch,
              seq: "1",
              kind: "output",
              data_b64: "",
              raw_len: 0,
            },
          } as DaemonEvent;
          for (const listener of listeners) listener(event);
        });
        return { epoch, replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24 };
      },
      sessionResize: async () => ({ cols: 80, rows: 24 }),
      sessionAck: () => undefined,
      transportStatus: async () => ({ controlAlive: healthy, dataAlive: healthy, generation }),
      reconnectTransport,
    } as unknown as DaemonClient;
    const reset = vi.fn();
    const terminal: TerminalLike = {
      open: () => undefined,
      write: (_data, callback) => callback?.(),
      resize: () => undefined,
      reset,
      dispose: () => undefined,
      onData: () => ({ dispose: () => undefined }),
      hasSelection: () => false,
      getSelection: () => "",
      attachCustomKeyEventHandler: () => undefined,
      loadAddon: () => undefined,
      element: null,
    };
    const registry = new TerminalRegistry({
      createTerminal: () => terminal,
      createDom: () => ({ className: "", parentElement: null, remove: () => undefined } satisfies RegistryDom),
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
    expect(attachCount).toBe(1);
    expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("live");

    if (cause === "exit" || delayed) {
      controller.handleEvent({ kind: "session.exited", payload: {
        session_id: "session-1", exit_code: 137, reason: "process_exit", descendants_remaining: false,
      } } as DaemonEvent);
      expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe("exited");
      if (cause === "replace") {
        useWorkbenchStore.setState(s => ({ panes: { ...s.panes, "leaf-1": {
          ...s.panes["leaf-1"], viewId: "new-view", sessionId: "new-session", workloadId: "new-workload", phase: "live",
        } } }));
      }
      if (cause === "close") useWorkbenchStore.getState().applyClose("leaf-1");
      if (cause === "disposed") controller.dispose();
      if (delayed) {
        expect(resolveList).toBeDefined();
        resolveList!([record]);
      }
      await flush();
      const pane = useWorkbenchStore.getState().panes["leaf-1"];
      if (delayed) expect(pane?.resume ?? null).toBeNull();
      else expect(pane.resume?.agentSessionId).toBe("known-session-1");
      expect(agentSessionList).toHaveBeenCalledWith({ workload_id: "workload-1", pty_session_id: "session-1", limit: 1 });
      expect(workloadLaunch).not.toHaveBeenCalled();
      controller.dispose();
      return;
    }

    // First watchdog pass establishes the healthy generation baseline.
    await vi.advanceTimersByTimeAsync(2000);
    healthy = false;
    await vi.advanceTimersByTimeAsync(2000);
    await flush();

    expect(reconnectTransport).toHaveBeenCalledTimes(1);
    expect(reset).toHaveBeenCalledTimes(restarted ? 0 : 1);
    expect(attachCount).toBe(restarted ? 1 : 2);
    expect(useWorkbenchStore.getState().revision).toBe(1);
    expect(useWorkbenchStore.getState().panes["leaf-1"].phase).toBe(restarted ? "exited" : "live");
    if (restarted) {
      expect(useWorkbenchStore.getState().panes["leaf-1"].error).toBeTruthy();
      expect(useWorkbenchStore.getState().workloads[0].state).toBe("INTERRUPTED");
      expect(useWorkbenchStore.getState().panes["leaf-1"].resume?.agentSessionId).toBe("known-session-1");
      expect(workloadLaunch).not.toHaveBeenCalled();
      healthy = false;
      await vi.advanceTimersByTimeAsync(2000);
      expect(attachCount).toBe(1);
      expect(reset).not.toHaveBeenCalled();
    }
    controller.dispose();
  });

  /**
   * 일반 셸 창들(에이전트 기록 없음)이 붙은 채 연결이 끊기는 harness. `restarted`이면 재접속한
   * 데몬이 그 작업들을 DAEMON_RESTART로 끝났다고 알리고, attach는 끝난 세션의 저널을 재생한다.
   */
  async function shellRestartHarness(options: { sessions: string[]; restarted: boolean; reconnectFails?: Error }) {
    vi.useFakeTimers();
    const baseSnapshot = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
    const listeners = new Set<DaemonEventListener>();
    let generation = 0;
    let healthy = true;
    const reconnectTransport = vi.fn(async () => {
      if (options.reconnectFails) throw options.reconnectFails;
      healthy = true;
      generation += 1;
    });
    const ended = () => options.restarted && generation > 0;
    let attachCount = 0;
    const client = {
      events: {
        subscribe(listener: DaemonEventListener) {
          listeners.add(listener);
          return { dispose: () => listeners.delete(listener) };
        },
      },
      systemSnapshot: async () => ({
        ...baseSnapshot,
        revision: generation ? 1 : 100,
        workloads: options.sessions.map((sessionId) => ({
          workload_id: `w-${sessionId}`, session_id: sessionId,
          state: ended() ? "INTERRUPTED" : "RUNNING", last_error_code: ended() ? "DAEMON_RESTART" : null,
          title: "shell", cwd: "/tmp",
        })),
      }),
      interventionList: async () => [],
      agentSessionList: async () => [],
      sessionAttach: async (params: AttachParams) => {
        attachCount += 1;
        const epoch = `epoch-${attachCount}`;
        queueMicrotask(() => {
          for (const listener of listeners) listener({ kind: "session.output", payload: {
            session_id: params.session_id, epoch, seq: "1", kind: "output", data_b64: "", raw_len: 0,
          } } as DaemonEvent);
        });
        return { epoch, replay_from_seq: "1", last_seq: "1", cols: 80, rows: 24, exited: ended() };
      },
      sessionResize: async () => ({ cols: 80, rows: 24 }),
      sessionAck: () => undefined,
      transportStatus: async () => ({ controlAlive: healthy, dataAlive: healthy, generation }),
      reconnectTransport,
    } as unknown as DaemonClient;
    const registry = new TerminalRegistry({
      createTerminal: () => terminalLike(),
      createDom: () => ({ className: "", parentElement: null, remove: () => undefined } satisfies RegistryDom),
    });
    const leaves = options.sessions.map((sessionId) => `leaf-${sessionId}`);
    useWorkbenchStore.setState({
      tabs: options.sessions.map((sessionId) => ({
        kind: "terminal" as const, id: `tab-${sessionId}`, title: "tab",
        root: { kind: "leaf" as const, id: `leaf-${sessionId}`, view_id: `view-${sessionId}`, session_id: sessionId },
      })),
      activeTabId: `tab-${options.sessions[0]}`,
      focusedLeafId: leaves[0],
      panes: Object.fromEntries(options.sessions.map((sessionId) => [`leaf-${sessionId}`, {
        leafId: `leaf-${sessionId}`, viewId: `view-${sessionId}`, sessionId, workloadId: `w-${sessionId}`,
        title: "shell", cwd: "/tmp", phase: "replaying" as const, error: null, usage: null, flowBlocked: false,
      }])),
      revision: 0,
    });
    const controller = new SessionController({ client, registry, platform: "darwin" });
    controller.start();
    await flush(4);
    /** 와치독이 건전한 기준을 세운 뒤 연결이 끊긴 것을 보게 한다. */
    const dropTransport = async () => {
      await vi.advanceTimersByTimeAsync(2000);
      healthy = false;
      await vi.advanceTimersByTimeAsync(2000);
      await flush(32);
    };
    return { controller, dropTransport, reconnectTransport };
  }

  it("데몬 재시작으로 끝난 일반 셸 창은 저널을 다시 재생한 뒤에도 재시작 사유를 남긴다", async () => {
    const { controller, dropTransport } = await shellRestartHarness({ sessions: ["s-1"], restarted: true });
    try {
      expect(useWorkbenchStore.getState().panes["leaf-s-1"].phase).toBe("live");
      await dropTransport();
      const pane = useWorkbenchStore.getState().panes["leaf-s-1"];
      expect(pane.phase).toBe("exited");
      expect(pane.error).toBe(t("terminal.session.daemonRestarted"));
    } finally {
      controller.dispose();
    }
  });

  it("재접속이 실패해도 이미 끝난 창의 종료 오버레이는 실패로 덮지 않는다", async () => {
    const { controller, dropTransport, reconnectTransport } = await shellRestartHarness({
      sessions: ["s-1", "s-2"],
      restarted: false,
      reconnectFails: new Error("daemon endpoint was not reachable within the timeout"),
    });
    try {
      controller.handleEvent({ kind: "session.exited", payload: {
        session_id: "s-2", exit_code: 137, reason: "process_exit", descendants_remaining: false,
      } } as DaemonEvent);
      await flush();
      expect(useWorkbenchStore.getState().panes["leaf-s-2"].phase).toBe("exited");
      await dropTransport();
      expect(reconnectTransport).toHaveBeenCalled();
      expect(useWorkbenchStore.getState().panes["leaf-s-1"].phase).toBe("failed");
      expect(useWorkbenchStore.getState().panes["leaf-s-2"].phase).toBe("exited");
    } finally {
      controller.dispose();
    }
  });
});
