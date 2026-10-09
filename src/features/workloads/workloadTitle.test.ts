/**
 * 관리 작업 목록 이름: 붙은 pane이 터미널에서 받은 최신 제목(OSC 0/2)을
 * 따르고, 창을 닫았으면 그 작업이 마지막으로 보고한 제목을, 그것도 없으면
 * 실행 제목으로 돌아간다. 제목 보고는 새 세션에서 초기화된다. 끝난 작업
 * 이름 앞의 에이전트 표시는 그 터미널에서 마지막으로 감지한 에이전트다.
 */

import { afterEach, describe, expect, it } from "vitest";
import type { AgentStatus } from "../../generated/AgentStatus";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import { MockDaemonClient } from "../daemon/mockClient";
import { FINISHED_WORKLOADS_RETAINED, useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { workloadListAgent, workloadListTitle } from "./workloadTitle";

function pane(overrides: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: "leaf-1",
    viewId: "view-1",
    sessionId: "session-1",
    workloadId: "workload-1",
    title: "zsh",
    cwd: null,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    ...overrides,
  };
}

function summary(overrides: Partial<WorkloadSummary> = {}): WorkloadSummary {
  return {
    workload_id: "workload-1",
    session_id: "session-1",
    mode: "shell",
    state: "RUNNING",
    priority: 1,
    title: "zsh",
    cwd: "/work",
    program: "/usr/bin/env",
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

function agent(id: string): AgentStatus {
  return { agent: id, pid: 4242, detected_at_ms: 1 };
}

const workload = { workload_id: "workload-1", session_id: "session-1", title: "claude" };

describe("workloadListTitle", () => {
  it("falls back to the launch title until the terminal reports one", () => {
    expect(workloadListTitle(workload, {}, {})).toBe("claude");
    // 재연결 안내·셸 라벨 같은 자리 표시 title은 이름으로 쓰지 않는다.
    expect(workloadListTitle(workload, { "leaf-1": pane({ title: "재연결" }) }, {})).toBe("claude");
  });

  it("uses the latest terminal title of the attached pane", () => {
    const panes = { "leaf-1": pane({ terminalTitle: "✳ Fix login bug" }) };
    expect(workloadListTitle(workload, panes, {})).toBe("✳ Fix login bug");
    // 붙은 pane의 최신 제목이 기억한 제목보다 앞선다.
    const memory = { "workload-1": { title: "older title", agent: null } };
    expect(workloadListTitle(workload, panes, memory)).toBe("✳ Fix login bug");
  });

  it("matches the pane by session when the pane has no workload id", () => {
    const panes = { "leaf-1": pane({ workloadId: null, terminalTitle: "vim README.md" }) };
    expect(workloadListTitle(workload, panes, {})).toBe("vim README.md");
  });

  it("ignores titles reported by other workloads' panes", () => {
    const other = pane({ leafId: "leaf-2", sessionId: "session-2", workloadId: "workload-2", terminalTitle: "htop" });
    expect(workloadListTitle(workload, { "leaf-2": other }, {})).toBe("claude");
    // 대기 중(session 없음) 작업은 session이 없는 pane과 짝지어지지 않는다.
    const detached = pane({ leafId: "leaf-3", sessionId: null, workloadId: null, terminalTitle: "stray" });
    expect(workloadListTitle({ workload_id: "queued", session_id: null, title: "codex" }, { "leaf-3": detached }, {})).toBe("codex");
  });

  it("uses the title the workload last reported once its pane is gone", () => {
    const memory = { "workload-1": { title: "✳ Fix login bug", agent: "claude" } };
    expect(workloadListTitle(workload, {}, memory)).toBe("✳ Fix login bug");
    // 에이전트만 기억하고 제목 보고가 없던 작업은 실행 제목으로 부른다.
    expect(workloadListTitle(workload, {}, { "workload-1": { title: null, agent: "codex" } })).toBe("claude");
    // 다른 작업의 기억, 프로토타입의 속성 이름은 쓰지 않는다.
    expect(workloadListTitle({ ...workload, workload_id: "constructor" }, {}, memory)).toBe("claude");
  });

  it("does not name a terminal after the command that ended its shell", () => {
    // oh-my-zsh는 명령 실행 직전 그 명령줄을 제목으로 보낸다 — `exit`로 끝낸 pane의 마지막 제목은 "exit"다.
    const panes = { "leaf-1": pane({ phase: "exited", terminalTitle: "exit" }) };
    const memory = { "workload-1": { title: "sh2orc@mac:~/project", agent: null } };
    expect(workloadListTitle(workload, panes, memory)).toBe("sh2orc@mac:~/project");
    expect(workloadListTitle(workload, panes, {})).toBe("claude");
  });
});

describe("workloadListAgent", () => {
  it("prefers the last agent remembered for the terminal", () => {
    const memory = { "workload-1": { title: null, agent: "codex" } };
    expect(workloadListAgent(summary({ agent: agent("claude") }), memory)).toBe("codex");
    expect(workloadListAgent(summary(), memory)).toBe("codex");
  });

  it("falls back to the agent still on the summary, and is null when none ran", () => {
    expect(workloadListAgent(summary({ agent: agent("opencode") }), {})).toBe("opencode");
    expect(workloadListAgent(summary(), {})).toBeNull();
    expect(workloadListAgent(summary(), { "workload-1": { title: "zsh", agent: null } })).toBeNull();
  });
});

describe("pane terminal title (store)", () => {
  afterEach(() => useWorkbenchStore.setState({ panes: {}, tabs: [], workloads: [], workloadMemory: {}, revision: 0 }));

  it("updates the header title and the reported title together", () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane({ title: "재연결" }) } });
    useWorkbenchStore.getState().paneTerminalTitle("leaf-1", "✳ Fix login bug");
    const next = useWorkbenchStore.getState().panes["leaf-1"];
    expect(next.title).toBe("✳ Fix login bug");
    expect(next.terminalTitle).toBe("✳ Fix login bug");
  });

  it("leaves state untouched when replay re-sends the same title", () => {
    useWorkbenchStore.setState({
      panes: { "leaf-1": pane({ title: "t", terminalTitle: "t" }) },
      workloadMemory: { "workload-1": { title: "t", agent: null } },
    });
    const before = useWorkbenchStore.getState();
    useWorkbenchStore.getState().paneTerminalTitle("leaf-1", "t");
    expect(useWorkbenchStore.getState().panes).toBe(before.panes);
    expect(useWorkbenchStore.getState().workloadMemory).toBe(before.workloadMemory);
  });

  it("catches up the remembered title without re-rendering the pane", () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane({ title: "t", terminalTitle: "t" }) } });
    const panes = useWorkbenchStore.getState().panes;
    useWorkbenchStore.getState().paneTerminalTitle("leaf-1", "t");
    expect(useWorkbenchStore.getState().panes).toBe(panes);
    expect(useWorkbenchStore.getState().workloadMemory["workload-1"]).toEqual({ title: "t", agent: null });
  });

  it("forgets the previous session's title when a new session is assigned", () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane({ terminalTitle: "old task" }) } });
    useWorkbenchStore.getState().paneSessionAssigned("leaf-1", "session-1", "workload-1");
    expect(useWorkbenchStore.getState().panes["leaf-1"].terminalTitle).toBe("old task");
    useWorkbenchStore.getState().paneSessionAssigned("leaf-1", "session-2", "workload-2");
    expect(useWorkbenchStore.getState().panes["leaf-1"].terminalTitle ?? null).toBeNull();
    const state = useWorkbenchStore.getState();
    expect(workloadListTitle({ workload_id: "workload-2", session_id: "session-2", title: "codex" }, state.panes, state.workloadMemory)).toBe("codex");
  });

  it("keeps the final title after the pane is closed", () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() }, workloads: [summary()] });
    const store = useWorkbenchStore.getState();
    store.paneTerminalTitle("leaf-1", "✳ Fix login bug");
    store.paneTerminalTitle("leaf-1", "✳ Ship login fix");
    store.removePaneMeta("leaf-1");
    store.upsertWorkload(summary({ state: "SUCCEEDED", connection: "detached" }));
    const state = useWorkbenchStore.getState();
    expect(state.panes).toEqual({});
    expect(workloadListTitle(summary(), state.panes, state.workloadMemory)).toBe("✳ Ship login fix");
  });

  it("keeps the title from before the exit command while the header follows the terminal", () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() } });
    const store = useWorkbenchStore.getState();
    store.paneTerminalTitle("leaf-1", "sh2orc@mac:~/project");
    const remembered = useWorkbenchStore.getState().workloadMemory;
    store.paneTerminalTitle("leaf-1", "exit");
    const state = useWorkbenchStore.getState();
    expect(state.panes["leaf-1"].title).toBe("exit");
    expect(state.workloadMemory).toBe(remembered);
    store.removePaneMeta("leaf-1");
    const closed = useWorkbenchStore.getState();
    expect(workloadListTitle(summary(), closed.panes, closed.workloadMemory)).toBe("sh2orc@mac:~/project");
  });

  it("does not hand a reused pane's old title to the next workload", () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() } });
    const store = useWorkbenchStore.getState();
    store.paneTerminalTitle("leaf-1", "first task");
    store.paneSessionAssigned("leaf-1", "session-2", "workload-2");
    store.paneTerminalTitle("leaf-1", "second task");
    const { workloadMemory } = useWorkbenchStore.getState();
    expect(workloadMemory["workload-1"]?.title).toBe("first task");
    expect(workloadMemory["workload-2"]?.title).toBe("second task");
  });

  it("finds the workload by session for panes attached without a workload id", () => {
    useWorkbenchStore.setState({ panes: { "leaf-1": pane({ workloadId: null }) }, workloads: [summary()] });
    useWorkbenchStore.getState().paneTerminalTitle("leaf-1", "vim README.md");
    expect(useWorkbenchStore.getState().workloadMemory["workload-1"]?.title).toBe("vim README.md");
    // 작업을 알 수 없는 pane의 제목은 기억할 곳이 없다.
    useWorkbenchStore.setState({ panes: { "leaf-2": pane({ leafId: "leaf-2", sessionId: "unknown", workloadId: null }) } });
    const before = useWorkbenchStore.getState().workloadMemory;
    useWorkbenchStore.getState().paneTerminalTitle("leaf-2", "stray");
    expect(useWorkbenchStore.getState().workloadMemory).toBe(before);
  });
});

