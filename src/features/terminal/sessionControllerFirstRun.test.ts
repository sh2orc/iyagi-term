/**
 * 첫 실행 동작(04-ui §1): 빈 워크벤치에서 newTerminal()은 "빈 프로젝트"
 * 화면 대신 전체 창을 쓰는 pane 1개(root가 split 아닌 leaf)를 만들고
 * resolveShell이 준 프로필로 세션을 시작한다.
 */

import { describe, expect, it } from "vitest";
import type { DaemonClient } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type TerminalLike } from "./registry";
import { useWorkbenchStore } from "../../store/workbenchStore";

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

function fakeDom(): { className: string; parentElement: null; remove: () => void } {
  return { className: "", parentElement: null, remove: () => undefined };
}

// launch까지만 흐르면 된다 — attach 단계의 실패는 컨트롤러가 pane 상태로
// 흡수하므로 테스트에 영향을 주지 않는다.
const stubClient = {
  workloadLaunch: async () => ({ session_id: "s1", workload_id: "w1" }),
} as unknown as DaemonClient;

describe("newTerminal on empty workbench (첫 실행)", () => {
  it("uses the resolved home and login arguments for the default macOS shell", async () => {
    useWorkbenchStore.setState({ tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
    const launches: unknown[] = [];
    const controller = new SessionController({
      client: { workloadLaunch: async (request: unknown) => {
        launches.push(request);
        return { session_id: "s1", workload_id: "w1" };
      } } as unknown as DaemonClient,
      registry: new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom }),
      platform: "darwin",
      config: { home: "/Users/test user" },
    });
    controller.newTerminal();
    // launchShell은 창 크기로 PTY를 처음부터 열려고 measuredGrid를 먼저 기다린다.
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(launches[0]).toMatchObject({
      program: "/usr/bin/env", argv: ["-u", "NO_COLOR", "-u", "FORCE_COLOR", "-u", "CLICOLOR", "-u", "CLICOLOR_FORCE", "/bin/zsh", "-l", "-i"], cwd: "/Users/test user",
    });
  });
  it("shows the daemon's reason when a shell launch is rejected", async () => {
    useWorkbenchStore.setState({ tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
    const controller = new SessionController({
      client: {
        workloadLaunch: async () => {
          throw new RpcClientError("INVALID_STATE", "host memory pressure is CRITICAL; retry shortly");
        },
      } as unknown as DaemonClient,
      registry: new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom }),
      platform: "darwin",
    });
    controller.newTerminal();
    await new Promise((resolve) => setTimeout(resolve, 0));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    expect(pane.phase).toBe("failed");
    expect(pane.error).toContain("INVALID_STATE");
    expect(pane.error).toContain("host memory pressure is CRITICAL; retry shortly");
  });

  it("전체 창을 쓰는 pane 1개를 만들고 지정 셸로 시작한다", async () => {
    useWorkbenchStore.setState({ tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
    const controller = new SessionController({
      client: stubClient,
      registry: new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom }),
      platform: "windows",
      resolveShell: () => ({ program: "C:\\ps.exe", argv: ["-NoLogo"], label: "PowerShell" }),
    });

    controller.newTerminal();

    const state = useWorkbenchStore.getState();
    expect(state.tabs).toHaveLength(1);
    expect(state.tabs[0].kind === "terminal" ? state.tabs[0].root?.kind : null).toBe("leaf"); // 분할 아님 — 전체 창 1개
    expect(Object.keys(state.panes)).toHaveLength(1);
    const pane = Object.values(state.panes)[0];
    expect(state.focusedLeafId).toBe(pane.leafId);
    expect(pane.phase).toBe("starting");

    await new Promise((resolve) => setTimeout(resolve, 0));
    const after = useWorkbenchStore.getState().panes[pane.leafId];
    expect(after.sessionId).toBe("s1");
    expect(after.title).toBe("PowerShell"); // pane 제목 = 프로필 라벨
  });

  it("이미 pane이 있으면 분할로 추가한다(기존 동작 유지)", () => {
    useWorkbenchStore.setState({ tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
    const controller = new SessionController({
      client: stubClient,
      registry: new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom }),
      platform: "windows",
    });
    controller.newTerminal();
    controller.newTerminal(); // 두 번째: focused pane을 좌우 분할

    const state = useWorkbenchStore.getState();
    expect(Object.keys(state.panes)).toHaveLength(2);
    expect(state.tabs[0].kind === "terminal" ? state.tabs[0].root?.kind : null).toBe("split");
  });
});
