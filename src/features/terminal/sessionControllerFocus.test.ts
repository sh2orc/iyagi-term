/**
 * focus 보고(08-pressure-relief §1): 컨트롤러는 "지금 보고 있는 세션"과 "지금
 * 화면에 보이는 세션들"(활성 탭의 살아 있는 pane + 포커스 세션)이 바뀔 때만
 * session.focus를 한 번 보낸다. 같은 값은 다시 보내지 않고, pane에 세션이 나중에
 * 붙어도 그때 보내며, 창이 숨거나 터미널 화면이 아니면 아무것도 보고 있지 않다고
 * 알린다. 데몬의 거절은 UI로 새지 않는다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import type { SessionFocusParams } from "../../generated/SessionFocusParams";
import type { DaemonClient } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import { MockDaemonClient } from "../daemon/mockClient";
import { useWorkbenchStore } from "../../store/workbenchStore";
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

async function flush(times = 16): Promise<void> {
  for (let i = 0; i < times; i++) await Promise.resolve();
}

type LaunchResolver = (outcome: { session_id: string; workload_id: string }) => void;

interface Harness {
  controller: SessionController;
  /** 보낸 session.focus params(순서대로). */
  focusCalls: SessionFocusParams[];
  /** 아직 응답하지 않은 launch(테스트가 세션 배정 시점을 정한다). */
  launches: LaunchResolver[];
}

/**
 * attach 단계는 일부러 실패시킨다 — 컨트롤러가 pane 상태로 흡수하므로
 * focus 보고 경로만 남는다(sessionControllerFirstRun의 stub과 같은 취지).
 */
async function harness(
  sessionFocus?: (params: SessionFocusParams) => Promise<{ focused_session_ids: string[] }>,
): Promise<Harness> {
  useWorkbenchStore.setState({ tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
  const snapshot = await new MockDaemonClient({ resourceIntervalMs: 0 }).systemSnapshot();
  const focusCalls: SessionFocusParams[] = [];
  const launches: LaunchResolver[] = [];
  const client = {
    events: { subscribe: () => ({ dispose: () => undefined }) },
    systemSnapshot: async () => ({ ...snapshot, workloads: [], queue: [] }),
    interventionList: async () => [],
    agentSessionList: async () => [],
    workloadLaunch: () => new Promise<unknown>((resolve) => launches.push(resolve as LaunchResolver)),
    sessionAttach: async () => {
      throw new Error("no transport in this test");
    },
    sessionFocus: async (params: SessionFocusParams) => {
      focusCalls.push(params);
      return sessionFocus ? await sessionFocus(params) : { focused_session_ids: [] };
    },
  } as unknown as DaemonClient;
  const controller = new SessionController({
    client,
    registry: new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom }),
    platform: "darwin",
  });
  controller.start();
  await flush();
  return { controller, focusCalls, launches };
}