describe("last agent memory (store)", () => {
  afterEach(() => useWorkbenchStore.setState({ panes: {}, tabs: [], workloads: [], workloadMemory: {}, revision: 0 }));

  it("keeps the last detected agent after the summary stops reporting it", () => {
    const store = useWorkbenchStore.getState();
    store.upsertWorkload(summary({ agent: agent("claude") }));
    // 에이전트를 끝내면 데몬 요약의 감지값이 비고, 곧 셸도 끝난다.
    store.upsertWorkload(summary({ agent: null }));
    store.upsertWorkload(summary({ state: "SUCCEEDED", connection: "detached", agent: null }));
    const state = useWorkbenchStore.getState();
    expect(workloadListAgent(state.workloads[0], state.workloadMemory)).toBe("claude");
  });

  it("switches to the agent that ran last and keeps the remembered title", () => {
    useWorkbenchStore.setState({ workloadMemory: { "workload-1": { title: "✳ Fix login bug", agent: null } } });
    const store = useWorkbenchStore.getState();
    store.upsertWorkload(summary({ agent: agent("claude") }));
    store.upsertWorkload(summary({ agent: agent("codex") }));
    expect(useWorkbenchStore.getState().workloadMemory["workload-1"]).toEqual({ title: "✳ Fix login bug", agent: "codex" });
  });

  it("ignores agent ids it cannot mark and leaves memory untouched on repeats", () => {
    const store = useWorkbenchStore.getState();
    store.upsertWorkload(summary({ agent: agent("aider") }));
    expect(useWorkbenchStore.getState().workloadMemory).toEqual({});
    store.upsertWorkload(summary({ agent: agent("opencode") }));
    const before = useWorkbenchStore.getState().workloadMemory;
    store.upsertWorkload(summary({ agent: agent("opencode"), usage: null }));
    expect(useWorkbenchStore.getState().workloadMemory).toBe(before);
  });
});

