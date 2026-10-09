/**
 * 끝난 일반 터미널을 이어서 열 때(최근 종료의 터미널 연결·창의 재실행)의 계약:
 * 1) 셸이 마지막으로 알린 경로(OSC 7이 갱신한 pane cwd)에서 연다.
 * 2) 그 경로가 사라졌으면(CWD_UNAVAILABLE) 처음 실행한 경로 → 프로젝트 root → home
 *    순으로 열고 알린다(04-ui §2-4). 다른 실패와 에이전트 대화 재개는 다시 시도하지 않는다.
 * 3) 보존한 화면에서 잇는 새 PTY는 지금 격자 크기로 시작한다(80×24로 접었다 펴지 않게).
 * 4) 끝난 세션에 붙어 있던 view는 데몬에서 뗀다(출력 펌프·저널 보존이 풀리게).
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { AgentResumeInfo } from "../agentSessions/types";
import type { DaemonClient } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { SessionController } from "./sessionController";
import { makeLeaf } from "./splitTree";
import { TerminalRegistry, type TerminalLike } from "./registry";

const ROOT = "/work/iyagi";
const HOME = "/home/me";
const LAST_CWD = `${ROOT}/crates/iyagi-termd`;

function fakeTerminal(): TerminalLike & { rows: number } {
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
    // 재생한 화면이 그려진 xterm의 지금 격자.
    cols: 132,
    rows: 40,
  };
}

interface Probe {
  client: DaemonClient;
  launches: LaunchRequest[];
  detaches: Array<{ session_id: string; view_id: string }>;
}

function probe(options: { missingCwds?: string[]; failWith?: RpcClientError } = {}): Probe {
  const launches: LaunchRequest[] = [];
  const detaches: Array<{ session_id: string; view_id: string }> = [];
  const client = {
    events: { subscribe: () => ({ dispose: () => undefined }) },
    agentSessionList: async () => [],
    workloadLaunch: async (request: LaunchRequest) => {
      launches.push(request);
      if (options.failWith) throw options.failWith;
      if (options.missingCwds?.includes(request.cwd)) {
        throw new RpcClientError("CWD_UNAVAILABLE", "cwd cannot be canonicalized");
      }
      return { session_id: `s-${launches.length}`, workload_id: `w-${launches.length}` };
    },
    // 새 PTY에 붙는 일은 이 계약 밖이다 — 응답 없이 둔다.
    sessionAttach: () => new Promise(() => undefined),
    sessionDetach: async (params: { session_id: string; view_id: string }) => {
      detaches.push(params);
      return { detached: true as const };
    },
  } as unknown as DaemonClient;
  return { client, launches, detaches };
}

function exitedPane(overrides: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: "leaf-1",
    viewId: "view-1",
    sessionId: "s-old",
    workloadId: "w-old",
    title: "zsh",
    cwd: LAST_CWD,
    phase: "exited",
    error: null,
    usage: null,
    flowBlocked: false,
    ...overrides,
  };
}

function seed(pane: PaneMeta): void {
  useWorkbenchStore.setState({
    tabs: [{ kind: "terminal", id: "tab-1", title: "탭 1", root: makeLeaf(pane.leafId, pane.viewId, pane.sessionId) }],
    panes: { [pane.leafId]: pane },
    activeTabId: "tab-1",
    focusedLeafId: pane.leafId,
    // 처음 실행한 경로는 프로젝트 root였다.
    workloads: [{
      workload_id: "w-old", session_id: "s-old", state: "CANCELLED", mode: "shell", priority: 1, title: "zsh",
      cwd: ROOT, program: "/bin/zsh", reservation_bytes: "0", cpu_slots: 1, enforcement: "observe",
      root_exited: true, cancel_requested: false, connection: "attached",
    } as unknown as WorkloadSummary],
    workloadMemory: {},
    queue: [],
    revision: 0,
    modal: null,
    toast: null,
  });
}

function controllerFor(client: DaemonClient): SessionController {
  const registry = new TerminalRegistry({
    createTerminal: () => fakeTerminal(),
    createDom: () => ({ className: "", parentElement: null, remove: () => undefined }),
  });
  registry.acquire("view-1");
  return new SessionController({ client, registry, platform: "darwin", config: { projectRoot: ROOT, home: HOME } });
}

const pane = () => useWorkbenchStore.getState().panes["leaf-1"];

beforeEach(() => {
  useWorkbenchStore.setState({ tabs: [], panes: {}, workloads: [], toast: null, modal: null });
});

describe("최근 종료의 터미널 연결: 끝난 일반 터미널을 그 자리에서 잇기", () => {
  it("셸이 마지막으로 알린 경로에서 지금 화면 크기로 시작하고, 끝난 세션의 view는 데몬에서 뗀다", async () => {
    const { client, launches, detaches } = probe();
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().sessionId).toBe("s-1"));
      expect(launches).toHaveLength(1);
      expect(launches[0]).toMatchObject({ cwd: LAST_CWD, cols: 132, rows: 40 });
      expect(detaches).toEqual([{ session_id: "s-old", view_id: "view-1" }]);
      expect(pane().viewId).toBe("view-1"); // 보존한 화면 아래에서 잇는다
      expect(useWorkbenchStore.getState().toast).toBeNull();
    } finally { controller.dispose(); }
  });

  it("마지막 경로가 사라졌으면 처음 실행한 경로에서 새 요청으로 열고 알린다", async () => {
    const { client, launches } = probe({ missingCwds: [LAST_CWD] });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().sessionId).toBe("s-2"));
      expect(launches.map(request => request.cwd)).toEqual([LAST_CWD, ROOT]);
      expect(launches[1].request_id).not.toBe(launches[0].request_id);
      expect(pane().cwd).toBe(ROOT);
      expect(pane().phase).not.toBe("failed");
      expect(useWorkbenchStore.getState().toast).toBe(t("terminal.session.cwdFallback", { cwd: ROOT }));
    } finally { controller.dispose(); }
  });

  it("후보 경로가 모두 사라졌으면 home까지 시도한 뒤 실패를 남긴다", async () => {
    const { client, launches } = probe({ missingCwds: [LAST_CWD, ROOT, HOME] });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().phase).toBe("failed"));
      expect(launches.map(request => request.cwd)).toEqual([LAST_CWD, ROOT, HOME]);
    } finally { controller.dispose(); }
  });

  it("경로가 사라진 것이 아닌 실패는 다른 경로로 다시 시도하지 않는다", async () => {
    const { client, launches } = probe({ failWith: new RpcClientError("SPAWN_FAILED", "spawn failed") });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().phase).toBe("failed"));
      expect(launches).toHaveLength(1);
    } finally { controller.dispose(); }
  });
});

describe("창의 재실행과 대화 재개", () => {
  it("일반 셸 재실행도 사라진 경로면 대체 경로로 열고 끝난 세션의 view를 뗀다", async () => {
    const { client, launches, detaches } = probe({ missingCwds: [LAST_CWD] });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      controller.retryPane("leaf-1");
      await vi.waitFor(() => expect(pane().sessionId).toBe("s-2"));
      expect(launches.map(request => request.cwd)).toEqual([LAST_CWD, ROOT]);
      expect(detaches).toEqual([{ session_id: "s-old", view_id: "view-1" }]);
    } finally { controller.dispose(); }
  });

  it("에이전트 대화 재개는 대화가 그 경로에 묶여 있어 다른 경로에서 열지 않는다", async () => {
    const { client, launches } = probe({ missingCwds: [ROOT] });
    const resume: AgentResumeInfo = {
      recordId: "rec-1", agent: "claude", agentSessionId: "agent-session-0001",
      cwd: ROOT, title: "iyagi", program: "/opt/bin/claude",
    };
    seed(exitedPane({ resume }));
    const controller = controllerFor(client);
    try {
      await controller.resumeAgentSession(resume, { leafId: "leaf-1" });
      expect(launches).toHaveLength(1);
      expect(launches[0].cwd).toBe(ROOT);
      expect(pane().phase).toBe("failed");
    } finally { controller.dispose(); }
  });
});
