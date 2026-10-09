/**
 * 끝난 일반 터미널을 이어서 열 때(최근 종료의 터미널 연결·창의 재실행)의 계약:
 * 1) 셸이 마지막으로 알린 경로(OSC 7이 갱신한 pane cwd)에서 연다.
 * 2) 그 경로가 사라졌으면(CWD_UNAVAILABLE) 가까운 상위 경로 → 처음 실행한 경로 →
 *    프로젝트 root → home → 뿌리 순으로 열고 알린다(04-ui §2-4).
 * 3) 경로가 아니라 셸이 뜨지 못하면(SPAWN_FAILED) 같은 경로에서 기본 셸로 바꿔 열어 본다.
 *    에이전트 대화 재개는 경로도 셸도 바꾸지 않고 사유를 남긴다.
 * 4) 보존한 화면에서 잇는 새 PTY는 지금 격자 크기로 시작한다(80×24로 접었다 펴지 않게).
 * 5) 끝난 세션에 붙어 있던 view는 데몬에서 뗀다(출력 펌프·저널 보존이 풀리게).
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
const LAST_PARENT = `${ROOT}/crates`;

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

function probe(options: {
  missingCwds?: string[];
  failWith?: RpcClientError;
  failFor?: (request: LaunchRequest) => RpcClientError | null;
} = {}): Probe {
  const launches: LaunchRequest[] = [];
  const detaches: Array<{ session_id: string; view_id: string }> = [];
  const client = {
    events: { subscribe: () => ({ dispose: () => undefined }) },
    agentSessionList: async () => [],
    workloadLaunch: async (request: LaunchRequest) => {
      launches.push(request);
      if (options.failWith) throw options.failWith;
      const failure = options.failFor?.(request);
      if (failure) throw failure;
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

  it("마지막 경로가 사라졌으면 가장 가까운 상위 경로에서 새 요청으로 열고 알린다", async () => {
    const { client, launches } = probe({ missingCwds: [LAST_CWD] });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().sessionId).toBe("s-2"));
      expect(launches.map(request => request.cwd)).toEqual([LAST_CWD, LAST_PARENT]);
      expect(launches[1].request_id).not.toBe(launches[0].request_id);
      expect(pane().cwd).toBe(LAST_PARENT);
      expect(pane().phase).not.toBe("failed");
      expect(useWorkbenchStore.getState().toast).toBe(
        t("terminal.session.cwdFallback", { from: LAST_CWD, cwd: LAST_PARENT }),
      );
    } finally { controller.dispose(); }
  });

  it("상위 경로도 사라졌으면 더 위로 올라가 처음 실행한 경로(프로젝트 root)에서 연다", async () => {
    const { client, launches } = probe({ missingCwds: [LAST_CWD, LAST_PARENT] });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().sessionId).toBe("s-3"));
      expect(launches.map(request => request.cwd)).toEqual([LAST_CWD, LAST_PARENT, ROOT]);
      expect(pane().cwd).toBe(ROOT);
    } finally { controller.dispose(); }
  });

  it("권한이 없어 거절된 경로는 그 사유로 알린다", async () => {
    const { client } = probe({
      failFor: request => request.cwd === LAST_CWD
        ? new RpcClientError("CWD_UNAVAILABLE", "cwd is not accessible", false, { reason_code: "cwd_permission_denied" })
        : null,
    });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().sessionId).toBe("s-2"));
      expect(useWorkbenchStore.getState().toast).toBe(
        t("terminal.session.cwdFallbackDenied", { from: LAST_CWD, cwd: LAST_PARENT }),
      );
    } finally { controller.dispose(); }
  });

  it("후보 경로가 모두 없으면 home과 뿌리까지 시도한 뒤 무엇을 못 열었는지 남긴다", async () => {
    const everything = [LAST_CWD, LAST_PARENT, ROOT, HOME, "/"];
    const { client, launches } = probe({ missingCwds: everything });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().phase).toBe("failed"));
      expect(launches.map(request => request.cwd)).toEqual(everything);
      expect(pane().error).toContain(t("terminal.session.cwdUnavailable", { cwd: LAST_CWD }));
      expect(pane().error).toContain("CWD_UNAVAILABLE");
    } finally { controller.dispose(); }
  });

  it("경로가 아니라 셸이 뜨지 못하면 같은 경로에서 기본 셸로 바꿔 열어 보고, 다 실패하면 사유를 남긴다", async () => {
    const { client, launches } = probe({ failWith: new RpcClientError("SPAWN_FAILED", "pty spawn failed") });
    seed(exitedPane());
    const controller = controllerFor(client);
    try {
      await controller.attachWorkloadTerminal("w-old");
      await vi.waitFor(() => expect(pane().phase).toBe("failed"));
      // 다른 경로로는 가지 않는다 — 셸만 /bin/zsh → /bin/sh로 바꾼다.
      expect(launches.map(request => request.cwd)).toEqual([LAST_CWD, LAST_CWD]);
      expect(launches[0].argv).toContain("/bin/zsh");
      expect(launches[1].argv).toContain("/bin/sh");
      expect(pane().error).toContain(t("terminal.session.spawnFailedHint"));
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
      expect(launches.map(request => request.cwd)).toEqual([LAST_CWD, LAST_PARENT]);
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
      expect(pane().error).toContain(t("terminal.resume.cwdUnavailable", { cwd: ROOT }));
    } finally { controller.dispose(); }
  });

  it("경로를 잃은 대화의 ‘새 셸’은 그 대화 경로의 가까운 상위 경로에서 연다", async () => {
    const { client, launches } = probe({ missingCwds: [ROOT, LAST_CWD] });
    const resume: AgentResumeInfo = {
      recordId: "rec-1", agent: "claude", agentSessionId: "agent-session-0001",
      cwd: ROOT, title: "iyagi", program: "/opt/bin/claude",
    };
    seed(exitedPane({ resume, cwd: null }));
    const controller = controllerFor(client);
    try {
      await controller.resumeAgentSession(resume, { leafId: "leaf-1" });
      expect(pane().phase).toBe("failed");
      controller.retryPane("leaf-1", { newShell: true });
      await vi.waitFor(() => expect(pane().sessionId).not.toBeNull());
      // 재개 1번(ROOT, 실패) 뒤 새 셸: ROOT(없음) → home. /work는 뿌리 바로 아래라 건너뛴다.
      expect(launches.map(request => request.cwd)).toEqual([ROOT, ROOT, HOME]);
      expect(launches[1].argv).toContain("/bin/zsh");
    } finally { controller.dispose(); }
  });
});
