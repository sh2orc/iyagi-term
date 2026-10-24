/**
 * quota 자동 재개 포기(stale) 알림의 "재개" 경로(resumeAgentInPane)와
 * session_hint 판정(matchSessionByHint) — Enter는 오직 이 클릭 경로로만
 * 나가고, live가 아니면 보내지 않는다.
 */

import { afterEach, describe, expect, it } from "vitest";
import type { DaemonClient } from "../daemon/client";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type TerminalLike } from "./registry";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import type { AgentStatus } from "../../generated/AgentStatus";

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
    modes: { bracketedPasteMode: false },
  };
}

function claudeAgent(sessionId: string | null): AgentStatus {
  return {
    agent: "claude",
    pid: 7,
    detected_at_ms: 1,
    session_id: sessionId,
  } as AgentStatus;
}

function pane(leafId: string, viewId: string, agent?: AgentStatus, cwd: string | null = null): PaneMeta {
  return {
    leafId,
    viewId,
    sessionId: `pty-${leafId}`,
    workloadId: null,
    title: "zsh",
    cwd,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    agent: agent ?? null,
  };
}

/** 같은 cwd의 Claude pane 두 개(서로 다른 CLI 세션)와 pane별 전송 기록. */
function setup(panes: Record<string, PaneMeta>) {
  const registry = new TerminalRegistry({
    createTerminal: () => fakeTerminal(),
    createDom: () => ({ className: "", parentElement: null, remove: () => undefined }),
  });
  for (const meta of Object.values(panes)) registry.acquire(meta.viewId);
  useWorkbenchStore.setState({
    panes,
    focusedLeafId: Object.keys(panes)[0],
    modal: null,
    broadcastInput: false,
    toast: null,
  });
  const controller = new SessionController({
    client: {} as unknown as DaemonClient,
    registry,
    platform: "darwin",
  });
  const sent: Record<string, string[]> = {};
  const pipelines = (controller as unknown as { pipelines: Map<string, unknown> }).pipelines;
  for (const meta of Object.values(panes)) {
    sent[meta.viewId] = [];
    pipelines.set(meta.viewId, {
      sendInput: (data: string) => {
        sent[meta.viewId].push(data);
        return true;
      },
    });
  }
  return { controller, sent };
}

afterEach(() => {
  useWorkbenchStore.setState({ panes: {}, focusedLeafId: null, modal: null, toast: null });
});

describe("resumeAgentInPane", () => {
  it("pane으로 이동하고 Enter(\\r)를 그 pane의 파이프라인으로만 보낸다", () => {
    const { controller, sent } = setup({
      "leaf-1": pane("leaf-1", "view-1", claudeAgent("cs-1"), "/repo"),
      "leaf-2": pane("leaf-2", "view-2", claudeAgent("cs-2"), "/repo"),
    });
    expect(controller.resumeAgentInPane("pty-leaf-2")).toBe(true);
    expect(sent["view-2"]).toEqual(["\r"]);
    expect(sent["view-1"]).toEqual([]);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe("leaf-2");
  });

  it("pane이 없으면 보내지 않고 false", () => {
    const { controller, sent } = setup({
      "leaf-1": pane("leaf-1", "view-1", claudeAgent("cs-1"), "/repo"),
    });
    expect(controller.resumeAgentInPane("pty-gone")).toBe(false);
    expect(sent["view-1"]).toEqual([]);
  });

  it("전송이 막히면(live 아님) 토스트로 알리고 false", () => {
    const panes = { "leaf-1": pane("leaf-1", "view-1", claudeAgent("cs-1"), "/repo") };
    const { controller } = setup(panes);
    const pipelines = (controller as unknown as { pipelines: Map<string, unknown> }).pipelines;
    pipelines.set("view-1", {
      sendInput: () => false,
    });
    expect(controller.resumeAgentInPane("pty-leaf-1")).toBe(false);
    expect(useWorkbenchStore.getState().toast).toBeTruthy();
  });
});

describe("matchSessionByHint", () => {
  it("CLI 세션 id로 같은 cwd의 pane을 구분한다", () => {
    const { controller } = setup({
      "leaf-1": pane("leaf-1", "view-1", claudeAgent("cs-1"), "/repo"),
      "leaf-2": pane("leaf-2", "view-2", claudeAgent("cs-2"), "/repo"),
    });
    const match = (controller as unknown as {
      matchSessionByHint: (hint: string | null) => string | null;
    }).matchSessionByHint;
    expect(match("cs-2")).toBe("pty-leaf-2");
    expect(match("cs-1")).toBe("pty-leaf-1");
  });

  it("아는 단서가 없으면 cwd 폴백, 그래도 없으면 null", () => {
    const { controller } = setup({
      "leaf-1": pane("leaf-1", "view-1", claudeAgent(null), "/repo"),
    });
    const match = (controller as unknown as {
      matchSessionByHint: (hint: string | null) => string | null;
    }).matchSessionByHint;
    expect(match("/repo")).toBe("pty-leaf-1");
    expect(match("/elsewhere")).toBeNull();
    expect(match(null)).toBeNull();
  });
});