describe("workload memory pruning (store)", () => {
  afterEach(() => useWorkbenchStore.setState({ panes: {}, tabs: [], workloads: [], workloadMemory: {}, revision: 0 }));

  it("drops memory the daemon snapshot no longer backs, unless a pane still points at it", async () => {
    const daemon = new MockDaemonClient({ resourceIntervalMs: 0 });
    const base = await daemon.systemSnapshot();
    daemon.dispose();
    useWorkbenchStore.setState({
      panes: { "leaf-1": pane({ workloadId: "on-screen", sessionId: "session-9" }) },
      workloadMemory: {
        "workload-1": { title: "kept", agent: null },
        "on-screen": { title: "still attached", agent: null },
        gone: { title: "forgotten by daemon", agent: "claude" },
      },
    });
    useWorkbenchStore.getState().applySnapshot({
      ...base,
      revision: 1,
      workloads: [summary({ agent: agent("codex"), state: "SUCCEEDED", connection: "detached" })],
    });
    expect(useWorkbenchStore.getState().workloadMemory).toEqual({
      "workload-1": { title: "kept", agent: "codex" },
      "on-screen": { title: "still attached", agent: null },
    });
  });

  it("forgets only workloads pushed out of the finished list by an update", () => {
    const finished = Array.from({ length: FINISHED_WORKLOADS_RETAINED }, (_, index) =>
      summary({ workload_id: `done-${index}`, session_id: `s-${index}`, state: "SUCCEEDED", connection: "detached" }),
    );
    useWorkbenchStore.setState({
      workloads: finished,
      workloadMemory: {
        "done-0": { title: "oldest", agent: "claude" },
        "done-1": { title: "second", agent: null },
        // 아직 목록에 오지 않은 작업(첫 snapshot 전) — 이벤트 하나로는 지우지 않는다.
        "not-listed-yet": { title: "starting", agent: null },
      },
    });
    useWorkbenchStore.getState().upsertWorkload(
      summary({ workload_id: "done-new", session_id: "s-new", state: "FAILED", connection: "detached" }),
    );
    const { workloads, workloadMemory } = useWorkbenchStore.getState();
    expect(workloads.some((w) => w.workload_id === "done-0")).toBe(false);
    expect(workloadMemory).toEqual({
      "done-1": { title: "second", agent: null },
      "not-listed-yet": { title: "starting", agent: null },
    });
  });
});
