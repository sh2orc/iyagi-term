/**
 * 붙여넣기 입구 두 개 — 네이티브 paste 이벤트(pasteText)와 클립보드 읽기
 * (pasteFromClipboard) — 가 같은 규칙(1 MiB 한도·개행 정규화·pane별 bracketed
 * paste)으로 가리킨 pane에 보내는지.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import type { DaemonClient } from "../daemon/client";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type TerminalLike } from "./registry";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { PASTE_MAX_BYTES } from "../security/paste";
import { t } from "../../i18n";

function fakeTerminal(bracketedPasteMode: boolean): TerminalLike {
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
    modes: { bracketedPasteMode },
  };
}

function pane(leafId: string, viewId: string): PaneMeta {
  return {
    leafId,
    viewId,
    sessionId: null,
    workloadId: null,
    title: "zsh",
    cwd: null,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
  };
}

/** leaf-1(view-1, 괄호 모드 꺼짐, 초점)·leaf-2(view-2, 괄호 모드 켜짐)와 pane별 전송 기록. */
function setup() {
  const terminals = [fakeTerminal(false), fakeTerminal(true)];
  const registry = new TerminalRegistry({
    createTerminal: () => terminals.shift() ?? fakeTerminal(false),
    createDom: () => ({ className: "", parentElement: null, remove: () => undefined }),
  });
  registry.acquire("view-1");
  registry.acquire("view-2");
  useWorkbenchStore.setState({
    panes: { "leaf-1": pane("leaf-1", "view-1"), "leaf-2": pane("leaf-2", "view-2") },
    focusedLeafId: "leaf-1",
    modal: null,
    broadcastInput: false,
  });
  const controller = new SessionController({ client: {} as unknown as DaemonClient, registry, platform: "darwin" });
  const sent: Record<string, string[]> = { "view-1": [], "view-2": [] };
  const pipelines = (controller as unknown as { pipelines: Map<string, unknown> }).pipelines;
  for (const viewId of Object.keys(sent)) {
    pipelines.set(viewId, {
      sendLargeInput: async (data: string) => {
        sent[viewId].push(data);
        return true;
      },
    });
  }
  return { controller, sent };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  useWorkbenchStore.setState({ panes: {}, focusedLeafId: null, modal: null, toast: null });
});

describe("SessionController paste", () => {
  it("pasteText sends to the pane that received the paste, in that pane's paste mode", () => {
    const { controller, sent } = setup();
    controller.pasteText("echo hi\nls", "leaf-2");
    expect(sent["view-2"]).toEqual(["\x1b[200~echo hi\rls\x1b[201~"]);
    expect(sent["view-1"]).toEqual([]);
  });

  it("pasteText without a leafId uses the focused pane", () => {
    const { controller, sent } = setup();
    controller.pasteText("a\r\nb\x1b[31m");
    expect(sent["view-1"]).toEqual(["a\rb[31m"]);
  });

  it("rejects more than 1 MiB with the notice and sends nothing", () => {
    const { controller, sent } = setup();
    controller.pasteText("x".repeat(PASTE_MAX_BYTES + 1), "leaf-1");
    expect(useWorkbenchStore.getState().modal).toMatchObject({ kind: "notice" });
    expect(sent["view-1"]).toEqual([]);
  });

  it("ignores empty text and pastes while a modal is open", () => {
    const { controller, sent } = setup();
    controller.pasteText("", "leaf-1");
    useWorkbenchStore.setState({ modal: { kind: "palette" } });
    controller.pasteText("ls", "leaf-1");
    expect(sent["view-1"]).toEqual([]);
  });

  it("pasteFromClipboard reads the clipboard into the same path", async () => {
    const { controller, sent } = setup();
    vi.stubGlobal("navigator", { clipboard: { readText: vi.fn(async () => "pwd") } });
    await controller.pasteFromClipboard();
    expect(sent["view-1"]).toEqual(["pwd"]);
  });

  it("pasteFromClipboard reports a failed read through the toast", async () => {
    vi.useFakeTimers();
    const { controller, sent } = setup();
    const readText = vi.fn(async (): Promise<string> => {
      throw new Error("denied");
    });
    vi.stubGlobal("navigator", { clipboard: { readText } });
    await controller.pasteFromClipboard();
    expect(useWorkbenchStore.getState().toast).toBe(t("terminal.clipboard.readFailed"));
    expect(sent["view-1"]).toEqual([]);
  });
});