let active: SessionController | null = null;
afterEach(() => {
  active?.dispose();
  active = null;
  useWorkbenchStore.setState({ tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
  vi.restoreAllMocks();
});

describe("SessionController focus reporting", () => {
  it("보고 값이 바뀔 때만 보낸다 — 세션 배정·중복 focus·focus 해제", async () => {
    const h = await harness();
    active = h.controller;

    // 1) pane은 생겼지만 아직 세션이 없다 — 데몬의 기본값(없음)과 같으니 침묵.
    h.controller.newTerminal();
    await flush();
    expect(h.focusCalls).toEqual([]);

    // 2) 세션이 나중에 붙으면 그때 보낸다(스토어 구독이 파생값 변화를 잡는다).
    const leafId = Object.keys(useWorkbenchStore.getState().panes)[0];
    h.launches[0]({ session_id: "s1", workload_id: "w1" });
    await flush();
    // 이 시험의 attach는 실패하므로 pane은 살아 있지 않다 — 보이는 세션은 포커스 세션뿐이다.
    expect(h.focusCalls).toEqual([{ session_id: "s1", visible_session_ids: ["s1"] }]);

    // 3) 같은 pane을 다시 focus해도 값이 그대로면 아무것도 보내지 않는다.
    useWorkbenchStore.getState().focusPane(leafId);
    await flush();
    expect(h.focusCalls).toEqual([{ session_id: "s1", visible_session_ids: ["s1"] }]);

    // 4) focus를 아예 놓으면 null.
    useWorkbenchStore.setState({ focusedLeafId: null });
    await flush();
    expect(h.focusCalls).toEqual([
      { session_id: "s1", visible_session_ids: ["s1"] },
      { session_id: null, visible_session_ids: [] },
    ]);
  });

  it("세션이 아직 없는 pane으로 focus가 옮겨가면 null을 보낸다", async () => {
    const h = await harness();
    active = h.controller;

    h.controller.newTerminal();
    await flush();
    h.launches[0]({ session_id: "s1", workload_id: "w1" });
    await flush();
    expect(h.focusCalls).toEqual([{ session_id: "s1", visible_session_ids: ["s1"] }]);

    // 분할로 생긴 새 pane이 focus를 가져간다 — 세션은 아직 없다.
    expect(h.controller.splitFocused("row")).toBe(true);
    await flush();
    expect(h.focusCalls).toEqual([
      { session_id: "s1", visible_session_ids: ["s1"] },
      { session_id: null, visible_session_ids: [] },
    ]);

    // 그 pane에 세션이 붙으면 다시 보낸다.
    h.launches[1]({ session_id: "s2", workload_id: "w2" });
    await flush();
    expect(h.focusCalls).toEqual([
      { session_id: "s1", visible_session_ids: ["s1"] },
      { session_id: null, visible_session_ids: [] },
      { session_id: "s2", visible_session_ids: ["s2"] },
    ]);
  });

  it("데몬이 거절해도 던지지 않고 다음 변화는 계속 보고한다", async () => {
    const debug = vi.spyOn(console, "debug").mockImplementation(() => undefined);
    const h = await harness(async () => {
      throw new RpcClientError("INVALID_ARGUMENT", "session not found");
    });
    active = h.controller;

    h.controller.newTerminal();
    await flush();
    h.launches[0]({ session_id: "s1", workload_id: "w1" });
    await flush();

    expect(h.focusCalls).toEqual([{ session_id: "s1", visible_session_ids: ["s1"] }]);
    expect(debug).toHaveBeenCalled();
    // 실패는 pane 상태나 toast로 새지 않는다(attach 실패만 pane에 남는다).
    expect(useWorkbenchStore.getState().toast).toBeNull();

    useWorkbenchStore.setState({ focusedLeafId: null });
    await flush();
    expect(h.focusCalls).toEqual([
      { session_id: "s1", visible_session_ids: ["s1"] },
      { session_id: null, visible_session_ids: [] },
    ]);
  });

  it("활성 탭의 살아 있는 pane은 포커스가 아니어도 보이는 세션으로 알리고, 탭을 바꾸면 다시 보고한다", async () => {
    const h = await harness();
    active = h.controller;

    h.controller.newTerminal();
    await flush();
    h.launches[0]({ session_id: "s1", workload_id: "w1" });
    await flush();
    expect(h.controller.splitFocused("row")).toBe(true);
    await flush();
    h.launches[1]({ session_id: "s2", workload_id: "w2" });
    await flush();
    const [first, second] = Object.keys(useWorkbenchStore.getState().panes);
    h.focusCalls.length = 0;

    // 두 pane이 모두 살아 있다(이 시험은 attach가 실패하므로 직접 live로 둔다).
    useWorkbenchStore.getState().panePhase(first, "live");
    useWorkbenchStore.getState().panePhase(second, "live");
    await flush();
    expect(h.focusCalls.at(-1)).toEqual({ session_id: "s2", visible_session_ids: ["s1", "s2"] });

    // 다른 탭으로 가면 그 탭에는 pane이 없다 — 보이는 세션도 없다.
    const firstTab = useWorkbenchStore.getState().activeTabId!;
    h.controller.newTab();
    await flush();
    expect(h.focusCalls.at(-1)).toEqual({ session_id: null, visible_session_ids: [] });

    // 돌아오면 다시 둘 다 보인다.
    useWorkbenchStore.getState().setActiveTab(firstTab);
    await flush();
    expect(h.focusCalls.at(-1)?.visible_session_ids).toEqual(["s1", "s2"]);
  });

  it("터미널 화면이 아니거나(설정) 창이 숨으면 아무것도 보고 있지 않다고 알리고, 돌아오면 다시 보고한다", async () => {
    const listeners = new Set<() => void>();
    const fakeDocument = {
      hidden: false,
      addEventListener: (_type: string, listener: () => void) => listeners.add(listener),
      removeEventListener: (_type: string, listener: () => void) => listeners.delete(listener),
    };
    vi.stubGlobal("document", fakeDocument);
    try {
      const h = await harness();
      active = h.controller;
      h.controller.newTerminal();
      await flush();
      h.launches[0]({ session_id: "s1", workload_id: "w1" });
      await flush();
      const leafId = Object.keys(useWorkbenchStore.getState().panes)[0];
      useWorkbenchStore.getState().panePhase(leafId, "live");
      await flush();
      expect(h.focusCalls.at(-1)).toEqual({ session_id: "s1", visible_session_ids: ["s1"] });

      // 설정 화면: 터미널이 가려진다.
      useWorkbenchStore.getState().setPage("settings");
      await flush();
      expect(h.focusCalls.at(-1)).toEqual({ session_id: null, visible_session_ids: [] });
      useWorkbenchStore.getState().setPage("terminal");
      await flush();
      expect(h.focusCalls.at(-1)).toEqual({ session_id: "s1", visible_session_ids: ["s1"] });

      // 창이 숨으면(스토어 변화 없이 문서 표시만 바뀐다) 문서 관찰이 다시 보고한다.
      const before = h.focusCalls.length;
      fakeDocument.hidden = true;
      for (const listener of listeners) listener();
      await flush();
      expect(h.focusCalls.at(-1)).toEqual({ session_id: null, visible_session_ids: [] });
      // 숨은 채 같은 값이면 다시 보내지 않는다.
      for (const listener of listeners) listener();
      await flush();
      expect(h.focusCalls).toHaveLength(before + 1);
      fakeDocument.hidden = false;
      for (const listener of listeners) listener();
      await flush();
      expect(h.focusCalls.at(-1)).toEqual({ session_id: "s1", visible_session_ids: ["s1"] });
    } finally {
      active?.dispose();
      active = null;
      vi.unstubAllGlobals();
      useWorkbenchStore.setState({ page: "terminal" });
    }
  });

  it("stop()으로 구독을 놓으면 더 이상 보고하지 않는다", async () => {
    const h = await harness();
    active = h.controller;

    h.controller.newTerminal();
    await flush();
    h.controller.stop();

    h.launches[0]({ session_id: "s1", workload_id: "w1" });
    await flush();
    expect(h.focusCalls).toEqual([]);
  });
});
